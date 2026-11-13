//! Bindings, assignment targets and destructuring.

use oxc_ast::ast::*;
use oxc_span::GetSpan;

use dj_ir::{Atom, BinaryOp, Constant, ErrorKind, InstKind, Span as IrSpan, Temp, TermKind, UnaryOp};

use crate::builder::{Lowerer, Resolved, sp};

/// An assignment target whose sub-expressions were already evaluated.
pub(crate) enum Prepared<'b, 'a> {
    Name(&'b str, IrSpan),
    Named { obj: Temp, name: Atom, span: IrSpan },
    Keyed { obj: Temp, key: Temp, span: IrSpan },
    Super { key: Temp, span: IrSpan },
    ArrayPat(&'b ArrayAssignmentTarget<'a>),
    ObjectPat(&'b ObjectAssignmentTarget<'a>),
    Invalid,
}

/// Property key of an object pattern / literal / class member.
pub(crate) enum Key {
    Named(Atom),
    Computed(Temp),
}

/// Canonical property-name string of a numeric literal key, when cheap to
/// compute exactly (integers below 2^53).
pub(crate) fn numeric_key(n: f64) -> Option<String> {
    if n.fract() == 0.0 && n.abs() < 9007199254740992.0 && !(n == 0.0 && n.is_sign_negative()) {
        Some(format!("{}", n as i64))
    } else {
        None
    }
}

impl<'s> Lowerer<'s> {
    /// Initialize a declared binding (no TDZ / const checks).
    pub(crate) fn init_name(&mut self, name: &str, value: Temp, span: IrSpan) {
        match self.resolve(name, u32::MAX) {
            Resolved::Local { local, .. } => self.store_local(local, value, span),
            Resolved::Upval { upval, .. } => self.emit(InstKind::StoreUpval { upval, src: value }, span),
            Resolved::Global => self.emit(InstKind::StoreGlobal { name: name.into(), src: value }, span),
        }
    }

    /// `name = value` with TDZ and const checks.
    pub(crate) fn assign_name(&mut self, name: &str, value: Temp, span: IrSpan) {
        match self.resolve(name, span.start) {
            Resolved::Local { local, check, immutable } => {
                if check {
                    let t = self.temp();
                    self.emit(InstKind::LoadLocal { dst: t, local, check_tdz: true }, span);
                }
                if immutable {
                    self.throw_const_assign(span);
                } else {
                    self.store_local(local, value, span);
                }
            }
            Resolved::Upval { upval, check, immutable } => {
                if check {
                    let t = self.temp();
                    self.emit(InstKind::LoadUpval { dst: t, upval, check_tdz: true }, span);
                }
                if immutable {
                    self.throw_const_assign(span);
                } else {
                    self.emit(InstKind::StoreUpval { upval, src: value }, span);
                }
            }
            Resolved::Global => {
                if name == "undefined" || name == "NaN" || name == "Infinity" {
                    self.throw_error(ErrorKind::TypeError, &format!("Cannot assign to read only property '{name}'"), span);
                } else {
                    self.emit(InstKind::StoreGlobal { name: name.into(), src: value }, span);
                }
            }
        }
    }

    pub(crate) fn throw_const_assign(&mut self, span: IrSpan) {
        self.throw_error(ErrorKind::TypeError, "Assignment to constant variable.", span);
    }

    pub(crate) fn throw_error(&mut self, kind: ErrorKind, msg: &str, span: IrSpan) {
        self.terminate(TermKind::ThrowError { kind, message: msg.into() }, span);
    }

    // --- binding patterns (declarations, parameters) -------------------------

    pub(crate) fn bind_pattern(&mut self, pat: &BindingPattern<'_>, value: Temp) {
        match pat {
            BindingPattern::BindingIdentifier(id) => self.init_name(id.name.as_str(), value, sp(id.span)),
            BindingPattern::AssignmentPattern(ap) => {
                let hint = match &ap.left {
                    BindingPattern::BindingIdentifier(id) => Some(id.name.as_str()),
                    _ => None,
                };
                let v = self.apply_default(value, &ap.right, hint, sp(ap.span));
                self.bind_pattern(&ap.left, v);
            }
            BindingPattern::ObjectPattern(op) => {
                let span = sp(op.span);
                let has_rest = op.rest.is_some();
                if op.properties.is_empty() {
                    self.require_object_coercible(value, span);
                }
                let mut excluded = Vec::new();
                for prop in &op.properties {
                    let pspan = sp(prop.span);
                    let key = self.prop_key(&prop.key, prop.computed);
                    let v = self.get_key(value, &key, pspan);
                    if has_rest {
                        excluded.push(self.key_temp(&key, pspan));
                    }
                    self.bind_pattern(&prop.value, v);
                }
                if let Some(r) = &op.rest {
                    let rs = sp(r.span);
                    let o = self.new_object(rs);
                    self.emit(InstKind::CopyDataProps { target: o, source: value, excluded }, rs);
                    self.bind_pattern(&r.argument, o);
                }
            }
            BindingPattern::ArrayPattern(ap) => {
                let span = sp(ap.span);
                let mut elems: Vec<Option<&BindingPattern<'_>>> = Vec::new();
                for e in &ap.elements {
                    elems.push(e.as_ref());
                }
                self.destructure_array(
                    value,
                    elems.len(),
                    ap.rest.is_some(),
                    span,
                    |this, i, v| {
                        if let Some(p) = elems[i] {
                            this.bind_pattern(p, v);
                        }
                    },
                    |_, i| elems[i].is_some(),
                    |this, arr| {
                        if let Some(r) = &ap.rest {
                            this.bind_pattern(&r.argument, arr);
                        }
                    },
                );
            }
        }
    }

    fn require_object_coercible(&mut self, value: Temp, span: IrSpan) {
        let n = self.konst(Constant::Null, span);
        let c = self.binary(BinaryOp::Eq, value, n, span);
        let bad = self.new_block();
        let ok = self.new_block();
        self.branch(c, bad, ok, span);
        self.switch_to(bad);
        self.throw_error(ErrorKind::TypeError, "Cannot destructure 'undefined' or 'null'.", span);
        self.switch_to(ok);
    }

    pub(crate) fn new_object(&mut self, span: IrSpan) -> Temp {
        let o = self.temp();
        self.emit(InstKind::NewObject { dst: o }, span);
        o
    }

    pub(crate) fn key_temp(&mut self, key: &Key, span: IrSpan) -> Temp {
        match key {
            Key::Named(n) => self.konst(Constant::String(n.clone()), span),
            Key::Computed(t) => *t,
        }
    }

    pub(crate) fn get_key(&mut self, obj: Temp, key: &Key, span: IrSpan) -> Temp {
        let dst = self.temp();
        match key {
            Key::Named(n) => self.emit(InstKind::GetNamed { dst, obj, name: n.clone() }, span),
            Key::Computed(k) => self.emit(InstKind::GetProp { dst, obj, key: *k }, span),
        }
        dst
    }

    pub(crate) fn prop_key(&mut self, key: &PropertyKey<'_>, computed: bool) -> Key {
        let span = sp(key.span());
        if !computed {
            match key {
                PropertyKey::StaticIdentifier(id) => return Key::Named(id.name.as_str().into()),
                PropertyKey::PrivateIdentifier(_) => {
                    self.unsupported(span, "private class members (#names)");
                    return Key::Named("".into());
                }
                PropertyKey::StringLiteral(s) => {
                    return Key::Named(self.js_str(s.value.as_str(), span).into());
                }
                PropertyKey::NumericLiteral(n) => {
                    return match numeric_key(n.value) {
                        Some(s) => Key::Named(s.as_str().into()),
                        None => {
                            let t = self.konst(Constant::Number(n.value), span);
                            Key::Computed(self.unary(UnaryOp::ToPropertyKey, t, span))
                        }
                    };
                }
                _ => {}
            }
        }
        match key.as_expression() {
            Some(e) => {
                let v = self.lower_expr(e);
                Key::Computed(self.unary(UnaryOp::ToPropertyKey, v, span))
            }
            None => {
                self.unsupported(span, "this property key");
                Key::Named("".into())
            }
        }
    }

    /// Generic array destructuring through the iterator protocol.
    ///
    /// The iterator is closed when the pattern does not exhaust it, and when
    /// binding throws (errors from `return()` are then suppressed). A throw
    /// from `next()` itself does not close the iterator.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn destructure_array(
        &mut self,
        value: Temp,
        count: usize,
        has_rest: bool,
        span: IrSpan,
        mut bind: impl FnMut(&mut Self, usize, Temp),
        mut wants: impl FnMut(&mut Self, usize) -> bool,
        mut bind_rest: impl FnMut(&mut Self, Temp),
    ) {
        let iter_l = self.synthetic("%iter", span);
        let done_l = self.synthetic("%done", span);
        let exc_l = self.synthetic("%exc", span);
        let it = self.temp();
        self.emit(InstKind::GetIterator { dst: it, iterable: value }, span);
        self.store_local(iter_l, it, span);
        let f = self.konst(Constant::Bool(false), span);
        self.store_local(done_l, f, span);

        let outer_h = self.fbr().handler;
        let close_h = self.new_block_h(outer_h);
        let region = self.new_block_h(Some(close_h));
        self.jump(region, span);
        self.fb().handler = Some(close_h);
        self.switch_to(region);

        for i in 0..count {
            let val_l = self.synthetic("%elem", span);
            let d = self.load_local(done_l, false, span);
            let step = self.new_block();
            let got = self.new_block();
            let skip = self.new_block();
            let cont = self.new_block();
            self.branch(d, skip, step, span);
            self.switch_to(step);
            let t = self.konst(Constant::Bool(true), span);
            self.store_local(done_l, t, span);
            let i2 = self.load_local(iter_l, false, span);
            let done = self.temp();
            let v = self.temp();
            self.emit(InstKind::IterNext { done, value: v, iter: i2 }, span);
            self.store_local(done_l, done, span);
            self.branch(done, skip, got, span);
            self.switch_to(got);
            self.store_local(val_l, v, span);
            self.jump(cont, span);
            self.switch_to(skip);
            let u = self.undef(span);
            self.store_local(val_l, u, span);
            self.jump(cont, span);
            self.switch_to(cont);
            if wants(self, i) {
                let ev = self.load_local(val_l, false, span);
                bind(self, i, ev);
            }
        }
        if has_rest {
            let arr = self.temp();
            self.emit(InstKind::NewArray { dst: arr }, span);
            let arr_l = self.synthetic("%rest", span);
            self.store_local(arr_l, arr, span);
            let head = self.new_block();
            let step = self.new_block();
            let push = self.new_block();
            let end = self.new_block();
            self.jump(head, span);
            self.switch_to(head);
            let d = self.load_local(done_l, false, span);
            self.branch(d, end, step, span);
            self.switch_to(step);
            let t = self.konst(Constant::Bool(true), span);
            self.store_local(done_l, t, span);
            let i2 = self.load_local(iter_l, false, span);
            let done = self.temp();
            let v = self.temp();
            self.emit(InstKind::IterNext { done, value: v, iter: i2 }, span);
            self.store_local(done_l, done, span);
            self.branch(done, end, push, span);
            self.switch_to(push);
            let a = self.load_local(arr_l, false, span);
            self.emit(InstKind::ArrayPush { array: a, value: v }, span);
            self.jump(head, span);
            self.switch_to(end);
            let a = self.load_local(arr_l, false, span);
            bind_rest(self, a);
        }
        self.fb().handler = outer_h;
        // Normal completion: close when not exhausted.
        let after = self.new_block_h(outer_h);
        let close = self.new_block_h(outer_h);
        let d = self.load_local(done_l, false, span);
        self.branch(d, after, close, span);
        self.switch_to(close);
        let i3 = self.load_local(iter_l, false, span);
        self.emit(InstKind::IterClose { iter: i3 }, span);
        self.jump(after, span);

        // Abrupt completion while binding.
        self.switch_to(close_h);
        let e = self.temp();
        self.emit(InstKind::CatchValue { dst: e }, span);
        self.store_local(exc_l, e, span);
        let swallow = self.new_block_h(outer_h);
        let closing = self.new_block_h(Some(swallow));
        let rethrow = self.new_block_h(outer_h);
        let d = self.load_local(done_l, false, span);
        self.branch(d, rethrow, closing, span);
        self.switch_to(closing);
        let i4 = self.load_local(iter_l, false, span);
        self.emit(InstKind::IterClose { iter: i4 }, span);
        self.jump(rethrow, span);
        self.switch_to(swallow);
        let ign = self.temp();
        self.emit(InstKind::CatchValue { dst: ign }, span);
        self.jump(rethrow, span);
        self.switch_to(rethrow);
        let ev = self.load_local(exc_l, false, span);
        self.terminate(TermKind::Throw(ev), span);

        self.switch_to(after);
    }

    // --- assignment targets ---------------------------------------------------

    /// Evaluate the reference part of an assignment target. `convert_key`
    /// applies ToPropertyKey once (compound assignment / update).
    pub(crate) fn prepare_target<'b, 'a>(&mut self, t: &'b AssignmentTarget<'a>, convert_key: bool) -> Prepared<'b, 'a> {
        match t {
            AssignmentTarget::ArrayAssignmentTarget(a) => Prepared::ArrayPat(a),
            AssignmentTarget::ObjectAssignmentTarget(o) => Prepared::ObjectPat(o),
            other => match other.as_simple_assignment_target() {
                Some(s) => self.prepare_simple(s, convert_key),
                None => Prepared::Invalid,
            },
        }
    }

    pub(crate) fn prepare_simple<'b, 'a>(
        &mut self,
        t: &'b SimpleAssignmentTarget<'a>,
        convert_key: bool,
    ) -> Prepared<'b, 'a> {
        match t {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => Prepared::Name(id.name.as_str(), sp(id.span)),
            SimpleAssignmentTarget::TSAsExpression(e) => self.prepare_expr(&e.expression, convert_key),
            SimpleAssignmentTarget::TSSatisfiesExpression(e) => self.prepare_expr(&e.expression, convert_key),
            SimpleAssignmentTarget::TSNonNullExpression(e) => self.prepare_expr(&e.expression, convert_key),
            SimpleAssignmentTarget::TSTypeAssertion(e) => self.prepare_expr(&e.expression, convert_key),
            SimpleAssignmentTarget::StaticMemberExpression(m) => self.prepare_static_member(m),
            SimpleAssignmentTarget::ComputedMemberExpression(m) => self.prepare_computed_member(m, convert_key),
            SimpleAssignmentTarget::PrivateFieldExpression(p) => {
                self.unsupported(sp(p.span), "private class members (#names)");
                Prepared::Invalid
            }
        }
    }

    fn prepare_expr<'b, 'a>(&mut self, e: &'b Expression<'a>, convert_key: bool) -> Prepared<'b, 'a> {
        match e {
            Expression::Identifier(id) => Prepared::Name(id.name.as_str(), sp(id.span)),
            Expression::StaticMemberExpression(m) => self.prepare_static_member(m),
            Expression::ComputedMemberExpression(m) => self.prepare_computed_member(m, convert_key),
            Expression::TSAsExpression(x) => self.prepare_expr(&x.expression, convert_key),
            Expression::TSSatisfiesExpression(x) => self.prepare_expr(&x.expression, convert_key),
            Expression::TSNonNullExpression(x) => self.prepare_expr(&x.expression, convert_key),
            Expression::TSTypeAssertion(x) => self.prepare_expr(&x.expression, convert_key),
            Expression::ParenthesizedExpression(x) => self.prepare_expr(&x.expression, convert_key),
            other => {
                self.semantic(sp(other.span()), "Invalid assignment target");
                Prepared::Invalid
            }
        }
    }

    fn prepare_static_member<'b, 'a>(&mut self, m: &'b StaticMemberExpression<'a>) -> Prepared<'b, 'a> {
        let span = sp(m.span);
        if m.optional {
            self.semantic(span, "Invalid left-hand side in assignment (optional chain)");
            return Prepared::Invalid;
        }
        if let Expression::Super(s) = &m.object {
            if !self.check_super_property(sp(s.span)) {
                return Prepared::Invalid;
            }
            let key = self.str_const(m.property.name.as_str(), span);
            return Prepared::Super { key, span };
        }
        let obj = self.lower_expr(&m.object);
        Prepared::Named { obj, name: m.property.name.as_str().into(), span }
    }

    fn prepare_computed_member<'b, 'a>(
        &mut self,
        m: &'b ComputedMemberExpression<'a>,
        convert_key: bool,
    ) -> Prepared<'b, 'a> {
        let span = sp(m.span);
        if m.optional {
            self.semantic(span, "Invalid left-hand side in assignment (optional chain)");
            return Prepared::Invalid;
        }
        if let Expression::Super(s) = &m.object {
            if !self.check_super_property(sp(s.span)) {
                return Prepared::Invalid;
            }
            let k = self.lower_expr(&m.expression);
            let key = self.unary(UnaryOp::ToPropertyKey, k, span);
            return Prepared::Super { key, span };
        }
        let obj = self.lower_expr(&m.object);
        let k = self.lower_expr(&m.expression);
        let key = if convert_key { self.unary(UnaryOp::ToPropertyKey, k, span) } else { k };
        Prepared::Keyed { obj, key, span }
    }

    pub(crate) fn load_prepared(&mut self, p: &Prepared<'_, '_>) -> Temp {
        match p {
            Prepared::Name(n, span) => self.load_name(n, *span),
            Prepared::Named { obj, name, span } => {
                let dst = self.temp();
                self.emit(InstKind::GetNamed { dst, obj: *obj, name: name.clone() }, *span);
                dst
            }
            Prepared::Keyed { obj, key, span } => {
                let dst = self.temp();
                self.emit(InstKind::GetProp { dst, obj: *obj, key: *key }, *span);
                dst
            }
            Prepared::Super { key, span } => {
                let dst = self.temp();
                self.emit(InstKind::SuperGet { dst, key: *key }, *span);
                dst
            }
            Prepared::ArrayPat(_) | Prepared::ObjectPat(_) | Prepared::Invalid => self.undef(IrSpan::DUMMY),
        }
    }

    pub(crate) fn store_prepared(&mut self, p: Prepared<'_, '_>, value: Temp, span: IrSpan) {
        match p {
            Prepared::Name(n, s) => self.assign_name(n, value, s),
            Prepared::Named { obj, name, span } => self.emit(InstKind::SetNamed { obj, name, value }, span),
            Prepared::Keyed { obj, key, span } => self.emit(InstKind::SetProp { obj, key, value }, span),
            Prepared::Super { key, span } => self.emit(InstKind::SuperSet { key, value }, span),
            Prepared::ArrayPat(a) => self.assign_array_pattern(a, value),
            Prepared::ObjectPat(o) => self.assign_object_pattern(o, value),
            Prepared::Invalid => {
                let _ = span;
            }
        }
    }

    fn assign_maybe_default(&mut self, t: &AssignmentTargetMaybeDefault<'_>, prepared: Prepared<'_, '_>, value: Temp) {
        let span = sp(t.span());
        let v = match t {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                let hint = match &d.binding {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.as_str()),
                    _ => None,
                };
                self.apply_default(value, &d.init, hint, span)
            }
            _ => value,
        };
        self.store_prepared(prepared, v, span);
    }

    fn prepare_maybe_default<'b, 'a>(&mut self, t: &'b AssignmentTargetMaybeDefault<'a>) -> Prepared<'b, 'a> {
        match t {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => self.prepare_target(&d.binding, false),
            other => match other.as_assignment_target() {
                Some(t) => self.prepare_target(t, false),
                None => Prepared::Invalid,
            },
        }
    }

    fn assign_array_pattern(&mut self, a: &ArrayAssignmentTarget<'_>, value: Temp) {
        let span = sp(a.span);
        let elems: Vec<Option<&AssignmentTargetMaybeDefault<'_>>> = a.elements.iter().map(|e| e.as_ref()).collect();
        // Targets are evaluated right before their element is read; keep the
        // prepared reference parts in synthetic locals across the iterator steps.
        self.destructure_array(
            value,
            elems.len(),
            a.rest.is_some(),
            span,
            |this, i, v| {
                if let Some(t) = elems[i] {
                    let p = this.prepare_maybe_default(t);
                    this.assign_maybe_default(t, p, v);
                }
            },
            |_, i| elems[i].is_some(),
            |this, arr| {
                if let Some(r) = &a.rest {
                    let p = this.prepare_target(&r.target, false);
                    this.store_prepared(p, arr, span);
                }
            },
        );
    }

    fn assign_object_pattern(&mut self, o: &ObjectAssignmentTarget<'_>, value: Temp) {
        let span = sp(o.span);
        if o.properties.is_empty() {
            self.require_object_coercible(value, span);
        }
        let has_rest = o.rest.is_some();
        let mut excluded = Vec::new();
        for prop in &o.properties {
            match prop {
                AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                    let pspan = sp(p.span);
                    let name = p.binding.name.as_str();
                    let key = Key::Named(name.into());
                    let v = self.get_key(value, &key, pspan);
                    if has_rest {
                        excluded.push(self.key_temp(&key, pspan));
                    }
                    let v = match &p.init {
                        Some(d) => self.apply_default(v, d, Some(name), pspan),
                        None => v,
                    };
                    self.assign_name(name, v, sp(p.binding.span));
                }
                AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                    let pspan = sp(p.span);
                    let key = self.prop_key(&p.name, p.computed);
                    let prepared = self.prepare_maybe_default(&p.binding);
                    let v = self.get_key(value, &key, pspan);
                    if has_rest {
                        excluded.push(self.key_temp(&key, pspan));
                    }
                    self.assign_maybe_default(&p.binding, prepared, v);
                }
            }
        }
        if let Some(r) = &o.rest {
            let rs = sp(r.span);
            let p = self.prepare_target(&r.target, false);
            let obj = self.new_object(rs);
            self.emit(InstKind::CopyDataProps { target: obj, source: value, excluded }, rs);
            self.store_prepared(p, obj, rs);
        }
    }
}

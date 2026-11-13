//! Expressions.

use oxc_ast::ast::*;
use oxc_span::GetSpan;

use dj_ir::{
    BinaryOp, Constant, FunctionKind, InstKind, MethodKind, Span as IrSpan, Temp, UnaryOp,
};

use crate::builder::{Lowerer, Resolved, sp};
use crate::lower_pattern::{Key, Prepared};

fn binop(op: BinaryOperator) -> BinaryOp {
    use BinaryOperator as B;
    match op {
        B::Equality => BinaryOp::Eq,
        B::Inequality => BinaryOp::Ne,
        B::StrictEquality => BinaryOp::StrictEq,
        B::StrictInequality => BinaryOp::StrictNe,
        B::LessThan => BinaryOp::Lt,
        B::LessEqualThan => BinaryOp::Le,
        B::GreaterThan => BinaryOp::Gt,
        B::GreaterEqualThan => BinaryOp::Ge,
        B::Addition => BinaryOp::Add,
        B::Subtraction => BinaryOp::Sub,
        B::Multiplication => BinaryOp::Mul,
        B::Division => BinaryOp::Div,
        B::Remainder => BinaryOp::Mod,
        B::Exponential => BinaryOp::Exp,
        B::ShiftLeft => BinaryOp::Shl,
        B::ShiftRight => BinaryOp::Shr,
        B::ShiftRightZeroFill => BinaryOp::UShr,
        B::BitwiseOR => BinaryOp::BitOr,
        B::BitwiseXOR => BinaryOp::BitXor,
        B::BitwiseAnd => BinaryOp::BitAnd,
        B::In => BinaryOp::In,
        B::Instanceof => BinaryOp::InstanceOf,
    }
}

fn compound_op(op: AssignmentOperator) -> Option<BinaryOp> {
    use AssignmentOperator as A;
    Some(match op {
        A::Addition => BinaryOp::Add,
        A::Subtraction => BinaryOp::Sub,
        A::Multiplication => BinaryOp::Mul,
        A::Division => BinaryOp::Div,
        A::Remainder => BinaryOp::Mod,
        A::Exponential => BinaryOp::Exp,
        A::ShiftLeft => BinaryOp::Shl,
        A::ShiftRight => BinaryOp::Shr,
        A::ShiftRightZeroFill => BinaryOp::UShr,
        A::BitwiseOR => BinaryOp::BitOr,
        A::BitwiseXOR => BinaryOp::BitXor,
        A::BitwiseAnd => BinaryOp::BitAnd,
        _ => return None,
    })
}

/// Strip TypeScript-only expression wrappers.
fn unwrap_ts<'b, 'a>(mut e: &'b Expression<'a>) -> &'b Expression<'a> {
    loop {
        e = match e {
            Expression::TSAsExpression(x) => &x.expression,
            Expression::TSSatisfiesExpression(x) => &x.expression,
            Expression::TSTypeAssertion(x) => &x.expression,
            Expression::TSNonNullExpression(x) => &x.expression,
            Expression::TSInstantiationExpression(x) => &x.expression,
            Expression::ParenthesizedExpression(x) => &x.expression,
            _ => return e,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Logic {
    And,
    Or,
    Nullish,
}

impl<'s> Lowerer<'s> {
    /// String literal value; lone surrogates cannot be represented and are rejected.
    pub(crate) fn js_str(&mut self, s: Option<&str>, span: IrSpan) -> String {
        match s {
            Some(s) => s.to_string(),
            None => {
                self.unsupported(span, "strings containing lone surrogates");
                String::new()
            }
        }
    }

    pub(crate) fn lower_expr(&mut self, e: &Expression<'_>) -> Temp {
        self.lower_expr_named(e, None)
    }

    /// Lower with a name for anonymous function / class expressions.
    pub(crate) fn lower_expr_named(&mut self, e: &Expression<'_>, name: Option<&str>) -> Temp {
        let span = sp(e.span());
        match e {
            Expression::BooleanLiteral(b) => self.konst(Constant::Bool(b.value), span),
            Expression::NullLiteral(_) => self.konst(Constant::Null, span),
            Expression::NumericLiteral(n) => self.konst(Constant::Number(n.value), span),
            Expression::BigIntLiteral(_) => self.unsupported_expr(span, "BigInt literals"),
            Expression::RegExpLiteral(_) => self.unsupported_expr(span, "regular expression literals"),
            Expression::StringLiteral(s) => {
                let v = self.js_str(s.value.as_str(), span);
                self.konst(Constant::String(v.as_str().into()), span)
            }
            Expression::TemplateLiteral(t) => self.lower_template(t),
            Expression::Identifier(id) => self.load_name(id.name.as_str(), span),
            Expression::Super(_) => {
                self.semantic(span, "'super' keyword unexpected here");
                self.undef(span)
            }
            Expression::ArrayExpression(a) => self.lower_array(&a.elements, span),
            Expression::ArrowFunctionExpression(a) => {
                let fid = self.lower_arrow(a, name.unwrap_or(""));
                let t = self.temp();
                self.emit(InstKind::MakeClosure { dst: t, func: fid }, span);
                t
            }
            Expression::AssignmentExpression(a) => self.lower_assign(a),
            Expression::AwaitExpression(a) => {
                let v = self.lower_expr(&a.argument);
                self.check_await(span);
                let dst = self.temp();
                self.emit(InstKind::Await { dst, value: v }, span);
                dst
            }
            Expression::BinaryExpression(b) => {
                let l = self.lower_expr(&b.left);
                let r = self.lower_expr(&b.right);
                self.binary(binop(b.operator), l, r, span)
            }
            Expression::CallExpression(c) => self.lower_call(c),
            Expression::ChainExpression(c) => self.lower_chain(c),
            Expression::ClassExpression(c) => self.lower_class(c, name),
            Expression::ConditionalExpression(c) => {
                let r = self.join_local(span);
                let t = self.lower_expr(&c.test);
                let a = self.new_block();
                let b = self.new_block();
                let end = self.new_block();
                self.branch(t, a, b, span);
                self.switch_to(a);
                let v = self.lower_expr_named(&c.consequent, name);
                self.store_local(r, v, span);
                self.jump(end, span);
                self.switch_to(b);
                let v = self.lower_expr_named(&c.alternate, name);
                self.store_local(r, v, span);
                self.jump(end, span);
                self.switch_to(end);
                self.load_local(r, false, span)
            }
            Expression::FunctionExpression(f) => {
                let own = f.id.as_ref().map(|i| i.name.as_str());
                let fname = own.or(name).unwrap_or("");
                let fid = self.lower_fn_node(f, fname, FunctionKind::Normal, own);
                let t = self.temp();
                self.emit(InstKind::MakeClosure { dst: t, func: fid }, span);
                t
            }
            Expression::ImportExpression(_) => self.unsupported_expr(span, "dynamic import()"),
            Expression::LogicalExpression(l) => {
                let kind = match l.operator {
                    LogicalOperator::And => Logic::And,
                    LogicalOperator::Or => Logic::Or,
                    LogicalOperator::Coalesce => Logic::Nullish,
                };
                let left = self.lower_expr(&l.left);
                let r = self.join_local(span);
                self.store_local(r, left, span);
                let rhs = self.new_block();
                let end = self.new_block();
                self.logic_branch(kind, left, rhs, end, span);
                self.switch_to(rhs);
                let v = self.lower_expr_named(&l.right, name);
                self.store_local(r, v, span);
                self.jump(end, span);
                self.switch_to(end);
                self.load_local(r, false, span)
            }
            Expression::NewExpression(n) => {
                let callee = self.lower_expr(&n.callee);
                let dst = self.temp();
                if n.arguments.iter().any(|a| matches!(a, Argument::SpreadElement(_))) {
                    let args = self.lower_args_array(&n.arguments, span);
                    self.emit(InstKind::NewSpread { dst, callee, args }, span);
                } else {
                    let args = self.lower_args(&n.arguments);
                    self.emit(InstKind::New { dst, callee, args }, span);
                }
                dst
            }
            Expression::ObjectExpression(o) => self.lower_object(o),
            Expression::ParenthesizedExpression(p) => self.lower_expr_named(&p.expression, name),
            Expression::SequenceExpression(s) => {
                let mut last = None;
                for e in &s.expressions {
                    last = Some(self.lower_expr(e));
                }
                last.unwrap_or_else(|| self.undef(span))
            }
            Expression::TaggedTemplateExpression(_) => self.unsupported_expr(span, "tagged templates"),
            Expression::ThisExpression(_) => self.lower_this(span),
            Expression::UnaryExpression(u) => self.lower_unary(u),
            Expression::UpdateExpression(u) => self.lower_update(u),
            Expression::YieldExpression(_) => self.unsupported_expr(span, "generators (yield)"),
            Expression::PrivateInExpression(_) => self.unsupported_expr(span, "private class members (#names)"),
            Expression::ImportMeta(_) => self.unsupported_expr(span, "import.meta"),
            Expression::NewTarget(_) => self.lower_new_target(span),
            Expression::JSXElement(_) | Expression::JSXFragment(_) => self.unsupported_expr(span, "JSX"),
            Expression::TSAsExpression(x) => self.lower_expr_named(&x.expression, name),
            Expression::TSSatisfiesExpression(x) => self.lower_expr_named(&x.expression, name),
            Expression::TSTypeAssertion(x) => self.lower_expr_named(&x.expression, name),
            Expression::TSNonNullExpression(x) => self.lower_expr_named(&x.expression, name),
            Expression::TSInstantiationExpression(x) => self.lower_expr_named(&x.expression, name),
            Expression::V8IntrinsicExpression(_) => self.unsupported_expr(span, "V8 intrinsics"),
            Expression::StaticMemberExpression(m) => self.lower_static_member(m),
            Expression::ComputedMemberExpression(m) => self.lower_computed_member(m),
            Expression::PrivateFieldExpression(_) => self.unsupported_expr(span, "private class members (#names)"),
        }
    }

    fn unsupported_expr(&mut self, span: IrSpan, what: &str) -> Temp {
        self.unsupported(span, what);
        self.undef(span)
    }

    fn check_await(&mut self, span: IrSpan) {
        let f = &mut self.fb().f;
        if f.kind == FunctionKind::Script {
            f.is_async = true;
        } else if !f.is_async {
            self.semantic(span, "'await' expressions are only allowed within async functions");
        }
    }

    /// Branch to `rhs` when the right operand must be evaluated, else to `end`.
    fn logic_branch(&mut self, kind: Logic, left: Temp, rhs: dj_ir::BlockId, end: dj_ir::BlockId, span: IrSpan) {
        match kind {
            Logic::And => self.branch(left, rhs, end, span),
            Logic::Or => self.branch(left, end, rhs, span),
            Logic::Nullish => {
                let n = self.konst(Constant::Null, span);
                let c = self.binary(BinaryOp::Eq, left, n, span);
                self.branch(c, rhs, end, span);
            }
        }
    }

    pub(crate) fn load_name(&mut self, name: &str, span: IrSpan) -> Temp {
        match self.resolve(name, span.start) {
            Resolved::Local { local, check, .. } => self.load_local(local, check, span),
            Resolved::Upval { upval, check, .. } => {
                let t = self.temp();
                self.emit(InstKind::LoadUpval { dst: t, upval, check_tdz: check }, span);
                t
            }
            Resolved::Global => match name {
                "undefined" => self.konst(Constant::Undefined, span),
                "NaN" => self.konst(Constant::Number(f64::NAN), span),
                "Infinity" => self.konst(Constant::Number(f64::INFINITY), span),
                "arguments" => self.unsupported_expr(span, "the `arguments` object"),
                _ => {
                    let t = self.temp();
                    self.emit(InstKind::LoadGlobal { dst: t, name: name.into() }, span);
                    t
                }
            },
        }
    }

    /// Validate `super.x` in the current function.
    pub(crate) fn check_super_property(&mut self, span: IrSpan) -> bool {
        match self.fbr().f.kind {
            FunctionKind::Method
            | FunctionKind::Getter
            | FunctionKind::Setter
            | FunctionKind::ClassConstructor
            | FunctionKind::DerivedConstructor => true,
            FunctionKind::Arrow => {
                self.unsupported(span, "`super` inside arrow functions");
                false
            }
            FunctionKind::FieldInit => {
                self.unsupported(span, "`super` in class field initializers");
                false
            }
            _ => {
                self.semantic(span, "'super' keyword unexpected here");
                false
            }
        }
    }

    // --- members and calls ---------------------------------------------------

    /// Short-circuit to the enclosing optional chain's nil block when `v` is nullish.
    fn chain_check(&mut self, v: Temp, span: IrSpan) {
        let Some(nil) = self.fbr().chain_nil else {
            self.semantic(span, "optional chain outside of a chain expression");
            return;
        };
        let n = self.konst(Constant::Null, span);
        let c = self.binary(BinaryOp::Eq, v, n, span);
        let cont = self.new_block();
        self.branch(c, nil, cont, span);
        self.switch_to(cont);
    }

    fn lower_static_member(&mut self, m: &StaticMemberExpression<'_>) -> Temp {
        let span = sp(m.span);
        if let Expression::Super(s) = &m.object {
            if !self.check_super_property(sp(s.span)) {
                return self.undef(span);
            }
            let key = self.str_const(m.property.name.as_str(), span);
            let dst = self.temp();
            self.emit(InstKind::SuperGet { dst, key }, span);
            return dst;
        }
        let obj = self.lower_expr(&m.object);
        if m.optional {
            self.chain_check(obj, span);
        }
        let dst = self.temp();
        self.emit(InstKind::GetNamed { dst, obj, name: m.property.name.as_str().into() }, span);
        dst
    }

    fn lower_computed_member(&mut self, m: &ComputedMemberExpression<'_>) -> Temp {
        let span = sp(m.span);
        if let Expression::Super(s) = &m.object {
            if !self.check_super_property(sp(s.span)) {
                return self.undef(span);
            }
            let k = self.lower_expr(&m.expression);
            let key = self.unary(UnaryOp::ToPropertyKey, k, span);
            let dst = self.temp();
            self.emit(InstKind::SuperGet { dst, key }, span);
            return dst;
        }
        let obj = self.lower_expr(&m.object);
        if m.optional {
            self.chain_check(obj, span);
        }
        let dst = self.temp();
        if let Expression::StringLiteral(s) = &m.expression {
            let name = self.js_str(s.value.as_str(), span);
            self.emit(InstKind::GetNamed { dst, obj, name: name.as_str().into() }, span);
        } else {
            let key = self.lower_expr(&m.expression);
            self.emit(InstKind::GetProp { dst, obj, key }, span);
        }
        dst
    }

    fn lower_chain(&mut self, c: &ChainExpression<'_>) -> Temp {
        let span = sp(c.span);
        let saved = self.fbr().chain_nil;
        let nil = self.new_block();
        self.fb().chain_nil = Some(nil);
        let r = self.join_local(span);
        let v = match &c.expression {
            ChainElement::CallExpression(call) => self.lower_call(call),
            ChainElement::TSNonNullExpression(x) => self.lower_expr(&x.expression),
            ChainElement::StaticMemberExpression(m) => self.lower_static_member(m),
            ChainElement::ComputedMemberExpression(m) => self.lower_computed_member(m),
            ChainElement::PrivateFieldExpression(_) => self.unsupported_expr(span, "private class members (#names)"),
        };
        self.store_local(r, v, span);
        let end = self.new_block();
        self.jump(end, span);
        self.switch_to(nil);
        let u = self.undef(span);
        self.store_local(r, u, span);
        self.jump(end, span);
        self.switch_to(end);
        self.fb().chain_nil = saved;
        self.load_local(r, false, span)
    }

    pub(crate) fn lower_args(&mut self, args: &[Argument<'_>]) -> Vec<Temp> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            if let Some(e) = a.as_expression() {
                out.push(self.lower_expr(e));
            }
        }
        out
    }

    pub(crate) fn lower_args_array(&mut self, args: &[Argument<'_>], span: IrSpan) -> Temp {
        let arr = self.temp();
        self.emit(InstKind::NewArray { dst: arr }, span);
        for a in args {
            match a {
                Argument::SpreadElement(s) => {
                    let v = self.lower_expr(&s.argument);
                    self.emit(InstKind::ArraySpread { array: arr, iterable: v }, sp(s.span));
                }
                other => {
                    if let Some(e) = other.as_expression() {
                        let v = self.lower_expr(e);
                        self.emit(InstKind::ArrayPush { array: arr, value: v }, sp(e.span()));
                    }
                }
            }
        }
        arr
    }

    pub(crate) fn lower_call(&mut self, c: &CallExpression<'_>) -> Temp {
        let span = sp(c.span);
        let callee_expr = unwrap_ts(&c.callee);
        if let Expression::Super(_) = callee_expr {
            return self.lower_super_call(c);
        }
        let (f, this) = match callee_expr {
            Expression::StaticMemberExpression(m) if !matches!(m.object, Expression::Super(_)) => {
                let o = self.lower_expr(&m.object);
                if m.optional {
                    self.chain_check(o, sp(m.span));
                }
                let dst = self.temp();
                self.emit(InstKind::GetNamed { dst, obj: o, name: m.property.name.as_str().into() }, sp(m.span));
                (dst, Some(o))
            }
            Expression::ComputedMemberExpression(m) if !matches!(m.object, Expression::Super(_)) => {
                let o = self.lower_expr(&m.object);
                if m.optional {
                    self.chain_check(o, sp(m.span));
                }
                let k = self.lower_expr(&m.expression);
                let dst = self.temp();
                self.emit(InstKind::GetProp { dst, obj: o, key: k }, sp(m.span));
                (dst, Some(o))
            }
            Expression::StaticMemberExpression(_) | Expression::ComputedMemberExpression(_) => {
                // super.m(...): receiver is the current `this`.
                let f = self.lower_expr(callee_expr);
                let t = self.lower_this(span);
                (f, Some(t))
            }
            other => (self.lower_expr(other), None),
        };
        if c.optional {
            self.chain_check(f, span);
        }
        let dst = self.temp();
        if c.arguments.iter().any(|a| matches!(a, Argument::SpreadElement(_))) {
            let args = self.lower_args_array(&c.arguments, span);
            self.emit(InstKind::CallSpread { dst, callee: f, this, args }, span);
        } else {
            let args = self.lower_args(&c.arguments);
            self.emit(InstKind::Call { dst, callee: f, this, args }, span);
        }
        dst
    }

    fn lower_super_call(&mut self, c: &CallExpression<'_>) -> Temp {
        let span = sp(c.span);
        match self.fbr().f.kind {
            FunctionKind::DerivedConstructor => {}
            FunctionKind::Arrow => return self.unsupported_expr(span, "`super` inside arrow functions"),
            _ => {
                self.semantic(span, "'super' call is only valid in a derived class constructor");
                return self.undef(span);
            }
        }
        let dst = self.temp();
        if c.arguments.iter().any(|a| matches!(a, Argument::SpreadElement(_))) {
            let args = self.lower_args_array(&c.arguments, span);
            self.emit(InstKind::SuperCallSpread { dst, args }, span);
        } else {
            let args = self.lower_args(&c.arguments);
            self.emit(InstKind::SuperCall { dst, args }, span);
        }
        let this_l = self.fbr().this_local.expect("this local");
        self.store_local(this_l, dst, span);
        let props = self.fbr().param_props.clone();
        for (name, local, pspan) in props {
            let t = self.load_local(this_l, false, pspan);
            let v = self.load_local(local, false, pspan);
            self.emit(InstKind::SetNamed { obj: t, name, value: v }, pspan);
        }
        dst
    }

    // --- literals ------------------------------------------------------------

    fn lower_template(&mut self, t: &TemplateLiteral<'_>) -> Temp {
        let span = sp(t.span);
        let mut acc: Option<Temp> = None;
        for (i, q) in t.quasis.iter().enumerate() {
            let qspan = sp(q.span);
            let text = match &q.value.cooked {
                Some(c) => self.js_str(c.as_str(), qspan),
                None => {
                    self.semantic(qspan, "invalid escape sequence in template literal");
                    String::new()
                }
            };
            if !text.is_empty() || (acc.is_none() && t.expressions.is_empty()) {
                let s = self.konst(Constant::String(text.as_str().into()), qspan);
                acc = Some(match acc {
                    None => s,
                    Some(a) => self.binary(BinaryOp::Add, a, s, qspan),
                });
            }
            if let Some(e) = t.expressions.get(i) {
                let espan = sp(e.span());
                let v = self.lower_expr(e);
                let s = self.unary(UnaryOp::ToString, v, espan);
                acc = Some(match acc {
                    None => s,
                    Some(a) => self.binary(BinaryOp::Add, a, s, espan),
                });
            }
        }
        acc.unwrap_or_else(|| self.str_const("", span))
    }

    fn lower_array(&mut self, elems: &[ArrayExpressionElement<'_>], span: IrSpan) -> Temp {
        let arr = self.temp();
        self.emit(InstKind::NewArray { dst: arr }, span);
        for el in elems {
            match el {
                ArrayExpressionElement::SpreadElement(s) => {
                    let v = self.lower_expr(&s.argument);
                    self.emit(InstKind::ArraySpread { array: arr, iterable: v }, sp(s.span));
                }
                ArrayExpressionElement::Elision(e) => self.emit(InstKind::ArrayHole { array: arr }, sp(e.span)),
                other => {
                    if let Some(e) = other.as_expression() {
                        let v = self.lower_expr(e);
                        self.emit(InstKind::ArrayPush { array: arr, value: v }, sp(e.span()));
                    }
                }
            }
        }
        arr
    }

    fn lower_object(&mut self, o: &ObjectExpression<'_>) -> Temp {
        let span = sp(o.span);
        let obj = self.new_object(span);
        for p in &o.properties {
            match p {
                ObjectPropertyKind::SpreadProperty(s) => {
                    let v = self.lower_expr(&s.argument);
                    self.emit(InstKind::CopyDataProps { target: obj, source: v, excluded: Vec::new() }, sp(s.span));
                }
                ObjectPropertyKind::ObjectProperty(p) => {
                    let pspan = sp(p.span);
                    let key = self.prop_key(&p.key, p.computed);
                    let hint: Option<String> = match &key {
                        Key::Named(n) => Some(n.to_string()),
                        Key::Computed(_) => None,
                    };
                    let method_kind = match p.kind {
                        PropertyKind::Get => Some((MethodKind::Getter, FunctionKind::Getter)),
                        PropertyKind::Set => Some((MethodKind::Setter, FunctionKind::Setter)),
                        PropertyKind::Init if p.method => Some((MethodKind::Method, FunctionKind::Method)),
                        PropertyKind::Init => None,
                    };
                    if let Some((mk, fk)) = method_kind {
                        let Expression::FunctionExpression(f) = &p.value else {
                            self.semantic(pspan, "malformed method");
                            continue;
                        };
                        let base = hint.clone().unwrap_or_default();
                        let fname = match mk {
                            MethodKind::Getter => format!("get {base}"),
                            MethodKind::Setter => format!("set {base}"),
                            MethodKind::Method => base,
                        };
                        let fid = self.lower_fn_node(f, &fname, fk, None);
                        let func = self.temp();
                        self.emit(InstKind::MakeClosure { dst: func, func: fid }, sp(f.span));
                        let k = self.key_temp(&key, pspan);
                        self.emit(InstKind::DefineMethod { obj, key: k, func, kind: mk, enumerable: true }, pspan);
                        continue;
                    }
                    if !p.computed && !p.shorthand && matches!(&key, Key::Named(n) if &**n == "__proto__") {
                        self.unsupported(pspan, "`__proto__: value` in object literals");
                        continue;
                    }
                    let v = self.lower_expr_named(&p.value, hint.as_deref());
                    match key {
                        Key::Named(name) => self.emit(InstKind::DefineNamed { obj, name, value: v }, pspan),
                        Key::Computed(k) => self.emit(InstKind::DefineField { obj, key: k, value: v }, pspan),
                    }
                }
            }
        }
        obj
    }

    // --- operators -----------------------------------------------------------

    fn lower_unary(&mut self, u: &UnaryExpression<'_>) -> Temp {
        let span = sp(u.span);
        match u.operator {
            UnaryOperator::UnaryNegation => {
                if let Expression::NumericLiteral(n) = unwrap_ts(&u.argument) {
                    return self.konst(Constant::Number(-n.value), span);
                }
                let v = self.lower_expr(&u.argument);
                self.unary(UnaryOp::Neg, v, span)
            }
            UnaryOperator::UnaryPlus => {
                let v = self.lower_expr(&u.argument);
                self.unary(UnaryOp::Plus, v, span)
            }
            UnaryOperator::LogicalNot => {
                let v = self.lower_expr(&u.argument);
                self.unary(UnaryOp::Not, v, span)
            }
            UnaryOperator::BitwiseNot => {
                let v = self.lower_expr(&u.argument);
                self.unary(UnaryOp::BitNot, v, span)
            }
            UnaryOperator::Typeof => {
                if let Expression::Identifier(id) = unwrap_ts(&u.argument) {
                    let name = id.name.as_str();
                    let ispan = sp(id.span);
                    match self.resolve(name, ispan.start) {
                        Resolved::Global if !matches!(name, "undefined" | "NaN" | "Infinity" | "arguments") => {
                            let dst = self.temp();
                            self.emit(InstKind::TypeofGlobal { dst, name: name.into() }, span);
                            return dst;
                        }
                        _ => {
                            let v = self.load_name(name, ispan);
                            return self.unary(UnaryOp::TypeOf, v, span);
                        }
                    }
                }
                let v = self.lower_expr(&u.argument);
                self.unary(UnaryOp::TypeOf, v, span)
            }
            UnaryOperator::Void => {
                self.lower_expr(&u.argument);
                self.undef(span)
            }
            UnaryOperator::Delete => match unwrap_ts(&u.argument) {
                Expression::StaticMemberExpression(m) if !m.optional && !matches!(m.object, Expression::Super(_)) => {
                    let o = self.lower_expr(&m.object);
                    let k = self.str_const(m.property.name.as_str(), span);
                    let dst = self.temp();
                    self.emit(InstKind::DeleteProp { dst, obj: o, key: k }, span);
                    dst
                }
                Expression::ComputedMemberExpression(m) if !m.optional && !matches!(m.object, Expression::Super(_)) => {
                    let o = self.lower_expr(&m.object);
                    let k = self.lower_expr(&m.expression);
                    let dst = self.temp();
                    self.emit(InstKind::DeleteProp { dst, obj: o, key: k }, span);
                    dst
                }
                Expression::Identifier(_) => self.unsupported_expr(span, "delete of an unqualified identifier"),
                Expression::ChainExpression(_) => self.unsupported_expr(span, "delete of an optional chain"),
                Expression::StaticMemberExpression(_) | Expression::ComputedMemberExpression(_) => {
                    self.unsupported_expr(span, "delete of a super property")
                }
                Expression::PrivateFieldExpression(_) => {
                    self.unsupported_expr(span, "private class members (#names)")
                }
                other => {
                    self.lower_expr(other);
                    self.konst(Constant::Bool(true), span)
                }
            },
        }
    }

    fn lower_update(&mut self, u: &UpdateExpression<'_>) -> Temp {
        let span = sp(u.span);
        let p = self.prepare_simple(&u.argument, true);
        if matches!(p, Prepared::Invalid) {
            return self.undef(span);
        }
        let old = self.load_prepared(&p);
        let op = match u.operator {
            UpdateOperator::Increment => UnaryOp::Inc,
            UpdateOperator::Decrement => UnaryOp::Dec,
        };
        if u.prefix {
            let new = self.unary(op, old, span);
            self.store_prepared(p, new, span);
            new
        } else {
            let num = self.unary(UnaryOp::Plus, old, span);
            let new = self.unary(op, num, span);
            self.store_prepared(p, new, span);
            num
        }
    }

    fn lower_assign(&mut self, a: &AssignmentExpression<'_>) -> Temp {
        let span = sp(a.span);
        let target_name = match &a.left {
            AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.as_str()),
            _ => None,
        };
        match a.operator {
            AssignmentOperator::Assign => {
                let p = self.prepare_target(&a.left, false);
                let v = self.lower_expr_named(&a.right, target_name);
                self.store_prepared(p, v, span);
                v
            }
            AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalOr | AssignmentOperator::LogicalNullish => {
                let kind = match a.operator {
                    AssignmentOperator::LogicalAnd => Logic::And,
                    AssignmentOperator::LogicalOr => Logic::Or,
                    _ => Logic::Nullish,
                };
                let p = self.prepare_target(&a.left, true);
                if matches!(p, Prepared::ArrayPat(_) | Prepared::ObjectPat(_) | Prepared::Invalid) {
                    self.semantic(span, "Invalid left-hand side in logical assignment");
                    return self.undef(span);
                }
                let old = self.load_prepared(&p);
                let r = self.join_local(span);
                self.store_local(r, old, span);
                let rhs = self.new_block();
                let end = self.new_block();
                self.logic_branch(kind, old, rhs, end, span);
                self.switch_to(rhs);
                let v = self.lower_expr_named(&a.right, target_name);
                self.store_local(r, v, span);
                self.store_prepared(p, v, span);
                self.jump(end, span);
                self.switch_to(end);
                self.load_local(r, false, span)
            }
            op => {
                let bop = compound_op(op).expect("compound operator");
                let p = self.prepare_target(&a.left, true);
                if matches!(p, Prepared::ArrayPat(_) | Prepared::ObjectPat(_) | Prepared::Invalid) {
                    self.semantic(span, "Invalid left-hand side in assignment");
                    return self.undef(span);
                }
                let old = self.load_prepared(&p);
                let r = self.lower_expr(&a.right);
                let new = self.binary(bop, old, r, span);
                self.store_prepared(p, new, span);
                new
            }
        }
    }
}

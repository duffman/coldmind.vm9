//! Classes and enums.

use oxc_ast::ast::*;

use dj_ir::{
    BinaryOp, Constant, FuncId, FunctionKind, InstKind, LocalId, LocalKind, MethodKind, ScopeKind, Span as IrSpan,
    Temp, TermKind, UnaryOp,
};

use crate::builder::{Lowerer, sp};
use crate::lower_func::{Body, FnSpec};
use crate::lower_pattern::Key;

enum FieldKey {
    Name(String),
    /// Computed key evaluated at class definition time, stored in a local of the
    /// defining function.
    Local(LocalId),
}

struct FieldRec<'b, 'a> {
    key: FieldKey,
    value: Option<&'b Expression<'a>>,
    span: IrSpan,
}

impl<'s> Lowerer<'s> {
    pub(crate) fn lower_class(&mut self, c: &Class<'_>, name_hint: Option<&str>) -> Temp {
        let span = sp(c.span);
        if !c.decorators.is_empty() {
            self.unsupported(span, "decorators");
        }
        let class_name = c.id.as_ref().map(|i| i.name.to_string()).or(name_hint.map(str::to_string)).unwrap_or_default();

        self.push_scope(ScopeKind::Class, span);
        let inner = c.id.as_ref().map(|id| {
            self.declare(id.name.as_str(), LocalKind::Class, sp(id.span), c.span.end, true, false)
        });
        self.emit_declares(span);

        let parent = c.heritage.as_ref().map(|h| self.lower_expr(&h.expression));
        let derived = parent.is_some();

        // Constructor.
        let ctor = c.body.body.iter().find_map(|el| match el {
            ClassElement::MethodDefinition(m)
                if m.kind == MethodDefinitionKind::Constructor && m.value.body.is_some() =>
            {
                Some(m)
            }
            _ => None,
        });
        let ctor_fid: FuncId = match ctor {
            Some(m) => {
                if m.value.generator || m.value.r#async {
                    self.semantic(sp(m.span), "Class constructor may not be a generator or async");
                }
                let body = &m.value.body.as_ref().unwrap().statements;
                self.lower_function(FnSpec {
                    name: class_name.clone(),
                    kind: if derived { FunctionKind::DerivedConstructor } else { FunctionKind::ClassConstructor },
                    is_async: false,
                    span: sp(m.value.span),
                    params: Some(&m.value.params),
                    body: Body::Stmts(body),
                    self_name: None,
                })
            }
            None => self.lower_default_ctor(&class_name, derived, span),
        };
        let ctor_t = self.temp();
        self.emit(InstKind::MakeClosure { dst: ctor_t, func: ctor_fid }, span);
        let cls = self.temp();
        self.emit(InstKind::MakeClass { dst: cls, ctor: ctor_t, parent }, span);
        let proto = self.temp();
        self.emit(InstKind::GetNamed { dst: proto, obj: cls, name: "prototype".into() }, span);

        let mut instance_fields: Vec<FieldRec<'_, '_>> = Vec::new();
        let mut static_fields: Vec<FieldRec<'_, '_>> = Vec::new();
        for el in &c.body.body {
            match el {
                ClassElement::MethodDefinition(m) => {
                    let mspan = sp(m.span);
                    if m.kind == MethodDefinitionKind::Constructor
                        || m.r#type == MethodDefinitionType::TSAbstractMethodDefinition
                        || m.value.body.is_none()
                    {
                        continue;
                    }
                    if !m.decorators.is_empty() {
                        self.unsupported(mspan, "decorators");
                    }
                    let key = self.prop_key(&m.key, m.computed);
                    let base = match &key {
                        Key::Named(n) => n.to_string(),
                        Key::Computed(_) => String::new(),
                    };
                    let (mk, fk, fname) = match m.kind {
                        MethodDefinitionKind::Get => (MethodKind::Getter, FunctionKind::Getter, format!("get {base}")),
                        MethodDefinitionKind::Set => (MethodKind::Setter, FunctionKind::Setter, format!("set {base}")),
                        _ => (MethodKind::Method, FunctionKind::Method, base),
                    };
                    let fid = self.lower_fn_node(&m.value, &fname, fk, None);
                    let func = self.temp();
                    self.emit(InstKind::MakeClosure { dst: func, func: fid }, sp(m.value.span));
                    let k = self.key_temp(&key, mspan);
                    let target = if m.r#static { cls } else { proto };
                    self.emit(InstKind::DefineMethod { obj: target, key: k, func, kind: mk, enumerable: false }, mspan);
                }
                ClassElement::PropertyDefinition(p) => {
                    let pspan = sp(p.span);
                    if p.r#type == PropertyDefinitionType::TSAbstractPropertyDefinition || p.declare {
                        continue;
                    }
                    if !p.decorators.is_empty() {
                        self.unsupported(pspan, "decorators");
                    }
                    let key = match self.prop_key(&p.key, p.computed) {
                        Key::Named(n) => FieldKey::Name(n.to_string()),
                        Key::Computed(t) => {
                            let l = self.synthetic("%fieldkey", pspan);
                            self.store_local(l, t, pspan);
                            FieldKey::Local(l)
                        }
                    };
                    let rec = FieldRec { key, value: p.value.as_ref(), span: pspan };
                    if p.r#static {
                        static_fields.push(rec);
                    } else {
                        instance_fields.push(rec);
                    }
                }
                ClassElement::AccessorProperty(a) => self.unsupported(sp(a.span), "auto-accessor fields (`accessor`)"),
                ClassElement::StaticBlock(b) => self.unsupported(sp(b.span), "class static blocks"),
                ClassElement::TSIndexSignature(_) => {}
            }
        }

        // TS parameter properties are class fields declared before all others
        // (defined as `undefined`; the constructor assigns them afterwards).
        if let (Some(m), false) = (ctor, instance_fields.is_empty()) {
            let props: Vec<FieldRec<'_, '_>> = m
                .value
                .params
                .items
                .iter()
                .filter(|p| p.accessibility.is_some() || p.readonly || p.r#override)
                .filter_map(|p| match &p.pattern {
                    BindingPattern::BindingIdentifier(id) => {
                        Some(FieldRec { key: FieldKey::Name(id.name.to_string()), value: None, span: sp(p.span) })
                    }
                    _ => None,
                })
                .collect();
            instance_fields.splice(0..0, props);
        }
        if !instance_fields.is_empty() {
            let fid = self.lower_field_init(&instance_fields, &format!("<fields {class_name}>"), span);
            let f = self.temp();
            self.emit(InstKind::MakeClosure { dst: f, func: fid }, span);
            self.emit(InstKind::SetFieldInit { class: cls, init: f }, span);
        }
        if let Some(inner) = inner {
            self.store_local(inner, cls, span);
        }
        if !static_fields.is_empty() {
            let fid = self.lower_field_init(&static_fields, &format!("<static {class_name}>"), span);
            let f = self.temp();
            self.emit(InstKind::MakeClosure { dst: f, func: fid }, span);
            let dst = self.temp();
            self.emit(InstKind::Call { dst, callee: f, this: Some(cls), args: Vec::new() }, span);
        }
        self.pop_scope();
        cls
    }

    /// Synthetic function defining fields on `this`, in declaration order.
    fn lower_field_init(&mut self, fields: &[FieldRec<'_, '_>], name: &str, span: IrSpan) -> FuncId {
        let id = self.begin_function(name, FunctionKind::FieldInit, span, false);
        self.setup_this(span);
        let this_l = self.fbr().this_local.unwrap();
        let d = self.depth();
        for f in fields {
            self.stmt(f.span);
            let hint = match &f.key {
                FieldKey::Name(n) => Some(n.clone()),
                FieldKey::Local(_) => None,
            };
            let key_t = match &f.key {
                FieldKey::Name(_) => None,
                FieldKey::Local(l) => {
                    let u = self.capture_into(d, d - 1, *l, "%fieldkey", LocalKind::Synthetic, false);
                    let t = self.temp();
                    self.emit(InstKind::LoadUpval { dst: t, upval: u, check_tdz: false }, f.span);
                    Some(t)
                }
            };
            let v = match f.value {
                Some(e) => self.lower_expr_named(e, hint.as_deref()),
                None => self.undef(f.span),
            };
            let obj = self.load_local(this_l, false, f.span);
            match (key_t, &f.key) {
                (Some(k), _) => self.emit(InstKind::DefineField { obj, key: k, value: v }, f.span),
                (None, FieldKey::Name(n)) => {
                    self.emit(InstKind::DefineNamed { obj, name: n.as_str().into(), value: v }, f.span)
                }
                (None, FieldKey::Local(_)) => unreachable!(),
            }
        }
        let end = IrSpan::new(span.end, span.end);
        let u = self.undef(end);
        self.terminate(TermKind::Return(u), end);
        self.end_function();
        id
    }

    /// `enum E { A, B = 5, C }` → object with forward and (numeric) reverse mappings.
    pub(crate) fn lower_enum(&mut self, e: &TSEnumDeclaration<'_>) -> Temp {
        let span = sp(e.span);
        let obj = self.new_object(span);
        self.push_scope(ScopeKind::Block, span);
        let prev = self.synthetic("%prev", span);
        let mut prev_was_string = false;
        for (i, m) in e.body.members.iter().enumerate() {
            let mspan = sp(m.span);
            let name: String = match &m.id {
                TSEnumMemberName::Identifier(id) => id.name.to_string(),
                TSEnumMemberName::String(s) | TSEnumMemberName::ComputedString(s) => {
                    self.js_str(s.value.as_str(), mspan)
                }
                TSEnumMemberName::ComputedTemplateString(t) => {
                    match (t.expressions.is_empty(), t.quasis.first().and_then(|q| q.value.cooked)) {
                        (true, Some(c)) => self.js_str(c.as_str(), mspan),
                        _ => {
                            self.unsupported(mspan, "computed enum member names");
                            String::new()
                        }
                    }
                }
            };
            let is_string_init = matches!(
                &m.initializer,
                Some(Expression::StringLiteral(_)) | Some(Expression::TemplateLiteral(_))
            );
            let v = match &m.initializer {
                Some(init) => self.lower_expr(init),
                None if i == 0 => self.konst(Constant::Number(0.0), mspan),
                None if prev_was_string => {
                    self.semantic(mspan, "Enum member must have initializer.");
                    self.undef(mspan)
                }
                None => {
                    let p = self.load_local(prev, false, mspan);
                    let one = self.konst(Constant::Number(1.0), mspan);
                    self.binary(BinaryOp::Add, p, one, mspan)
                }
            };
            self.store_local(prev, v, mspan);
            let member = self.declare(&name, LocalKind::Synthetic, mspan, 0, true, false);
            self.store_local(member, v, mspan);
            self.emit(InstKind::DefineNamed { obj, name: name.as_str().into(), value: v }, mspan);
            if !is_string_init {
                let k = self.unary(UnaryOp::ToPropertyKey, v, mspan);
                let n = self.str_const(&name, mspan);
                self.emit(InstKind::DefineField { obj, key: k, value: n }, mspan);
            }
            prev_was_string = is_string_init;
        }
        self.pop_scope();
        obj
    }
}

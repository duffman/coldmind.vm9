//! Programs and functions.

use oxc_ast::ast::*;

use dj_ir::{
    BinaryOp, Constant, FuncId, FunctionKind, InstKind, Local, LocalId, LocalKind, Module, ScopeId, Span as IrSpan,
    TermKind,
};

use crate::builder::{Lowerer, sp};

pub(crate) enum Body<'b, 'a> {
    Stmts(&'b [Statement<'a>]),
    Expr(&'b Expression<'a>),
    Empty,
}

pub(crate) struct FnSpec<'b, 'a> {
    pub name: String,
    pub kind: FunctionKind,
    pub is_async: bool,
    pub span: IrSpan,
    pub params: Option<&'b FormalParameters<'a>>,
    pub body: Body<'b, 'a>,
    /// Name bound inside a named function expression.
    pub self_name: Option<&'b str>,
}

impl<'s> Lowerer<'s> {
    pub(crate) fn lower_program(&mut self, prog: &Program<'_>) -> Module {
        let span = sp(prog.span);
        let entry = self.begin_function("<main>", FunctionKind::Script, span, false);
        self.declare_vars(&prog.body);
        let h = self.declare_lexical(prog.body.iter());
        self.emit_hoisted(h, span);
        self.lower_stmts(&prog.body);
        let end = IrSpan::new(span.end, span.end);
        let u = self.undef(end);
        self.terminate(TermKind::Return(u), end);
        self.end_function();
        let functions = std::mem::take(&mut self.funcs).into_iter().map(|f| f.expect("function finished")).collect();
        Module { name: self.file_name.clone(), source: self.source.into(), functions, entry }
    }

    pub(crate) fn lower_function_decl(&mut self, f: &Function<'_>) -> FuncId {
        let name = f.id.as_ref().map(|i| i.name.to_string()).unwrap_or_default();
        self.lower_fn_node(f, &name, FunctionKind::Normal, None)
    }

    /// Lower an oxc `Function` node (declaration, expression or method value).
    pub(crate) fn lower_fn_node(
        &mut self,
        f: &Function<'_>,
        name: &str,
        kind: FunctionKind,
        self_name: Option<&str>,
    ) -> FuncId {
        if f.generator {
            self.unsupported(sp(f.span), if f.r#async { "async generator functions" } else { "generator functions" });
        }
        let body = match &f.body {
            Some(b) => Body::Stmts(&b.statements),
            None => Body::Empty,
        };
        self.lower_function(FnSpec {
            name: name.to_string(),
            kind,
            is_async: f.r#async,
            span: sp(f.span),
            params: Some(&f.params),
            body,
            self_name,
        })
    }

    pub(crate) fn lower_arrow(&mut self, a: &ArrowFunctionExpression<'_>, name: &str) -> FuncId {
        let body = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => Body::Stmts(&b.statements),
            other => match other.as_expression() {
                Some(e) => Body::Expr(e),
                None => Body::Empty,
            },
        };
        self.lower_function(FnSpec {
            name: name.to_string(),
            kind: FunctionKind::Arrow,
            is_async: a.r#async,
            span: sp(a.span),
            params: Some(&a.params),
            body,
            self_name: None,
        })
    }

    pub(crate) fn lower_function(&mut self, spec: FnSpec<'_, '_>) -> FuncId {
        let span = spec.span;
        let id = self.begin_function(&spec.name, spec.kind, span, spec.is_async);

        // 1. Parameter locals occupy indices 0..param_count (+ rest).
        let mut param_locals: Vec<LocalId> = Vec::new();
        let mut rest_local: Option<LocalId> = None;
        if let Some(params) = spec.params {
            for (i, p) in params.items.iter().enumerate() {
                if !p.decorators.is_empty() {
                    self.unsupported(sp(p.span), "parameter decorators");
                }
                let (name, kind) = match &p.pattern {
                    BindingPattern::BindingIdentifier(id) => (id.name.to_string(), LocalKind::Param),
                    _ => (format!("%arg{i}"), LocalKind::Synthetic),
                };
                let l = self.fb().f.new_local(Local {
                    name: name.as_str().into(),
                    kind,
                    scope: ScopeId(0),
                    captured: false,
                    needs_tdz: false,
                    span: sp(p.span),
                });
                if kind == LocalKind::Param {
                    self.bind_existing(&name, l, LocalKind::Param);
                }
                param_locals.push(l);
            }
            self.fb().f.param_count = params.items.len() as u32;
            if let Some(r) = &params.rest {
                let (name, kind) = match &r.rest.argument {
                    BindingPattern::BindingIdentifier(id) => (id.name.to_string(), LocalKind::Param),
                    _ => ("%rest".to_string(), LocalKind::Synthetic),
                };
                let l = self.fb().f.new_local(Local {
                    name: name.as_str().into(),
                    kind,
                    scope: ScopeId(0),
                    captured: false,
                    needs_tdz: false,
                    span: sp(r.span),
                });
                if kind == LocalKind::Param {
                    self.bind_existing(&name, l, LocalKind::Param);
                }
                self.fb().f.has_rest = true;
                rest_local = Some(l);
            }
        }

        // 2. `this`, field initialization.
        self.setup_this(span);
        if spec.kind == FunctionKind::ClassConstructor {
            self.emit(InstKind::InitFields, span);
        }

        // 3. Destructured parameter names, then defaults/patterns in order.
        if let Some(params) = spec.params {
            for p in params.items.iter() {
                if !matches!(p.pattern, BindingPattern::BindingIdentifier(_)) {
                    let mut names = Vec::new();
                    crate::decls::binding_names(&p.pattern, &mut names);
                    for (n, s) in names {
                        self.declare(n, LocalKind::Param, s, 0, false, true);
                    }
                }
            }
            if let Some(r) = &params.rest {
                if !matches!(r.rest.argument, BindingPattern::BindingIdentifier(_)) {
                    let mut names = Vec::new();
                    crate::decls::binding_names(&r.rest.argument, &mut names);
                    for (n, s) in names {
                        self.declare(n, LocalKind::Param, s, 0, false, true);
                    }
                }
            }
            for (i, p) in params.items.iter().enumerate() {
                let l = param_locals[i];
                let pspan = sp(p.span);
                match (&p.pattern, &p.initializer) {
                    (BindingPattern::BindingIdentifier(_), None) => {}
                    (BindingPattern::BindingIdentifier(id), Some(def)) => {
                        let v = self.load_local(l, false, pspan);
                        let v = self.apply_default(v, def, Some(id.name.as_str()), pspan);
                        self.store_local(l, v, pspan);
                    }
                    (pat, def) => {
                        let v = self.load_local(l, false, pspan);
                        let v = match def {
                            Some(d) => self.apply_default(v, d, None, pspan),
                            None => v,
                        };
                        self.bind_pattern(pat, v);
                    }
                }
            }
            if let (Some(r), Some(l)) = (&params.rest, rest_local) {
                if !matches!(r.rest.argument, BindingPattern::BindingIdentifier(_)) {
                    let v = self.load_local(l, false, sp(r.span));
                    self.bind_pattern(&r.rest.argument, v);
                }
            }

            // TS parameter properties.
            for (i, p) in params.items.iter().enumerate() {
                if p.accessibility.is_none() && !p.readonly && !p.r#override {
                    continue;
                }
                let pspan = sp(p.span);
                let BindingPattern::BindingIdentifier(id) = &p.pattern else {
                    self.semantic(pspan, "A parameter property may not be declared using a binding pattern");
                    continue;
                };
                match spec.kind {
                    FunctionKind::ClassConstructor => {
                        let this_l = self.fbr().this_local.unwrap();
                        let this_t = self.load_local(this_l, false, pspan);
                        let v = self.load_local(param_locals[i], false, pspan);
                        self.emit(InstKind::SetNamed { obj: this_t, name: id.name.as_str().into(), value: v }, pspan);
                    }
                    FunctionKind::DerivedConstructor => {
                        self.fb().param_props.push((id.name.as_str().into(), param_locals[i], pspan));
                    }
                    _ => self.semantic(pspan, "A parameter property is only allowed in a constructor implementation"),
                }
            }
        }

        // 4. Body declarations, self binding, hoisted functions.
        let stmts: &[Statement<'_>] = match &spec.body {
            Body::Stmts(s) => s,
            _ => &[],
        };
        self.declare_vars(stmts);
        let h = self.declare_lexical(stmts.iter());
        if let Some(n) = spec.self_name {
            if !self.is_declared_in_function_scope(n) {
                let l = self.declare(n, LocalKind::Function, span, 0, true, true);
                let t = self.temp();
                self.emit(InstKind::LoadCallee { dst: t }, span);
                self.store_local(l, t, span);
            }
        }
        self.emit_hoisted(h, span);

        // 5. Body.
        match spec.body {
            Body::Stmts(s) => self.lower_stmts(s),
            Body::Expr(e) => {
                let es = sp(e.span());
                self.stmt(es);
                let v = self.lower_expr(e);
                self.emit_return(v, es);
            }
            Body::Empty => {}
        }

        // 6. Implicit return.
        let end = IrSpan::new(span.end.saturating_sub(1).max(span.start), span.end);
        self.implicit_return(end);
        self.end_function();
        id
    }

    fn implicit_return(&mut self, span: IrSpan) {
        if self.fbr().f.kind == FunctionKind::DerivedConstructor {
            let l = self.fbr().this_local.unwrap();
            let t = self.load_local(l, true, span);
            self.terminate(TermKind::Return(t), span);
        } else {
            let u = self.undef(span);
            self.terminate(TermKind::Return(u), span);
        }
    }

    /// `class X {}` / `class X extends Y {}` without an explicit constructor.
    pub(crate) fn lower_default_ctor(&mut self, name: &str, derived: bool, span: IrSpan) -> FuncId {
        let kind = if derived { FunctionKind::DerivedConstructor } else { FunctionKind::ClassConstructor };
        let id = self.begin_function(name, kind, span, false);
        if derived {
            let args = self.fb().f.new_local(Local {
                name: "args".into(),
                kind: LocalKind::Param,
                scope: ScopeId(0),
                captured: false,
                needs_tdz: false,
                span,
            });
            self.fb().f.has_rest = true;
            self.setup_this(span);
            let a = self.load_local(args, false, span);
            let r = self.temp();
            self.emit(InstKind::SuperCallSpread { dst: r, args: a }, span);
            let this_l = self.fbr().this_local.unwrap();
            self.store_local(this_l, r, span);
        } else {
            self.setup_this(span);
            self.emit(InstKind::InitFields, span);
        }
        self.implicit_return(span);
        self.end_function();
        id
    }

    /// `if (v === undefined) v = default` as a value join.
    pub(crate) fn apply_default(
        &mut self,
        value: dj_ir::Temp,
        default: &Expression<'_>,
        name: Option<&str>,
        span: IrSpan,
    ) -> dj_ir::Temp {
        let r = self.join_local(span);
        self.store_local(r, value, span);
        let u = self.undef(span);
        let c = self.binary(BinaryOp::StrictEq, value, u, span);
        let def_bb = self.new_block();
        let join = self.new_block();
        self.branch(c, def_bb, join, span);
        self.switch_to(def_bb);
        let d = self.lower_expr_named(default, name);
        self.store_local(r, d, span);
        self.jump(join, span);
        self.switch_to(join);
        self.load_local(r, false, span)
    }

    /// Nearest enclosing non-arrow function depth.
    pub(crate) fn this_function_depth(&self) -> usize {
        (0..self.fns.len()).rev().find(|&i| self.fns[i].f.kind != FunctionKind::Arrow).unwrap_or(0)
    }

    pub(crate) fn lower_this(&mut self, span: IrSpan) -> dj_ir::Temp {
        let d = self.depth();
        let k = self.this_function_depth();
        let fk = self.fns[k].f.kind;
        if fk == FunctionKind::Script {
            return self.konst(Constant::Undefined, span);
        }
        let l = self.fns[k].this_local.expect("this local");
        let tdz = fk == FunctionKind::DerivedConstructor;
        if k == d {
            self.load_local(l, tdz, span)
        } else {
            let u = self.capture_into(d, k, l, "this", LocalKind::This, tdz);
            let t = self.temp();
            self.emit(InstKind::LoadUpval { dst: t, upval: u, check_tdz: tdz }, span);
            t
        }
    }

    pub(crate) fn lower_new_target(&mut self, span: IrSpan) -> dj_ir::Temp {
        let d = self.depth();
        let k = self.this_function_depth();
        if self.fns[k].f.kind == FunctionKind::Script {
            self.semantic(span, "new.target expression is not allowed here");
            return self.undef(span);
        }
        if k == d {
            let t = self.temp();
            self.emit(InstKind::LoadNewTarget { dst: t }, span);
            return t;
        }
        let l = match self.fns[k].new_target_local {
            Some(l) => l,
            None => {
                let fspan = self.fns[k].f.span;
                let fb = &mut self.fns[k];
                let l = fb.f.new_local(Local {
                    name: "new.target".into(),
                    kind: LocalKind::NewTarget,
                    scope: ScopeId(0),
                    captured: false,
                    needs_tdz: false,
                    span: fspan,
                });
                let t = fb.f.new_temp();
                let entry = fb.f.entry;
                let insts = &mut fb.f.block_mut(entry).insts;
                insts.insert(0, dj_ir::Inst { kind: InstKind::StoreLocal { local: l, src: t }, span: fspan, scope: ScopeId(0) });
                insts.insert(0, dj_ir::Inst { kind: InstKind::LoadNewTarget { dst: t }, span: fspan, scope: ScopeId(0) });
                fb.new_target_local = Some(l);
                l
            }
        };
        let u = self.capture_into(d, k, l, "new.target", LocalKind::NewTarget, false);
        let t = self.temp();
        self.emit(InstKind::LoadUpval { dst: t, upval: u, check_tdz: false }, span);
        t
    }
}

use oxc_span::GetSpan;

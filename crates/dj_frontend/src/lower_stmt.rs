//! Statements and structured control flow.

use oxc_ast::ast::*;
use oxc_span::GetSpan;

use dj_ir::{BinaryOp, Constant, InstKind, LocalKind, ScopeKind, Span as IrSpan, TermKind};

use crate::builder::{Ctl, FinCtx, Lowerer, TargetKind, sp};

impl<'s> Lowerer<'s> {
    pub(crate) fn lower_stmts(&mut self, stmts: &[Statement<'_>]) {
        for s in stmts {
            self.lower_stmt(s, Vec::new());
        }
    }

    /// Lower a statement list in a fresh block scope.
    pub(crate) fn lower_block(&mut self, stmts: &[Statement<'_>], kind: ScopeKind, span: IrSpan) {
        self.push_scope(kind, span);
        let h = self.declare_lexical(stmts.iter());
        self.emit_hoisted(h, span);
        self.lower_stmts(stmts);
        self.pop_scope();
    }

    /// A statement used as a loop / if body.
    fn lower_sub_stmt(&mut self, s: &Statement<'_>) {
        match s {
            Statement::VariableDeclaration(d) if d.kind != VariableDeclarationKind::Var => {
                self.semantic(sp(d.span), "Lexical declaration cannot appear in a single-statement context");
            }
            Statement::ClassDeclaration(c) => {
                self.semantic(sp(c.span), "Class declaration cannot appear in a single-statement context");
            }
            Statement::FunctionDeclaration(f) => {
                self.semantic(sp(f.span), "Function declaration cannot appear in a single-statement context");
            }
            _ => self.lower_stmt(s, Vec::new()),
        }
    }

    pub(crate) fn lower_stmt(&mut self, s: &Statement<'_>, labels: Vec<String>) {
        let span = sp(s.span());
        match s {
            Statement::BlockStatement(b) => self.lower_block(&b.body, ScopeKind::Block, sp(b.span)),
            Statement::BreakStatement(b) => {
                self.stmt(span);
                let label = b.label.as_ref().map(|l| l.name.as_str());
                match self.find_break(label) {
                    Some(id) => self.emit_jump(id, false, span),
                    None => self.semantic(span, match label {
                        Some(l) => format!("Undefined label '{l}'"),
                        None => "Illegal break statement".into(),
                    }),
                }
            }
            Statement::ContinueStatement(c) => {
                self.stmt(span);
                let label = c.label.as_ref().map(|l| l.name.as_str());
                match self.find_continue(label) {
                    Ok(id) => self.emit_jump(id, true, span),
                    Err(_) => self.semantic(span, match label {
                        Some(l) => format!("Undefined label '{l}' for continue"),
                        None => "Illegal continue statement: no surrounding iteration statement".into(),
                    }),
                }
            }
            Statement::DebuggerStatement(_) => {
                self.stmt(span);
                self.emit(InstKind::Debugger, span);
            }
            Statement::DoWhileStatement(d) => self.lower_do_while(d, labels),
            Statement::EmptyStatement(_) => {}
            Statement::ExpressionStatement(e) => {
                self.stmt(span);
                self.lower_expr(&e.expression);
            }
            Statement::ForInStatement(f) => self.lower_for_in_of(&f.left, &f.right, &f.body, false, span, labels),
            Statement::ForOfStatement(f) => {
                if f.r#await {
                    self.unsupported(span, "for await...of");
                    return;
                }
                self.lower_for_in_of(&f.left, &f.right, &f.body, true, span, labels)
            }
            Statement::ForStatement(f) => self.lower_for(f, labels),
            Statement::IfStatement(i) => {
                self.stmt(span);
                let t = self.lower_expr(&i.test);
                let then_bb = self.new_block();
                let join = self.new_block();
                let else_bb = if i.alternate.is_some() { self.new_block() } else { join };
                self.branch(t, then_bb, else_bb, span);
                self.switch_to(then_bb);
                self.lower_sub_stmt(&i.consequent);
                self.jump(join, span);
                if let Some(a) = &i.alternate {
                    self.switch_to(else_bb);
                    self.lower_sub_stmt(a);
                    self.jump(join, span);
                }
                self.switch_to(join);
            }
            Statement::LabeledStatement(l) => {
                let mut labels = labels;
                labels.push(l.label.name.to_string());
                match &l.body {
                    Statement::ForStatement(_)
                    | Statement::ForInStatement(_)
                    | Statement::ForOfStatement(_)
                    | Statement::WhileStatement(_)
                    | Statement::DoWhileStatement(_)
                    | Statement::LabeledStatement(_) => self.lower_stmt(&l.body, labels),
                    Statement::FunctionDeclaration(_) => self.unsupported(span, "labeled function declarations"),
                    body => {
                        let after = self.new_block();
                        self.push_target(labels, TargetKind::Block, after, None, None);
                        self.lower_sub_stmt(body);
                        self.pop_ctl();
                        self.jump(after, span);
                        self.switch_to(after);
                    }
                }
            }
            Statement::ReturnStatement(r) => {
                if self.fbr().f.kind == dj_ir::FunctionKind::Script {
                    self.semantic(span, "A 'return' statement can only be used within a function body");
                    return;
                }
                self.stmt(span);
                let v = match &r.argument {
                    Some(e) => self.lower_expr(e),
                    None => self.undef(span),
                };
                self.emit_return(v, span);
            }
            Statement::SwitchStatement(sw) => self.lower_switch(sw, labels),
            Statement::ThrowStatement(t) => {
                self.stmt(span);
                let v = self.lower_expr(&t.argument);
                self.terminate(TermKind::Throw(v), span);
            }
            Statement::TryStatement(t) => self.lower_try(t),
            Statement::WhileStatement(w) => self.lower_while(w, labels),
            Statement::WithStatement(_) => self.unsupported(span, "with statements"),
            Statement::VariableDeclaration(d) => self.lower_var_decl(d),
            Statement::FunctionDeclaration(_) => {}
            Statement::ClassDeclaration(c) => {
                if c.declare {
                    return;
                }
                self.stmt(span);
                let v = self.lower_class(c, None);
                if let Some(id) = &c.id {
                    self.init_name(id.name.as_str(), v, sp(id.span));
                }
            }
            Statement::TSTypeAliasDeclaration(_) | Statement::TSInterfaceDeclaration(_) => {}
            Statement::TSGlobalDeclaration(_) => {}
            Statement::TSEnumDeclaration(e) => {
                if e.declare {
                    return;
                }
                self.stmt(span);
                let v = self.lower_enum(e);
                self.init_name(e.id.name.as_str(), v, sp(e.id.span));
            }
            Statement::TSExternalModuleDeclaration(m) => {
                if !m.declare {
                    self.unsupported(span, "module declarations");
                }
            }
            Statement::TSNamespaceDeclaration(n) => {
                if !n.declare {
                    self.unsupported(span, "namespaces");
                }
            }
            Statement::TSImportEqualsDeclaration(i) => {
                if i.import_kind != ImportOrExportKind::Type {
                    self.unsupported(span, "import = require()");
                }
            }
            Statement::ImportDeclaration(i) => {
                if i.import_kind != ImportOrExportKind::Type {
                    self.unsupported(span, "runtime imports (modules)");
                }
            }
            Statement::ExportDeclaration(e) => {
                let type_only = match &e.declaration {
                    Declaration::TSTypeAliasDeclaration(_) | Declaration::TSInterfaceDeclaration(_) => true,
                    Declaration::VariableDeclaration(d) => d.declare,
                    Declaration::FunctionDeclaration(f) => f.declare || f.body.is_none(),
                    Declaration::ClassDeclaration(c) => c.declare,
                    Declaration::TSEnumDeclaration(en) => en.declare,
                    Declaration::TSNamespaceDeclaration(n) => n.declare,
                    Declaration::TSExternalModuleDeclaration(m) => m.declare,
                    Declaration::TSGlobalDeclaration(_) => true,
                    Declaration::TSImportEqualsDeclaration(i) => i.import_kind == ImportOrExportKind::Type,
                };
                if !type_only {
                    self.unsupported(span, "runtime exports (modules)");
                }
            }
            Statement::ExportNamedDeclaration(e) => {
                if e.export_kind != ImportOrExportKind::Type {
                    self.unsupported(span, "runtime exports (modules)");
                }
            }
            Statement::ExportFromDeclaration(e) => {
                if e.export_kind != ImportOrExportKind::Type {
                    self.unsupported(span, "runtime exports (modules)");
                }
            }
            Statement::ExportDefaultDeclaration(e) => {
                if !matches!(e.declaration, ExportDefaultDeclarationKind::TSInterfaceDeclaration(_)) {
                    self.unsupported(span, "runtime exports (modules)");
                }
            }
            Statement::ExportAllDeclaration(e) => {
                if e.export_kind != ImportOrExportKind::Type {
                    self.unsupported(span, "runtime exports (modules)");
                }
            }
            Statement::TSExportAssignment(_) => self.unsupported(span, "export = (modules)"),
            Statement::TSNamespaceExportDeclaration(_) => {}
        }
    }

    pub(crate) fn lower_var_decl(&mut self, d: &VariableDeclaration<'_>) {
        if d.declare {
            return;
        }
        if matches!(d.kind, VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing) {
            self.unsupported(sp(d.span), "using declarations");
            return;
        }
        for decl in &d.declarations {
            let dspan = sp(decl.span);
            match &decl.init {
                Some(init) => {
                    self.stmt(dspan);
                    let hint = match &decl.id {
                        BindingPattern::BindingIdentifier(id) => Some(id.name.as_str()),
                        _ => None,
                    };
                    let v = self.lower_expr_named(init, hint);
                    self.bind_pattern(&decl.id, v);
                }
                None if d.kind != VariableDeclarationKind::Var => {
                    self.stmt(dspan);
                    let u = self.undef(dspan);
                    self.bind_pattern(&decl.id, u);
                }
                None => {}
            }
        }
    }

    fn lower_while(&mut self, w: &WhileStatement<'_>, labels: Vec<String>) {
        let span = sp(w.span);
        let header = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();
        self.jump(header, span);
        self.switch_to(header);
        let tspan = sp(w.test.span());
        self.stmt(tspan);
        let t = self.lower_expr(&w.test);
        self.branch(t, body, exit, tspan);
        self.push_target(labels, TargetKind::Loop, exit, Some(header), None);
        self.switch_to(body);
        self.lower_sub_stmt(&w.body);
        self.jump(header, span);
        self.pop_ctl();
        self.switch_to(exit);
    }

    fn lower_do_while(&mut self, d: &DoWhileStatement<'_>, labels: Vec<String>) {
        let span = sp(d.span);
        let body = self.new_block();
        let cont = self.new_block();
        let exit = self.new_block();
        self.jump(body, span);
        self.push_target(labels, TargetKind::Loop, exit, Some(cont), None);
        self.switch_to(body);
        self.lower_sub_stmt(&d.body);
        self.jump(cont, span);
        self.pop_ctl();
        self.switch_to(cont);
        let tspan = sp(d.test.span());
        self.stmt(tspan);
        let t = self.lower_expr(&d.test);
        self.branch(t, body, exit, tspan);
        self.switch_to(exit);
    }

    fn lower_for(&mut self, f: &ForStatement<'_>, labels: Vec<String>) {
        let span = sp(f.span);
        let scoped = matches!(&f.init, Some(ForStatementInit::VariableDeclaration(d)) if d.kind != VariableDeclarationKind::Var);
        if scoped {
            self.push_scope(ScopeKind::For, span);
            if let Some(ForStatementInit::VariableDeclaration(d)) = &f.init {
                // Declared through a synthetic statement list of one element.
                let kind = if d.kind == VariableDeclarationKind::Const { LocalKind::Const } else { LocalKind::Let };
                for decl in &d.declarations {
                    let mut names = Vec::new();
                    crate::decls::binding_names(&decl.id, &mut names);
                    for (n, s) in names {
                        self.declare(n, kind, s, decl.span.end, kind == LocalKind::Const, false);
                    }
                }
            }
            self.emit_declares(span);
        }
        match &f.init {
            Some(ForStatementInit::VariableDeclaration(d)) => self.lower_var_decl(d),
            Some(init) => {
                if let Some(e) = init.as_expression() {
                    let es = sp(e.span());
                    self.stmt(es);
                    self.lower_expr(e);
                }
            }
            None => {}
        }
        let loop_vars: Vec<dj_ir::LocalId> = if scoped {
            let s = self.scopes.last().unwrap();
            s.lexicals.iter().copied().filter(|l| self.fbr().f.locals[l.index()].kind == LocalKind::Let).collect()
        } else {
            Vec::new()
        };
        for &l in &loop_vars {
            self.emit(InstKind::CopyBinding { local: l }, span);
        }
        let header = self.new_block();
        let body = self.new_block();
        let cont = self.new_block();
        let exit = self.new_block();
        self.jump(header, span);
        self.switch_to(header);
        match &f.test {
            Some(t) => {
                let tspan = sp(t.span());
                self.stmt(tspan);
                let c = self.lower_expr(t);
                self.branch(c, body, exit, tspan);
            }
            None => self.jump(body, span),
        }
        self.push_target(labels, TargetKind::Loop, exit, Some(cont), None);
        self.switch_to(body);
        self.lower_sub_stmt(&f.body);
        self.jump(cont, span);
        self.pop_ctl();
        self.switch_to(cont);
        for &l in &loop_vars {
            self.emit(InstKind::CopyBinding { local: l }, span);
        }
        if let Some(u) = &f.update {
            let us = sp(u.span());
            self.stmt(us);
            self.lower_expr(u);
        }
        self.jump(header, span);
        self.switch_to(exit);
        if scoped {
            self.pop_scope();
        }
    }

    fn lower_for_in_of(
        &mut self,
        left: &ForStatementLeft<'_>,
        right: &Expression<'_>,
        body: &Statement<'_>,
        is_of: bool,
        span: IrSpan,
        labels: Vec<String>,
    ) {
        // Loop-head lexical declaration: TDZ covers the `right` expression.
        let decl = match left {
            ForStatementLeft::VariableDeclaration(d) => Some(&**d),
            _ => None,
        };
        let scoped = decl.is_some_and(|d| d.kind != VariableDeclarationKind::Var);
        if let Some(d) = decl {
            if matches!(d.kind, VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing) {
                self.unsupported(sp(d.span), "using declarations");
                return;
            }
        }
        let rspan = sp(right.span());
        if scoped {
            self.push_scope(ScopeKind::For, span);
            let d = decl.unwrap();
            let kind = if d.kind == VariableDeclarationKind::Const { LocalKind::Const } else { LocalKind::Let };
            for decl in &d.declarations {
                let mut names = Vec::new();
                crate::decls::binding_names(&decl.id, &mut names);
                for (n, s) in names {
                    self.declare(n, kind, s, rspan.end, kind == LocalKind::Const, false);
                }
            }
            self.emit_declares(span);
        }
        self.stmt(IrSpan::new(span.start, rspan.end));
        let r = self.lower_expr(right);
        let iter_local = self.synthetic("%iter", span);
        let it = self.temp();
        if is_of {
            self.emit(InstKind::GetIterator { dst: it, iterable: r }, rspan);
        } else {
            self.emit(InstKind::ForInIterator { dst: it, obj: r }, rspan);
        }
        self.store_local(iter_local, it, rspan);

        let outer_h = self.fbr().handler;
        let header = self.new_block_h(outer_h);
        let exit = self.new_block_h(outer_h);
        let close_h = if is_of { Some(self.new_block_h(outer_h)) } else { None };
        let body_h = close_h.or(outer_h);
        let body_bb = self.new_block_h(body_h);
        self.jump(header, span);

        self.switch_to(header);
        let lspan = sp(left.span());
        self.stmt(lspan);
        let i = self.load_local(iter_local, false, lspan);
        let done = self.temp();
        let value = self.temp();
        self.emit(InstKind::IterNext { done, value, iter: i }, lspan);
        self.branch(done, exit, body_bb, lspan);

        self.push_target(labels, TargetKind::Loop, exit, Some(header), if is_of { Some((iter_local, outer_h)) } else { None });
        self.fb().handler = body_h;
        self.switch_to(body_bb);
        match left {
            ForStatementLeft::VariableDeclaration(d) => {
                if scoped {
                    // Fresh per-iteration bindings.
                    self.emit_declares(lspan);
                }
                if let Some(decl) = d.declarations.first() {
                    if decl.init.is_some() {
                        self.semantic(sp(decl.span), "for-in/of loop variable declaration may not have an initializer");
                    }
                    self.bind_pattern(&decl.id, value);
                }
            }
            other => {
                if let Some(target) = other.as_assignment_target() {
                    let p = self.prepare_target(target, false);
                    self.store_prepared(p, value, lspan);
                }
            }
        }
        self.lower_sub_stmt(body);
        self.jump(header, span);
        self.pop_ctl();
        self.fb().handler = outer_h;

        if let Some(close_h) = close_h {
            // Exception inside the body: close the iterator (ignoring errors
            // from `return()`), then rethrow the original exception.
            self.switch_to(close_h);
            let exc = self.synthetic("%exc", span);
            let e = self.temp();
            self.emit(InstKind::CatchValue { dst: e }, span);
            self.store_local(exc, e, span);
            let swallow = self.new_block_h(outer_h);
            let closing = self.new_block_h(Some(swallow));
            let rethrow = self.new_block_h(outer_h);
            self.jump(closing, span);
            self.switch_to(closing);
            let i2 = self.load_local(iter_local, false, span);
            self.emit(InstKind::IterClose { iter: i2 }, span);
            self.jump(rethrow, span);
            self.switch_to(swallow);
            let ignored = self.temp();
            self.emit(InstKind::CatchValue { dst: ignored }, span);
            self.jump(rethrow, span);
            self.switch_to(rethrow);
            let ev = self.load_local(exc, false, span);
            self.terminate(TermKind::Throw(ev), span);
        }

        self.switch_to(exit);
        if scoped {
            self.pop_scope();
        }
    }

    fn lower_switch(&mut self, sw: &SwitchStatement<'_>, labels: Vec<String>) {
        let span = sp(sw.span);
        self.stmt(IrSpan::new(span.start, sp(sw.discriminant.span()).end));
        let d = self.lower_expr(&sw.discriminant);
        self.push_scope(ScopeKind::Switch, span);
        let h = self.declare_lexical(sw.cases.iter().flat_map(|c| c.consequent.iter()));
        self.emit_hoisted(h, span);

        let exit = self.new_block();
        let case_bbs: Vec<dj_ir::BlockId> = sw.cases.iter().map(|_| self.new_block()).collect();
        let mut default_idx = None;
        for (i, c) in sw.cases.iter().enumerate() {
            match &c.test {
                Some(t) => {
                    let tspan = sp(t.span());
                    let v = self.lower_expr(t);
                    let eq = self.binary(BinaryOp::StrictEq, d, v, tspan);
                    let next = self.new_block();
                    self.branch(eq, case_bbs[i], next, tspan);
                    self.switch_to(next);
                }
                None => default_idx = Some(i),
            }
        }
        let fallback = default_idx.map(|i| case_bbs[i]).unwrap_or(exit);
        self.jump(fallback, span);

        self.push_target(labels, TargetKind::Switch, exit, None, None);
        for (i, c) in sw.cases.iter().enumerate() {
            self.switch_to(case_bbs[i]);
            self.lower_stmts(&c.consequent);
            let next = case_bbs.get(i + 1).copied().unwrap_or(exit);
            self.jump(next, span);
        }
        self.pop_ctl();
        self.pop_scope();
        self.switch_to(exit);
    }

    fn lower_try(&mut self, t: &TryStatement<'_>) {
        let span = sp(t.span);
        let outer_h = self.fbr().handler;
        let after = self.new_block_h(outer_h);

        let fin = if t.finalizer.is_some() {
            let fkind = self.synthetic("%fkind", span);
            let fval = self.synthetic("%fval", span);
            let entry = self.new_block_h(outer_h);
            let throw_bb = self.new_block_h(outer_h);
            Some((fkind, fval, entry, throw_bb))
        } else {
            None
        };
        if let Some((fkind, fval, entry, _)) = fin {
            self.fb().ctl.push(Ctl::Finally(FinCtx { entry, fkind, fval, used_return: false, exits: Vec::new() }));
        }
        let catch_handler = fin.map(|f| f.3).or(outer_h);
        let catch_bb = t.handler.as_ref().map(|_| self.new_block_h(catch_handler));
        let try_h = catch_bb.or(fin.map(|f| f.3));

        // try block
        self.fb().handler = try_h;
        let body = self.new_block();
        self.jump(body, span);
        self.switch_to(body);
        self.lower_block(&t.block.body, ScopeKind::Block, sp(t.block.span));
        self.normal_exit(fin.map(|f| (f.0, f.2)), after, span);

        // catch clause
        if let (Some(c), Some(catch_bb)) = (&t.handler, catch_bb) {
            let cspan = sp(c.span);
            self.fb().handler = catch_handler;
            self.switch_to(catch_bb);
            let e = self.temp();
            self.emit(InstKind::CatchValue { dst: e }, cspan);
            self.push_scope(ScopeKind::Catch, cspan);
            if let Some(p) = &c.param {
                let mut names = Vec::new();
                crate::decls::binding_names(&p.pattern, &mut names);
                let mut locals = Vec::new();
                for (n, s) in names {
                    locals.push(self.declare(n, LocalKind::Catch, s, 0, false, false));
                }
                // Fresh binding per catch execution.
                for l in locals {
                    self.emit(InstKind::DeclareLocal { local: l }, cspan);
                }
                self.bind_pattern(&p.pattern, e);
            }
            self.lower_block(&c.body.body, ScopeKind::Block, sp(c.body.span));
            self.pop_scope();
            self.normal_exit(fin.map(|f| (f.0, f.2)), after, span);
        }

        // finally
        if let Some((fkind, fval, entry, throw_bb)) = fin {
            let Some(Ctl::Finally(fc)) = self.fb().ctl.pop() else { unreachable!("finally ctx") };
            self.fb().handler = outer_h;

            self.switch_to(throw_bb);
            let x = self.temp();
            self.emit(InstKind::CatchValue { dst: x }, span);
            self.store_local(fval, x, span);
            let two = self.konst(Constant::Number(2.0), span);
            self.store_local(fkind, two, span);
            self.jump(entry, span);

            self.switch_to(entry);
            let fspan = sp(t.finalizer.as_ref().unwrap().span);
            self.lower_block(&t.finalizer.as_ref().unwrap().body, ScopeKind::Block, fspan);

            // Dispatch on the completion kind.
            let end = IrSpan::new(fspan.end, fspan.end);
            let k = self.load_local(fkind, false, end);
            let mut cases: Vec<(u32, Option<(u32, bool)>)> = vec![(0, None), (2, None)];
            if fc.used_return {
                cases.push((1, None));
            }
            for (code, target, cont) in &fc.exits {
                cases.push((*code, Some((*target, *cont))));
            }
            for (code, jump) in cases {
                let c = self.konst(Constant::Number(code as f64), end);
                let eq = self.binary(BinaryOp::StrictEq, k, c, end);
                let action = self.new_block();
                let next = self.new_block();
                self.branch(eq, action, next, end);
                self.switch_to(action);
                match (code, jump) {
                    (0, _) => self.jump(after, end),
                    (2, _) => {
                        let v = self.load_local(fval, false, end);
                        self.terminate(TermKind::Throw(v), end);
                    }
                    (1, _) => {
                        let v = self.load_local(fval, false, end);
                        self.emit_return(v, end);
                    }
                    (_, Some((target, cont))) => self.emit_jump(target, cont, end),
                    _ => unreachable!(),
                }
                self.switch_to(next);
            }
            self.terminate(TermKind::Unreachable, end);
        }

        self.fb().handler = outer_h;
        self.switch_to(after);
    }

    fn normal_exit(&mut self, fin: Option<(dj_ir::LocalId, dj_ir::BlockId)>, after: dj_ir::BlockId, span: IrSpan) {
        match fin {
            Some((fkind, entry)) => {
                let z = self.konst(Constant::Number(0.0), span);
                self.store_local(fkind, z, span);
                self.jump(entry, span);
            }
            None => self.jump(after, span),
        }
    }
}

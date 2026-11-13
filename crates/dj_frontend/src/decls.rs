//! Declaration collection and hoisting.

use oxc_ast::ast::*;

use dj_ir::{InstKind, LocalId, LocalKind, Span as IrSpan};

use crate::builder::{Lowerer, sp};

/// Function declarations to instantiate at scope entry.
pub(crate) struct Hoisted<'b, 'a> {
    pub funcs: Vec<(&'b Function<'a>, LocalId)>,
}

pub(crate) fn binding_names<'b>(p: &'b BindingPattern<'_>, out: &mut Vec<(&'b str, IrSpan)>) {
    match p {
        BindingPattern::BindingIdentifier(id) => out.push((id.name.as_str(), sp(id.span))),
        BindingPattern::ObjectPattern(o) => {
            for prop in &o.properties {
                binding_names(&prop.value, out);
            }
            if let Some(r) = &o.rest {
                binding_names(&r.argument, out);
            }
        }
        BindingPattern::ArrayPattern(a) => {
            for e in a.elements.iter().flatten() {
                binding_names(e, out);
            }
            if let Some(r) = &a.rest {
                binding_names(&r.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(a) => binding_names(&a.left, out),
    }
}

fn collect_vars_stmt<'b>(s: &'b Statement<'_>, out: &mut Vec<(&'b str, IrSpan)>) {
    match s {
        Statement::VariableDeclaration(d) => collect_var_decl(d, out),
        Statement::BlockStatement(b) => b.body.iter().for_each(|s| collect_vars_stmt(s, out)),
        Statement::IfStatement(i) => {
            collect_vars_stmt(&i.consequent, out);
            if let Some(a) = &i.alternate {
                collect_vars_stmt(a, out);
            }
        }
        Statement::WhileStatement(w) => collect_vars_stmt(&w.body, out),
        Statement::DoWhileStatement(w) => collect_vars_stmt(&w.body, out),
        Statement::ForStatement(f) => {
            if let Some(ForStatementInit::VariableDeclaration(d)) = &f.init {
                collect_var_decl(d, out);
            }
            collect_vars_stmt(&f.body, out);
        }
        Statement::ForInStatement(f) => {
            if let ForStatementLeft::VariableDeclaration(d) = &f.left {
                collect_var_decl(d, out);
            }
            collect_vars_stmt(&f.body, out);
        }
        Statement::ForOfStatement(f) => {
            if let ForStatementLeft::VariableDeclaration(d) = &f.left {
                collect_var_decl(d, out);
            }
            collect_vars_stmt(&f.body, out);
        }
        Statement::LabeledStatement(l) => collect_vars_stmt(&l.body, out),
        Statement::TryStatement(t) => {
            t.block.body.iter().for_each(|s| collect_vars_stmt(s, out));
            if let Some(h) = &t.handler {
                h.body.body.iter().for_each(|s| collect_vars_stmt(s, out));
            }
            if let Some(f) = &t.finalizer {
                f.body.iter().for_each(|s| collect_vars_stmt(s, out));
            }
        }
        Statement::SwitchStatement(sw) => {
            for c in &sw.cases {
                c.consequent.iter().for_each(|s| collect_vars_stmt(s, out));
            }
        }
        _ => {}
    }
}

fn collect_var_decl<'b>(d: &'b VariableDeclaration<'_>, out: &mut Vec<(&'b str, IrSpan)>) {
    if d.kind == VariableDeclarationKind::Var && !d.declare {
        for decl in &d.declarations {
            binding_names(&decl.id, out);
        }
    }
}

impl<'s> Lowerer<'s> {
    /// Declare `var`s of a function body (deep) in the function scope.
    pub(crate) fn declare_vars(&mut self, stmts: &[Statement<'_>]) {
        let mut names = Vec::new();
        for s in stmts {
            collect_vars_stmt(s, &mut names);
        }
        for (name, span) in names {
            self.declare(name, LocalKind::Var, span, 0, false, true);
        }
    }

    /// Declare lexical bindings and function declarations of a statement list
    /// in the innermost scope. Returns the functions to instantiate.
    pub(crate) fn declare_lexical<'b, 'a>(&mut self, stmts: impl IntoIterator<Item = &'b Statement<'a>>) -> Hoisted<'b, 'a>
    where
        'a: 'b,
    {
        let mut funcs = Vec::new();
        for s in stmts {
            match s {
                Statement::VariableDeclaration(d) if !d.declare => {
                    let kind = match d.kind {
                        VariableDeclarationKind::Let => LocalKind::Let,
                        VariableDeclarationKind::Const => LocalKind::Const,
                        _ => continue,
                    };
                    for decl in &d.declarations {
                        let mut names = Vec::new();
                        binding_names(&decl.id, &mut names);
                        for (name, span) in names {
                            self.declare(name, kind, span, decl.span.end, kind == LocalKind::Const, false);
                        }
                    }
                }
                Statement::FunctionDeclaration(f) if !f.declare && f.body.is_some() => {
                    let Some(id) = &f.id else { continue };
                    if f.generator {
                        self.unsupported(sp(f.span), "generator functions");
                        continue;
                    }
                    let l = self.declare(id.name.as_str(), LocalKind::Function, sp(id.span), 0, false, false);
                    funcs.push((&**f, l));
                }
                Statement::ClassDeclaration(c) if !c.declare => {
                    if let Some(id) = &c.id {
                        self.declare(id.name.as_str(), LocalKind::Class, sp(id.span), c.span.end, false, false);
                    }
                }
                Statement::TSEnumDeclaration(e) if !e.declare => {
                    self.declare(e.id.name.as_str(), LocalKind::Let, sp(e.id.span), e.span.end, false, false);
                }
                _ => {}
            }
        }
        Hoisted { funcs }
    }

    /// Emit `DeclareLocal`s of the innermost scope and create hoisted functions.
    pub(crate) fn emit_hoisted(&mut self, h: Hoisted<'_, '_>, span: IrSpan) {
        self.emit_declares(span);
        for (f, local) in h.funcs {
            let fid = self.lower_function_decl(f);
            let t = self.temp();
            let fspan = sp(f.span);
            self.emit(InstKind::MakeClosure { dst: t, func: fid }, fspan);
            self.store_local(local, t, fspan);
        }
    }
}

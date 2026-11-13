//! Lowering state: function builders, lexical scopes, name resolution,
//! instruction emission and structured control-flow unwinding.

use std::collections::HashMap;

use dj_ir::*;

use crate::{Diagnostic, Severity};

pub(crate) fn sp(s: oxc_span::Span) -> Span {
    Span::new(s.start, s.end)
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub local: LocalId,
    pub kind: LocalKind,
    pub fn_depth: usize,
    /// End offset of the declaration; same-function references before it are TDZ-checked.
    pub decl_end: u32,
    /// Declared directly in a `switch` body: every same-function reference is checked.
    pub in_switch: bool,
    /// Assignment throws TypeError (const, class inner name, named function expression name).
    pub immutable: bool,
    /// The binding can be observed uninitialized.
    pub tdz: bool,
}

pub(crate) struct LexScope {
    pub ir: ScopeId,
    pub fn_depth: usize,
    pub kind: ScopeKind,
    pub names: HashMap<String, Binding>,
    /// Lexical (TDZ) bindings in declaration order, for `DeclareLocal`.
    pub lexicals: Vec<LocalId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetKind {
    Loop,
    Switch,
    Block,
}

pub(crate) struct FinCtx {
    pub entry: BlockId,
    pub fkind: LocalId,
    pub fval: LocalId,
    pub used_return: bool,
    /// Jump exits routed through this finally: (code, target id, is_continue).
    pub exits: Vec<(u32, u32, bool)>,
}

pub(crate) enum Ctl {
    Target {
        id: u32,
        labels: Vec<String>,
        kind: TargetKind,
        break_bb: BlockId,
        continue_bb: Option<BlockId>,
        /// for-of iterator to close when leaving the loop: (iterator local, handler outside the loop).
        iter: Option<(LocalId, Option<BlockId>)>,
    },
    Finally(FinCtx),
}

pub(crate) struct FnBuilder {
    pub f: Function,
    pub cur: BlockId,
    pub handler: Option<BlockId>,
    pub scope: ScopeId,
    pub ctl: Vec<Ctl>,
    pub this_local: Option<LocalId>,
    pub new_target_local: Option<LocalId>,
    pub upvals: HashMap<(usize, LocalId), UpvalId>,
    pub chain_nil: Option<BlockId>,
    /// TS parameter properties of a derived constructor, assigned after each `super()`.
    pub param_props: Vec<(Atom, LocalId, Span)>,
}

pub(crate) enum Resolved {
    Local { local: LocalId, check: bool, immutable: bool },
    Upval { upval: UpvalId, check: bool, immutable: bool },
    Global,
}

pub(crate) struct Lowerer<'s> {
    pub source: &'s str,
    pub file_name: String,
    pub funcs: Vec<Option<Function>>,
    pub fns: Vec<FnBuilder>,
    pub scopes: Vec<LexScope>,
    pub diags: Vec<Diagnostic>,
    pub next_target: u32,
}

impl<'s> Lowerer<'s> {
    pub fn new(source: &'s str, file_name: &str) -> Self {
        Lowerer {
            source,
            file_name: file_name.to_string(),
            funcs: Vec::new(),
            fns: Vec::new(),
            scopes: Vec::new(),
            diags: Vec::new(),
            next_target: 0,
        }
    }

    // --- diagnostics -------------------------------------------------------

    pub fn unsupported(&mut self, span: Span, what: &str) {
        self.diags.push(Diagnostic {
            severity: Severity::Error,
            message: format!("unsupported: {what}"),
            span,
            code: "unsupported",
        });
    }

    pub fn semantic(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic { severity: Severity::Error, message: msg.into(), span, code: "semantic" });
    }

    // --- function builders -------------------------------------------------

    pub fn fb(&mut self) -> &mut FnBuilder {
        self.fns.last_mut().expect("no function")
    }

    pub fn fbr(&self) -> &FnBuilder {
        self.fns.last().expect("no function")
    }

    pub fn depth(&self) -> usize {
        self.fns.len() - 1
    }

    pub fn begin_function(&mut self, name: &str, kind: FunctionKind, span: Span, is_async: bool) -> FuncId {
        let id = FuncId(self.funcs.len() as u32);
        self.funcs.push(None);
        let parent = self.fns.last().map(|b| b.f.id);
        let mut f = Function::new(id, name.into(), kind, span, parent);
        f.is_async = is_async;
        self.fns.push(FnBuilder {
            f,
            cur: BlockId(0),
            handler: None,
            scope: ScopeId(0),
            ctl: Vec::new(),
            this_local: None,
            new_target_local: None,
            upvals: HashMap::new(),
            chain_nil: None,
            param_props: Vec::new(),
        });
        let depth = self.depth();
        self.scopes.push(LexScope {
            ir: ScopeId(0),
            fn_depth: depth,
            kind: ScopeKind::Function,
            names: HashMap::new(),
            lexicals: Vec::new(),
        });
        id
    }

    pub fn end_function(&mut self) -> FuncId {
        let depth = self.depth();
        let s = self.scopes.pop().expect("function scope");
        assert!(s.fn_depth == depth && s.kind == ScopeKind::Function, "unbalanced scopes");
        let b = self.fns.pop().expect("function");
        let mut f = b.f;
        prune_unreachable(&mut f);
        let id = f.id;
        self.funcs[id.index()] = Some(f);
        id
    }

    /// Create the `this` binding of a non-arrow function.
    pub fn setup_this(&mut self, span: Span) {
        let kind = self.fbr().f.kind;
        match kind {
            FunctionKind::Script | FunctionKind::Arrow => {}
            FunctionKind::DerivedConstructor => {
                let l = self.fb().f.new_local(Local {
                    name: "this".into(),
                    kind: LocalKind::This,
                    scope: ScopeId(0),
                    captured: false,
                    needs_tdz: true,
                    span,
                });
                self.fb().this_local = Some(l);
                self.emit(InstKind::DeclareLocal { local: l }, span);
            }
            _ => {
                let l = self.fb().f.new_local(Local {
                    name: "this".into(),
                    kind: LocalKind::This,
                    scope: ScopeId(0),
                    captured: false,
                    needs_tdz: false,
                    span,
                });
                self.fb().this_local = Some(l);
                let t = self.temp();
                self.emit(InstKind::LoadThis { dst: t }, span);
                self.emit(InstKind::StoreLocal { local: l, src: t }, span);
            }
        }
    }

    // --- emission ----------------------------------------------------------

    pub fn temp(&mut self) -> Temp {
        self.fb().f.new_temp()
    }

    pub fn emit(&mut self, kind: InstKind, span: Span) {
        let fb = self.fb();
        let scope = fb.scope;
        let cur = fb.cur;
        fb.f.block_mut(cur).insts.push(Inst { kind, span, scope });
    }

    pub fn konst(&mut self, value: Constant, span: Span) -> Temp {
        let dst = self.temp();
        self.emit(InstKind::Const { dst, value }, span);
        dst
    }

    pub fn undef(&mut self, span: Span) -> Temp {
        self.konst(Constant::Undefined, span)
    }

    pub fn str_const(&mut self, s: &str, span: Span) -> Temp {
        self.konst(Constant::String(s.into()), span)
    }

    pub fn unary(&mut self, op: UnaryOp, src: Temp, span: Span) -> Temp {
        let dst = self.temp();
        self.emit(InstKind::Unary { dst, op, src }, span);
        dst
    }

    pub fn binary(&mut self, op: BinaryOp, lhs: Temp, rhs: Temp, span: Span) -> Temp {
        let dst = self.temp();
        self.emit(InstKind::Binary { dst, op, lhs, rhs }, span);
        dst
    }

    pub fn stmt(&mut self, span: Span) {
        self.emit(InstKind::Stmt, span);
    }

    pub fn new_block(&mut self) -> BlockId {
        let h = self.fbr().handler;
        self.new_block_h(h)
    }

    pub fn new_block_h(&mut self, handler: Option<BlockId>) -> BlockId {
        let fb = self.fb();
        let scope = fb.scope;
        let span = fb.f.span;
        let b = fb.f.new_block(span, scope);
        fb.f.block_mut(b).handler = handler;
        b
    }

    pub fn switch_to(&mut self, b: BlockId) {
        self.fb().cur = b;
    }

    /// Terminate the current block and continue in a fresh (unreachable) block.
    pub fn terminate(&mut self, kind: TermKind, span: Span) {
        let fb = self.fb();
        let scope = fb.scope;
        let cur = fb.cur;
        fb.f.block_mut(cur).term = Terminator { kind, span, scope };
        let fresh = self.new_block();
        self.switch_to(fresh);
    }

    pub fn jump(&mut self, target: BlockId, span: Span) {
        self.terminate(TermKind::Jump(target), span);
    }

    pub fn branch(&mut self, cond: Temp, then_bb: BlockId, else_bb: BlockId, span: Span) {
        self.terminate(TermKind::Branch { cond, then_bb, else_bb }, span);
    }

    pub fn synthetic(&mut self, name: &str, span: Span) -> LocalId {
        self.fb().f.new_local(Local {
            name: name.into(),
            kind: LocalKind::Synthetic,
            scope: ScopeId(0),
            captured: false,
            needs_tdz: false,
            span,
        })
    }

    pub fn store_local(&mut self, local: LocalId, src: Temp, span: Span) {
        self.emit(InstKind::StoreLocal { local, src }, span);
    }

    pub fn load_local(&mut self, local: LocalId, check_tdz: bool, span: Span) -> Temp {
        let dst = self.temp();
        self.emit(InstKind::LoadLocal { dst, local, check_tdz }, span);
        dst
    }

    // --- scopes ------------------------------------------------------------

    pub fn push_scope(&mut self, kind: ScopeKind, span: Span) -> ScopeId {
        let parent = self.fbr().scope;
        let id = self.fb().f.new_scope(parent, kind, span);
        self.fb().scope = id;
        let depth = self.depth();
        self.scopes.push(LexScope { ir: id, fn_depth: depth, kind, names: HashMap::new(), lexicals: Vec::new() });
        id
    }

    pub fn pop_scope(&mut self) {
        let s = self.scopes.pop().expect("scope");
        debug_assert!(s.kind != ScopeKind::Function);
        let parent = self.fbr().f.scopes[s.ir.index()].parent.expect("parent scope");
        self.fb().scope = parent;
    }

    fn function_scope_index(&self) -> usize {
        let d = self.depth();
        self.scopes.iter().rposition(|s| s.fn_depth == d && s.kind == ScopeKind::Function).unwrap()
    }

    /// Declare `name` in the innermost scope (or the function scope for
    /// var-like kinds when `function_level`).
    pub fn declare(
        &mut self,
        name: &str,
        kind: LocalKind,
        span: Span,
        decl_end: u32,
        immutable: bool,
        function_level: bool,
    ) -> LocalId {
        let si = if function_level { self.function_scope_index() } else { self.scopes.len() - 1 };
        let var_like = |k: LocalKind| matches!(k, LocalKind::Var | LocalKind::Function | LocalKind::Param);
        if let Some(existing) = self.scopes[si].names.get(name) {
            if var_like(existing.kind) && var_like(kind) {
                return existing.local;
            }
            let local = existing.local;
            self.semantic(span, format!("Identifier '{name}' has already been declared"));
            return local;
        }
        let scope_ir = self.scopes[si].ir;
        let scope_kind = self.scopes[si].kind;
        let tdz = kind.is_lexical();
        let local = self.fb().f.new_local(Local {
            name: name.into(),
            kind,
            scope: scope_ir,
            captured: false,
            needs_tdz: false,
            span,
        });
        let depth = self.depth();
        let s = &mut self.scopes[si];
        s.names.insert(
            name.to_string(),
            Binding {
                local,
                kind,
                fn_depth: depth,
                decl_end,
                in_switch: scope_kind == ScopeKind::Switch,
                immutable,
                tdz,
            },
        );
        if tdz {
            s.lexicals.push(local);
        }
        local
    }

    /// Register an existing local under `name` in the innermost scope (parameters).
    pub fn bind_existing(&mut self, name: &str, local: LocalId, kind: LocalKind) {
        let depth = self.depth();
        let s = self.scopes.last_mut().unwrap();
        s.names.insert(
            name.to_string(),
            Binding {
                local,
                kind,
                fn_depth: depth,
                decl_end: 0,
                in_switch: false,
                immutable: false,
                tdz: false,
            },
        );
    }

    pub fn is_declared_in_function_scope(&self, name: &str) -> bool {
        self.scopes[self.function_scope_index()].names.contains_key(name)
    }

    /// `DeclareLocal` for every lexical binding of the innermost scope.
    pub fn emit_declares(&mut self, span: Span) {
        let ls = self.scopes.last().unwrap().lexicals.clone();
        for l in ls {
            self.emit(InstKind::DeclareLocal { local: l }, span);
        }
    }

    pub fn resolve(&mut self, name: &str, pos: u32) -> Resolved {
        let mut found: Option<Binding> = None;
        for s in self.scopes.iter().rev() {
            if let Some(b) = s.names.get(name) {
                found = Some(b.clone());
                break;
            }
        }
        let Some(b) = found else { return Resolved::Global };
        let d = self.depth();
        if b.fn_depth == d {
            let check = b.tdz && (pos < b.decl_end || b.in_switch);
            if check {
                self.fb().f.locals[b.local.index()].needs_tdz = true;
            }
            Resolved::Local { local: b.local, check, immutable: b.immutable }
        } else {
            let u = self.capture_into(d, b.fn_depth, b.local, name, b.kind, b.tdz);
            Resolved::Upval { upval: u, check: b.tdz, immutable: b.immutable }
        }
    }

    /// Upvalue in function `depth` for local `local` of function `origin`.
    pub fn capture_into(
        &mut self,
        depth: usize,
        origin: usize,
        local: LocalId,
        name: &str,
        kind: LocalKind,
        tdz: bool,
    ) -> UpvalId {
        if let Some(&u) = self.fns[depth].upvals.get(&(origin, local)) {
            return u;
        }
        let source = if depth == origin + 1 {
            let l = &mut self.fns[origin].f.locals[local.index()];
            l.captured = true;
            if tdz {
                l.needs_tdz = true;
            }
            CaptureSource::ParentLocal(local)
        } else {
            CaptureSource::ParentUpval(self.capture_into(depth - 1, origin, local, name, kind, tdz))
        };
        let u = self.fns[depth].f.new_upvalue(Upvalue { name: name.into(), kind, needs_tdz: tdz, source });
        self.fns[depth].upvals.insert((origin, local), u);
        u
    }

    // --- control-flow targets ------------------------------------------------

    pub fn push_target(
        &mut self,
        labels: Vec<String>,
        kind: TargetKind,
        break_bb: BlockId,
        continue_bb: Option<BlockId>,
        iter: Option<(LocalId, Option<BlockId>)>,
    ) -> u32 {
        let id = self.next_target;
        self.next_target += 1;
        self.fb().ctl.push(Ctl::Target { id, labels, kind, break_bb, continue_bb, iter });
        id
    }

    pub fn pop_ctl(&mut self) -> Ctl {
        self.fb().ctl.pop().expect("ctl")
    }

    pub fn find_break(&self, label: Option<&str>) -> Option<u32> {
        for c in self.fbr().ctl.iter().rev() {
            if let Ctl::Target { id, labels, kind, .. } = c {
                match label {
                    Some(l) if labels.iter().any(|x| x == l) => return Some(*id),
                    None if *kind != TargetKind::Block => return Some(*id),
                    _ => {}
                }
            }
        }
        None
    }

    pub fn find_continue(&self, label: Option<&str>) -> Result<u32, &'static str> {
        for c in self.fbr().ctl.iter().rev() {
            if let Ctl::Target { id, labels, kind, .. } = c {
                match label {
                    Some(l) if labels.iter().any(|x| x == l) => {
                        return if *kind == TargetKind::Loop { Ok(*id) } else { Err("not a loop") };
                    }
                    None if *kind == TargetKind::Loop => return Ok(*id),
                    _ => {}
                }
            }
        }
        Err("unknown")
    }

    pub fn emit_iter_close(&mut self, iter: LocalId, outer_handler: Option<BlockId>, span: Span) {
        let b = self.new_block_h(outer_handler);
        self.jump(b, span);
        self.switch_to(b);
        let it = self.load_local(iter, false, span);
        self.emit(InstKind::IterClose { iter: it }, span);
    }

    /// `return value`, routed through enclosing finally blocks and closing
    /// for-of iterators on the way out.
    pub fn emit_return(&mut self, value: Temp, span: Span) {
        let n = self.fbr().ctl.len();
        for i in (0..n).rev() {
            match &self.fbr().ctl[i] {
                Ctl::Target { iter: Some((it, h)), .. } => {
                    let (it, h) = (*it, *h);
                    self.emit_iter_close(it, h, span);
                }
                Ctl::Target { .. } => {}
                Ctl::Finally(fc) => {
                    let (entry, fkind, fval) = (fc.entry, fc.fkind, fc.fval);
                    if let Ctl::Finally(fc) = &mut self.fb().ctl[i] {
                        fc.used_return = true;
                    }
                    self.store_local(fval, value, span);
                    let k = self.konst(Constant::Number(1.0), span);
                    self.store_local(fkind, k, span);
                    self.jump(entry, span);
                    return;
                }
            }
        }
        self.terminate(TermKind::Return(value), span);
    }

    /// `break` / `continue` to target `id`.
    pub fn emit_jump(&mut self, id: u32, is_continue: bool, span: Span) {
        let n = self.fbr().ctl.len();
        for i in (0..n).rev() {
            match &self.fbr().ctl[i] {
                Ctl::Target { id: tid, break_bb, continue_bb, iter, .. } if *tid == id => {
                    let dest = if is_continue { continue_bb.expect("continue target") } else { *break_bb };
                    if !is_continue {
                        if let Some((it, h)) = *iter {
                            self.emit_iter_close(it, h, span);
                        }
                    }
                    self.jump(dest, span);
                    return;
                }
                Ctl::Target { iter: Some((it, h)), .. } => {
                    let (it, h) = (*it, *h);
                    self.emit_iter_close(it, h, span);
                }
                Ctl::Target { .. } => {}
                Ctl::Finally(fc) => {
                    let (entry, fkind) = (fc.entry, fc.fkind);
                    let code = match fc.exits.iter().find(|(_, t, c)| *t == id && *c == is_continue) {
                        Some((code, _, _)) => *code,
                        None => {
                            let code = 3 + fc.exits.len() as u32;
                            if let Ctl::Finally(fc) = &mut self.fb().ctl[i] {
                                fc.exits.push((code, id, is_continue));
                            }
                            code
                        }
                    };
                    let k = self.konst(Constant::Number(code as f64), span);
                    self.store_local(fkind, k, span);
                    self.jump(entry, span);
                    return;
                }
            }
        }
        // Target vanished (only on earlier semantic errors).
        self.terminate(TermKind::Unreachable, span);
    }

    /// Value join through a synthetic local: returns (local, end block).
    pub fn join_local(&mut self, span: Span) -> LocalId {
        self.synthetic("%join", span)
    }
}

/// Drop blocks unreachable from the entry (through normal and exceptional edges).
fn prune_unreachable(f: &mut Function) {
    let n = f.blocks.len();
    let mut seen = vec![false; n];
    let mut stack = vec![f.entry];
    seen[f.entry.index()] = true;
    while let Some(b) = stack.pop() {
        for s in f.successors(b) {
            if !seen[s.index()] {
                seen[s.index()] = true;
                stack.push(s);
            }
        }
    }
    if seen.iter().all(|&s| s) {
        return;
    }
    let mut remap = vec![None; n];
    let mut kept = Vec::new();
    for (i, blk) in std::mem::take(&mut f.blocks).into_iter().enumerate() {
        if seen[i] {
            remap[i] = Some(BlockId(kept.len() as u32));
            kept.push(blk);
        }
    }
    for blk in &mut kept {
        blk.term.kind.successors_mut(|s| *s = remap[s.index()].unwrap());
        blk.handler = blk.handler.map(|h| remap[h.index()].unwrap());
    }
    f.entry = remap[f.entry.index()].unwrap();
    f.blocks = kept;
}

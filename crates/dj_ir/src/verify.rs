//! IR well-formedness checks. Run after lowering and after every pass in
//! debug builds; codegen requires verified input.

use std::fmt;

use crate::ir::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    pub func: FuncId,
    pub block: Option<BlockId>,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.block {
            Some(b) => write!(f, "{} {}: {}", self.func, b, self.message),
            None => write!(f, "{}: {}", self.func, self.message),
        }
    }
}

pub fn verify_module(m: &Module) -> Result<(), Vec<VerifyError>> {
    let mut errs = Vec::new();
    if m.entry.index() >= m.functions.len() {
        errs.push(VerifyError { func: m.entry, block: None, message: "entry function out of range".into() });
        return Err(errs);
    }
    if m.func(m.entry).parent.is_some() || !m.func(m.entry).upvalues.is_empty() {
        errs.push(VerifyError { func: m.entry, block: None, message: "entry function must have no parent/upvalues".into() });
    }
    for (i, f) in m.functions.iter().enumerate() {
        if f.id.index() != i {
            errs.push(VerifyError { func: f.id, block: None, message: format!("function id {} stored at index {i}", f.id) });
            continue;
        }
        verify_function(m, f, &mut errs);
    }
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

struct Ctx<'a> {
    f: &'a Function,
    errs: &'a mut Vec<VerifyError>,
    block: Option<BlockId>,
}

impl Ctx<'_> {
    fn err(&mut self, msg: impl Into<String>) {
        self.errs.push(VerifyError { func: self.f.id, block: self.block, message: msg.into() });
    }
}

pub fn verify_function(m: &Module, f: &Function, errs: &mut Vec<VerifyError>) {
    let mut cx = Ctx { f, errs, block: None };

    // --- structure -----------------------------------------------------------
    if f.blocks.is_empty() || f.entry.index() >= f.blocks.len() {
        cx.err("missing entry block");
        return;
    }
    if f.scopes.is_empty() || f.scopes[0].kind != ScopeKind::Function || f.scopes[0].parent.is_some() {
        cx.err("scope 0 must be the parentless function scope");
    }
    for (i, s) in f.scopes.iter().enumerate().skip(1) {
        match s.parent {
            Some(p) if p.index() < i => {}
            _ => cx.err(format!("scope s{i} must have a parent with a smaller index")),
        }
    }
    let nparams = f.param_count as usize + f.has_rest as usize;
    if f.locals.len() < nparams {
        cx.err("fewer locals than parameters");
    }
    for (i, l) in f.locals.iter().enumerate() {
        if l.scope.index() >= f.scopes.len() {
            cx.err(format!("local l{i} has invalid scope"));
        }
        if i < nparams && l.kind != LocalKind::Param && l.kind != LocalKind::Synthetic {
            cx.err(format!("parameter local l{i} has kind {}", l.kind.as_str()));
        }
    }
    match f.parent {
        Some(p) if p.index() < m.functions.len() => {
            let parent = m.func(p);
            for (i, u) in f.upvalues.iter().enumerate() {
                match u.source {
                    CaptureSource::ParentLocal(l) => match parent.locals.get(l.index()) {
                        Some(pl) if pl.captured => {}
                        Some(_) => cx.err(format!("upval u{i} captures non-captured parent local {l}")),
                        None => cx.err(format!("upval u{i} references missing parent local {l}")),
                    },
                    CaptureSource::ParentUpval(v) => {
                        if v.index() >= parent.upvalues.len() {
                            cx.err(format!("upval u{i} references missing parent upval {v}"));
                        }
                    }
                }
            }
        }
        Some(p) => cx.err(format!("parent {p} out of range")),
        None => {
            if !f.upvalues.is_empty() {
                cx.err("parentless function with upvalues");
            }
        }
    }

    // --- instructions ----------------------------------------------------------
    let ntemps = f.temp_count as usize;
    // def site of each temp: (block, inst index)
    let mut def_site: Vec<Option<(usize, usize)>> = vec![None; ntemps];
    let handler_targets: Vec<bool> = {
        let mut v = vec![false; f.blocks.len()];
        for b in &f.blocks {
            if let Some(h) = b.handler {
                if h.index() < v.len() {
                    v[h.index()] = true;
                }
            }
        }
        v
    };

    for (bi, b) in f.blocks.iter().enumerate() {
        cx.block = Some(BlockId(bi as u32));
        if let Some(h) = b.handler {
            if h.index() >= f.blocks.len() {
                cx.err(format!("handler {h} out of range"));
            }
        }
        if handler_targets[bi] && !matches!(b.insts.first().map(|i| &i.kind), Some(InstKind::CatchValue { .. })) {
            cx.err("handler block must start with CatchValue");
        }
        for (ii, inst) in b.insts.iter().enumerate() {
            if inst.scope.index() >= f.scopes.len() {
                cx.err(format!("inst {ii} has invalid scope"));
            }
            if let InstKind::CatchValue { .. } = inst.kind {
                if ii != 0 || !handler_targets[bi] {
                    cx.err("CatchValue outside handler block entry");
                }
            }
            let mut bad = Vec::new();
            for t in inst.kind.uses() {
                if t.index() >= ntemps {
                    bad.push(format!("use of out-of-range {t}"));
                }
            }
            for t in inst.kind.defs() {
                if t.index() >= ntemps {
                    bad.push(format!("def of out-of-range {t}"));
                } else if let Some(prev) = def_site[t.index()] {
                    bad.push(format!("{t} defined twice (first in bb{} #{})", prev.0, prev.1));
                } else {
                    def_site[t.index()] = Some((bi, ii));
                }
            }
            for msg in bad {
                cx.err(format!("inst {ii}: {msg}"));
            }
            check_inst_refs(&mut cx, m, ii, &inst.kind);
        }
        if b.term.scope.index() >= f.scopes.len() {
            cx.err("terminator has invalid scope");
        }
        for s in b.term.kind.successors() {
            if s.index() >= f.blocks.len() {
                cx.err(format!("successor {s} out of range"));
            }
        }
    }
    cx.block = None;
    if !cx.errs.is_empty() {
        return;
    }

    // --- dominance of defs over uses ---------------------------------------
    let dom = SplitDominators::compute(f);
    for (bi, b) in f.blocks.iter().enumerate() {
        if !dom.reachable(bi) {
            continue;
        }
        cx.block = Some(BlockId(bi as u32));
        let check = |cx: &mut Ctx, t: Temp, pos: usize| {
            let Some((db, di)) = def_site[t.index()] else {
                cx.err(format!("{t} used but never defined"));
                return;
            };
            let ok = if db == bi { di < pos } else { dom.def_dominates_block(db, bi) };
            if !ok {
                cx.err(format!("use of {t} at #{pos} not dominated by its definition in bb{db}"));
            }
        };
        for (ii, inst) in b.insts.iter().enumerate() {
            for t in inst.kind.uses() {
                check(&mut cx, t, ii);
            }
        }
        for t in b.term.kind.uses() {
            check(&mut cx, t, b.insts.len());
        }
    }
}

fn check_inst_refs(cx: &mut Ctx, m: &Module, ii: usize, k: &InstKind) {
    let f = cx.f;
    let local_ok = |l: LocalId| l.index() < f.locals.len();
    match k {
        InstKind::LoadLocal { local, .. }
        | InstKind::StoreLocal { local, .. }
        | InstKind::DeclareLocal { local }
        | InstKind::CopyBinding { local } => {
            if !local_ok(*local) {
                cx.err(format!("inst {ii}: local {local} out of range"));
            }
        }
        InstKind::LoadUpval { upval, .. } | InstKind::StoreUpval { upval, .. } => {
            if upval.index() >= f.upvalues.len() {
                cx.err(format!("inst {ii}: upval {upval} out of range"));
            }
        }
        InstKind::MakeClosure { func, .. } => match m.functions.get(func.index()) {
            Some(child) if child.parent == Some(f.id) => {}
            Some(_) => cx.err(format!("inst {ii}: closure {func} is not a child of {}", f.id)),
            None => cx.err(format!("inst {ii}: closure {func} out of range")),
        },
        InstKind::Await { .. } => {
            if !f.is_async {
                cx.err(format!("inst {ii}: await in non-async function"));
            }
        }
        InstKind::SuperCall { .. } | InstKind::SuperCallSpread { .. } => {
            if f.kind != FunctionKind::DerivedConstructor {
                cx.err(format!("inst {ii}: super call outside derived constructor"));
            }
        }
        _ => {}
    }
}

/// Dominators over a graph where every block `b` is split into `entry(b)`
/// and `body(b)`: exceptional edges leave from `entry(b)` (an exception may be
/// raised before any instruction of `b` completed), normal edges from
/// `body(b)`. A temp defined in block `d` dominates block `u` iff `body(d)`
/// dominates `entry(u)`.
pub struct SplitDominators {
    idom: Vec<Option<usize>>, // indexed by node
    rpo_index: Vec<usize>,
}

impl SplitDominators {
    pub fn compute(f: &Function) -> Self {
        let n = f.blocks.len() * 2;
        let entry = |b: usize| b * 2;
        let body = |b: usize| b * 2 + 1;
        let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (bi, b) in f.blocks.iter().enumerate() {
            succs[entry(bi)].push(body(bi));
            if let Some(h) = b.handler {
                succs[entry(bi)].push(entry(h.index()));
            }
            for s in b.term.kind.successors() {
                succs[body(bi)].push(entry(s.index()));
            }
        }
        // reverse postorder from entry(f.entry)
        let start = entry(f.entry.index());
        let mut visited = vec![false; n];
        let mut post = Vec::new();
        let mut stack = vec![(start, 0usize)];
        visited[start] = true;
        while let Some(&mut (node, ref mut i)) = stack.last_mut() {
            if *i < succs[node].len() {
                let s = succs[node][*i];
                *i += 1;
                if !visited[s] {
                    visited[s] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(node);
                stack.pop();
            }
        }
        let rpo: Vec<usize> = post.iter().rev().copied().collect();
        let mut rpo_index = vec![usize::MAX; n];
        for (i, &node) in rpo.iter().enumerate() {
            rpo_index[node] = i;
        }
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (a, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s].push(a);
            }
        }
        // Cooper, Harvey, Kennedy: "A Simple, Fast Dominance Algorithm".
        let mut idom: Vec<Option<usize>> = vec![None; n];
        idom[start] = Some(start);
        let mut changed = true;
        while changed {
            changed = false;
            for &node in rpo.iter().skip(1) {
                let mut new_idom: Option<usize> = None;
                for &p in &preds[node] {
                    if idom[p].is_none() {
                        continue;
                    }
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect(&idom, &rpo_index, p, cur),
                    });
                }
                if new_idom != idom[node] {
                    idom[node] = new_idom;
                    changed = true;
                }
            }
        }
        SplitDominators { idom, rpo_index }
    }

    pub fn reachable(&self, block: usize) -> bool {
        self.rpo_index[block * 2] != usize::MAX
    }

    fn dominates(&self, a: usize, mut b: usize) -> bool {
        if self.rpo_index[b] == usize::MAX {
            return true; // unreachable: vacuous
        }
        loop {
            if a == b {
                return true;
            }
            match self.idom[b] {
                Some(p) if p != b => b = p,
                _ => return false,
            }
        }
    }

    pub fn def_dominates_block(&self, def_block: usize, use_block: usize) -> bool {
        self.dominates(def_block * 2 + 1, use_block * 2)
    }

    pub fn block_dominates(&self, a: usize, b: usize) -> bool {
        self.dominates(a * 2, b * 2)
    }
}

fn intersect(idom: &[Option<usize>], rpo_index: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while rpo_index[a] > rpo_index[b] {
            a = idom[a].unwrap();
        }
        while rpo_index[b] > rpo_index[a] {
            b = idom[b].unwrap();
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    fn inst(kind: InstKind) -> Inst {
        Inst { kind, span: Span::DUMMY, scope: ScopeId(0) }
    }

    fn module_with(f: Function) -> Module {
        Module { name: "t".into(), source: "".into(), functions: vec![f], entry: FuncId(0) }
    }

    #[test]
    fn rejects_use_in_handler_of_temp_defined_in_protected_block() {
        let mut f = Function::new(FuncId(0), "main".into(), FunctionKind::Script, Span::DUMMY, None);
        let t0 = f.new_temp();
        let t1 = f.new_temp();
        let h = f.new_block(Span::DUMMY, ScopeId(0));
        f.blocks[0].handler = Some(h);
        f.blocks[0].insts.push(inst(InstKind::Const { dst: t0, value: Constant::Number(1.0) }));
        f.blocks[0].term.kind = TermKind::Return(t0);
        f.blocks[1].insts.push(inst(InstKind::CatchValue { dst: t1 }));
        f.blocks[1].term.kind = TermKind::Return(t0);
        let errs = verify_module(&module_with(f)).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("not dominated")), "{errs:?}");
    }

    #[test]
    fn rejects_double_definition() {
        let mut f = Function::new(FuncId(0), "main".into(), FunctionKind::Script, Span::DUMMY, None);
        let t0 = f.new_temp();
        f.blocks[0].insts.push(inst(InstKind::Const { dst: t0, value: Constant::Null }));
        f.blocks[0].insts.push(inst(InstKind::Const { dst: t0, value: Constant::Null }));
        f.blocks[0].term.kind = TermKind::Return(t0);
        let errs = verify_module(&module_with(f)).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("defined twice")), "{errs:?}");
    }

    #[test]
    fn accepts_simple_function() {
        let mut f = Function::new(FuncId(0), "main".into(), FunctionKind::Script, Span::DUMMY, None);
        let t0 = f.new_temp();
        f.blocks[0].insts.push(inst(InstKind::Const { dst: t0, value: Constant::Null }));
        f.blocks[0].term.kind = TermKind::Return(t0);
        verify_module(&module_with(f)).unwrap();
    }
}

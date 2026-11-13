//! CFG simplification: drop unreachable blocks, thread jumps through empty
//! blocks, merge straight-line block pairs. Block order is otherwise kept
//! (codegen lays blocks out in index order).

use super::Pass;
use crate::ir::*;

pub struct SimplifyCfg;

impl Pass for SimplifyCfg {
    fn name(&self) -> &'static str {
        "simplify-cfg"
    }

    fn run(&mut self, f: &mut Function) -> bool {
        let mut changed = false;
        changed |= thread_jumps(f);
        changed |= merge_blocks(f);
        changed |= remove_unreachable(f);
        changed
    }
}

fn is_handler(f: &Function, b: BlockId) -> bool {
    f.blocks.iter().any(|x| x.handler == Some(b))
}

/// Redirect edges that target an instruction-free block ending in `jump T`.
fn thread_jumps(f: &mut Function) -> bool {
    let n = f.blocks.len();
    let mut target: Vec<Option<BlockId>> = vec![None; n];
    for (i, b) in f.blocks.iter().enumerate() {
        if b.insts.is_empty() && BlockId(i as u32) != f.entry {
            if let TermKind::Jump(t) = b.term.kind {
                if t.index() != i {
                    target[i] = Some(t);
                }
            }
        }
    }
    let resolve = |mut b: BlockId| {
        let mut steps = 0;
        while let Some(t) = target[b.index()] {
            b = t;
            steps += 1;
            if steps > n {
                break; // cycle of empty blocks: leave as is
            }
        }
        b
    };
    let mut changed = false;
    for bi in 0..n {
        let mut kind = f.blocks[bi].term.kind.clone();
        kind.successors_mut(|s| {
            let r = resolve(*s);
            if r != *s {
                *s = r;
                changed = true;
            }
        });
        if let TermKind::Branch { then_bb, else_bb, .. } = kind {
            if then_bb == else_bb {
                // Condition was computed already; dropping the branch keeps
                // its (pure) evaluation for DCE to clean up.
                kind = TermKind::Jump(then_bb);
            }
        }
        f.blocks[bi].term.kind = kind;
    }
    changed
}

/// `A: ...; jump B` where B has A as its only predecessor → append B to A.
fn merge_blocks(f: &mut Function) -> bool {
    let mut changed = false;
    loop {
        let preds = f.predecessors();
        let mut merged = false;
        for a in 0..f.blocks.len() {
            let TermKind::Jump(b) = f.blocks[a].term.kind else { continue };
            let bi = b.index();
            if bi == a
                || b == f.entry
                || preds[bi].len() != 1
                || f.blocks[a].handler != f.blocks[bi].handler
                || is_handler(f, b)
            {
                continue;
            }
            let tail = std::mem::replace(&mut f.blocks[bi], Block::new(Span::DUMMY, ScopeId(0)));
            let blk = &mut f.blocks[a];
            blk.insts.extend(tail.insts);
            blk.term = tail.term;
            // B is now an orphan ending in `unreachable`; removed below.
            merged = true;
            changed = true;
            break;
        }
        if !merged {
            break;
        }
    }
    changed
}

fn remove_unreachable(f: &mut Function) -> bool {
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
        return false;
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
    true
}

use crate::Span;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passes::{ConstFold, DeadCodeElim};

    fn inst(kind: InstKind) -> Inst {
        Inst { kind, span: Span::DUMMY, scope: ScopeId(0) }
    }

    #[test]
    fn folds_constant_branch_and_drops_dead_arm_keeping_stmt_markers() {
        // bb0: stmt; t0 = const true; branch t0, bb1, bb2
        // bb1: stmt; t1 = const 1; return t1
        // bb2: stmt; t2 = const 2; return t2
        let mut f = Function::new(FuncId(0), "main".into(), FunctionKind::Script, Span::DUMMY, None);
        let (t0, t1, t2) = (f.new_temp(), f.new_temp(), f.new_temp());
        let b1 = f.new_block(Span::DUMMY, ScopeId(0));
        let b2 = f.new_block(Span::DUMMY, ScopeId(0));
        f.blocks[0].insts = vec![inst(InstKind::Stmt), inst(InstKind::Const { dst: t0, value: Constant::Bool(true) })];
        f.blocks[0].term.kind = TermKind::Branch { cond: t0, then_bb: b1, else_bb: b2 };
        f.blocks[1].insts = vec![inst(InstKind::Stmt), inst(InstKind::Const { dst: t1, value: Constant::Number(1.0) })];
        f.blocks[1].term.kind = TermKind::Return(t1);
        f.blocks[2].insts = vec![inst(InstKind::Stmt), inst(InstKind::Const { dst: t2, value: Constant::Number(2.0) })];
        f.blocks[2].term.kind = TermKind::Return(t2);

        assert!(ConstFold.run(&mut f));
        assert!(SimplifyCfg.run(&mut f));
        assert!(DeadCodeElim.run(&mut f));
        assert_eq!(f.blocks.len(), 1);
        let stmts = f.blocks[0].insts.iter().filter(|i| i.kind == InstKind::Stmt).count();
        assert_eq!(stmts, 2, "both reachable statement markers survive");
        assert_eq!(f.blocks[0].term.kind, TermKind::Return(t1));
    }
}

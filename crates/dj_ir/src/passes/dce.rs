//! Dead code elimination: removes side-effect-free instructions whose results
//! are never used. `Stmt` markers and stores are never removed.

use super::Pass;
use crate::ir::*;

pub struct DeadCodeElim;

impl Pass for DeadCodeElim {
    fn name(&self) -> &'static str {
        "dce"
    }

    fn run(&mut self, f: &mut Function) -> bool {
        let mut changed = false;
        loop {
            let mut uses = vec![0u32; f.temp_count as usize];
            for b in &f.blocks {
                for i in &b.insts {
                    i.kind.visit(|_| {}, |t| uses[t.index()] += 1);
                }
                for t in b.term.kind.uses() {
                    uses[t.index()] += 1;
                }
            }
            let mut removed = false;
            for b in &mut f.blocks {
                b.insts.retain(|i| {
                    if !i.kind.is_removable_if_unused() {
                        return true;
                    }
                    let live = i.kind.defs().iter().any(|t| uses[t.index()] > 0);
                    if !live {
                        removed = true;
                    }
                    live
                });
            }
            if !removed {
                break;
            }
            changed = true;
        }
        changed
    }
}

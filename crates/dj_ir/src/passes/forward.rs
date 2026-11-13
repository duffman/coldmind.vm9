//! Store-to-load forwarding for non-captured locals within a block.
//!
//! `store l, t1; ...; t2 = load l` (no intervening write to `l`) rewrites all
//! uses of `t2` to `t1` and deletes the load. Captured locals live in cells
//! that nested closures may write, so they are left alone. Stores are kept,
//! so the debugger still observes every assignment.

use super::{Pass, substitute};
use crate::ir::*;

pub struct StoreLoadForward;

impl Pass for StoreLoadForward {
    fn name(&self) -> &'static str {
        "store-load-forward"
    }

    fn run(&mut self, f: &mut Function) -> bool {
        let mut subst: Vec<Option<Temp>> = vec![None; f.temp_count as usize];
        let mut changed = false;
        let captured: Vec<bool> = f.locals.iter().map(|l| l.captured).collect();
        for b in &mut f.blocks {
            let mut known: Vec<(LocalId, Temp)> = Vec::new();
            let mut keep = Vec::with_capacity(b.insts.len());
            for inst in b.insts.drain(..) {
                match &inst.kind {
                    InstKind::StoreLocal { local, src } if !captured[local.index()] => {
                        known.retain(|(l, _)| l != local);
                        known.push((*local, *src));
                    }
                    InstKind::DeclareLocal { local } | InstKind::CopyBinding { local } => {
                        known.retain(|(l, _)| l != local);
                    }
                    InstKind::LoadLocal { dst, local, .. } => {
                        if let Some(&(_, src)) = known.iter().find(|(l, _)| l == local) {
                            subst[dst.index()] = Some(src);
                            changed = true;
                            continue;
                        }
                    }
                    _ => {}
                }
                keep.push(inst);
            }
            b.insts = keep;
        }
        if changed {
            substitute(f, &subst);
        }
        changed
    }
}

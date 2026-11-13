//! IR transformation passes.
//!
//! Debug-info contract every pass upholds (tested end to end by
//! `dj_cli/tests/debugger.rs::stops_are_stable_across_opt_levels_and_gc`):
//! * instructions keep their `span` and `scope`;
//! * `Stmt` markers are never removed, reordered across each other, or moved
//!   to another scope, except when their whole block is unreachable;
//! * stores to user-visible locals are never removed, so variable inspection
//!   at any statement boundary observes the same values as unoptimized code.

mod const_fold;
mod dce;
mod forward;
mod simplify_cfg;

pub use const_fold::ConstFold;
pub use dce::DeadCodeElim;
pub use forward::StoreLoadForward;
pub use simplify_cfg::SimplifyCfg;

use crate::ir::{Function, Module};

pub trait Pass {
    fn name(&self) -> &'static str;
    /// Returns `true` when the function changed.
    fn run(&mut self, f: &mut Function) -> bool;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptLevel {
    /// No transformation.
    #[default]
    None,
    /// The standard pipeline, iterated to a fixed point.
    Default,
}

pub fn default_pipeline() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(StoreLoadForward),
        Box::new(ConstFold),
        Box::new(DeadCodeElim),
        Box::new(SimplifyCfg),
    ]
}

/// Runs the pipeline over every function. In debug builds the module is
/// re-verified after every pass and a verifier failure panics with the pass
/// name.
pub fn optimize(m: &mut Module, level: OptLevel) {
    if level == OptLevel::None {
        return;
    }
    let mut passes = default_pipeline();
    for fi in 0..m.functions.len() {
        for _ in 0..8 {
            let mut changed = false;
            for p in passes.iter_mut() {
                let did = p.run(&mut m.functions[fi]);
                #[cfg(debug_assertions)]
                if did {
                    let mut errs = Vec::new();
                    crate::verify::verify_function(m, &m.functions[fi], &mut errs);
                    assert!(errs.is_empty(), "pass {} broke IR: {:?}", p.name(), errs);
                }
                changed |= did;
            }
            if !changed {
                break;
            }
        }
    }
}

/// Replace every use of temps according to `subst` (applied transitively).
pub(crate) fn substitute(f: &mut Function, subst: &[Option<crate::ir::Temp>]) {
    let resolve = |mut t: crate::ir::Temp| {
        while let Some(n) = subst[t.index()] {
            t = n;
        }
        t
    };
    for b in &mut f.blocks {
        for i in &mut b.insts {
            i.kind.for_each_use_mut(|t| *t = resolve(*t));
        }
        b.term.kind.for_each_use_mut(|t| *t = resolve(*t));
    }
}

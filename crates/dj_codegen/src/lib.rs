//! `dj_codegen` — compiles verified IR into bytecode.
//!
//! Per function:
//! 1. **Layout**: reachable blocks (normal + exceptional edges) in index
//!    order with the entry block first. A `Jump` to the next laid-out block
//!    is elided.
//! 2. **Frame layout** (`regalloc`): parameters occupy registers
//!    `0..params`; every user-visible or captured local gets a dedicated
//!    register for the whole activation (so the debugger can always find
//!    it); temps and non-captured synthetic locals share registers through
//!    liveness-based linear scan. A final window of scratch registers holds
//!    outgoing call arguments.
//! 3. **Selection** (`emit`): one IR instruction → zero or more bytecode
//!    instructions. Two peepholes remove moves: a temp defined right before
//!    `store l, t` is computed directly into `l`'s register, and a temp
//!    loaded from a dedicated local reads the local's register when no
//!    write to the local intervenes before the temp's last use.
//! 4. **Tables**: exception handler ranges per protected block, line table
//!    with statement flags from `Stmt` markers, pc → scope map, variable
//!    locations + live ranges, pc → IR position map.

mod emit;
mod regalloc;

use dj_bytecode as bc;
use dj_ir as ir;

#[derive(Clone, Debug)]
pub struct Options {
    /// Emit the debug section.
    pub debug_info: bool,
    /// Embed the source text in the debug section.
    pub embed_source: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { debug_info: true, embed_source: true }
    }
}

/// Compile a verified IR module. Panics on IR that fails verification
/// invariants (codegen is not an IR validator; run `dj_ir::verify` first).
pub fn generate(m: &ir::Module, opts: &Options) -> bc::Module {
    let lines = ir::LineIndex::new(m.source.clone());
    let layouts: Vec<regalloc::FrameLayout> = m.functions.iter().map(regalloc::FrameLayout::compute).collect();
    let mut pool = emit::ConstPool::default();
    let mut functions = Vec::with_capacity(m.functions.len());
    let mut debug = Vec::with_capacity(m.functions.len());
    for f in &m.functions {
        let (bf, fd) = emit::emit_function(m, f, &layouts, &mut pool, &lines);
        functions.push(bf);
        debug.push(fd);
    }
    bc::Module {
        constants: pool.into_constants(),
        functions,
        entry: m.entry.0,
        debug: opts.debug_info.then(|| bc::DebugInfo {
            source_name: m.name.clone(),
            source: opts.embed_source.then(|| m.source.to_string()),
            functions: debug,
        }),
    }
}

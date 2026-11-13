//! Source ↔ IR ↔ bytecode maps, scopes and variable live ranges.

use dj_bytecode as bc;
use dj_bytecode::debug::{Pos, VarKind, VarLocation};
use dj_cli::{CompileOptions, OptLevel, compile, compile_ir};

const SRC: &str = "\
function add(a: number, b: number) {
  const sum = a + b;
  return sum;
}
let total = 0;
for (let i = 0; i < 3; i++) {
  total = add(total, i);
}
function mk() {
  let hidden = 1;
  return () => hidden++;
}
console.log(total, mk()());
";

fn module(opt: OptLevel) -> bc::Module {
    compile(SRC, "sm.ts", &CompileOptions { opt, debug_info: true, embed_source: true }).unwrap()
}

fn func_named(m: &bc::Module, name: &str) -> u32 {
    (0..m.functions.len() as u32).find(|&i| m.string(m.functions[i as usize].name) == Some(name)).unwrap()
}

#[test]
fn debug_info_describes_source() {
    let m = module(OptLevel::Default);
    let d = m.debug.as_ref().unwrap();
    assert_eq!(d.source_name, "sm.ts");
    assert_eq!(d.source.as_deref(), Some(SRC));
    assert_eq!(d.functions.len(), m.functions.len());
    let add = &d.functions[func_named(&m, "add") as usize];
    assert_eq!(add.start, Pos { line: 1, col: 1 });
    assert_eq!(add.end.line, 4);
    let stmt_lines: Vec<u32> = add.stmt_starts().map(|e| e.pos.line).collect();
    assert_eq!(stmt_lines, vec![2, 3], "{:?}", add.lines);
    // Statement entries carry the statement's own start, not its first
    // instruction's expression (declarations: one stop per declarator).
    let mk = &d.functions[func_named(&m, "mk") as usize];
    let mk_stmts: Vec<Pos> = mk.stmt_starts().map(|e| e.pos).collect();
    assert_eq!(mk_stmts, vec![Pos { line: 10, col: 7 }, Pos { line: 11, col: 3 }], "{:?}", mk.lines);
    // Line table is sorted by pc and covers every function's first instruction.
    for f in &d.functions {
        assert!(f.lines.windows(2).all(|w| w[0].pc < w[1].pc));
        assert!(f.pos_at(0).is_some());
    }
}

#[test]
fn breakpoints_resolve_to_statements() {
    for opt in [OptLevel::None, OptLevel::Default] {
        let m = module(opt);
        let d = m.debug.as_ref().unwrap();
        let add = func_named(&m, "add");
        // Line 1 (function header) resolves forward to the first body statement.
        let b = d.resolve_breakpoint(1, None).unwrap();
        assert_eq!((b.func, b.pos.line), (add, 2), "{opt:?}");
        assert!(d.functions[add as usize].is_stmt_start(b.pc));
        // Blank / closing-brace lines move to the next statement.
        let b = d.resolve_breakpoint(4, None).unwrap();
        assert_eq!(b.pos.line, 5);
        // Inner-most function wins on a shared line.
        let b = d.resolve_breakpoint(11, Some(10)).unwrap();
        assert_eq!(m.string(m.functions[b.func as usize].name), Some(""), "arrow body on line 11");
        // Past the end: nothing.
        assert!(d.resolve_breakpoint(100, None).is_none());
        // Line 6 holds the loop init, test and update.
        let locs = d.breakpoint_locations(6);
        assert!(locs.len() >= 3, "{locs:?}");
        assert!(locs.windows(2).all(|w| w[0].pos <= w[1].pos));
    }
}

#[test]
fn variables_have_scopes_kinds_and_ranges() {
    let m = module(OptLevel::Default);
    let d = m.debug.as_ref().unwrap();
    let add = &d.functions[func_named(&m, "add") as usize];
    let names = |pc: u32| {
        let mut v: Vec<&str> = add.visible_variables(pc).map(|v| v.name.as_str()).collect();
        v.sort();
        v
    };
    let line3 = add.stmt_starts().find(|e| e.pos.line == 3).unwrap().pc;
    assert_eq!(names(line3), vec!["a", "b", "sum", "this"]);
    let sum = add.variables.iter().find(|v| v.name == "sum").unwrap();
    assert_eq!(sum.kind, VarKind::Const);
    assert_eq!(sum.decl, Pos { line: 2, col: 9 });
    assert!(add.variables.iter().filter(|v| v.name == "a" || v.name == "b").all(|v| v.kind == VarKind::Param));

    // Captured binding lives in a cell; the closure sees it as an upvalue.
    let mk = &d.functions[func_named(&m, "mk") as usize];
    let hidden = mk.variables.iter().find(|v| v.name == "hidden").unwrap();
    assert!(matches!(hidden.location, VarLocation::Cell(_)), "{hidden:?}");
    let arrow = d.functions.iter().find(|f| f.start.line == 11).unwrap();
    let up = arrow.variables.iter().find(|v| v.name == "hidden").unwrap();
    assert!(matches!(up.location, VarLocation::Upvalue(_)) && up.scope.is_none());

    // Loop variable `i` is only in scope inside the loop.
    let main = &d.functions[m.entry as usize];
    let i = main.variables.iter().find(|v| v.name == "i").unwrap();
    let line13 = main.stmt_starts().find(|e| e.pos.line == 13).unwrap().pc;
    let line7 = main.stmt_starts().find(|e| e.pos.line == 7).unwrap().pc;
    assert!(i.in_scope_at(line7) && !i.in_scope_at(line13));
    // Scope chain at line 7: loop body block → loop scope → ... → function.
    let chain = main.scope_chain(line7);
    assert!(chain.len() >= 2, "{chain:?}");
}

#[test]
fn ir_map_links_bytecode_to_ir() {
    for opt in [OptLevel::None, OptLevel::Default] {
        let ir = compile_ir(SRC, "sm.ts", opt).unwrap();
        let m = module(opt);
        let d = m.debug.as_ref().unwrap();
        assert_eq!(ir.functions.len(), m.functions.len());
        for (fi, f) in d.functions.iter().enumerate() {
            assert!(!f.ir_map.is_empty(), "function {fi} has no IR map");
            let blocks = &ir.functions[fi].blocks;
            for p in &f.ir_map {
                let b = blocks.get(p.block as usize).unwrap_or_else(|| panic!("{opt:?} f{fi}: block {} out of range", p.block));
                assert!(p.inst == u32::MAX || (p.inst as usize) < b.insts.len(), "{opt:?} f{fi}: {p:?}");
                assert_eq!(f.ir_pos_at(p.pc), Some(*p));
            }
            // A statement's IR instruction carries the statement's source span.
            for e in f.stmt_starts() {
                let Some(p) = f.ir_pos_at(e.pc) else { continue };
                let span = if p.inst == u32::MAX {
                    blocks[p.block as usize].term.span
                } else {
                    blocks[p.block as usize].insts[p.inst as usize].span
                };
                let lc = dj_ir::LineIndex::new(SRC.into()).line_col(span.start);
                assert_eq!(lc.line, e.pos.line, "{opt:?} f{fi} pc {}", e.pc);
            }
        }
    }
}

#[test]
fn stripping_removes_metadata() {
    let m = compile(SRC, "sm.ts", &CompileOptions { opt: OptLevel::Default, debug_info: false, embed_source: false }).unwrap();
    assert!(m.debug.is_none());
    let with_src = module(OptLevel::Default);
    let no_src = compile(SRC, "sm.ts", &CompileOptions { opt: OptLevel::Default, debug_info: true, embed_source: false }).unwrap();
    assert!(no_src.debug.as_ref().unwrap().source.is_none());
    assert!(no_src.to_bytes().len() < with_src.to_bytes().len());
}

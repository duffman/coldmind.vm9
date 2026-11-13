//! Frontend lowering tests: representative programs must lower to verified
//! IR; unsupported or invalid programs must be rejected with located
//! diagnostics.

use dj_frontend::{compile_to_ir, render_diagnostics};
use dj_ir::{FunctionKind, InstKind, LocalKind, Module, TermKind};

fn ok(src: &str) -> Module {
    match compile_to_ir(src, "test.ts") {
        Ok(m) => {
            dj_ir::verify::verify_module(&m).unwrap_or_else(|e| panic!("verify failed: {e:?}\n{}", dj_ir::print::print_module(&m)));
            m
        }
        Err(d) => panic!("unexpected diagnostics:\n{}", render_diagnostics(&d, src, "test.ts")),
    }
}

/// Expect a rejection with `code` whose first diagnostic sits at `line:col`.
fn rejected(src: &str, code: &str, line: u32, col: u32) -> String {
    let diags = compile_to_ir(src, "test.ts").expect_err("expected diagnostics");
    let d = diags.iter().find(|d| d.code == code).unwrap_or_else(|| panic!("no `{code}` diagnostic in {diags:?}"));
    let rendered = render_diagnostics(std::slice::from_ref(d), src, "test.ts");
    let head = format!("test.ts:{line}:{col}: error[{code}]:");
    assert!(rendered.starts_with(&head), "expected `{head}`, got:\n{rendered}");
    rendered
}

fn count_insts(m: &Module, pred: impl Fn(&InstKind) -> bool) -> usize {
    m.functions.iter().flat_map(|f| f.blocks.iter()).flat_map(|b| b.insts.iter()).filter(|i| pred(&i.kind)).count()
}

// ---------------------------------------------------------------------------
// Accepted programs
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_and_variables() {
    let m = ok("let a: number = 1 + 2 * 3; const b = a ** 2 % 5; var c = -a; console.log(a, b, c);");
    assert_eq!(m.functions.len(), 1);
    assert_eq!(m.func(m.entry).kind, FunctionKind::Script);
}

#[test]
fn functions_and_hoisting() {
    let m = ok("console.log(f(2)); function f(x: number): number { return g(x) + 1; } function g(y) { return y * 2; }");
    assert_eq!(m.functions.len(), 3);
}

#[test]
fn closures_capture_and_upvalue_chain() {
    let m = ok("function outer() { let x = 1; return function mid() { return () => x++; }; }");
    let arrow = m.functions.iter().find(|f| f.kind == FunctionKind::Arrow).unwrap();
    assert_eq!(arrow.upvalues.len(), 1);
    let mid = m.functions.iter().find(|f| &*f.name == "mid").unwrap();
    assert_eq!(mid.upvalues.len(), 1, "intermediate function relays the capture");
    let outer = m.functions.iter().find(|f| &*f.name == "outer").unwrap();
    assert!(outer.locals.iter().any(|l| &*l.name == "x" && l.captured && l.needs_tdz));
}

#[test]
fn closures_in_loops_get_per_iteration_bindings() {
    let m = ok("const fs: (() => number)[] = []; for (let i = 0; i < 3; i++) { fs.push(() => i); } for (const v of [1,2]) { fs.push(() => v); }");
    assert!(count_insts(&m, |k| matches!(k, InstKind::CopyBinding { .. })) >= 2);
    assert!(count_insts(&m, |k| matches!(k, InstKind::DeclareLocal { .. })) >= 2);
}

#[test]
fn tdz_checks_only_where_needed() {
    let m = ok("let a = 1; console.log(a); function f() { return b; } let b = 2;");
    let main = m.func(m.entry);
    let a = main.locals.iter().find(|l| &*l.name == "a").unwrap();
    assert!(!a.needs_tdz);
    let b = main.locals.iter().find(|l| &*l.name == "b").unwrap();
    assert!(b.needs_tdz && b.captured);
}

#[test]
fn tdz_self_reference_in_initializer() {
    let m = ok("let x = 1; { let x = x + 1; }");
    assert!(count_insts(&m, |k| matches!(k, InstKind::LoadLocal { check_tdz: true, .. })) == 1);
}

#[test]
fn const_assignment_throws_type_error() {
    let m = ok("const c = 1; c = 2;");
    let main = m.func(m.entry);
    assert!(main.blocks.iter().any(|b| matches!(&b.term.kind, TermKind::ThrowError { message, .. } if message.contains("constant"))));
}

#[test]
fn if_else_and_conditionals() {
    ok("let x = 3; if (x > 2) { x = 1 } else if (x < 0) x = 2; else { x = 3 } const y = x ? 'a' : 'b';");
}

#[test]
fn while_do_while_for() {
    ok("let i = 0; while (i < 10) { i++; if (i == 5) continue; if (i == 8) break; } do { i--; } while (i > 0); for (;;) { break; } for (var j = 0, k = 1; j < 3; j++, k--) {}");
}

#[test]
fn labels_and_labeled_jumps() {
    ok("outer: for (let i = 0; i < 3; i++) { inner: for (let j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; } } blk: { break blk; }");
}

#[test]
fn switch_with_fallthrough_and_default_in_middle() {
    ok("function f(x: number) { switch (x) { case 1: case 2: return 'a'; default: let y = 1; case 3: y = 2; return y; } }");
}

#[test]
fn switch_lexical_bindings_need_tdz() {
    let m = ok("function f(x) { switch (x) { case 0: let a = 1; return a; case 1: return a; } }");
    let f = m.functions.iter().find(|f| &*f.name == "f").unwrap();
    assert!(f.locals.iter().any(|l| &*l.name == "a" && l.needs_tdz));
}

#[test]
fn try_catch() {
    ok("try { throw new Error('x'); } catch (e) { console.log(e.message); } try { f(); } catch { }");
}

#[test]
fn try_finally_with_return_inside_loop() {
    let m = ok("function f() { for (let i = 0; i < 3; i++) { try { if (i == 1) return i; } finally { console.log('f', i); } } return -1; }");
    let f = m.functions.iter().find(|f| &*f.name == "f").unwrap();
    assert!(f.locals.iter().any(|l| &*l.name == "%fkind"));
}

#[test]
fn try_finally_with_break_and_continue() {
    ok("for (let i = 0; i < 5; i++) { try { if (i == 1) continue; if (i == 3) break; } finally { console.log(i); } }");
}

#[test]
fn nested_finally() {
    ok("function f() { try { try { return 1; } finally { console.log('inner'); } } finally { console.log('outer'); } }");
}

#[test]
fn try_catch_finally_rethrow() {
    ok("function f() { try { throw 1; } catch (e) { throw e + 1; } finally { console.log('done'); } }");
}

#[test]
fn catch_parameter_destructuring_gets_fresh_binding() {
    let m = ok("try { throw {a: 1}; } catch ({ a }) { console.log(a); }");
    assert!(m.func(m.entry).locals.iter().any(|l| &*l.name == "a" && l.kind == LocalKind::Catch));
}

#[test]
fn for_of_and_for_in() {
    let m = ok("for (const x of [1, 2, 3]) { if (x == 2) break; } const o = {a: 1, b: 2}; for (const k in o) console.log(k); let z; for (z of 'ab') {}");
    assert!(count_insts(&m, |k| matches!(k, InstKind::IterClose { .. })) >= 2);
    assert!(count_insts(&m, |k| matches!(k, InstKind::ForInIterator { .. })) == 1);
}

#[test]
fn for_of_return_closes_iterator() {
    ok("function f(xs: number[]) { for (const x of xs) { if (x > 1) return x; } return 0; }");
}

#[test]
fn object_and_array_literals() {
    ok("const k = 'dyn'; const o = { a: 1, 'b': 2, 3: 3, [k]: 4, k, ...{ c: 5 }, m() { return this.a; }, get g() { return 1; }, set g(v) {} }; const a = [1, , ...[2, 3], 4];");
}

#[test]
fn destructuring_declarations() {
    ok("const { a, b: { c = 3 } = {}, ...rest } = { a: 1, b: {}, d: 4 }; const [x, , y = 2, ...zs] = [1, 2, undefined, 4, 5]; let [p, [q]] = [1, [2]];");
}

#[test]
fn destructuring_assignment() {
    ok("let a = 1, b = 2; [a, b] = [b, a]; const o: any = {}; ({ x: o.x, y: o['y'] = 5 } = { x: 1 }); [o.z, ...o.r] = [1, 2, 3];");
}

#[test]
fn parameters_defaults_rest_destructuring() {
    let m = ok("function f(a, b = a + 1, { c, d } = { c: 1, d: 2 }, [e] = [9], ...rest) { return a + b + c + d + e + rest.length; }");
    let f = m.functions.iter().find(|f| &*f.name == "f").unwrap();
    assert_eq!(f.param_count, 4);
    assert!(f.has_rest);
}

#[test]
fn spread_calls_and_new() {
    ok("function f(...xs) { return xs; } f(...[1, 2], 3); new Array(...[1, 2]); Math.max(1, ...[2, 3]);");
}

#[test]
fn method_calls_keep_receiver() {
    let m = ok("const o = { v: 1, m() { return this.v; } }; o.m(); o['m'](); (o.m)();");
    assert_eq!(count_insts(&m, |k| matches!(k, InstKind::Call { this: Some(_), .. })), 3);
}

#[test]
fn optional_chaining_and_nullish() {
    ok("const o: any = null; const a = o?.b?.c; const b = o?.[0]; const c = o?.m?.(1); const d = o ?? 5; let e: any; e ??= 1; e ||= 2; e &&= 3;");
}

#[test]
fn template_literals_and_operators() {
    ok("const n = 5; const s = `a${n}b${n + 1}`; const t = `plain`; const u = typeof n; const v = void 0; const w = 'a' in {a: 1}; const x = [] instanceof Array; let y = 1; y += 2; y **= 2; y >>>= 1; const z = (y++, ++y, y--, --y);");
}

#[test]
fn delete_and_typeof_globals() {
    let m = ok("const o: any = { a: 1 }; delete o.a; delete o['b']; const t = typeof notDefinedAnywhere;");
    assert_eq!(count_insts(&m, |k| matches!(k, InstKind::TypeofGlobal { .. })), 1);
}

#[test]
fn classes_with_fields_methods_static_accessors() {
    let m = ok(
        "class Point { x: number; y = 0; static count = 0; static origin() { return new Point(0, 0); }
           constructor(x: number, y: number) { this.x = x; this.y = y; Point.count++; }
           get len() { return Math.sqrt(this.x ** 2 + this.y ** 2); } set len(v) {} toString() { return `${this.x},${this.y}`; } }
         const p = new Point(3, 4);",
    );
    assert!(m.functions.iter().any(|f| f.kind == FunctionKind::ClassConstructor));
    assert_eq!(m.functions.iter().filter(|f| f.kind == FunctionKind::FieldInit).count(), 2);
    assert_eq!(count_insts(&m, |k| matches!(k, InstKind::InitFields)), 1);
}

#[test]
fn derived_classes_and_super() {
    let m = ok(
        "class A { constructor(public n: number) {} hi() { return 'A' + this.n; } static s() { return 1; } }
         class B extends A { extra = 1; constructor(n: number, private k = 2) { super(n * 2); } hi() { return 'B' + super.hi(); } }
         class C extends B {}
         console.log(new C(1).hi());",
    );
    let derived = m.functions.iter().filter(|f| f.kind == FunctionKind::DerivedConstructor).count();
    assert_eq!(derived, 2);
    assert!(count_insts(&m, |k| matches!(k, InstKind::SuperCallSpread { .. })) == 1, "default derived ctor");
    assert!(count_insts(&m, |k| matches!(k, InstKind::SuperGet { .. })) == 1);
}

#[test]
fn class_expression_and_computed_members() {
    ok("const key = 'k'; const C = class { [key] = 1; static [key + '2'] = 2; [key + 'm']() { return 1; } }; new C();");
}

#[test]
fn arrows_capture_this_and_new_target() {
    let m = ok("function F(this: any) { this.v = 1; const g = () => this.v; const t = () => new.target; return g(); }");
    let arrows: Vec<_> = m.functions.iter().filter(|f| f.kind == FunctionKind::Arrow).collect();
    assert!(arrows.iter().any(|a| a.upvalues.iter().any(|u| u.kind == LocalKind::This)));
    assert!(arrows.iter().any(|a| a.upvalues.iter().any(|u| u.kind == LocalKind::NewTarget)));
}

#[test]
fn async_await_and_top_level_await() {
    let m = ok("async function f(x: Promise<number>) { const v = await x; return v + 1; } const g = async () => await f(Promise.resolve(1)); await g();");
    assert!(m.func(m.entry).is_async);
    assert!(m.functions.iter().filter(|f| f.is_async).count() >= 3);
}

#[test]
fn enums_with_reverse_mapping() {
    let m = ok("enum Color { Red, Green = 5, Blue, Mixed = Red | Green } enum S { A = 'a', B = 'b' } const enum K { X } console.log(Color.Blue, Color[6], S.A, K.X);");
    assert!(count_insts(&m, |k| matches!(k, InstKind::DefineField { .. })) >= 4);
}

#[test]
fn type_only_syntax_is_erased() {
    let m = ok(
        "interface I { a: number } type T = string | number; declare const ext: number; declare function df(): void;
         function over(a: string): string; function over(a: any) { return a; }
         abstract class Base<T> implements I { a = 1; abstract m(): void; } let x = <number>(1 as any)!; let y = (2 satisfies number);
         import type { Foo } from './foo'; export type { T };",
    );
    assert!(m.functions.iter().any(|f| &*f.name == "over"));
}

#[test]
fn named_function_expression_binds_itself() {
    let m = ok("const fact = function f(n: number): number { return n <= 1 ? 1 : n * f(n - 1); };");
    assert_eq!(count_insts(&m, |k| matches!(k, InstKind::LoadCallee { .. })), 1);
}

#[test]
fn name_inference() {
    let m = ok("const a = () => 1; const o = { b: function () {}, c: () => 2 }; let d; d = () => 3; const E = class {};");
    for n in ["a", "b", "c", "d", "E"] {
        assert!(m.functions.iter().any(|f| &*f.name == n), "missing function named {n}");
    }
}

#[test]
fn debugger_statement_and_stmt_markers() {
    let m = ok("let a = 1;\ndebugger;\na++;");
    assert_eq!(count_insts(&m, |k| matches!(k, InstKind::Debugger)), 1);
    assert_eq!(count_insts(&m, |k| matches!(k, InstKind::Stmt)), 3);
}

#[test]
fn every_instruction_has_a_real_span_and_scope() {
    let src = "function f(a) { { let b = a; return () => b; } }\nf(1);";
    let m = ok(src);
    for f in &m.functions {
        for b in &f.blocks {
            for i in &b.insts {
                assert!(i.span.end as usize <= src.len());
                assert!(i.scope.index() < f.scopes.len());
            }
        }
    }
}

#[test]
fn lowering_is_deterministic() {
    let src = "class A { x = 1; m() { return [1, 2].map(v => v + this.x); } } for (const k in {a: 1}) {} try {} finally {}";
    let a = dj_ir::print::print_module(&ok(src));
    let b = dj_ir::print::print_module(&ok(src));
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------------
// Rejected programs
// ---------------------------------------------------------------------------

#[test]
fn rejects_generators() {
    rejected("let a = 1;\nfunction* g() { yield 1; }", "unsupported", 2, 1);
}

#[test]
fn rejects_async_generators() {
    rejected("async function* g() {}", "unsupported", 1, 1);
}

#[test]
fn rejects_regex_literals() {
    let r = rejected("const r = /ab+c/;", "unsupported", 1, 11);
    assert!(r.contains("^^^^^^"), "{r}");
}

#[test]
fn rejects_bigint() {
    rejected("const b =\n  10n;", "unsupported", 2, 3);
}

#[test]
fn rejects_decorators() {
    rejected("function d(x: any) {}\n@d class A {}", "unsupported", 2, 1);
}

#[test]
fn rejects_namespaces() {
    rejected("namespace N { export const x = 1; }", "unsupported", 1, 1);
}

#[test]
fn rejects_imports() {
    rejected("import { x } from './x';", "unsupported", 1, 1);
}

#[test]
fn rejects_exports() {
    rejected("export const x = 1;", "unsupported", 1, 1);
}

#[test]
fn rejects_private_names() {
    rejected("class A {\n  #x = 1;\n}", "unsupported", 2, 3);
}

#[test]
fn rejects_tagged_templates() {
    rejected("const t = (s: any) => s;\nt`x`;", "unsupported", 2, 1);
}

#[test]
fn rejects_arguments_object() {
    rejected("function f() { return arguments.length; }", "unsupported", 1, 23);
}

#[test]
fn rejects_super_in_arrow() {
    rejected("class A { m() { return 1; } }\nclass B extends A { m() { return (() => super.m())(); } }", "unsupported", 2, 41);
}

#[test]
fn rejects_static_blocks() {
    rejected("class A { static { } }", "unsupported", 1, 11);
}

#[test]
fn rejects_for_await() {
    rejected("async function f(xs: any) { for await (const x of xs) {} }", "unsupported", 1, 29);
}

#[test]
fn rejects_delete_identifier() {
    // Strict-mode parse error or our explicit rejection; either way located.
    let diags = compile_to_ir("let a = 1;\ndelete a;", "test.ts").unwrap_err();
    let r = render_diagnostics(&diags, "let a = 1;\ndelete a;", "test.ts");
    assert!(r.starts_with("test.ts:2:"), "{r}");
}

#[test]
fn rejects_import_meta_and_dynamic_import() {
    rejected("const u = import.meta.url;", "unsupported", 1, 11);
    rejected("const m = import('./x');", "unsupported", 1, 11);
}

#[test]
fn rejects_accessor_fields() {
    rejected("class A { accessor x = 1; }", "unsupported", 1, 11);
}

#[test]
fn rejects_using_declarations() {
    rejected("{ using r = null; }", "unsupported", 1, 3);
}

#[test]
fn rejects_duplicate_lexical_declaration() {
    rejected("let a = 1;\nlet a = 2;", "semantic", 2, 5);
}

#[test]
fn rejects_unknown_label() {
    let diags = compile_to_ir("for (;;) { break nowhere; }", "test.ts").unwrap_err();
    assert!(!diags.is_empty());
}

#[test]
fn rejects_top_level_return() {
    let diags = compile_to_ir("return 1;", "test.ts").unwrap_err();
    let r = render_diagnostics(&diags, "return 1;", "test.ts");
    assert!(r.starts_with("test.ts:1:1:"), "{r}");
}

#[test]
fn reports_parse_errors_with_location() {
    let src = "let x = 1;\nlet y = ;";
    let diags = compile_to_ir(src, "test.ts").unwrap_err();
    assert!(diags.iter().all(|d| d.code == "parse"));
    let r = render_diagnostics(&diags, src, "test.ts");
    assert!(r.starts_with("test.ts:2:"), "{r}");
    assert!(r.contains("  | let y = ;"), "{r}");
}

#[test]
fn rejects_enum_member_without_initializer_after_string() {
    rejected("enum E { A = 'a', B }", "semantic", 1, 19);
}

#[test]
fn rejects_proto_literal() {
    rejected("const o = { __proto__: null };", "unsupported", 1, 13);
}

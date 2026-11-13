//! Uncaught errors and `Error.prototype.stack` carry function names and
//! source positions, at every optimization level.

use dj_cli::{CompileOptions, OptLevel, run, run_on_big_stack};
use dj_vm::VmOptions;

const SRC: &str = "\
function inner(x: number) {
  if (x > 1) throw new Error(\"bad \" + x);
  return x;
}
const outer = (v: number) => [v].map(n => inner(n + 1));
class K { method() { return outer(1); } }
new K().method();
";

fn run_src(src: &'static str, opt: OptLevel) -> dj_cli::RunResult {
    run_on_big_stack(move || {
        run(src, "st.ts", &CompileOptions { opt, debug_info: true, embed_source: true }, VmOptions::default()).unwrap()
    })
}

#[test]
fn uncaught_error_stack() {
    for opt in [OptLevel::None, OptLevel::Default] {
        let r = run_src(SRC, opt);
        let e = r.error.expect("uncaught error");
        assert_eq!(e.message, "Error: bad 2");
        let want = "\
Error: bad 2
    at inner (st.ts:2:20)
    at <anonymous> (st.ts:5:43)
    at map (native)
    at outer (st.ts:5:30)
    at method (st.ts:6:29)
    at <main> (st.ts:7:1)";
        assert_eq!(e.stack.as_deref(), Some(want), "{opt:?}");
    }
}

#[test]
fn caught_error_stack_and_rethrow() {
    let r = run_src(
        "\
function a() { b(); }
function b() { throw new TypeError(\"t\"); }
try { a(); } catch (e) { console.log((e as Error).stack); }
const f = () => { throw new RangeError(\"r\"); };
f();
",
        OptLevel::Default,
    );
    assert_eq!(r.output, "TypeError: t\n    at b (st.ts:2:22)\n    at a (st.ts:1:16)\n    at <main> (st.ts:3:7)\n");
    let e = r.error.unwrap();
    assert_eq!(e.stack.as_deref(), Some("RangeError: r\n    at f (st.ts:4:25)\n    at <main> (st.ts:5:1)"));
}

#[test]
fn runtime_type_errors_have_positions() {
    let r = run_src("const o: any = undefined;\nfunction g() { return o.x; }\ng();\n", OptLevel::Default);
    let e = r.error.unwrap();
    assert!(e.message.starts_with("TypeError: Cannot read properties of undefined"), "{}", e.message);
    let stack = e.stack.unwrap();
    assert!(stack.contains("\n    at g (st.ts:2:"), "{stack}");
    assert!(stack.ends_with("\n    at <main> (st.ts:3:1)"), "{stack}");
}

#[test]
fn non_error_throw_and_stack_overflow() {
    let r = run_src("throw { code: 42 };\n", OptLevel::Default);
    let e = r.error.unwrap();
    assert_eq!(e.message, "{ code: 42 }");
    assert!(e.stack.is_none());

    let r = run_src("function rec(n: number): number { return rec(n + 1) + 1; }\nrec(0);\n", OptLevel::Default);
    let e = r.error.unwrap();
    assert!(e.message.starts_with("RangeError: Maximum call stack size exceeded"), "{}", e.message);
}

#[test]
fn async_rejection_is_reported() {
    let r = run_src("async function f() { await null; throw new Error(\"late\"); }\nf();\n", OptLevel::Default);
    let e = r.error.expect("unhandled rejection");
    assert!(e.message.contains("late"), "{}", e.message);
}

#[test]
fn stripped_debug_info_still_runs() {
    let r = run_on_big_stack(|| {
        run(SRC, "st.ts", &CompileOptions { opt: OptLevel::Default, debug_info: false, embed_source: false }, VmOptions::default())
            .unwrap()
    });
    assert_eq!(r.error.unwrap().message, "Error: bad 2");
}

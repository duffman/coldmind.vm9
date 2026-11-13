//! Debugger hooks end to end through the DAP-shaped session API:
//! breakpoints, stepping, pause, exceptions and inspection.

use std::time::Duration;

use dj_cli::{CompileOptions, OptLevel, compile};
use dj_debug::{
    BreakpointSpec, Event, ExceptionFilters, FrameInfo, LaunchConfig, Request, Response, Session, StopReason, Variable,
};

const PROG: &str = "\
function add(a: number, b: number) {
  const sum = a + b;
  return sum;
}
let total = 0;
for (let i = 0; i < 3; i++) {
  total = add(total, i);
}
function counter() {
  let n = 10;
  return () => { n++; return n; };
}
const c = counter();
c();
console.log(\"total\", total);
";

fn launch_opt(src: &str, opt: OptLevel, cfg: LaunchConfig) -> Session {
    let m = compile(src, "dbg.ts", &CompileOptions { opt, debug_info: true, embed_source: true }).unwrap();
    Session::launch(m, cfg)
}

fn launch(src: &str, cfg: LaunchConfig) -> Session {
    launch_opt(src, OptLevel::Default, cfg)
}

fn bp(line: u32) -> BreakpointSpec {
    BreakpointSpec { line, column: None, condition: None }
}

fn set_bps(s: &Session, specs: Vec<BreakpointSpec>) -> Vec<dj_debug::Breakpoint> {
    match s.request(Request::SetBreakpoints(specs)) {
        Response::Breakpoints(b) => b,
        r => panic!("{r:?}"),
    }
}

fn start(s: &Session) {
    assert_eq!(s.request(Request::ConfigurationDone), Response::Ack);
}

/// Next stop: (reason, top frame).
fn stop(s: &Session, out: &mut String) -> (StopReason, FrameInfo) {
    match s.next_stop_or_exit(out) {
        Some(Event::Stopped { reason, frame }) => (reason, frame.expect("frame")),
        e => panic!("expected a stop, got {e:?} (output so far: {out:?})"),
    }
}

fn exited(s: &Session, out: &mut String) -> Option<String> {
    match s.next_stop_or_exit(out) {
        Some(Event::Exited { error }) => error,
        e => panic!("expected exit, got {e:?}"),
    }
}

fn line(f: &FrameInfo) -> u32 {
    f.pos.expect("position").line
}

fn resume(s: &Session, r: Request) {
    assert_eq!(s.request(r), Response::Ack);
}

/// All variables of all scopes of `frame`, by name.
fn vars(s: &Session, frame: usize) -> Vec<(String, Variable)> {
    let Response::Scopes(scopes) = s.request(Request::Scopes { frame }) else { panic!() };
    let mut out = Vec::new();
    for sc in scopes {
        if sc.name == "Global" {
            continue;
        }
        let Response::Variables(vs) = s.request(Request::Variables { reference: sc.variables_ref }) else { panic!() };
        out.extend(vs.into_iter().map(|v| (format!("{}:{}", sc.name, v.name), v)));
    }
    out
}

fn var(s: &Session, frame: usize, scoped_name: &str) -> Variable {
    let all = vars(s, frame);
    all.iter().find(|(n, _)| n == scoped_name).map(|(_, v)| v.clone()).unwrap_or_else(|| {
        panic!("{scoped_name} not found in {:?}", all.iter().map(|(n, v)| format!("{n}={}", v.value)).collect::<Vec<_>>())
    })
}

fn eval(s: &Session, frame: usize, e: &str) -> Result<String, String> {
    match s.request(Request::Evaluate { frame, expression: e.into() }) {
        Response::Evaluate(r) => r.map(|v| v.value),
        r => panic!("{r:?}"),
    }
}

#[test]
fn breakpoints_stack_and_variables() {
    let s = launch(PROG, LaunchConfig::default());
    let b = set_bps(&s, vec![bp(2), bp(4), bp(100)]);
    assert!(b[0].verified && b[0].line == 2);
    assert!(b[1].verified && b[1].line == 5, "line 4 moves to the next statement: {:?}", b[1]);
    assert!(!b[2].verified);
    start(&s);
    let mut out = String::new();

    // Line 5 (`let total = 0`) first.
    let (r, f) = stop(&s, &mut out);
    assert_eq!((r, line(&f)), (StopReason::Breakpoint(vec![b[1].id]), 5));
    // TDZ binding shows as uninitialized before its declaration completes.
    let total = var(&s, 0, "Local:total");
    assert_eq!(total.type_name, "uninitialized", "{total:?}");
    resume(&s, Request::Continue);

    for (i, want_total) in [0, 0, 1].into_iter().enumerate() {
        let (r, f) = stop(&s, &mut out);
        assert_eq!((r, line(&f), f.name.as_str()), (StopReason::Breakpoint(vec![b[0].id]), 2, "add"));
        let Response::StackTrace(frames) = s.request(Request::StackTrace) else { panic!() };
        let names: Vec<(&str, u32)> = frames.iter().map(|f| (f.name.as_str(), line(f))).collect();
        assert_eq!(names, vec![("add", 2), ("<main>", 7)]);
        assert_eq!(var(&s, 0, "Local:a").value, want_total.to_string());
        assert_eq!(var(&s, 0, "Local:b").value, i.to_string());
        assert_eq!(var(&s, 0, "Local:sum").type_name, "uninitialized");
        // Caller frame: loop variable in a block scope.
        assert_eq!(var(&s, 1, "Block:i").value, i.to_string());
        assert_eq!(eval(&s, 1, "i").unwrap(), i.to_string());
        resume(&s, Request::Continue);
    }
    assert_eq!(exited(&s, &mut out), None);
    assert_eq!(out, "total 3\n");
}

#[test]
fn conditional_breakpoint_and_evaluate() {
    let s = launch(PROG, LaunchConfig::default());
    set_bps(&s, vec![BreakpointSpec { line: 7, column: None, condition: Some("i".into()) }]);
    start(&s);
    let mut out = String::new();
    let mut seen = Vec::new();
    while let Some(Event::Stopped { frame, .. }) = s.next_stop_or_exit(&mut out) {
        seen.push(eval(&s, 0, "i").unwrap());
        assert_eq!(line(&frame.unwrap()), 7);
        assert!(eval(&s, 0, "nosuchname").is_err());
        assert_eq!(eval(&s, 0, "total").unwrap(), if seen.len() == 1 { "0" } else { "1" });
        resume(&s, Request::Continue);
    }
    assert_eq!(seen, vec!["1", "2"]);
}

#[test]
fn stepping_over_in_and_out() {
    let s = launch(PROG, LaunchConfig { stop_on_entry: true, gc_stress: false });
    start(&s);
    let mut out = String::new();
    let (r, f) = stop(&s, &mut out);
    assert_eq!((r, line(&f)), (StopReason::Entry, 5));
    let mut lines = vec![5];
    for _ in 0..4 {
        resume(&s, Request::Next);
        let (r, f) = stop(&s, &mut out);
        assert_eq!(r, StopReason::Step);
        lines.push(line(&f));
    }
    // let total; loop init; loop test; body; update.
    assert_eq!(lines, vec![5, 6, 6, 7, 6]);
    step(&s, &mut out, Request::Next, 6, "<main>"); // loop test
    step(&s, &mut out, Request::Next, 7, "<main>");
    step(&s, &mut out, Request::StepIn, 2, "add");
    step(&s, &mut out, Request::Next, 3, "add");
    step(&s, &mut out, Request::StepOut, 7, "<main>");
    resume(&s, Request::Terminate);
    assert!(exited(&s, &mut out).is_some());
    assert_eq!(out, "");
}

fn step(s: &Session, out: &mut String, r: Request, want_line: u32, want_fn: &str) {
    let label = format!("{r:?}");
    resume(s, r);
    let (reason, f) = stop(s, out);
    assert_eq!((reason, line(&f), f.name.as_str()), (StopReason::Step, want_line, want_fn), "after {label} at {:?}", f.pos);
}

#[test]
fn closures_show_captured_variables() {
    let s = launch(PROG, LaunchConfig::default());
    set_bps(&s, vec![BreakpointSpec { line: 11, column: Some(18), condition: None }]);
    start(&s);
    let mut out = String::new();
    let (_, f) = stop(&s, &mut out);
    assert_eq!((line(&f), f.pos.unwrap().col), (11, 18));
    assert_eq!(var(&s, 0, "Closure:n").value, "10");
    resume(&s, Request::Next);
    stop(&s, &mut out);
    assert_eq!(var(&s, 0, "Closure:n").value, "11");
    assert_eq!(eval(&s, 0, "n").unwrap(), "11");
}

#[test]
fn debugger_statement_and_object_inspection() {
    let s = launch(
        "const o = { a: 1, nested: { b: [1, 2] } };\nclass P { x = 5 }\nconst p = new P();\ndebugger;\nconsole.log(o.a);\n",
        LaunchConfig::default(),
    );
    start(&s);
    let mut out = String::new();
    let (r, f) = stop(&s, &mut out);
    assert_eq!((r, line(&f)), (StopReason::DebuggerStatement, 4));
    let o = var(&s, 0, "Local:o");
    assert!(o.variables_ref != 0, "{o:?}");
    let Response::Variables(props) = s.request(Request::Variables { reference: o.variables_ref }) else { panic!() };
    let names: Vec<&str> = props.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["a", "nested", "[[Prototype]]"]);
    assert_eq!(eval(&s, 0, "o.nested.b[1]").unwrap(), "2");
    assert_eq!(eval(&s, 0, "p.x").unwrap(), "5");
    resume(&s, Request::Continue);
    assert_eq!(exited(&s, &mut out), None);
    assert_eq!(out, "1\n");
}

const THROWS: &str = "\
function risky(n: number) {
  if (n > 0) throw new Error(\"n=\" + n);
}
try { risky(1); } catch (e) { console.log(\"caught\"); }
risky(2);
";

#[test]
fn exception_filters() {
    // all: stops at the caught throw, then at the uncaught one.
    let s = launch(THROWS, LaunchConfig::default());
    assert_eq!(s.request(Request::SetExceptionFilters(ExceptionFilters { all: true, uncaught: false })), Response::Ack);
    start(&s);
    let mut out = String::new();
    let (r, f) = stop(&s, &mut out);
    assert_eq!((r, line(&f)), (StopReason::Exception { uncaught: false }, 2));
    let Response::ExceptionInfo(Some(info)) = s.request(Request::ExceptionInfo) else { panic!() };
    assert_eq!(info.description, "Error: n=1");
    assert!(info.stack.unwrap().contains("at risky (dbg.ts:2:"));
    resume(&s, Request::Continue);
    let (r, _) = stop(&s, &mut out);
    assert_eq!(r, StopReason::Exception { uncaught: true });
    resume(&s, Request::Continue);
    assert!(exited(&s, &mut out).unwrap().contains("n=2"));
    assert_eq!(out, "caught\n");

    // uncaught only: one stop, with the throwing frame still inspectable.
    let s = launch(THROWS, LaunchConfig::default());
    s.request(Request::SetExceptionFilters(ExceptionFilters { all: false, uncaught: true }));
    start(&s);
    let mut out = String::new();
    let (r, f) = stop(&s, &mut out);
    assert_eq!((r, line(&f), f.name.as_str()), (StopReason::Exception { uncaught: true }, 2, "risky"));
    assert_eq!(var(&s, 0, "Local:n").value, "2");
    resume(&s, Request::Continue);
    assert!(exited(&s, &mut out).is_some());
}

#[test]
fn pause_running_program() {
    let s = launch("let n = 0;\nwhile (true) {\n  n++;\n}\n", LaunchConfig::default());
    start(&s);
    std::thread::sleep(Duration::from_millis(50));
    s.pause();
    let mut out = String::new();
    let (r, f) = stop(&s, &mut out);
    assert_eq!(r, StopReason::Pause);
    assert!((2..=3).contains(&line(&f)));
    let n: f64 = eval(&s, 0, "n").unwrap().parse().unwrap();
    assert!(n > 0.0);
    resume(&s, Request::Terminate);
    assert!(exited(&s, &mut out).is_some());
}

#[test]
fn step_over_await_stays_in_async_function() {
    let src = "\
async function work() {
  const a = await Promise.resolve(1);
  const b = await new Promise<number>(r => setTimeout(() => r(2), 5));
  return a + b;
}
work().then(v => console.log(\"v\", v));
console.log(\"sync done\");
";
    let s = launch(src, LaunchConfig::default());
    set_bps(&s, vec![bp(2)]);
    start(&s);
    let mut out = String::new();
    let (_, f) = stop(&s, &mut out);
    assert_eq!((line(&f), f.name.as_str()), (2, "work"));
    let id = f.id;
    for want in [3, 4] {
        resume(&s, Request::Next);
        let (r, f) = stop(&s, &mut out);
        assert_eq!((r, line(&f), f.name.as_str(), f.id), (StopReason::Step, want, "work", id));
    }
    assert_eq!(out, "sync done\n", "the rest of the script ran while `work` was suspended");
    assert_eq!(var(&s, 0, "Local:a").value, "1");
    assert_eq!(var(&s, 0, "Local:b").value, "2");
    resume(&s, Request::Continue);
    assert_eq!(exited(&s, &mut out), None);
    assert_eq!(out, "sync done\nv 3\n");
}

/// The same session script stops at the same lines, with the same values,
/// at every optimization level and under GC stress.
#[test]
fn stops_are_stable_across_opt_levels_and_gc() {
    let mut runs = Vec::new();
    for (opt, gc_stress) in [(OptLevel::None, false), (OptLevel::Default, false), (OptLevel::Default, true)] {
        let s = launch_opt(PROG, opt, LaunchConfig { stop_on_entry: true, gc_stress });
        set_bps(&s, vec![bp(3), bp(11)]);
        start(&s);
        let mut out = String::new();
        let mut trace = Vec::new();
        loop {
            match s.next_stop_or_exit(&mut out) {
                Some(Event::Stopped { frame, .. }) => {
                    let f = frame.unwrap();
                    let mut vs: Vec<String> =
                        vars(&s, 0).into_iter().map(|(n, v)| format!("{n}={}:{}", v.value, v.type_name)).collect();
                    vs.sort();
                    trace.push(format!("{}@{} {}", f.name, line(&f), vs.join(",")));
                    resume(&s, if trace.len() < 12 { Request::StepIn } else { Request::Continue });
                }
                Some(Event::Exited { error }) => {
                    assert_eq!(error, None);
                    break;
                }
                e => panic!("{e:?}"),
            }
        }
        assert_eq!(out, "total 3\n");
        runs.push(trace);
    }
    assert_eq!(runs[0], runs[1], "O0 vs optimized");
    assert_eq!(runs[1], runs[2], "gc stress");
}

#[test]
fn breakpoint_locations_and_terminate_before_start() {
    let s = launch(PROG, LaunchConfig::default());
    let Response::Locations(locs) = s.request(Request::BreakpointLocations(6)) else { panic!() };
    assert!(locs.len() >= 3 && locs.iter().all(|&(l, _)| l == 6), "{locs:?}");
    assert!(matches!(s.request(Request::StackTrace), Response::Error(_)));
    resume(&s, Request::Terminate);
    let mut out = String::new();
    assert_eq!(exited(&s, &mut out).as_deref(), Some("terminated before start"));
}

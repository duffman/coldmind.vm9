//! End-to-end corpus: every `tests/cases/*.ts` is compiled and run, and its
//! console output must equal `*.out` (generated once from Node by
//! `tests/gen_expected.sh`). Each mode reports every failing case.

use std::fs;
use std::path::PathBuf;

use dj_cli::{CompileOptions, OptLevel, run, run_on_big_stack};
use dj_vm::VmOptions;

fn cases() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut v: Vec<PathBuf> =
        fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "ts")).collect();
    v.sort();
    assert!(!v.is_empty(), "no cases found in {}", dir.display());
    v
}

fn first_diff(want: &str, got: &str) -> String {
    let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    for i in 0..w.len().max(g.len()) {
        let (a, b) = (w.get(i).copied(), g.get(i).copied());
        if a != b {
            return format!("line {}:\n  want: {}\n  got:  {}", i + 1, a.unwrap_or("<eof>"), b.unwrap_or("<eof>"));
        }
    }
    "outputs differ only in trailing whitespace".into()
}

fn run_all(opt: OptLevel, gc_stress: bool) {
    let mut failures = Vec::new();
    let all = cases();
    for path in &all {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(path).unwrap();
        let want = fs::read_to_string(path.with_extension("out")).unwrap_or_else(|_| panic!("missing {name} .out"));
        let opts = CompileOptions { opt, debug_info: true, embed_source: true };
        let vm_opts = VmOptions { gc_stress, ..VmOptions::default() };
        let file = name.clone();
        let result = run_on_big_stack(move || run(&source, &file, &opts, vm_opts));
        match result {
            Err(e) => failures.push(format!("{name}: compile error:\n{e}")),
            Ok(r) => {
                let mut got = r.output;
                if let Some(e) = r.error {
                    got.push_str(&format!("Uncaught {}\n", e.message));
                }
                if got != want {
                    failures.push(format!("{name}: {}", first_diff(&want, &got)));
                }
            }
        }
    }
    eprintln!("{} cases, {} failed ({opt:?}, gc_stress={gc_stress})", all.len(), failures.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
fn cases_unoptimized() {
    run_all(OptLevel::None, false);
}

#[test]
fn cases_optimized() {
    run_all(OptLevel::Default, false);
}

#[test]
fn cases_gc_stress() {
    run_all(OptLevel::Default, true);
}

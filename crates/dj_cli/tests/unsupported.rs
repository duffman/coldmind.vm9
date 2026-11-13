//! Unsupported and invalid constructs are rejected with a coded diagnostic
//! at the construct's source location.

use dj_cli::{CompileError, CompileOptions, compile};

/// Compile `line` as line 2 of a file and return the rendered diagnostics.
fn diagnose(line: &str) -> (Vec<&'static str>, String) {
    let src = format!("let ok = 1;\n{line}\nconsole.log(ok);\n");
    match compile(&src, "t.ts", &CompileOptions::default()) {
        Err(CompileError::Diagnostics { rendered, diagnostics }) => (diagnostics.iter().map(|d| d.code).collect(), rendered),
        Err(e) => panic!("{line}: unexpected internal error {e}"),
        Ok(_) => panic!("{line}: compiled, expected a diagnostic"),
    }
}

fn expect(code: &str, cases: &[(&str, u32, &str)]) {
    let mut failures = Vec::new();
    for &(line, col, msg) in cases {
        let (codes, rendered) = diagnose(line);
        let head = format!("t.ts:2:{col}: error[{code}]: ");
        let first = rendered.lines().next().unwrap_or("");
        if codes.first() != Some(&code) || !first.starts_with(&head) || !first.contains(msg) {
            failures.push(format!("{line}\n  want: {head}...{msg}\n  got:  {first}"));
        }
        // The caret line points at the construct.
        if !rendered.lines().nth(2).is_some_and(|l| l.starts_with(&format!("  |{}^", " ".repeat(col as usize)))) {
            failures.push(format!("{line}: caret not at column {col}:\n{rendered}"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn unsupported_constructs() {
    expect(
        "unsupported",
        &[
            ("function* g() {}", 1, "generator functions"),
            ("async function* ag() {}", 1, "generator functions"),
            ("async function f() { for await (const x of []) {} }", 22, "for await...of"),
            ("const r = /re/;", 11, "regular expression literals"),
            ("const b = 10n;", 11, "BigInt literals"),
            ("class C { #p = 1; }", 11, "private class members"),
            ("class D { static { } }", 11, "class static blocks"),
            ("class E { accessor a = 1; }", 11, "auto-accessor fields"),
            ("namespace N { export const x = 1; }", 1, "namespaces"),
            ("import { x } from \"./m\";", 1, "runtime imports"),
            ("export const e = 1;", 1, "runtime exports"),
            ("const t = String.raw`x`;", 11, "tagged templates"),
            ("function a() { return arguments; }", 23, "the `arguments` object"),
            ("const m = import.meta;", 11, "import.meta"),
            ("const d = import(\"./m\");", 11, "dynamic import()"),
            ("label: function lf() {}", 1, "labeled function declarations"),
            ("delete ok;", 1, "delete of an unqualified identifier"),
            ("const o = { __proto__: null };", 13, "`__proto__: value`"),
            ("class F extends Object { m() { return () => super.toString(); } }", 45, "`super` inside arrow functions"),
            ("with ({}) {}", 1, "with statements"),
            ("using res = null;", 1, "using declarations"),
        ],
    );
}

#[test]
fn semantic_errors() {
    expect(
        "semantic",
        &[
            ("let ok = 2;", 5, ""),
            ("break;", 1, ""),
            ("lbl: { continue lbl; }", 8, ""),
            ("function g() { super(); }", 16, ""),
        ],
    );
}

#[test]
fn parse_errors() {
    expect(
        "parse",
        &[
            ("return 1;", 1, "'return' statement"),
            ("function f() { await 1; }", 16, "`await` is only allowed"),
            ("1 = 2;", 1, "Cannot assign"),
        ],
    );
    let (codes, rendered) = diagnose("let = = 1;");
    assert_eq!(codes.first(), Some(&"parse"), "{rendered}");
    assert!(rendered.starts_with("t.ts:2:"), "{rendered}");
}

#[test]
fn all_diagnostics_are_reported() {
    let src = "function* a() {}\nconst r = /x/;\nconst b = 1n;\n";
    let Err(CompileError::Diagnostics { diagnostics, .. }) = compile(src, "t.ts", &CompileOptions::default()) else {
        panic!("expected diagnostics");
    };
    assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
}

//! Compilation is deterministic and the bytecode format round-trips.

use std::fs;
use std::path::PathBuf;

use dj_bytecode as bc;
use dj_cli::{CompileOptions, OptLevel, compile};

fn sources() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut v: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

fn all_options() -> Vec<CompileOptions> {
    let mut out = Vec::new();
    for opt in [OptLevel::None, OptLevel::Default] {
        for debug_info in [true, false] {
            out.push(CompileOptions { opt, debug_info, embed_source: debug_info });
        }
    }
    out
}

#[test]
fn same_input_same_bytes() {
    for (name, src) in sources() {
        for o in all_options() {
            let a = compile(&src, &name, &o).unwrap().to_bytes();
            let b = compile(&src, &name, &o).unwrap().to_bytes();
            assert!(a == b, "{name} ({:?}, debug={}): two compilations differ", o.opt, o.debug_info);
        }
    }
}

#[test]
fn bytes_round_trip() {
    for (name, src) in sources() {
        for o in all_options() {
            let m = compile(&src, &name, &o).unwrap();
            let bytes = m.to_bytes();
            let back = bc::Module::from_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(back == m, "{name}: decoded module differs from the original");
            assert!(back.to_bytes() == bytes, "{name}: re-encoding differs");
            assert_eq!(bc::disasm::disassemble(&back), bc::disasm::disassemble(&m), "{name}: disassembly differs");
            bc::validate::validate(&back).unwrap_or_else(|e| panic!("{name}: decoded module invalid: {e:?}"));
        }
    }
}

#[test]
fn debug_info_does_not_change_code() {
    for (name, src) in sources() {
        let with = compile(&src, &name, &CompileOptions { opt: OptLevel::Default, debug_info: true, embed_source: true }).unwrap();
        let without = compile(&src, &name, &CompileOptions { opt: OptLevel::Default, debug_info: false, embed_source: false }).unwrap();
        assert!(with.debug.is_some() && without.debug.is_none());
        assert!(with.functions == without.functions && with.constants == without.constants, "{name}: debug info changed the code");
    }
}

#[test]
fn corrupt_input_is_rejected() {
    let m = compile("console.log(1 + 2);", "t.ts", &CompileOptions::default()).unwrap();
    let bytes = m.to_bytes();
    assert!(bc::Module::from_bytes(&bytes[..bytes.len() / 2]).is_err(), "truncated input accepted");
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert!(bc::Module::from_bytes(&bad).is_err(), "bad magic accepted");
    assert!(bc::Module::from_bytes(&[]).is_err(), "empty input accepted");
}

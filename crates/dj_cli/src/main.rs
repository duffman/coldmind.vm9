//! `djungle` command-line interface.

use std::process::ExitCode;

use dj_bytecode as bc;
use dj_cli::{compile, compile_ir, run_on_big_stack, CompileOptions, OptLevel};
use dj_vm::{SystemClock, Vm, VmOptions};

const USAGE: &str = "usage: djungle <command> [options] <file>

commands:
  run <file.ts>                  compile and run
  check <file.ts>                report diagnostics only
  ir <file.ts>                   print the IR
  disasm <file.ts|file.djbc>     print bytecode disassembly
  compile <file.ts> -o <out>     write a bytecode module (.djbc)
  exec <file.djbc>               run a bytecode module

options:
  -O0             disable IR optimization
  --strip-debug   omit debug info (compile)
  --gc-stress     collect garbage at every safepoint
  --real-time     timers wait in wall-clock time (default: virtual time)";

struct Args {
    command: String,
    file: String,
    out: Option<String>,
    opt: OptLevel,
    strip_debug: bool,
    gc_stress: bool,
    real_time: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or("missing command")?;
    let mut a = Args { command, file: String::new(), out: None, opt: OptLevel::Default, strip_debug: false, gc_stress: false, real_time: false };
    while let Some(x) = it.next() {
        match x.as_str() {
            "-O0" => a.opt = OptLevel::None,
            "-O" | "-O1" => a.opt = OptLevel::Default,
            "--strip-debug" => a.strip_debug = true,
            "--gc-stress" => a.gc_stress = true,
            "--real-time" => a.real_time = true,
            "-o" => a.out = Some(it.next().ok_or("-o needs a path")?),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s if a.file.is_empty() => a.file = s.to_string(),
            s => return Err(format!("unexpected argument {s}")),
        }
    }
    if a.file.is_empty() {
        return Err("missing file".into());
    }
    Ok(a)
}

fn load_module(a: &Args) -> Result<bc::Module, String> {
    if a.file.ends_with(".djbc") {
        let bytes = std::fs::read(&a.file).map_err(|e| format!("{}: {e}", a.file))?;
        return bc::Module::from_bytes(&bytes).map_err(|e| format!("{}: {e}", a.file));
    }
    let src = std::fs::read_to_string(&a.file).map_err(|e| format!("{}: {e}", a.file))?;
    let opts = CompileOptions { opt: a.opt, debug_info: !a.strip_debug, embed_source: !a.strip_debug };
    compile(&src, &a.file, &opts).map_err(|e| e.to_string())
}

fn execute(m: bc::Module, a: &Args) -> ExitCode {
    let (gc_stress, real_time) = (a.gc_stress, a.real_time);
    run_on_big_stack(move || {
        let mut vm = Vm::new(VmOptions { gc_stress, ..VmOptions::default() });
        if real_time {
            vm.set_scheduler(Box::new(SystemClock::default()));
        }
        let id = match vm.load(m) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
        match vm.run_module(id) {
            Ok(_) => ExitCode::SUCCESS,
            Err(u) => {
                eprintln!("{u}");
                ExitCode::FAILURE
            }
        }
    })
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("djungle: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let r: Result<ExitCode, String> = (|| match a.command.as_str() {
        "run" | "exec" => Ok(execute(load_module(&a)?, &a)),
        "check" => {
            let src = std::fs::read_to_string(&a.file).map_err(|e| format!("{}: {e}", a.file))?;
            compile_ir(&src, &a.file, OptLevel::None).map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        "ir" => {
            let src = std::fs::read_to_string(&a.file).map_err(|e| format!("{}: {e}", a.file))?;
            let m = compile_ir(&src, &a.file, a.opt).map_err(|e| e.to_string())?;
            print!("{}", dj_ir::print::print_module(&m));
            Ok(ExitCode::SUCCESS)
        }
        "disasm" => {
            print!("{}", bc::disasm::disassemble(&load_module(&a)?));
            Ok(ExitCode::SUCCESS)
        }
        "compile" => {
            let out = a.out.clone().unwrap_or_else(|| format!("{}.djbc", a.file.trim_end_matches(".ts")));
            let m = load_module(&a)?;
            std::fs::write(&out, m.to_bytes()).map_err(|e| format!("{out}: {e}"))?;
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    })();
    match r {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

//! Bytecode validation. The VM refuses to load a module that fails any of
//! these checks; after validation the interpreter may rely on:
//! * every instruction decodes and every operand is in range (registers,
//!   constants, string-typed names, functions, upvalues, argument windows);
//! * every jump / handler target is an instruction boundary;
//! * control cannot run off the end of a function;
//! * handler targets start with `GetException`, and `GetException` appears
//!   only there;
//! * context-restricted instructions appear only in valid function kinds
//!   (`Await` in async functions, `SuperCall*` in derived constructors, ...);
//! * closure upvalue descriptors reference valid registers / upvalues of the
//!   creating function;
//! * debug metadata (if present) is consistent with the code.

use std::collections::BTreeSet;

use crate::debug::VarLocation;
use crate::instr::{decode_all, Instr, OperandKind};
use crate::module::*;

/// Upper bound on registers per frame (bounds allocation per call).
pub const MAX_REGISTERS: u32 = 1 << 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub func: Option<u32>,
    pub offset: Option<u32>,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.func, self.offset) {
            (Some(func), Some(off)) => write!(f, "f{func}@{off:#06x}: {}", self.message),
            (Some(func), None) => write!(f, "f{func}: {}", self.message),
            _ => write!(f, "module: {}", self.message),
        }
    }
}

impl std::error::Error for ValidationError {}

pub fn validate(m: &Module) -> Result<(), Vec<ValidationError>> {
    let mut errs = Vec::new();
    let mut err = |func: Option<u32>, offset: Option<u32>, message: String| {
        errs.push(ValidationError { func, offset, message });
    };

    if m.entry as usize >= m.functions.len() {
        err(None, None, format!("entry function {} out of range", m.entry));
        return Err(errs);
    }
    let entry = &m.functions[m.entry as usize];
    if entry.kind != FunctionKind::Script || !entry.upvalues.is_empty() || entry.param_count != 0 {
        err(Some(m.entry), None, "entry must be a script function without params or upvalues".into());
    }

    for (fi, f) in m.functions.iter().enumerate() {
        let fid = Some(fi as u32);
        if m.string(f.name).is_none() {
            err(fid, None, format!("name k{} is not a string constant", f.name));
        }
        if f.register_count > MAX_REGISTERS {
            err(fid, None, format!("register count {} exceeds {MAX_REGISTERS}", f.register_count));
            continue;
        }
        let needed = f.param_count as u64 + f.has_rest as u64;
        if (f.register_count as u64) < needed {
            err(fid, None, "fewer registers than parameters".into());
            continue;
        }
        let instrs = match decode_all(&f.code) {
            Ok(v) => v,
            Err(e) => {
                err(fid, None, e.to_string());
                continue;
            }
        };
        let Some(last) = instrs.last() else {
            err(fid, None, "empty function body".into());
            continue;
        };
        if !last.1.is_terminator() {
            err(fid, Some(last.0), "control can fall off the end of the function".into());
        }
        let boundaries: BTreeSet<u32> = instrs.iter().map(|(o, _)| *o).collect();
        let code_len = f.code.len() as u32;
        let handler_targets: BTreeSet<u32> = f.handlers.iter().map(|h| h.target).collect();

        for h in &f.handlers {
            let ok_start = boundaries.contains(&h.start);
            let ok_end = h.end == code_len || boundaries.contains(&h.end);
            if !ok_start || !ok_end || h.start >= h.end {
                err(fid, None, format!("bad handler range [{:#x}, {:#x})", h.start, h.end));
            }
            match instrs.iter().find(|(o, _)| *o == h.target) {
                Some((_, Instr::GetException { .. })) => {}
                Some(_) => err(fid, Some(h.target), "handler target must start with GetException".into()),
                None => err(fid, None, format!("handler target {:#x} is not an instruction boundary", h.target)),
            }
        }

        let regs = f.register_count;
        for &(off, instr) in &instrs {
            let at = Some(off);
            for (name, kind, v) in instr.operands() {
                // Window bases are checked as windows below (an empty window may start at `regs`).
                let window_base = matches!(
                    (instr, name),
                    (Instr::Call { .. } | Instr::CallFn { .. } | Instr::New { .. } | Instr::SuperCall { .. }, "args")
                        | (Instr::CopyDataProps { .. }, "excl")
                );
                let bad = match kind {
                    OperandKind::Reg => !window_base && v >= regs,
                    OperandKind::Const => v as usize >= m.constants.len(),
                    OperandKind::Name => m.string(v).is_none(),
                    OperandKind::Func => v as usize >= m.functions.len(),
                    OperandKind::Upval => v as usize >= f.upvalues.len(),
                    OperandKind::Target => !boundaries.contains(&v),
                    OperandKind::Count | OperandKind::Imm | OperandKind::U8 => false,
                };
                if bad {
                    err(fid, at, format!("{}: operand `{name}` ({kind:?}) = {v} invalid", instr.name()));
                }
            }
            let window = |start: u32, count: u32| start as u64 + count as u64 <= regs as u64;
            if let Instr::Call { args, argc, .. }
            | Instr::CallFn { args, argc, .. }
            | Instr::New { args, argc, .. }
            | Instr::SuperCall { args, argc, .. } = instr
            {
                if !window(args, argc) {
                    err(fid, at, "argument window exceeds register file".into());
                }
            }
            match instr {
                Instr::CopyDataProps { excl, count, .. } => {
                    if !window(excl, count) {
                        err(fid, at, "excluded-key window exceeds register file".into());
                    }
                }
                Instr::DefineMethod { flags, .. } => {
                    if flags & 3 == 3 || flags & !7 != 0 {
                        err(fid, at, format!("bad DefineMethod flags {flags:#x}"));
                    }
                }
                Instr::MakeClass { has_parent, .. } => {
                    if has_parent > 1 {
                        err(fid, at, "bad MakeClass has_parent".into());
                    }
                }
                Instr::ThrowError { kind, .. } => {
                    if kind > 3 {
                        err(fid, at, format!("bad error kind {kind}"));
                    }
                }
                Instr::GetException { .. } => {
                    if !handler_targets.contains(&off) {
                        err(fid, at, "GetException outside a handler entry".into());
                    }
                }
                Instr::Await { .. } => {
                    if !f.is_async {
                        err(fid, at, "Await in non-async function".into());
                    }
                }
                Instr::SuperCall { .. } | Instr::SuperCallSpread { .. } => {
                    if f.kind != FunctionKind::DerivedConstructor {
                        err(fid, at, "super call outside derived constructor".into());
                    }
                }
                Instr::InitFields {} => {
                    if f.kind != FunctionKind::ClassConstructor {
                        err(fid, at, "InitFields outside base class constructor".into());
                    }
                }
                Instr::Closure { func, .. } => {
                    if let Some(child) = m.functions.get(func as usize) {
                        if child.kind == FunctionKind::Script {
                            err(fid, at, "closure over a script function".into());
                        }
                        for (ui, u) in child.upvalues.iter().enumerate() {
                            let ok = if u.from_parent_register { u.index < regs } else { (u.index as usize) < f.upvalues.len() };
                            if !ok {
                                err(fid, at, format!("closure f{func} upvalue u{ui} references invalid parent slot {}", u.index));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(d) = &m.debug {
        if d.functions.len() != m.functions.len() {
            err(None, None, "debug info function count mismatch".into());
        } else {
            for (fi, (f, fd)) in m.functions.iter().zip(&d.functions).enumerate() {
                let fid = Some(fi as u32);
                let Ok(instrs) = decode_all(&f.code) else { continue };
                let boundaries: BTreeSet<u32> = instrs.iter().map(|(o, _)| *o).collect();
                let code_len = f.code.len() as u32;
                if !fd.lines.windows(2).all(|w| w[0].pc < w[1].pc) || fd.lines.iter().any(|e| !boundaries.contains(&e.pc)) {
                    err(fid, None, "line table not sorted or not on instruction boundaries".into());
                }
                for (si, s) in fd.scopes.iter().enumerate() {
                    if s.parent.is_some_and(|p| p as usize >= si) {
                        err(fid, None, format!("debug scope {si} has invalid parent"));
                    }
                }
                if !fd.scope_map.windows(2).all(|w| w[0].0 < w[1].0)
                    || fd.scope_map.iter().any(|&(pc, s)| !boundaries.contains(&pc) || s as usize >= fd.scopes.len())
                {
                    err(fid, None, "invalid scope map".into());
                }
                for v in &fd.variables {
                    let ok = match v.location {
                        VarLocation::Register(r) | VarLocation::Cell(r) => r < f.register_count,
                        VarLocation::Upvalue(u) => (u as usize) < f.upvalues.len(),
                    };
                    let ranges_ok = v.ranges.iter().all(|&(s, e)| s < e && e <= code_len);
                    let scope_ok = v.scope.is_none_or(|s| (s as usize) < fd.scopes.len());
                    if !ok || !ranges_ok || !scope_ok {
                        err(fid, None, format!("invalid debug variable `{}`", v.name));
                    }
                }
                if !fd.ir_map.windows(2).all(|w| w[0].pc < w[1].pc) || fd.ir_map.iter().any(|p| !boundaries.contains(&p.pc)) {
                    err(fid, None, "invalid IR position map".into());
                }
            }
        }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(code: Vec<Instr>, regs: u32) -> Module {
        let mut bytes = Vec::new();
        for i in &code {
            i.encode(&mut bytes);
        }
        Module {
            constants: vec![Constant::String("main".into()), Constant::Number(1.0)],
            functions: vec![Function {
                name: 0,
                kind: FunctionKind::Script,
                is_async: false,
                param_count: 0,
                has_rest: false,
                register_count: regs,
                upvalues: vec![],
                code: bytes,
                handlers: vec![],
            }],
            entry: 0,
            debug: None,
        }
    }

    #[test]
    fn accepts_minimal() {
        validate(&module(vec![Instr::LoadUndef { dst: 0 }, Instr::Ret { src: 0 }], 1)).unwrap();
    }

    #[test]
    fn rejects_register_out_of_range() {
        let e = validate(&module(vec![Instr::LoadUndef { dst: 3 }, Instr::Ret { src: 0 }], 1)).unwrap_err();
        assert!(e[0].message.contains("operand `dst`"), "{e:?}");
    }

    #[test]
    fn rejects_fall_through_and_bad_target_and_name_type() {
        let e = validate(&module(vec![Instr::LoadUndef { dst: 0 }], 1)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("fall off")));
        let e = validate(&module(vec![Instr::Jmp { target: 1 }], 1)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("operand `target`")));
        let e = validate(&module(vec![Instr::GetGlobal { dst: 0, name: 1 }, Instr::Ret { src: 0 }], 1)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("operand `name`")), "number constant used as name");
    }

    #[test]
    fn rejects_context_restricted_instructions() {
        let e = validate(&module(vec![Instr::Await { dst: 0, value: 0 }, Instr::Ret { src: 0 }], 1)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("Await")));
        let e = validate(&module(vec![Instr::GetException { dst: 0 }, Instr::Ret { src: 0 }], 1)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("GetException")));
        let e = validate(&module(vec![Instr::CallFn { dst: 0, callee: 0, args: 1, argc: 5 }, Instr::Ret { src: 0 }], 2)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("argument window")));
        let e = validate(&module(vec![Instr::SuperCall { dst: 0, args: 0, argc: 0 }, Instr::Ret { src: 0 }], 1)).unwrap_err();
        assert!(e.iter().any(|e| e.message.contains("super call outside derived constructor")), "{e:?}");
    }

    /// An empty argument / excluded-key window may start at the end of the
    /// register file (codegen places the outgoing-args window there).
    #[test]
    fn accepts_empty_window_at_register_file_end() {
        let r = validate(&module(vec![Instr::CallFn { dst: 0, callee: 0, args: 1, argc: 0 }, Instr::Ret { src: 0 }], 1));
        assert!(r.is_ok(), "{r:?}");
        let r = validate(&module(
            vec![Instr::NewObject { dst: 0 }, Instr::CopyDataProps { target: 0, source: 0, excl: 1, count: 0 }, Instr::Ret { src: 0 }],
            1,
        ));
        assert!(r.is_ok(), "{r:?}");
    }
}

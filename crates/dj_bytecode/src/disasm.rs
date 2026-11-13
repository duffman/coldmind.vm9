//! Disassembler (`djungle disasm file.ts`).

use std::fmt::Write;

use crate::debug::VarLocation;
use crate::instr::{decode_all, OperandKind};
use crate::module::*;

pub fn disassemble(m: &Module) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "; djungle bytecode v{} — {} constants, {} functions, entry f{}{}",
        crate::format::VERSION,
        m.constants.len(),
        m.functions.len(),
        m.entry,
        if m.debug.is_some() { ", debug info" } else { "" }
    );
    out.push_str("constants:\n");
    for (i, c) in m.constants.iter().enumerate() {
        let _ = writeln!(out, "  k{i:<4} {}", const_text(c));
    }
    for fi in 0..m.functions.len() {
        out.push('\n');
        disassemble_function_into(&mut out, m, fi as u32);
    }
    out
}

fn const_text(c: &Constant) -> String {
    match c {
        Constant::Number(n) => {
            if *n == 0.0 && n.is_sign_negative() {
                "-0".into()
            } else {
                format!("{n}")
            }
        }
        Constant::String(s) => format!("{s:?}"),
    }
}

pub fn disassemble_function(m: &Module, fi: u32) -> String {
    let mut out = String::new();
    disassemble_function_into(&mut out, m, fi);
    out
}

fn disassemble_function_into(out: &mut String, m: &Module, fi: u32) {
    let f = &m.functions[fi as usize];
    let fd = m.debug.as_ref().and_then(|d| d.functions.get(fi as usize));
    let _ = write!(
        out,
        "f{fi} <{}> {} params={}{} regs={}",
        m.function_name(fi),
        f.kind.as_str(),
        f.param_count,
        if f.has_rest { "+rest" } else { "" },
        f.register_count
    );
    if f.is_async {
        out.push_str(" async");
    }
    if let Some(fd) = fd {
        let _ = write!(out, " @{}-{}", fd.start, fd.end);
    }
    out.push('\n');
    for (i, u) in f.upvalues.iter().enumerate() {
        let src = if u.from_parent_register { format!("parent r{}", u.index) } else { format!("parent u{}", u.index) };
        let _ = writeln!(out, "  upvalue u{i} <- {src}");
    }
    let instrs = match decode_all(&f.code) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(out, "  <undecodable: {e}>");
            return;
        }
    };
    for (off, instr) in instrs {
        let ops: Vec<String> = instr
            .operands()
            .into_iter()
            .map(|(_, kind, v)| match kind {
                OperandKind::Reg => format!("r{v}"),
                OperandKind::Const | OperandKind::Name => match m.constants.get(v as usize) {
                    Some(c) => format!("k{v}({})", const_text(c)),
                    None => format!("k{v}(?)"),
                },
                OperandKind::Func => format!("f{v}<{}>", m.function_name(v)),
                OperandKind::Upval => format!("u{v}"),
                OperandKind::Target => format!("->{v:04x}"),
                OperandKind::Imm => format!("#{}", v as i32),
                OperandKind::Count => format!("x{v}"),
                OperandKind::U8 => format!("{v}"),
            })
            .collect();
        let text = format!("{:<16}{}", instr.name(), ops.join(", "));
        let mut note = String::new();
        if let Some(fd) = fd {
            if let Some(e) = fd.lines.iter().find(|e| e.pc == off) {
                let _ = write!(note, "{}{}", e.pos, if e.is_stmt { " stmt" } else { "" });
            }
        }
        if note.is_empty() {
            let _ = writeln!(out, "  {off:04x}  {text}");
        } else {
            let _ = writeln!(out, "  {off:04x}  {text:<56} ; {note}");
        }
    }
    for h in &f.handlers {
        let _ = writeln!(out, "  handler [{:04x}, {:04x}) -> {:04x}", h.start, h.end, h.target);
    }
    if let Some(fd) = fd {
        for v in &fd.variables {
            let loc = match v.location {
                VarLocation::Register(r) => format!("r{r}"),
                VarLocation::Cell(r) => format!("cell r{r}"),
                VarLocation::Upvalue(u) => format!("u{u}"),
            };
            let ranges: Vec<String> = v.ranges.iter().map(|(s, e)| format!("[{s:04x},{e:04x})")).collect();
            let _ = writeln!(out, "  var {} {} {} decl {} live {}", v.name, v.kind.as_str(), loc, v.decl, ranges.join(" "));
        }
    }
}

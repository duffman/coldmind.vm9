//! Binary file format (`.djbc`), version 1. All integers are ULEB128 unless
//! noted; strings are `uleb len` + UTF-8 bytes.
//!
//! ```text
//! file      := "DJBC" u16le(version=1) u16le(flags: bit0 = has debug section)
//!              constants functions uleb(entry) [debug]
//! constants := uleb(n) { u8 tag (0 = f64le, 1 = string) payload }*
//! functions := uleb(n) { uleb(name) u8(kind) u8(flags: bit0 async, bit1 rest)
//!                        uleb(params) uleb(registers)
//!                        uleb(n) { u8(from_parent_register) uleb(index) }*
//!                        uleb(len) code-bytes
//!                        uleb(n) { uleb(start) uleb(end) uleb(target) }* }*
//! debug     := str(source_name) u8(has_source) [str(source)]
//!              { per function, same order as the function table:
//!                pos(start) pos(end)
//!                uleb(n) { uleb(pc) pos u8(is_stmt) }*                   line table
//!                uleb(n) { uleb(parent+1) u8(kind) pos(start) pos(end) }* scopes
//!                uleb(n) { uleb(pc) uleb(scope) }*                        scope map
//!                uleb(n) { str(name) u8(kind) uleb(scope+1) u8(loc kind)
//!                          uleb(loc index) pos(decl)
//!                          uleb(n) { uleb(start) uleb(end) }* }*          variables
//!                uleb(n) { uleb(pc) uleb(block) uleb(inst+1) }* }         IR map
//! pos       := uleb(line) uleb(col)
//! ```
//!
//! Serialization is deterministic: the same `Module` always yields the same
//! bytes, and `from_bytes(to_bytes(m)) == m`.

use crate::debug::*;
use crate::leb::{read_uleb, write_uleb};
use crate::module::*;

pub const MAGIC: &[u8; 4] = b"DJBC";
pub const VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError {
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid bytecode file at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for FormatError {}

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v)
    }
    fn uleb(&mut self, v: u32) {
        write_uleb(&mut self.0, v)
    }
    fn len(&mut self, v: usize) {
        self.uleb(u32::try_from(v).expect("section too large"))
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
    fn pos(&mut self, p: Pos) {
        self.uleb(p.line);
        self.uleb(p.col);
    }
}

impl Module {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.0.extend_from_slice(&VERSION.to_le_bytes());
        w.0.extend_from_slice(&(self.debug.is_some() as u16).to_le_bytes());

        w.len(self.constants.len());
        for c in &self.constants {
            match c {
                Constant::Number(n) => {
                    w.u8(0);
                    w.0.extend_from_slice(&n.to_bits().to_le_bytes());
                }
                Constant::String(s) => {
                    w.u8(1);
                    w.str(s);
                }
            }
        }

        w.len(self.functions.len());
        for f in &self.functions {
            w.uleb(f.name);
            w.u8(f.kind as u8);
            w.u8(f.is_async as u8 | (f.has_rest as u8) << 1);
            w.uleb(f.param_count);
            w.uleb(f.register_count);
            w.len(f.upvalues.len());
            for u in &f.upvalues {
                w.u8(u.from_parent_register as u8);
                w.uleb(u.index);
            }
            w.len(f.code.len());
            w.0.extend_from_slice(&f.code);
            w.len(f.handlers.len());
            for h in &f.handlers {
                w.uleb(h.start);
                w.uleb(h.end);
                w.uleb(h.target);
            }
        }
        w.uleb(self.entry);

        if let Some(d) = &self.debug {
            w.str(&d.source_name);
            match &d.source {
                Some(s) => {
                    w.u8(1);
                    w.str(s);
                }
                None => w.u8(0),
            }
            for fd in &d.functions {
                w.pos(fd.start);
                w.pos(fd.end);
                w.len(fd.lines.len());
                for e in &fd.lines {
                    w.uleb(e.pc);
                    w.pos(e.pos);
                    w.u8(e.is_stmt as u8);
                }
                w.len(fd.scopes.len());
                for s in &fd.scopes {
                    w.uleb(s.parent.map_or(0, |p| p + 1));
                    w.u8(s.kind as u8);
                    w.pos(s.start);
                    w.pos(s.end);
                }
                w.len(fd.scope_map.len());
                for &(pc, s) in &fd.scope_map {
                    w.uleb(pc);
                    w.uleb(s);
                }
                w.len(fd.variables.len());
                for v in &fd.variables {
                    w.str(&v.name);
                    w.u8(v.kind as u8);
                    w.uleb(v.scope.map_or(0, |s| s + 1));
                    let (lk, li) = match v.location {
                        VarLocation::Register(r) => (0, r),
                        VarLocation::Cell(r) => (1, r),
                        VarLocation::Upvalue(u) => (2, u),
                    };
                    w.u8(lk);
                    w.uleb(li);
                    w.pos(v.decl);
                    w.len(v.ranges.len());
                    for &(s, e) in &v.ranges {
                        w.uleb(s);
                        w.uleb(e);
                    }
                }
                w.len(fd.ir_map.len());
                for p in &fd.ir_map {
                    w.uleb(p.pc);
                    w.uleb(p.block);
                    w.uleb(p.inst.wrapping_add(1));
                }
            }
        }
        w.0
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Module, FormatError> {
        let mut r = R { b: bytes, p: 0 };
        if r.take(4)? != MAGIC {
            return Err(r.err("bad magic"));
        }
        let version = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
        if version != VERSION {
            return Err(r.err(&format!("unsupported version {version}")));
        }
        let flags = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
        if flags & !1 != 0 {
            return Err(r.err("unknown flags"));
        }

        let n = r.count()?;
        let mut constants = Vec::with_capacity(n);
        for _ in 0..n {
            constants.push(match r.u8()? {
                0 => Constant::Number(f64::from_bits(u64::from_le_bytes(r.take(8)?.try_into().unwrap()))),
                1 => Constant::String(r.str()?),
                t => return Err(r.err(&format!("bad constant tag {t}"))),
            });
        }

        let n = r.count()?;
        let mut functions = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.uleb()?;
            let kind = FunctionKind::from_u8(r.u8()?).ok_or_else(|| r.err("bad function kind"))?;
            let fl = r.u8()?;
            if fl & !3 != 0 {
                return Err(r.err("bad function flags"));
            }
            let param_count = r.uleb()?;
            let register_count = r.uleb()?;
            let nu = r.count()?;
            let mut upvalues = Vec::with_capacity(nu);
            for _ in 0..nu {
                let from_parent_register = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(r.err("bad upvalue flag")),
                };
                upvalues.push(UpvalueDesc { from_parent_register, index: r.uleb()? });
            }
            let len = r.count()?;
            let code = r.take(len)?.to_vec();
            let nh = r.count()?;
            let mut handlers = Vec::with_capacity(nh);
            for _ in 0..nh {
                handlers.push(Handler { start: r.uleb()?, end: r.uleb()?, target: r.uleb()? });
            }
            functions.push(Function {
                name,
                kind,
                is_async: fl & 1 != 0,
                param_count,
                has_rest: fl & 2 != 0,
                register_count,
                upvalues,
                code,
                handlers,
            });
        }
        let entry = r.uleb()?;

        let debug = if flags & 1 != 0 {
            let source_name = r.str()?;
            let source = match r.u8()? {
                0 => None,
                1 => Some(r.str()?),
                _ => return Err(r.err("bad has_source flag")),
            };
            let mut fds = Vec::with_capacity(functions.len());
            for _ in 0..functions.len() {
                let start = r.pos()?;
                let end = r.pos()?;
                let n = r.count()?;
                let mut lines = Vec::with_capacity(n);
                for _ in 0..n {
                    let pc = r.uleb()?;
                    let pos = r.pos()?;
                    let is_stmt = r.u8()? != 0;
                    lines.push(LineEntry { pc, pos, is_stmt });
                }
                let n = r.count()?;
                let mut scopes = Vec::with_capacity(n);
                for _ in 0..n {
                    let parent = r.uleb()?.checked_sub(1);
                    let kind = ScopeKind::from_u8(r.u8()?).ok_or_else(|| r.err("bad scope kind"))?;
                    scopes.push(ScopeDebug { parent, kind, start: r.pos()?, end: r.pos()? });
                }
                let n = r.count()?;
                let mut scope_map = Vec::with_capacity(n);
                for _ in 0..n {
                    scope_map.push((r.uleb()?, r.uleb()?));
                }
                let n = r.count()?;
                let mut variables = Vec::with_capacity(n);
                for _ in 0..n {
                    let name = r.str()?;
                    let kind = VarKind::from_u8(r.u8()?).ok_or_else(|| r.err("bad variable kind"))?;
                    let scope = r.uleb()?.checked_sub(1);
                    let lk = r.u8()?;
                    let li = r.uleb()?;
                    let location = match lk {
                        0 => VarLocation::Register(li),
                        1 => VarLocation::Cell(li),
                        2 => VarLocation::Upvalue(li),
                        _ => return Err(r.err("bad variable location")),
                    };
                    let decl = r.pos()?;
                    let nr = r.count()?;
                    let mut ranges = Vec::with_capacity(nr);
                    for _ in 0..nr {
                        ranges.push((r.uleb()?, r.uleb()?));
                    }
                    variables.push(VarDebug { name, kind, scope, location, decl, ranges });
                }
                let n = r.count()?;
                let mut ir_map = Vec::with_capacity(n);
                for _ in 0..n {
                    let pc = r.uleb()?;
                    let block = r.uleb()?;
                    let inst = r.uleb()?.wrapping_sub(1);
                    ir_map.push(IrPos { pc, block, inst });
                }
                fds.push(FunctionDebug { start, end, lines, scopes, scope_map, variables, ir_map });
            }
            Some(DebugInfo { source_name, source, functions: fds })
        } else {
            None
        };
        if r.p != bytes.len() {
            return Err(r.err("trailing bytes"));
        }
        Ok(Module { constants, functions, entry, debug })
    }
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl R<'_> {
    fn err(&self, m: &str) -> FormatError {
        FormatError { offset: self.p, message: m.to_string() }
    }
    fn take(&mut self, n: usize) -> Result<&[u8], FormatError> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| self.err("unexpected end of file"))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }
    fn uleb(&mut self) -> Result<u32, FormatError> {
        read_uleb(self.b, &mut self.p).ok_or_else(|| self.err("bad LEB128 integer"))
    }
    /// A length that must not exceed the remaining input (guards allocation).
    fn count(&mut self) -> Result<usize, FormatError> {
        let n = self.uleb()? as usize;
        if n > self.b.len() - self.p {
            return Err(self.err("count exceeds file size"));
        }
        Ok(n)
    }
    fn str(&mut self) -> Result<String, FormatError> {
        let n = self.count()?;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| self.err("invalid UTF-8"))
    }
    fn pos(&mut self) -> Result<Pos, FormatError> {
        Ok(Pos { line: self.uleb()?, col: self.uleb()? })
    }
}

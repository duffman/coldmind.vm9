//! Loading validated bytecode into executable form.
//!
//! The loader runs [`dj_bytecode::validate`], then pre-decodes each function
//! into a vector of [`Instr`] whose jump targets are *instruction indices*.
//! The VM executes only this form; byte offsets remain available for the
//! debugger and stack traces via `offsets`.

use std::rc::Rc;

use dj_bytecode::{self as bc, instr::decode_all, Instr, OperandKind};

use crate::value::{JsStr, Value};

pub struct LoadedFunction {
    pub index: u32,
    pub name: JsStr,
    pub kind: bc::FunctionKind,
    pub is_async: bool,
    pub param_count: u32,
    pub has_rest: bool,
    pub register_count: u32,
    pub upvalues: Vec<bc::UpvalueDesc>,
    pub code: Vec<Instr>,
    /// Instruction index → bytecode byte offset.
    pub offsets: Vec<u32>,
    /// `(start, end, target)` in instruction indices.
    pub handlers: Vec<(u32, u32, u32)>,
    /// Instruction index starts a statement (breakpoint location).
    pub stmt: Vec<bool>,
}

impl LoadedFunction {
    /// Instruction index of a byte offset.
    pub fn index_of_offset(&self, off: u32) -> Option<usize> {
        self.offsets.binary_search(&off).ok()
    }

    pub fn handler_for(&self, at: usize) -> Option<usize> {
        let at = at as u32;
        self.handlers.iter().find(|h| h.0 <= at && at < h.1).map(|h| h.2 as usize)
    }
}

pub struct LoadedModule {
    pub id: u32,
    pub bytecode: bc::Module,
    /// Constant pool as runtime values.
    pub constants: Vec<Value>,
    pub functions: Vec<Rc<LoadedFunction>>,
}

impl LoadedModule {
    pub fn name(&self, k: u32) -> JsStr {
        match &self.constants[k as usize] {
            Value::String(s) => s.clone(),
            _ => unreachable!("validated: name operand is a string"),
        }
    }

    pub fn debug(&self, func: u32) -> Option<&bc::FunctionDebug> {
        self.bytecode.debug.as_ref().and_then(|d| d.functions.get(func as usize))
    }

    pub fn source_name(&self) -> &str {
        self.bytecode.debug.as_ref().map_or("<unknown>", |d| d.source_name.as_str())
    }
}

#[derive(Debug)]
pub enum LoadError {
    Invalid(Vec<bc::ValidationError>),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Invalid(errs) => {
                writeln!(f, "bytecode failed validation:")?;
                for e in errs {
                    writeln!(f, "  {e}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LoadError {}

pub fn load(m: bc::Module, id: u32) -> Result<LoadedModule, LoadError> {
    bc::validate(&m).map_err(LoadError::Invalid)?;
    let constants = m
        .constants
        .iter()
        .map(|c| match c {
            bc::Constant::Number(n) => Value::Number(*n),
            bc::Constant::String(s) => Value::String(JsStr::new(s)),
        })
        .collect();
    let mut functions = Vec::with_capacity(m.functions.len());
    for (fi, f) in m.functions.iter().enumerate() {
        let decoded = decode_all(&f.code).expect("validated");
        let offsets: Vec<u32> = decoded.iter().map(|(o, _)| *o).collect();
        let idx = |off: u32| offsets.binary_search(&off).expect("validated target") as u32;
        let code: Vec<Instr> = decoded
            .into_iter()
            .map(|(_, mut i)| {
                i.operands_mut(|k, v| {
                    if k == OperandKind::Target {
                        *v = idx(*v);
                    }
                });
                i
            })
            .collect();
        let end_idx = |off: u32| if off as usize == f.code.len() { code.len() as u32 } else { idx(off) };
        let handlers = f.handlers.iter().map(|h| (idx(h.start), end_idx(h.end), idx(h.target))).collect();
        let mut stmt = vec![false; code.len()];
        if let Some(d) = m.debug.as_ref().and_then(|d| d.functions.get(fi)) {
            for e in d.stmt_starts() {
                stmt[idx(e.pc) as usize] = true;
            }
        }
        let name = match &m.constants[f.name as usize] {
            bc::Constant::String(s) => JsStr::new(s),
            _ => unreachable!(),
        };
        functions.push(Rc::new(LoadedFunction {
            index: fi as u32,
            name,
            kind: f.kind,
            is_async: f.is_async,
            param_count: f.param_count,
            has_rest: f.has_rest,
            register_count: f.register_count,
            upvalues: f.upvalues.clone(),
            code,
            offsets,
            handlers,
            stmt,
        }));
    }
    Ok(LoadedModule { id, bytecode: m, constants, functions })
}

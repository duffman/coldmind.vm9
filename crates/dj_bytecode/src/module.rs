//! Module container.

use crate::debug::DebugInfo;

#[derive(Clone, Debug)]
pub enum Constant {
    Number(f64),
    String(String),
}

impl PartialEq for Constant {
    /// Bitwise equality for numbers (NaN == NaN, 0 != -0): constant-pool identity.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
            (Constant::String(a), Constant::String(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FunctionKind {
    Script = 0,
    Normal = 1,
    Arrow = 2,
    Method = 3,
    Getter = 4,
    Setter = 5,
    ClassConstructor = 6,
    DerivedConstructor = 7,
    FieldInit = 8,
}

impl FunctionKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Script,
            1 => Self::Normal,
            2 => Self::Arrow,
            3 => Self::Method,
            4 => Self::Getter,
            5 => Self::Setter,
            6 => Self::ClassConstructor,
            7 => Self::DerivedConstructor,
            8 => Self::FieldInit,
            _ => return None,
        })
    }

    /// Can be invoked with `new`.
    pub fn is_constructor(self) -> bool {
        matches!(self, Self::Normal | Self::ClassConstructor | Self::DerivedConstructor)
    }

    /// `this` is taken lexically (arrows); calls ignore the receiver.
    pub fn is_arrow(self) -> bool {
        self == Self::Arrow
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Normal => "normal",
            Self::Arrow => "arrow",
            Self::Method => "method",
            Self::Getter => "getter",
            Self::Setter => "setter",
            Self::ClassConstructor => "class-ctor",
            Self::DerivedConstructor => "derived-ctor",
            Self::FieldInit => "field-init",
        }
    }
}

/// Where a closure's upvalue comes from when the closure is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpvalueDesc {
    /// `true`: the creating frame's register `index` holds the cell.
    /// `false`: the creating closure's own upvalue `index`.
    pub from_parent_register: bool,
    pub index: u32,
}

/// Instructions in `[start, end)` are protected; a throw transfers control to
/// `target` (whose first instruction is `GetException`). The innermost
/// (first listed) matching entry wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Constant-pool index of the name string.
    pub name: u32,
    pub kind: FunctionKind,
    pub is_async: bool,
    pub param_count: u32,
    /// Register `param_count` receives the rest-argument array.
    pub has_rest: bool,
    pub register_count: u32,
    pub upvalues: Vec<UpvalueDesc>,
    pub code: Vec<u8>,
    pub handlers: Vec<Handler>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub constants: Vec<Constant>,
    pub functions: Vec<Function>,
    pub entry: u32,
    /// Optional, strippable debug metadata (one entry per function).
    pub debug: Option<DebugInfo>,
}

impl Module {
    pub fn string(&self, k: u32) -> Option<&str> {
        match self.constants.get(k as usize) {
            Some(Constant::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn function_name(&self, f: u32) -> &str {
        self.functions.get(f as usize).and_then(|func| self.string(func.name)).unwrap_or("?")
    }

    /// Remove debug metadata (release builds of bytecode).
    pub fn strip_debug(&mut self) {
        self.debug = None;
    }
}

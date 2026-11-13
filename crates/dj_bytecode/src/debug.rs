//! Debug metadata model.
//!
//! Everything a debugger needs, keyed by *bytecode offset* (`pc`):
//! * line table: `pc → (line, column, is_stmt)`; `is_stmt` entries are
//!   breakpoint / stepping locations (statement boundaries);
//! * scope tree per function with a `pc → innermost scope` map;
//! * variables: name, kind, declaring scope, storage location (register,
//!   cell-in-register or upvalue) and the pc ranges where the binding is in
//!   scope (live ranges);
//! * IR position map: `pc → (IR block, IR instruction)` linking bytecode back
//!   to the IR the frontend produced.

/// 1-based line / 1-based UTF-16 column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for Pos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineEntry {
    pub pc: u32,
    pub pos: Pos,
    pub is_stmt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ScopeKind {
    Function = 0,
    Block = 1,
    For = 2,
    Switch = 3,
    Catch = 4,
    Class = 5,
}

impl ScopeKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Function,
            1 => Self::Block,
            2 => Self::For,
            3 => Self::Switch,
            4 => Self::Catch,
            5 => Self::Class,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Block => "block",
            Self::For => "for",
            Self::Switch => "switch",
            Self::Catch => "catch",
            Self::Class => "class",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeDebug {
    pub parent: Option<u32>,
    pub kind: ScopeKind,
    pub start: Pos,
    pub end: Pos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VarKind {
    Param = 0,
    Var = 1,
    Let = 2,
    Const = 3,
    Function = 4,
    Class = 5,
    Catch = 6,
    This = 7,
}

impl VarKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Param,
            1 => Self::Var,
            2 => Self::Let,
            3 => Self::Const,
            4 => Self::Function,
            5 => Self::Class,
            6 => Self::Catch,
            7 => Self::This,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Param => "param",
            Self::Var => "var",
            Self::Let => "let",
            Self::Const => "const",
            Self::Function => "function",
            Self::Class => "class",
            Self::Catch => "catch",
            Self::This => "this",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VarLocation {
    /// Value lives directly in a register.
    Register(u32),
    /// The register holds a cell; the value is the cell's content.
    Cell(u32),
    /// Captured from an enclosing function: closure upvalue index.
    Upvalue(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarDebug {
    pub name: String,
    pub kind: VarKind,
    /// Declaring scope; `None` for upvalues (bindings of enclosing functions).
    pub scope: Option<u32>,
    pub location: VarLocation,
    pub decl: Pos,
    /// Half-open pc ranges where the binding is in scope. Upvalues are in
    /// scope for the whole function.
    pub ranges: Vec<(u32, u32)>,
}

impl VarDebug {
    pub fn in_scope_at(&self, pc: u32) -> bool {
        self.ranges.iter().any(|&(s, e)| s <= pc && pc < e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrPos {
    pub pc: u32,
    pub block: u32,
    /// Instruction index in the IR block; `u32::MAX` for the terminator.
    pub inst: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionDebug {
    pub start: Pos,
    pub end: Pos,
    /// Sorted by pc; at most one entry per pc.
    pub lines: Vec<LineEntry>,
    pub scopes: Vec<ScopeDebug>,
    /// `(pc, scope)`: from `pc` onwards the innermost scope is `scope` (sorted).
    pub scope_map: Vec<(u32, u32)>,
    pub variables: Vec<VarDebug>,
    pub ir_map: Vec<IrPos>,
}

impl FunctionDebug {
    /// Source position of the instruction at `pc`.
    pub fn pos_at(&self, pc: u32) -> Option<Pos> {
        let i = self.lines.partition_point(|e| e.pc <= pc);
        if i == 0 { None } else { Some(self.lines[i - 1].pos) }
    }

    /// `pc` begins a statement (breakpoint / step location).
    pub fn is_stmt_start(&self, pc: u32) -> bool {
        self.lines.binary_search_by_key(&pc, |e| e.pc).map(|i| self.lines[i].is_stmt).unwrap_or(false)
    }

    pub fn stmt_starts(&self) -> impl Iterator<Item = &LineEntry> {
        self.lines.iter().filter(|e| e.is_stmt)
    }

    pub fn scope_at(&self, pc: u32) -> Option<u32> {
        let i = self.scope_map.partition_point(|&(p, _)| p <= pc);
        if i == 0 { None } else { Some(self.scope_map[i - 1].1) }
    }

    /// Innermost-first chain of scopes enclosing `pc`.
    pub fn scope_chain(&self, pc: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = self.scope_at(pc);
        while let Some(s) = cur {
            out.push(s);
            cur = self.scopes.get(s as usize).and_then(|sc| sc.parent);
        }
        out
    }

    /// Bindings in scope at `pc`.
    pub fn visible_variables(&self, pc: u32) -> impl Iterator<Item = &VarDebug> {
        self.variables.iter().filter(move |v| v.in_scope_at(pc))
    }

    pub fn ir_pos_at(&self, pc: u32) -> Option<IrPos> {
        self.ir_map.binary_search_by_key(&pc, |p| p.pc).ok().map(|i| self.ir_map[i])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugInfo {
    pub source_name: String,
    pub source: Option<String>,
    /// Parallel to `Module::functions`.
    pub functions: Vec<FunctionDebug>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakpointLocation {
    pub func: u32,
    pub pc: u32,
    pub pos: Pos,
}

impl DebugInfo {
    /// Resolve a requested source position to the first statement boundary
    /// at or after it: smallest `(line, col)` with `line >= line` (and
    /// `col >= col` on the requested line when a column is given). Ties
    /// between functions prefer the innermost (latest-starting) function.
    pub fn resolve_breakpoint(&self, line: u32, col: Option<u32>) -> Option<BreakpointLocation> {
        let mut best: Option<(Pos, std::cmp::Reverse<Pos>, BreakpointLocation)> = None;
        for (fi, f) in self.functions.iter().enumerate() {
            for e in f.stmt_starts() {
                let ok = e.pos.line > line || (e.pos.line == line && col.is_none_or(|c| e.pos.col >= c));
                if !ok {
                    continue;
                }
                let key = (e.pos, std::cmp::Reverse(f.start));
                let loc = BreakpointLocation { func: fi as u32, pc: e.pc, pos: e.pos };
                if best.as_ref().is_none_or(|(p, r, _)| key < (*p, *r)) {
                    best = Some((key.0, key.1, loc));
                }
            }
        }
        best.map(|b| b.2)
    }

    /// All statement boundaries on `line`, for "breakpoint locations" requests.
    pub fn breakpoint_locations(&self, line: u32) -> Vec<BreakpointLocation> {
        let mut out = Vec::new();
        for (fi, f) in self.functions.iter().enumerate() {
            for e in f.stmt_starts().filter(|e| e.pos.line == line) {
                out.push(BreakpointLocation { func: fi as u32, pc: e.pc, pos: e.pos });
            }
        }
        out.sort_by_key(|b| (b.pos, b.func));
        out
    }
}

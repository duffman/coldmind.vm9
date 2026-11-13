//! IR data model.

use std::rc::Rc;

use crate::source::Span;

/// Interned-ish immutable string used for names and string constants.
pub type Atom = Rc<str>;

macro_rules! id_type {
    ($($(#[$m:meta])* $name:ident = $prefix:literal),* $(,)?) => { $(
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);
        impl $name {
            #[inline]
            pub fn index(self) -> usize { self.0 as usize }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    )* };
}

id_type! {
    /// Index into [`Module::functions`].
    FuncId = "f",
    /// Index into [`Function::blocks`].
    BlockId = "bb",
    /// Single-assignment value produced by an instruction.
    Temp = "t",
    /// Index into [`Function::locals`].
    LocalId = "l",
    /// Index into [`Function::upvalues`].
    UpvalId = "u",
    /// Index into [`Function::scopes`].
    ScopeId = "s",
}

// ---------------------------------------------------------------------------
// Module / function
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Module {
    /// Source name (file name) used in diagnostics and stack traces.
    pub name: String,
    /// Full source text; spans index into it.
    pub source: Rc<str>,
    pub functions: Vec<Function>,
    pub entry: FuncId,
}

impl Module {
    pub fn func(&self, id: FuncId) -> &Function {
        &self.functions[id.index()]
    }

    pub fn func_mut(&mut self, id: FuncId) -> &mut Function {
        &mut self.functions[id.index()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FunctionKind {
    /// Top-level program body.
    Script,
    /// `function` declaration / expression.
    Normal,
    /// Arrow function: lexical `this`, not constructible.
    Arrow,
    /// Object / class method (has a home object, not constructible).
    Method,
    Getter,
    Setter,
    /// Constructor of a base class.
    ClassConstructor,
    /// Constructor of a class with `extends`.
    DerivedConstructor,
    /// Synthetic function running class field initializers with `this` bound.
    FieldInit,
}

impl FunctionKind {
    pub fn is_constructor(self) -> bool {
        matches!(self, Self::Normal | Self::ClassConstructor | Self::DerivedConstructor)
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

#[derive(Clone, Debug)]
pub struct Function {
    pub id: FuncId,
    /// Name used for `Function.prototype.name`, stack traces and the debugger.
    pub name: Atom,
    pub kind: FunctionKind,
    pub is_async: bool,
    /// Formal positional parameters are locals `0..param_count`.
    pub param_count: u32,
    /// When set, local `param_count` receives an array of the remaining arguments.
    pub has_rest: bool,
    pub span: Span,
    /// Lexically enclosing function (`None` only for the entry script).
    pub parent: Option<FuncId>,
    /// Scope tree. Scope 0 is the function scope.
    pub scopes: Vec<Scope>,
    pub locals: Vec<Local>,
    pub upvalues: Vec<Upvalue>,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    pub temp_count: u32,
}

impl Function {
    pub fn new(id: FuncId, name: Atom, kind: FunctionKind, span: Span, parent: Option<FuncId>) -> Self {
        Function {
            id,
            name,
            kind,
            is_async: false,
            param_count: 0,
            has_rest: false,
            span,
            parent,
            scopes: vec![Scope { parent: None, kind: ScopeKind::Function, span }],
            locals: Vec::new(),
            upvalues: Vec::new(),
            blocks: vec![Block::new(span, ScopeId(0))],
            entry: BlockId(0),
            temp_count: 0,
        }
    }

    pub fn new_temp(&mut self) -> Temp {
        let t = Temp(self.temp_count);
        self.temp_count += 1;
        t
    }

    pub fn new_block(&mut self, span: Span, scope: ScopeId) -> BlockId {
        self.blocks.push(Block::new(span, scope));
        BlockId(self.blocks.len() as u32 - 1)
    }

    pub fn new_scope(&mut self, parent: ScopeId, kind: ScopeKind, span: Span) -> ScopeId {
        self.scopes.push(Scope { parent: Some(parent), kind, span });
        ScopeId(self.scopes.len() as u32 - 1)
    }

    pub fn new_local(&mut self, local: Local) -> LocalId {
        self.locals.push(local);
        LocalId(self.locals.len() as u32 - 1)
    }

    pub fn new_upvalue(&mut self, up: Upvalue) -> UpvalId {
        self.upvalues.push(up);
        UpvalId(self.upvalues.len() as u32 - 1)
    }

    pub fn block(&self, b: BlockId) -> &Block {
        &self.blocks[b.index()]
    }

    pub fn block_mut(&mut self, b: BlockId) -> &mut Block {
        &mut self.blocks[b.index()]
    }

    /// Normal successors followed by the exception handler (if any).
    pub fn successors(&self, b: BlockId) -> Vec<BlockId> {
        let blk = self.block(b);
        let mut out = blk.term.kind.successors();
        if let Some(h) = blk.handler {
            out.push(h);
        }
        out
    }

    /// Predecessor lists, including exceptional edges.
    pub fn predecessors(&self) -> Vec<Vec<BlockId>> {
        let mut preds = vec![Vec::new(); self.blocks.len()];
        for i in 0..self.blocks.len() {
            for s in self.successors(BlockId(i as u32)) {
                if !preds[s.index()].contains(&BlockId(i as u32)) {
                    preds[s.index()].push(BlockId(i as u32));
                }
            }
        }
        preds
    }

    /// `true` when `inner` is `outer` or nested inside it.
    pub fn scope_within(&self, inner: ScopeId, outer: ScopeId) -> bool {
        let mut cur = Some(inner);
        while let Some(s) = cur {
            if s == outer {
                return true;
            }
            cur = self.scopes[s.index()].parent;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Scopes, locals, upvalues
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Function,
    Block,
    For,
    Switch,
    Catch,
    Class,
}

impl ScopeKind {
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

#[derive(Clone, Debug)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalKind {
    Param,
    Var,
    Let,
    Const,
    Function,
    Class,
    Catch,
    /// The function's `this` binding (captured by arrows).
    This,
    /// The function's `new.target` binding (captured by arrows).
    NewTarget,
    /// Compiler-introduced storage (join values, iterators, completions).
    Synthetic,
}

impl LocalKind {
    /// Bindings with a temporal dead zone.
    pub fn is_lexical(self) -> bool {
        matches!(self, Self::Let | Self::Const | Self::Class)
    }

    /// Shown by the debugger.
    pub fn is_user_visible(self) -> bool {
        !matches!(self, Self::Synthetic | Self::NewTarget)
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
            Self::NewTarget => "new.target",
            Self::Synthetic => "synthetic",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: Atom,
    pub kind: LocalKind,
    /// Scope that declares the binding; it is visible wherever that scope is.
    pub scope: ScopeId,
    /// Referenced by a nested function: storage is a heap cell.
    pub captured: bool,
    /// Some read may observe the binding before initialization.
    pub needs_tdz: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureSource {
    /// A local of the immediately enclosing function.
    ParentLocal(LocalId),
    /// An upvalue of the immediately enclosing function.
    ParentUpval(UpvalId),
}

#[derive(Clone, Debug)]
pub struct Upvalue {
    pub name: Atom,
    pub kind: LocalKind,
    pub needs_tdz: bool,
    pub source: CaptureSource,
}

// ---------------------------------------------------------------------------
// Values and operators
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Atom),
}

impl Constant {
    pub fn truthy(&self) -> bool {
        match self {
            Constant::Undefined | Constant::Null => false,
            Constant::Bool(b) => *b,
            Constant::Number(n) => !(*n == 0.0 || n.is_nan()),
            Constant::String(s) => !s.is_empty(),
        }
    }

    pub fn type_of(&self) -> &'static str {
        match self {
            Constant::Undefined => "undefined",
            Constant::Null => "object",
            Constant::Bool(_) => "boolean",
            Constant::Number(_) => "number",
            Constant::String(_) => "string",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `+x` (ToNumber)
    Plus,
    /// `~x`
    BitNot,
    /// `!x`
    Not,
    /// `typeof x`
    TypeOf,
    /// ToString (template literals)
    ToString,
    /// ToPropertyKey (computed keys evaluated once)
    ToPropertyKey,
    /// ToNumber(x) + 1
    Inc,
    /// ToNumber(x) - 1
    Dec,
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Neg => "neg",
            Self::Plus => "tonumber",
            Self::BitNot => "bitnot",
            Self::Not => "not",
            Self::TypeOf => "typeof",
            Self::ToString => "tostring",
            Self::ToPropertyKey => "topropertykey",
            Self::Inc => "inc",
            Self::Dec => "dec",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
    InstanceOf,
    In,
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Mod => "mod",
            Self::Exp => "exp",
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::UShr => "ushr",
            Self::BitAnd => "bitand",
            Self::BitOr => "bitor",
            Self::BitXor => "bitxor",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::StrictEq => "stricteq",
            Self::StrictNe => "strictne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::InstanceOf => "instanceof",
            Self::In => "in",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MethodKind {
    Method,
    Getter,
    Setter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    TypeError,
    ReferenceError,
    SyntaxError,
    RangeError,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TypeError => "TypeError",
            Self::ReferenceError => "ReferenceError",
            Self::SyntaxError => "SyntaxError",
            Self::RangeError => "RangeError",
        }
    }
}

// ---------------------------------------------------------------------------
// Instructions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum InstKind {
    /// Statement boundary: a breakpoint / stepping location. Never removed by
    /// optimization passes (except together with its unreachable block).
    Stmt,
    /// `debugger;`
    Debugger,

    Const { dst: Temp, value: Constant },

    /// Read a local. `check_tdz` throws a ReferenceError on an uninitialized binding.
    LoadLocal { dst: Temp, local: LocalId, check_tdz: bool },
    /// Write (and initialize) a local.
    StoreLocal { local: LocalId, src: Temp },
    /// Start a fresh binding: lexical locals enter their TDZ; captured locals
    /// get a new cell (per-iteration bindings in loops).
    DeclareLocal { local: LocalId },
    /// `for (let ...)` per-iteration copy: new cell holding the current value.
    CopyBinding { local: LocalId },
    LoadUpval { dst: Temp, upval: UpvalId, check_tdz: bool },
    StoreUpval { upval: UpvalId, src: Temp },

    /// Global object property read; ReferenceError when absent.
    LoadGlobal { dst: Temp, name: Atom },
    StoreGlobal { name: Atom, src: Temp },
    /// `typeof name` for an unresolved identifier (never throws).
    TypeofGlobal { dst: Temp, name: Atom },

    /// Raw `this` of the current activation.
    LoadThis { dst: Temp },
    LoadNewTarget { dst: Temp },
    /// The function object being executed (named function expressions).
    LoadCallee { dst: Temp },

    Unary { dst: Temp, op: UnaryOp, src: Temp },
    Binary { dst: Temp, op: BinaryOp, lhs: Temp, rhs: Temp },

    GetProp { dst: Temp, obj: Temp, key: Temp },
    GetNamed { dst: Temp, obj: Temp, name: Atom },
    SetProp { obj: Temp, key: Temp, value: Temp },
    SetNamed { obj: Temp, name: Atom, value: Temp },
    DeleteProp { dst: Temp, obj: Temp, key: Temp },

    NewObject { dst: Temp },
    NewArray { dst: Temp },
    ArrayPush { array: Temp, value: Temp },
    /// Append every value produced by iterating `iterable`.
    ArraySpread { array: Temp, iterable: Temp },
    /// Append an elision (`[1, , 3]`).
    ArrayHole { array: Temp },
    /// CreateDataProperty (object literals, class fields).
    DefineField { obj: Temp, key: Temp, value: Temp },
    DefineNamed { obj: Temp, name: Atom, value: Temp },
    /// Define a method / accessor and set the function's home object.
    DefineMethod { obj: Temp, key: Temp, func: Temp, kind: MethodKind, enumerable: bool },
    /// Copy own enumerable properties of `source` onto `target`, skipping
    /// the keys in `excluded` (object spread / object rest).
    CopyDataProps { target: Temp, source: Temp, excluded: Vec<Temp> },

    MakeClosure { dst: Temp, func: FuncId },
    /// Turn closure `ctor` into a class constructor with a fresh prototype
    /// object inheriting from `parent.prototype` (or `Object.prototype`).
    MakeClass { dst: Temp, ctor: Temp, parent: Option<Temp> },
    /// Attach a field-initializer function to class constructor `class`.
    SetFieldInit { class: Temp, init: Temp },
    /// Run the current class constructor's field initializer on `this`
    /// (base classes; derived classes run it inside `SuperCall`).
    InitFields,

    Call { dst: Temp, callee: Temp, this: Option<Temp>, args: Vec<Temp> },
    /// `args` is an array holding the argument list.
    CallSpread { dst: Temp, callee: Temp, this: Option<Temp>, args: Temp },
    New { dst: Temp, callee: Temp, args: Vec<Temp> },
    NewSpread { dst: Temp, callee: Temp, args: Temp },
    /// `super(...)`: constructs the parent, binds `this`, runs field initializers.
    SuperCall { dst: Temp, args: Vec<Temp> },
    SuperCallSpread { dst: Temp, args: Temp },
    /// `super[key]` with the current `this` as receiver.
    SuperGet { dst: Temp, key: Temp },
    SuperSet { key: Temp, value: Temp },

    GetIterator { dst: Temp, iterable: Temp },
    /// Advance an iterator: `done` receives a boolean, `value` the value.
    IterNext { done: Temp, value: Temp, iter: Temp },
    /// IteratorClose (calls `return` when present).
    IterClose { iter: Temp },
    /// Iterator over enumerable string keys (`for ... in`).
    ForInIterator { dst: Temp, obj: Temp },

    /// Suspend the async function until `value` settles.
    Await { dst: Temp, value: Temp },

    /// First instruction of an exception handler block: the thrown value.
    CatchValue { dst: Temp },
}

/// Shared operand visitor. Works for `&InstKind` (yielding `&Temp`) and
/// `&mut InstKind` (yielding `&mut Temp`) thanks to match ergonomics.
macro_rules! visit_inst_operands {
    ($kind:expr, $def:expr, $use_:expr) => {{
        #[allow(unused_mut)]
        let mut d = $def;
        #[allow(unused_mut)]
        let mut u = $use_;
        match $kind {
            InstKind::Stmt | InstKind::Debugger | InstKind::InitFields => {}
            InstKind::Const { dst, .. } => d(dst),
            InstKind::LoadLocal { dst, .. } => d(dst),
            InstKind::StoreLocal { src, .. } => u(src),
            InstKind::DeclareLocal { .. } | InstKind::CopyBinding { .. } => {}
            InstKind::LoadUpval { dst, .. } => d(dst),
            InstKind::StoreUpval { src, .. } => u(src),
            InstKind::LoadGlobal { dst, .. } => d(dst),
            InstKind::StoreGlobal { src, .. } => u(src),
            InstKind::TypeofGlobal { dst, .. } => d(dst),
            InstKind::LoadThis { dst } | InstKind::LoadNewTarget { dst } | InstKind::LoadCallee { dst } => d(dst),
            InstKind::Unary { dst, src, .. } => {
                u(src);
                d(dst)
            }
            InstKind::Binary { dst, lhs, rhs, .. } => {
                u(lhs);
                u(rhs);
                d(dst)
            }
            InstKind::GetProp { dst, obj, key } => {
                u(obj);
                u(key);
                d(dst)
            }
            InstKind::GetNamed { dst, obj, .. } => {
                u(obj);
                d(dst)
            }
            InstKind::SetProp { obj, key, value } => {
                u(obj);
                u(key);
                u(value)
            }
            InstKind::SetNamed { obj, value, .. } => {
                u(obj);
                u(value)
            }
            InstKind::DeleteProp { dst, obj, key } => {
                u(obj);
                u(key);
                d(dst)
            }
            InstKind::NewObject { dst } | InstKind::NewArray { dst } => d(dst),
            InstKind::ArrayPush { array, value } => {
                u(array);
                u(value)
            }
            InstKind::ArraySpread { array, iterable } => {
                u(array);
                u(iterable)
            }
            InstKind::ArrayHole { array } => u(array),
            InstKind::DefineField { obj, key, value } => {
                u(obj);
                u(key);
                u(value)
            }
            InstKind::DefineNamed { obj, value, .. } => {
                u(obj);
                u(value)
            }
            InstKind::DefineMethod { obj, key, func, .. } => {
                u(obj);
                u(key);
                u(func)
            }
            InstKind::CopyDataProps { target, source, excluded } => {
                u(target);
                u(source);
                for e in excluded {
                    u(e)
                }
            }
            InstKind::MakeClosure { dst, .. } => d(dst),
            InstKind::MakeClass { dst, ctor, parent } => {
                u(ctor);
                if let Some(p) = parent {
                    u(p)
                }
                d(dst)
            }
            InstKind::SetFieldInit { class, init } => {
                u(class);
                u(init)
            }
            InstKind::Call { dst, callee, this, args } => {
                u(callee);
                if let Some(t) = this {
                    u(t)
                }
                for a in args {
                    u(a)
                }
                d(dst)
            }
            InstKind::CallSpread { dst, callee, this, args } => {
                u(callee);
                if let Some(t) = this {
                    u(t)
                }
                u(args);
                d(dst)
            }
            InstKind::New { dst, callee, args } => {
                u(callee);
                for a in args {
                    u(a)
                }
                d(dst)
            }
            InstKind::NewSpread { dst, callee, args } => {
                u(callee);
                u(args);
                d(dst)
            }
            InstKind::SuperCall { dst, args } => {
                for a in args {
                    u(a)
                }
                d(dst)
            }
            InstKind::SuperCallSpread { dst, args } => {
                u(args);
                d(dst)
            }
            InstKind::SuperGet { dst, key } => {
                u(key);
                d(dst)
            }
            InstKind::SuperSet { key, value } => {
                u(key);
                u(value)
            }
            InstKind::GetIterator { dst, iterable } => {
                u(iterable);
                d(dst)
            }
            InstKind::IterNext { done, value, iter } => {
                u(iter);
                d(done);
                d(value)
            }
            InstKind::IterClose { iter } => u(iter),
            InstKind::ForInIterator { dst, obj } => {
                u(obj);
                d(dst)
            }
            InstKind::Await { dst, value } => {
                u(value);
                d(dst)
            }
            InstKind::CatchValue { dst } => d(dst),
        }
    }};
}

impl InstKind {
    /// Calls `def` for every temp defined and `use_` for every temp read.
    pub fn visit(&self, mut def: impl FnMut(Temp), mut use_: impl FnMut(Temp)) {
        visit_inst_operands!(self, |t: &Temp| def(*t), |t: &Temp| use_(*t))
    }

    pub fn visit_mut(&mut self, mut def: impl FnMut(&mut Temp), mut use_: impl FnMut(&mut Temp)) {
        visit_inst_operands!(self, |t: &mut Temp| def(t), |t: &mut Temp| use_(t))
    }

    pub fn defs(&self) -> Vec<Temp> {
        let mut v = Vec::new();
        self.visit(|t| v.push(t), |_| {});
        v
    }

    pub fn uses(&self) -> Vec<Temp> {
        let mut v = Vec::new();
        self.visit(|_| {}, |t| v.push(t));
        v
    }

    pub fn for_each_use_mut(&mut self, f: impl FnMut(&mut Temp)) {
        self.visit_mut(|_| {}, f)
    }

    /// No observable effect and cannot throw: removable when its result is unused.
    pub fn is_removable_if_unused(&self) -> bool {
        match self {
            InstKind::Const { .. }
            | InstKind::LoadThis { .. }
            | InstKind::LoadNewTarget { .. }
            | InstKind::LoadCallee { .. }
            | InstKind::MakeClosure { .. }
            | InstKind::NewObject { .. }
            | InstKind::NewArray { .. }
            | InstKind::TypeofGlobal { .. } => true,
            InstKind::LoadLocal { check_tdz, .. } | InstKind::LoadUpval { check_tdz, .. } => !check_tdz,
            InstKind::Unary { op, .. } => matches!(op, UnaryOp::Not | UnaryOp::TypeOf),
            InstKind::Binary { op, .. } => matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNe),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub kind: InstKind,
    pub span: Span,
    pub scope: ScopeId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TermKind {
    Jump(BlockId),
    Branch { cond: Temp, then_bb: BlockId, else_bb: BlockId },
    Return(Temp),
    Throw(Temp),
    /// Throw a freshly created error of the given kind (e.g. assignment to const).
    ThrowError { kind: ErrorKind, message: Atom },
    Unreachable,
}

impl TermKind {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            TermKind::Jump(b) => vec![*b],
            TermKind::Branch { then_bb, else_bb, .. } => {
                if then_bb == else_bb {
                    vec![*then_bb]
                } else {
                    vec![*then_bb, *else_bb]
                }
            }
            _ => Vec::new(),
        }
    }

    pub fn successors_mut(&mut self, mut f: impl FnMut(&mut BlockId)) {
        match self {
            TermKind::Jump(b) => f(b),
            TermKind::Branch { then_bb, else_bb, .. } => {
                f(then_bb);
                f(else_bb)
            }
            _ => {}
        }
    }

    pub fn uses(&self) -> Vec<Temp> {
        match self {
            TermKind::Branch { cond, .. } => vec![*cond],
            TermKind::Return(t) | TermKind::Throw(t) => vec![*t],
            _ => Vec::new(),
        }
    }

    pub fn for_each_use_mut(&mut self, mut f: impl FnMut(&mut Temp)) {
        match self {
            TermKind::Branch { cond, .. } => f(cond),
            TermKind::Return(t) | TermKind::Throw(t) => f(t),
            _ => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Terminator {
    pub kind: TermKind,
    pub span: Span,
    pub scope: ScopeId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Terminator,
    /// Exception handler for every instruction (and the terminator) of this block.
    pub handler: Option<BlockId>,
}

impl Block {
    pub fn new(span: Span, scope: ScopeId) -> Self {
        Block {
            insts: Vec::new(),
            term: Terminator { kind: TermKind::Unreachable, span, scope },
            handler: None,
        }
    }
}

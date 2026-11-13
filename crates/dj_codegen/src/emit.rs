//! Instruction selection and debug-table construction.

use std::collections::HashMap;

use dj_bytecode::{self as bc, Instr, Pos};
use dj_ir::*;

use crate::regalloc::{FrameLayout, Storage};

#[derive(Default)]
pub struct ConstPool {
    items: Vec<bc::Constant>,
    numbers: HashMap<u64, u32>,
    strings: HashMap<String, u32>,
}

impl ConstPool {
    pub fn number(&mut self, n: f64) -> u32 {
        if let Some(&i) = self.numbers.get(&n.to_bits()) {
            return i;
        }
        let i = self.items.len() as u32;
        self.items.push(bc::Constant::Number(n));
        self.numbers.insert(n.to_bits(), i);
        i
    }

    pub fn string(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.strings.get(s) {
            return i;
        }
        let i = self.items.len() as u32;
        self.items.push(bc::Constant::String(s.to_string()));
        self.strings.insert(s.to_string(), i);
        i
    }

    pub fn into_constants(self) -> Vec<bc::Constant> {
        self.items
    }
}

struct Emitter<'a> {
    f: &'a Function,
    layout: &'a FrameLayout,
    pool: &'a mut ConstPool,
    lines: &'a LineIndex,
    code: Vec<u8>,
    line_table: Vec<bc::LineEntry>,
    scope_map: Vec<(u32, u32)>,
    ir_map: Vec<bc::IrPos>,
    // current debug context
    /// Start of the statement whose first instruction is emitted next.
    pending_stmt: Option<Pos>,
    cur_pos: Pos,
    cur_scope: u32,
    cur_ir: (u32, u32),
    ir_mapped: bool,
    fixups: Vec<(usize, BlockId)>,
}

fn pos(lines: &LineIndex, offset: u32) -> Pos {
    let lc = lines.line_col(offset);
    Pos { line: lc.line, col: lc.col }
}

impl Emitter<'_> {
    fn pc(&self) -> u32 {
        self.code.len() as u32
    }

    fn set_ctx(&mut self, span: Span, scope: ScopeId, ir: (u32, u32)) {
        self.cur_pos = pos(self.lines, span.start);
        self.cur_scope = scope.0;
        self.cur_ir = ir;
        self.ir_mapped = false;
    }

    fn emit(&mut self, i: Instr) {
        let pc = self.pc();
        let stmt = self.pending_stmt.take();
        let is_stmt = stmt.is_some();
        let entry_pos = stmt.unwrap_or(self.cur_pos);
        match self.line_table.last_mut() {
            Some(last) if last.pc == pc => {
                last.pos = entry_pos;
                last.is_stmt |= is_stmt;
            }
            Some(last) if last.pos == entry_pos && !is_stmt => {}
            _ => self.line_table.push(bc::LineEntry { pc, pos: entry_pos, is_stmt }),
        }
        if self.scope_map.last().is_none_or(|&(_, s)| s != self.cur_scope) {
            self.scope_map.push((pc, self.cur_scope));
        }
        if !self.ir_mapped {
            self.ir_map.push(bc::IrPos { pc, block: self.cur_ir.0, inst: self.cur_ir.1 });
            self.ir_mapped = true;
        }
        i.encode(&mut self.code);
    }

    /// Emit a jump-family instruction whose last operand is a target block.
    fn emit_jump(&mut self, i: Instr, to: BlockId) {
        self.emit(i);
        self.fixups.push((self.code.len() - 4, to));
    }

    fn t(&self, t: Temp) -> u32 {
        let r = self.layout.temp_reg[t.index()];
        debug_assert!(r != u32::MAX, "temp {t} without register in {}", self.f.id);
        r
    }

    fn store(&self, l: LocalId) -> Storage {
        self.layout.local_store[l.index()]
    }

    fn name(&mut self, s: &str) -> u32 {
        self.pool.string(s)
    }

    fn local_name(&mut self, l: LocalId) -> u32 {
        let n = self.f.locals[l.index()].name.clone();
        self.name(&n)
    }

    fn load_const(&mut self, dst: u32, c: &Constant) {
        let i = match c {
            Constant::Undefined => Instr::LoadUndef { dst },
            Constant::Null => Instr::LoadNull { dst },
            Constant::Bool(true) => Instr::LoadTrue { dst },
            Constant::Bool(false) => Instr::LoadFalse { dst },
            Constant::Number(n) => {
                let small = n.fract() == 0.0 && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 && !(*n == 0.0 && n.is_sign_negative());
                if small {
                    Instr::LoadInt { dst, value: *n as i32 as u32 }
                } else {
                    let k = self.pool.number(*n);
                    Instr::LoadConst { dst, k }
                }
            }
            Constant::String(s) => {
                let k = self.pool.string(s);
                Instr::LoadConst { dst, k }
            }
        };
        self.emit(i);
    }

    /// Move `regs` into the outgoing window; returns its base.
    fn out_args(&mut self, regs: &[u32]) -> u32 {
        let base = self.layout.out_base;
        for (i, &r) in regs.iter().enumerate() {
            self.emit(Instr::Mov { dst: base + i as u32, src: r });
        }
        base
    }

    fn inst(&mut self, k: &InstKind) {
        use InstKind as K;
        match k {
            K::Stmt => self.pending_stmt = Some(self.cur_pos),
            K::Debugger => self.emit(Instr::Debugger {}),
            K::Const { dst, value } => {
                let d = self.t(*dst);
                self.load_const(d, value)
            }
            K::LoadLocal { dst, local, check_tdz } => {
                let d = self.t(*dst);
                match self.store(*local) {
                    Storage::Reg(r) => {
                        if *check_tdz {
                            let name = self.local_name(*local);
                            self.emit(Instr::CheckTdz { reg: r, name });
                        }
                        if d != r {
                            self.emit(Instr::Mov { dst: d, src: r });
                        }
                    }
                    Storage::Cell(r) => {
                        if *check_tdz {
                            let name = self.local_name(*local);
                            self.emit(Instr::CellGetChecked { dst: d, cell: r, name });
                        } else {
                            self.emit(Instr::CellGet { dst: d, cell: r });
                        }
                    }
                }
            }
            K::StoreLocal { local, src } => {
                let s = self.t(*src);
                match self.store(*local) {
                    Storage::Reg(r) => {
                        if r != s {
                            self.emit(Instr::Mov { dst: r, src: s })
                        }
                    }
                    Storage::Cell(r) => self.emit(Instr::CellSet { cell: r, src: s }),
                }
            }
            K::DeclareLocal { local } => {
                let l = &self.f.locals[local.index()];
                let hole = l.needs_tdz || l.kind.is_lexical();
                match self.store(*local) {
                    Storage::Cell(r) => {
                        if hole {
                            self.emit(Instr::NewCell { dst: r });
                        } else {
                            self.emit(Instr::LoadUndef { dst: r });
                            self.emit(Instr::Box { reg: r });
                        }
                    }
                    Storage::Reg(r) => {
                        if hole {
                            self.emit(Instr::LoadHole { dst: r });
                        }
                    }
                }
            }
            K::CopyBinding { local } => {
                if let Storage::Cell(r) = self.store(*local) {
                    self.emit(Instr::CellCopy { cell: r });
                }
            }
            K::LoadUpval { dst, upval, check_tdz } => {
                let d = self.t(*dst);
                if *check_tdz {
                    let n = self.f.upvalues[upval.index()].name.clone();
                    let name = self.name(&n);
                    self.emit(Instr::UpvalGetChecked { dst: d, idx: upval.0, name });
                } else {
                    self.emit(Instr::UpvalGet { dst: d, idx: upval.0 });
                }
            }
            K::StoreUpval { upval, src } => {
                let s = self.t(*src);
                self.emit(Instr::UpvalSet { idx: upval.0, src: s })
            }
            K::LoadGlobal { dst, name } => {
                let (d, n) = (self.t(*dst), self.name(name));
                self.emit(Instr::GetGlobal { dst: d, name: n })
            }
            K::StoreGlobal { name, src } => {
                let (s, n) = (self.t(*src), self.name(name));
                self.emit(Instr::SetGlobal { name: n, src: s })
            }
            K::TypeofGlobal { dst, name } => {
                let (d, n) = (self.t(*dst), self.name(name));
                self.emit(Instr::TypeofGlobal { dst: d, name: n })
            }
            K::LoadThis { dst } => {
                let d = self.t(*dst);
                self.emit(Instr::LoadThis { dst: d })
            }
            K::LoadNewTarget { dst } => {
                let d = self.t(*dst);
                self.emit(Instr::LoadNewTarget { dst: d })
            }
            K::LoadCallee { dst } => {
                let d = self.t(*dst);
                self.emit(Instr::LoadCallee { dst: d })
            }
            K::Unary { dst, op, src } => {
                let (dst, src) = (self.t(*dst), self.t(*src));
                self.emit(match op {
                    UnaryOp::Neg => Instr::Neg { dst, src },
                    UnaryOp::Plus => Instr::ToNumber { dst, src },
                    UnaryOp::BitNot => Instr::BitNot { dst, src },
                    UnaryOp::Not => Instr::Not { dst, src },
                    UnaryOp::TypeOf => Instr::TypeOf { dst, src },
                    UnaryOp::ToString => Instr::ToStr { dst, src },
                    UnaryOp::ToPropertyKey => Instr::ToPropKey { dst, src },
                    UnaryOp::Inc => Instr::Inc { dst, src },
                    UnaryOp::Dec => Instr::Dec { dst, src },
                })
            }
            K::Binary { dst, op, lhs, rhs } => {
                let (dst, lhs, rhs) = (self.t(*dst), self.t(*lhs), self.t(*rhs));
                self.emit(match op {
                    BinaryOp::Add => Instr::Add { dst, lhs, rhs },
                    BinaryOp::Sub => Instr::Sub { dst, lhs, rhs },
                    BinaryOp::Mul => Instr::Mul { dst, lhs, rhs },
                    BinaryOp::Div => Instr::Div { dst, lhs, rhs },
                    BinaryOp::Mod => Instr::Mod { dst, lhs, rhs },
                    BinaryOp::Exp => Instr::Exp { dst, lhs, rhs },
                    BinaryOp::Shl => Instr::Shl { dst, lhs, rhs },
                    BinaryOp::Shr => Instr::Shr { dst, lhs, rhs },
                    BinaryOp::UShr => Instr::UShr { dst, lhs, rhs },
                    BinaryOp::BitAnd => Instr::BitAnd { dst, lhs, rhs },
                    BinaryOp::BitOr => Instr::BitOr { dst, lhs, rhs },
                    BinaryOp::BitXor => Instr::BitXor { dst, lhs, rhs },
                    BinaryOp::Eq => Instr::Eq { dst, lhs, rhs },
                    BinaryOp::Ne => Instr::Ne { dst, lhs, rhs },
                    BinaryOp::StrictEq => Instr::StrictEq { dst, lhs, rhs },
                    BinaryOp::StrictNe => Instr::StrictNe { dst, lhs, rhs },
                    BinaryOp::Lt => Instr::Lt { dst, lhs, rhs },
                    BinaryOp::Le => Instr::Le { dst, lhs, rhs },
                    BinaryOp::Gt => Instr::Gt { dst, lhs, rhs },
                    BinaryOp::Ge => Instr::Ge { dst, lhs, rhs },
                    BinaryOp::InstanceOf => Instr::InstanceOf { dst, lhs, rhs },
                    BinaryOp::In => Instr::In { dst, lhs, rhs },
                })
            }
            K::GetProp { dst, obj, key } => {
                let i = Instr::GetProp { dst: self.t(*dst), obj: self.t(*obj), key: self.t(*key) };
                self.emit(i)
            }
            K::GetNamed { dst, obj, name } => {
                let i = Instr::GetNamed { dst: self.t(*dst), obj: self.t(*obj), name: self.name(name) };
                self.emit(i)
            }
            K::SetProp { obj, key, value } => {
                let i = Instr::SetProp { obj: self.t(*obj), key: self.t(*key), value: self.t(*value) };
                self.emit(i)
            }
            K::SetNamed { obj, name, value } => {
                let i = Instr::SetNamed { obj: self.t(*obj), name: self.name(name), value: self.t(*value) };
                self.emit(i)
            }
            K::DeleteProp { dst, obj, key } => {
                let i = Instr::DeleteProp { dst: self.t(*dst), obj: self.t(*obj), key: self.t(*key) };
                self.emit(i)
            }
            K::NewObject { dst } => {
                let d = self.t(*dst);
                self.emit(Instr::NewObject { dst: d })
            }
            K::NewArray { dst } => {
                let d = self.t(*dst);
                self.emit(Instr::NewArray { dst: d })
            }
            K::ArrayPush { array, value } => {
                let i = Instr::ArrayPush { array: self.t(*array), value: self.t(*value) };
                self.emit(i)
            }
            K::ArraySpread { array, iterable } => {
                let i = Instr::ArraySpread { array: self.t(*array), iterable: self.t(*iterable) };
                self.emit(i)
            }
            K::ArrayHole { array } => {
                let a = self.t(*array);
                self.emit(Instr::ArrayHole { array: a })
            }
            K::DefineField { obj, key, value } => {
                let i = Instr::DefineField { obj: self.t(*obj), key: self.t(*key), value: self.t(*value) };
                self.emit(i)
            }
            K::DefineNamed { obj, name, value } => {
                let i = Instr::DefineNamed { obj: self.t(*obj), name: self.name(name), value: self.t(*value) };
                self.emit(i)
            }
            K::DefineMethod { obj, key, func, kind, enumerable } => {
                let kind_bits = match kind {
                    MethodKind::Method => 0,
                    MethodKind::Getter => 1,
                    MethodKind::Setter => 2,
                };
                let i = Instr::DefineMethod {
                    obj: self.t(*obj),
                    key: self.t(*key),
                    func: self.t(*func),
                    flags: kind_bits | (*enumerable as u32) << 2,
                };
                self.emit(i)
            }
            K::CopyDataProps { target, source, excluded } => {
                let regs: Vec<u32> = excluded.iter().map(|t| self.t(*t)).collect();
                let excl = self.out_args(&regs);
                let i = Instr::CopyDataProps { target: self.t(*target), source: self.t(*source), excl, count: regs.len() as u32 };
                self.emit(i)
            }
            K::MakeClosure { dst, func } => {
                let d = self.t(*dst);
                self.emit(Instr::Closure { dst: d, func: func.0 })
            }
            K::MakeClass { dst, ctor, parent } => {
                let i = Instr::MakeClass {
                    dst: self.t(*dst),
                    ctor: self.t(*ctor),
                    parent: parent.map(|p| self.t(p)).unwrap_or(0),
                    has_parent: parent.is_some() as u32,
                };
                self.emit(i)
            }
            K::SetFieldInit { class, init } => {
                let i = Instr::SetFieldInit { class: self.t(*class), init: self.t(*init) };
                self.emit(i)
            }
            K::InitFields => self.emit(Instr::InitFields {}),
            K::Call { dst, callee, this, args } => {
                let regs: Vec<u32> = args.iter().map(|t| self.t(*t)).collect();
                let base = self.out_args(&regs);
                let (dst, callee, argc) = (self.t(*dst), self.t(*callee), regs.len() as u32);
                let i = match this {
                    Some(th) => Instr::Call { dst, callee, this: self.t(*th), args: base, argc },
                    None => Instr::CallFn { dst, callee, args: base, argc },
                };
                self.emit(i)
            }
            K::CallSpread { dst, callee, this, args } => {
                let (dst, callee, array) = (self.t(*dst), self.t(*callee), self.t(*args));
                let i = match this {
                    Some(th) => Instr::CallSpread { dst, callee, this: self.t(*th), array },
                    None => Instr::CallFnSpread { dst, callee, array },
                };
                self.emit(i)
            }
            K::New { dst, callee, args } => {
                let regs: Vec<u32> = args.iter().map(|t| self.t(*t)).collect();
                let base = self.out_args(&regs);
                let i = Instr::New { dst: self.t(*dst), callee: self.t(*callee), args: base, argc: regs.len() as u32 };
                self.emit(i)
            }
            K::NewSpread { dst, callee, args } => {
                let i = Instr::NewSpread { dst: self.t(*dst), callee: self.t(*callee), array: self.t(*args) };
                self.emit(i)
            }
            K::SuperCall { dst, args } => {
                let regs: Vec<u32> = args.iter().map(|t| self.t(*t)).collect();
                let base = self.out_args(&regs);
                let i = Instr::SuperCall { dst: self.t(*dst), args: base, argc: regs.len() as u32 };
                self.emit(i)
            }
            K::SuperCallSpread { dst, args } => {
                let i = Instr::SuperCallSpread { dst: self.t(*dst), array: self.t(*args) };
                self.emit(i)
            }
            K::SuperGet { dst, key } => {
                let i = Instr::SuperGet { dst: self.t(*dst), key: self.t(*key) };
                self.emit(i)
            }
            K::SuperSet { key, value } => {
                let i = Instr::SuperSet { key: self.t(*key), value: self.t(*value) };
                self.emit(i)
            }
            K::GetIterator { dst, iterable } => {
                let i = Instr::GetIter { dst: self.t(*dst), iterable: self.t(*iterable) };
                self.emit(i)
            }
            K::IterNext { done, value, iter } => {
                let i = Instr::IterNext { done: self.t(*done), value: self.t(*value), iter: self.t(*iter) };
                self.emit(i)
            }
            K::IterClose { iter } => {
                let i = Instr::IterClose { iter: self.t(*iter) };
                self.emit(i)
            }
            K::ForInIterator { dst, obj } => {
                let i = Instr::ForInIter { dst: self.t(*dst), obj: self.t(*obj) };
                self.emit(i)
            }
            K::Await { dst, value } => {
                let i = Instr::Await { dst: self.t(*dst), value: self.t(*value) };
                self.emit(i)
            }
            K::CatchValue { dst } => {
                let d = self.t(*dst);
                self.emit(Instr::GetException { dst: d })
            }
        }
    }

    fn term(&mut self, k: &TermKind, next: Option<BlockId>) {
        match k {
            TermKind::Jump(b) => {
                if Some(*b) != next {
                    self.emit_jump(Instr::Jmp { target: 0 }, *b);
                }
            }
            TermKind::Branch { cond, then_bb, else_bb } => {
                let c = self.t(*cond);
                if Some(*else_bb) == next {
                    self.emit_jump(Instr::JmpTrue { cond: c, target: 0 }, *then_bb);
                } else if Some(*then_bb) == next {
                    self.emit_jump(Instr::JmpFalse { cond: c, target: 0 }, *else_bb);
                } else {
                    self.emit_jump(Instr::JmpTrue { cond: c, target: 0 }, *then_bb);
                    self.emit_jump(Instr::Jmp { target: 0 }, *else_bb);
                }
            }
            TermKind::Return(t) => {
                let s = self.t(*t);
                self.emit(Instr::Ret { src: s })
            }
            TermKind::Throw(t) => {
                let s = self.t(*t);
                self.emit(Instr::Throw { src: s })
            }
            TermKind::ThrowError { kind, message } => {
                let kind = match kind {
                    ErrorKind::TypeError => 0,
                    ErrorKind::ReferenceError => 1,
                    ErrorKind::SyntaxError => 2,
                    ErrorKind::RangeError => 3,
                };
                let msg = self.name(message);
                self.emit(Instr::ThrowError { kind, msg })
            }
            TermKind::Unreachable => {
                let msg = self.name("internal error: reached unreachable code");
                self.emit(Instr::ThrowError { kind: 0, msg })
            }
        }
    }
}

fn var_kind(k: LocalKind) -> Option<bc::VarKind> {
    Some(match k {
        LocalKind::Param => bc::VarKind::Param,
        LocalKind::Var => bc::VarKind::Var,
        LocalKind::Let => bc::VarKind::Let,
        LocalKind::Const => bc::VarKind::Const,
        LocalKind::Function => bc::VarKind::Function,
        LocalKind::Class => bc::VarKind::Class,
        LocalKind::Catch => bc::VarKind::Catch,
        LocalKind::This => bc::VarKind::This,
        LocalKind::NewTarget | LocalKind::Synthetic => return None,
    })
}

fn scope_kind(k: ScopeKind) -> bc::ScopeKind {
    match k {
        ScopeKind::Function => bc::ScopeKind::Function,
        ScopeKind::Block => bc::ScopeKind::Block,
        ScopeKind::For => bc::ScopeKind::For,
        ScopeKind::Switch => bc::ScopeKind::Switch,
        ScopeKind::Catch => bc::ScopeKind::Catch,
        ScopeKind::Class => bc::ScopeKind::Class,
    }
}

/// Follow an upvalue to the local that originally declares it.
fn origin_local<'m>(m: &'m Module, f: &'m Function, u: UpvalId) -> &'m Local {
    let mut func = f;
    let mut up = u;
    loop {
        let parent = m.func(func.parent.expect("upvalue in parentless function"));
        match func.upvalues[up.index()].source {
            CaptureSource::ParentLocal(l) => return &parent.locals[l.index()],
            CaptureSource::ParentUpval(pu) => {
                func = parent;
                up = pu;
            }
        }
    }
}

pub fn emit_function(
    m: &Module,
    f: &Function,
    layouts: &[FrameLayout],
    pool: &mut ConstPool,
    lines: &LineIndex,
) -> (bc::Function, bc::FunctionDebug) {
    let layout = &layouts[f.id.index()];
    let name = pool.string(&f.name);
    let mut e = Emitter {
        f,
        layout,
        pool,
        lines,
        code: Vec::new(),
        line_table: Vec::new(),
        scope_map: Vec::new(),
        ir_map: Vec::new(),
        pending_stmt: None,
        cur_pos: pos(lines, f.span.start),
        cur_scope: 0,
        cur_ir: (f.entry.0, 0),
        ir_mapped: true, // prologue has no IR counterpart
        fixups: Vec::new(),
    };

    // Prologue: create cells for captured non-lexical bindings.
    let nparams = f.param_count as usize + f.has_rest as usize;
    for (i, l) in f.locals.iter().enumerate() {
        if let Storage::Cell(r) = layout.local_store[i] {
            if i < nparams {
                e.emit(Instr::Box { reg: r });
            } else if l.needs_tdz || l.kind.is_lexical() {
                e.emit(Instr::NewCell { dst: r });
            } else {
                e.emit(Instr::LoadUndef { dst: r });
                e.emit(Instr::Box { reg: r });
            }
        }
    }

    let mut block_pc = vec![u32::MAX; f.blocks.len()];
    let mut ranges: Vec<(u32, u32, BlockId)> = Vec::new();
    for (oi, &b) in layout.order.iter().enumerate() {
        let blk = f.block(b);
        let start = e.pc();
        block_pc[b.index()] = start;
        for (ii, inst) in blk.insts.iter().enumerate() {
            e.set_ctx(inst.span, inst.scope, (b.0, ii as u32));
            e.inst(&inst.kind);
        }
        e.set_ctx(blk.term.span, blk.term.scope, (b.0, u32::MAX));
        e.term(&blk.term.kind, layout.order.get(oi + 1).copied());
        if let Some(h) = blk.handler {
            let end = e.pc();
            if end > start {
                match ranges.last_mut() {
                    Some(last) if last.1 == start && last.2 == h => last.1 = end,
                    _ => ranges.push((start, end, h)),
                }
            }
        }
    }
    for (at, b) in std::mem::take(&mut e.fixups) {
        let target = block_pc[b.index()];
        e.code[at..at + 4].copy_from_slice(&target.to_le_bytes());
    }
    let handlers = ranges
        .into_iter()
        .map(|(start, end, h)| bc::Handler { start, end, target: block_pc[h.index()] })
        .collect();

    let upvalues = f
        .upvalues
        .iter()
        .map(|u| match u.source {
            CaptureSource::ParentLocal(l) => {
                let parent = &layouts[f.parent.unwrap().index()];
                bc::UpvalueDesc { from_parent_register: true, index: parent.local_store[l.index()].reg() }
            }
            CaptureSource::ParentUpval(pu) => bc::UpvalueDesc { from_parent_register: false, index: pu.0 },
        })
        .collect();

    // --- debug tables ----------------------------------------------------------
    let code_len = e.code.len() as u32;
    let scopes = f
        .scopes
        .iter()
        .map(|s| bc::ScopeDebug {
            parent: s.parent.map(|p| p.0),
            kind: scope_kind(s.kind),
            start: pos(lines, s.span.start),
            end: pos(lines, s.span.end),
        })
        .collect();
    let runs: Vec<(u32, u32, u32)> = e
        .scope_map
        .iter()
        .enumerate()
        .map(|(i, &(pc, s))| (pc, e.scope_map.get(i + 1).map_or(code_len, |n| n.0), s))
        .collect();
    let mut variables = Vec::new();
    for (i, l) in f.locals.iter().enumerate() {
        let Some(kind) = var_kind(l.kind) else { continue };
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for &(s, end, scope) in &runs {
            if s < end && f.scope_within(ScopeId(scope), l.scope) {
                match ranges.last_mut() {
                    Some(last) if last.1 == s => last.1 = end,
                    _ => ranges.push((s, end)),
                }
            }
        }
        let location = match layout.local_store[i] {
            Storage::Reg(r) => bc::VarLocation::Register(r),
            Storage::Cell(r) => bc::VarLocation::Cell(r),
        };
        variables.push(bc::VarDebug {
            name: l.name.to_string(),
            kind,
            scope: Some(l.scope.0),
            location,
            decl: pos(lines, l.span.start),
            ranges,
        });
    }
    for (i, u) in f.upvalues.iter().enumerate() {
        let Some(kind) = var_kind(u.kind) else { continue };
        let origin = origin_local(m, f, UpvalId(i as u32));
        variables.push(bc::VarDebug {
            name: u.name.to_string(),
            kind,
            scope: None,
            location: bc::VarLocation::Upvalue(i as u32),
            decl: pos(lines, origin.span.start),
            ranges: if code_len > 0 { vec![(0, code_len)] } else { vec![] },
        });
    }

    let fd = bc::FunctionDebug {
        start: pos(lines, f.span.start),
        end: pos(lines, f.span.end),
        lines: e.line_table,
        scopes,
        scope_map: e.scope_map,
        variables,
        ir_map: e.ir_map,
    };
    let bf = bc::Function {
        name,
        kind: match f.kind {
            FunctionKind::Script => bc::FunctionKind::Script,
            FunctionKind::Normal => bc::FunctionKind::Normal,
            FunctionKind::Arrow => bc::FunctionKind::Arrow,
            FunctionKind::Method => bc::FunctionKind::Method,
            FunctionKind::Getter => bc::FunctionKind::Getter,
            FunctionKind::Setter => bc::FunctionKind::Setter,
            FunctionKind::ClassConstructor => bc::FunctionKind::ClassConstructor,
            FunctionKind::DerivedConstructor => bc::FunctionKind::DerivedConstructor,
            FunctionKind::FieldInit => bc::FunctionKind::FieldInit,
        },
        is_async: f.is_async,
        param_count: f.param_count,
        has_rest: f.has_rest,
        register_count: layout.register_count.max(1),
        upvalues,
        code: e.code,
        handlers,
    };
    (bf, fd)
}

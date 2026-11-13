//! The instruction set.
//!
//! Register machine: every function activation owns `register_count`
//! registers; parameters arrive in registers `0..param_count`. An
//! instruction is a one-byte opcode followed by its operands:
//!
//! | operand kind | encoding                 | meaning                                  |
//! |--------------|--------------------------|------------------------------------------|
//! | `Reg`        | ULEB128                  | register index                           |
//! | `Const`      | ULEB128                  | constant-pool index (any constant)       |
//! | `Name`       | ULEB128                  | constant-pool index of a string          |
//! | `Func`       | ULEB128                  | function-table index                     |
//! | `Upval`      | ULEB128                  | index into the closure's upvalues        |
//! | `Count`      | ULEB128                  | element count                            |
//! | `Imm`        | SLEB128                  | signed 32-bit immediate                  |
//! | `U8`         | 1 byte                   | small enum / flags                       |
//! | `Target`     | 4 bytes little-endian    | absolute byte offset of an instruction   |
//!
//! Jump targets are fixed-width so code can be emitted in one pass and
//! patched; everything else is compact.

use crate::leb;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperandKind {
    Reg,
    Const,
    Name,
    Func,
    Upval,
    Count,
    Imm,
    U8,
    Target,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated { offset: usize },
    BadOpcode { offset: usize, opcode: u8 },
    BadOperand { offset: usize },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Truncated { offset } => write!(f, "truncated instruction at {offset:#06x}"),
            DecodeError::BadOpcode { offset, opcode } => write!(f, "unknown opcode {opcode:#04x} at {offset:#06x}"),
            DecodeError::BadOperand { offset } => write!(f, "malformed operand in instruction at {offset:#06x}"),
        }
    }
}

fn encode_operand(kind: OperandKind, v: u32, out: &mut Vec<u8>) {
    match kind {
        OperandKind::U8 => out.push(v as u8),
        OperandKind::Target => out.extend_from_slice(&v.to_le_bytes()),
        OperandKind::Imm => leb::write_sleb(out, v as i32),
        _ => leb::write_uleb(out, v),
    }
}

fn decode_operand(kind: OperandKind, code: &[u8], pos: &mut usize, at: usize) -> Result<u32, DecodeError> {
    let v = match kind {
        OperandKind::U8 => {
            let b = *code.get(*pos).ok_or(DecodeError::Truncated { offset: at })?;
            *pos += 1;
            Some(b as u32)
        }
        OperandKind::Target => {
            let bytes = code.get(*pos..*pos + 4).ok_or(DecodeError::Truncated { offset: at })?;
            *pos += 4;
            Some(u32::from_le_bytes(bytes.try_into().unwrap()))
        }
        OperandKind::Imm => leb::read_sleb(code, pos).map(|v| v as u32),
        _ => leb::read_uleb(code, pos),
    };
    v.ok_or(DecodeError::BadOperand { offset: at })
}

macro_rules! instructions {
    ($( $(#[doc = $doc:literal])* $name:ident = $code:literal { $($field:ident : $kind:ident),* $(,)? } ),* $(,)?) => {
        /// A decoded instruction. All operands are `u32`; `Imm` operands hold
        /// the bit pattern of an `i32`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Instr {
            $( $(#[doc = $doc])* $name { $($field: u32),* }, )*
        }

        impl Instr {
            pub fn opcode(&self) -> u8 {
                match self { $( Instr::$name { .. } => $code, )* }
            }

            pub fn name(&self) -> &'static str {
                match self { $( Instr::$name { .. } => stringify!($name), )* }
            }

            pub fn encode(&self, out: &mut Vec<u8>) {
                match *self {
                    $( Instr::$name { $($field),* } => {
                        out.push($code);
                        $( encode_operand(OperandKind::$kind, $field, out); )*
                    } )*
                }
            }

            /// Decodes the instruction at `*pos` and advances `pos` past it.
            pub fn decode(code: &[u8], pos: &mut usize) -> Result<Instr, DecodeError> {
                let at = *pos;
                let op = *code.get(at).ok_or(DecodeError::Truncated { offset: at })?;
                *pos += 1;
                #[allow(unused_variables)]
                let p = pos;
                match op {
                    $( $code => Ok(Instr::$name { $($field: decode_operand(OperandKind::$kind, code, p, at)?),* }), )*
                    other => Err(DecodeError::BadOpcode { offset: at, opcode: other }),
                }
            }

            /// `(operand name, kind, value)` in encoding order.
            pub fn operands(&self) -> Vec<(&'static str, OperandKind, u32)> {
                match *self {
                    $( Instr::$name { $($field),* } => vec![$( (stringify!($field), OperandKind::$kind, $field) ),*], )*
                }
            }

            #[allow(unused_mut, unused_variables)]
            pub fn operands_mut(&mut self, mut f: impl FnMut(OperandKind, &mut u32)) {
                match self {
                    $( Instr::$name { $($field),* } => { $( f(OperandKind::$kind, $field); )* } )*
                }
            }
        }

        /// `(opcode, mnemonic, operands)` for every instruction, in opcode order.
        pub const OPCODE_TABLE: &[(u8, &str, &[(&str, OperandKind)], &str)] = &[
            $( ($code, stringify!($name), &[$( (stringify!($field), OperandKind::$kind) ),*], concat!($($doc),*)), )*
        ];
    };
}

instructions! {
    /// No operation.
    Nop = 0x00 {},
    /// `dst = src`
    Mov = 0x01 { dst: Reg, src: Reg },
    /// `dst = constants[k]`
    LoadConst = 0x02 { dst: Reg, k: Const },
    /// `dst = value` (small integer)
    LoadInt = 0x03 { dst: Reg, value: Imm },
    /// `dst = undefined`
    LoadUndef = 0x04 { dst: Reg },
    /// `dst = null`
    LoadNull = 0x05 { dst: Reg },
    /// `dst = true`
    LoadTrue = 0x06 { dst: Reg },
    /// `dst = false`
    LoadFalse = 0x07 { dst: Reg },
    /// `dst = <hole>` — uninitialized lexical binding (TDZ).
    LoadHole = 0x08 { dst: Reg },
    /// Throw ReferenceError naming `name` when `reg` holds a hole.
    CheckTdz = 0x09 { reg: Reg, name: Name },
    /// Replace the value in `reg` with a fresh cell containing that value.
    Box = 0x0a { reg: Reg },
    /// `dst = new cell(<hole>)`
    NewCell = 0x0b { dst: Reg },
    /// `dst = cell.value`
    CellGet = 0x0c { dst: Reg, cell: Reg },
    /// `dst = cell.value`, ReferenceError naming `name` on a hole.
    CellGetChecked = 0x0d { dst: Reg, cell: Reg, name: Name },
    /// `cell.value = src`
    CellSet = 0x0e { cell: Reg, src: Reg },
    /// `cell = new cell(cell.value)` — per-iteration binding copy.
    CellCopy = 0x0f { cell: Reg },
    /// `dst = upvalues[idx].value`
    UpvalGet = 0x10 { dst: Reg, idx: Upval },
    /// `dst = upvalues[idx].value`, ReferenceError naming `name` on a hole.
    UpvalGetChecked = 0x11 { dst: Reg, idx: Upval, name: Name },
    /// `upvalues[idx].value = src`
    UpvalSet = 0x12 { idx: Upval, src: Reg },
    /// `dst = global[name]`, ReferenceError when absent.
    GetGlobal = 0x13 { dst: Reg, name: Name },
    /// `global[name] = src`, ReferenceError when absent (strict mode).
    SetGlobal = 0x14 { name: Name, src: Reg },
    /// `dst = typeof global[name]` ("undefined" when absent).
    TypeofGlobal = 0x15 { dst: Reg, name: Name },
    /// `dst = this` of the activation.
    LoadThis = 0x16 { dst: Reg },
    /// `dst = new.target` of the activation.
    LoadNewTarget = 0x17 { dst: Reg },
    /// `dst = ` the running function object.
    LoadCallee = 0x18 { dst: Reg },

    /// `dst = lhs + rhs`
    Add = 0x20 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs - rhs`
    Sub = 0x21 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs * rhs`
    Mul = 0x22 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs / rhs`
    Div = 0x23 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs % rhs`
    Mod = 0x24 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs ** rhs`
    Exp = 0x25 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs << rhs`
    Shl = 0x26 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs >> rhs`
    Shr = 0x27 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs >>> rhs`
    UShr = 0x28 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs & rhs`
    BitAnd = 0x29 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs | rhs`
    BitOr = 0x2a { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs ^ rhs`
    BitXor = 0x2b { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs == rhs`
    Eq = 0x2c { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs != rhs`
    Ne = 0x2d { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs === rhs`
    StrictEq = 0x2e { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs !== rhs`
    StrictNe = 0x2f { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs < rhs`
    Lt = 0x30 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs <= rhs`
    Le = 0x31 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs > rhs`
    Gt = 0x32 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs >= rhs`
    Ge = 0x33 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs instanceof rhs`
    InstanceOf = 0x34 { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs in rhs`
    In = 0x35 { dst: Reg, lhs: Reg, rhs: Reg },

    /// `dst = -src`
    Neg = 0x40 { dst: Reg, src: Reg },
    /// `dst = ToNumber(src)`
    ToNumber = 0x41 { dst: Reg, src: Reg },
    /// `dst = ~src`
    BitNot = 0x42 { dst: Reg, src: Reg },
    /// `dst = !src`
    Not = 0x43 { dst: Reg, src: Reg },
    /// `dst = typeof src`
    TypeOf = 0x44 { dst: Reg, src: Reg },
    /// `dst = ToString(src)`
    ToStr = 0x45 { dst: Reg, src: Reg },
    /// `dst = ToPropertyKey(src)`
    ToPropKey = 0x46 { dst: Reg, src: Reg },
    /// `dst = ToNumber(src) + 1`
    Inc = 0x47 { dst: Reg, src: Reg },
    /// `dst = ToNumber(src) - 1`
    Dec = 0x48 { dst: Reg, src: Reg },

    /// `dst = obj[key]`
    GetProp = 0x50 { dst: Reg, obj: Reg, key: Reg },
    /// `dst = obj.name`
    GetNamed = 0x51 { dst: Reg, obj: Reg, name: Name },
    /// `obj[key] = value`
    SetProp = 0x52 { obj: Reg, key: Reg, value: Reg },
    /// `obj.name = value`
    SetNamed = 0x53 { obj: Reg, name: Name, value: Reg },
    /// `dst = delete obj[key]`
    DeleteProp = 0x54 { dst: Reg, obj: Reg, key: Reg },
    /// CreateDataProperty(obj, key, value)
    DefineField = 0x55 { obj: Reg, key: Reg, value: Reg },
    /// CreateDataProperty(obj, name, value)
    DefineNamed = 0x56 { obj: Reg, name: Name, value: Reg },
    /// Define method/getter/setter `func` on `obj[key]` and set its home object. `flags`: bits 0-1 kind (0 method, 1 getter, 2 setter), bit 2 enumerable.
    DefineMethod = 0x57 { obj: Reg, key: Reg, func: Reg, flags: U8 },
    /// Copy own enumerable properties of `source` to `target` except the keys in registers `excl..excl+count`.
    CopyDataProps = 0x58 { target: Reg, source: Reg, excl: Reg, count: Count },
    /// `dst = {}`
    NewObject = 0x59 { dst: Reg },
    /// `dst = []`
    NewArray = 0x5a { dst: Reg },
    /// `array.push(value)` (no user code)
    ArrayPush = 0x5b { array: Reg, value: Reg },
    /// Append all values produced by iterating `iterable`.
    ArraySpread = 0x5c { array: Reg, iterable: Reg },
    /// Append a hole (elision).
    ArrayHole = 0x5d { array: Reg },

    /// `dst = closure(functions[func])`, capturing upvalues per the function's descriptors.
    Closure = 0x70 { dst: Reg, func: Func },
    /// Make closure `ctor` a class constructor. `has_parent` = 1 → inherit from `parent`.
    MakeClass = 0x71 { dst: Reg, ctor: Reg, parent: Reg, has_parent: U8 },
    /// Attach instance field initializer `init` to class `class`.
    SetFieldInit = 0x72 { class: Reg, init: Reg },
    /// Run the running constructor's field initializer on `this`.
    InitFields = 0x73 {},
    /// `dst = callee.call(this, args..args+argc)`
    Call = 0x74 { dst: Reg, callee: Reg, this: Reg, args: Reg, argc: Count },
    /// `dst = callee(args..args+argc)` with `this = undefined`
    CallFn = 0x75 { dst: Reg, callee: Reg, args: Reg, argc: Count },
    /// `dst = callee.apply(this, array)`
    CallSpread = 0x76 { dst: Reg, callee: Reg, this: Reg, array: Reg },
    /// `dst = callee.apply(undefined, array)`
    CallFnSpread = 0x77 { dst: Reg, callee: Reg, array: Reg },
    /// `dst = new callee(args..args+argc)`
    New = 0x78 { dst: Reg, callee: Reg, args: Reg, argc: Count },
    /// `dst = new callee(...array)`
    NewSpread = 0x79 { dst: Reg, callee: Reg, array: Reg },
    /// `dst = super(args..args+argc)`; binds `this`, runs field initializers.
    SuperCall = 0x7a { dst: Reg, args: Reg, argc: Count },
    /// `dst = super(...array)`
    SuperCallSpread = 0x7b { dst: Reg, array: Reg },
    /// `dst = super[key]` (receiver = this)
    SuperGet = 0x7c { dst: Reg, key: Reg },
    /// `super[key] = value` (receiver = this)
    SuperSet = 0x7d { key: Reg, value: Reg },

    /// `dst = iterable[Symbol.iterator]()`
    GetIter = 0x90 { dst: Reg, iterable: Reg },
    /// `{done, value} = iter.next()`
    IterNext = 0x91 { done: Reg, value: Reg, iter: Reg },
    /// IteratorClose(iter)
    IterClose = 0x92 { iter: Reg },
    /// `dst` = iterator over the enumerable string keys of `obj`
    ForInIter = 0x93 { dst: Reg, obj: Reg },

    /// Unconditional jump.
    Jmp = 0xa0 { target: Target },
    /// Jump when `cond` is truthy.
    JmpTrue = 0xa1 { cond: Reg, target: Target },
    /// Jump when `cond` is falsy.
    JmpFalse = 0xa2 { cond: Reg, target: Target },
    /// Return `src`.
    Ret = 0xa3 { src: Reg },
    /// Throw `src`.
    Throw = 0xa4 { src: Reg },
    /// Throw a new error. `kind`: 0 TypeError, 1 ReferenceError, 2 SyntaxError, 3 RangeError.
    ThrowError = 0xa5 { kind: U8, msg: Name },
    /// `dst` = the exception being handled. First instruction of every handler.
    GetException = 0xa6 { dst: Reg },
    /// Suspend the async function until `value` settles; `dst` = fulfilled value (or throw).
    Await = 0xa7 { dst: Reg, value: Reg },
    /// `debugger` statement.
    Debugger = 0xa8 {},
}

impl Instr {
    /// Control never falls through to the next instruction.
    pub fn is_terminator(&self) -> bool {
        matches!(self, Instr::Jmp { .. } | Instr::Ret { .. } | Instr::Throw { .. } | Instr::ThrowError { .. })
    }

    /// Jump targets (byte offsets in encoded form).
    pub fn targets(&self) -> Vec<u32> {
        self.operands().into_iter().filter(|(_, k, _)| *k == OperandKind::Target).map(|(_, _, v)| v).collect()
    }

    pub fn encoded_len(&self) -> usize {
        let mut v = Vec::new();
        self.encode(&mut v);
        v.len()
    }
}

/// Decode a whole function body into `(byte offset, instruction)` pairs.
pub fn decode_all(code: &[u8]) -> Result<Vec<(u32, Instr)>, DecodeError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < code.len() {
        let at = pos;
        let i = Instr::decode(code, &mut pos)?;
        out.push((at as u32, i));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcodes_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (code, name, _, _) in OPCODE_TABLE {
            assert!(seen.insert(*code), "duplicate opcode {code:#x} ({name})");
        }
    }

    #[test]
    fn every_instruction_roundtrips() {
        for (code, _, ops, _) in OPCODE_TABLE {
            let mut bytes = vec![*code];
            let mut expect = Vec::new();
            for (i, (_, k)) in ops.iter().enumerate() {
                let v = match k {
                    OperandKind::U8 => 3,
                    OperandKind::Imm => (-5i32 - i as i32) as u32,
                    OperandKind::Target => 0x1234_5678,
                    _ => 300 + i as u32,
                };
                encode_operand(*k, v, &mut bytes);
                expect.push(v);
            }
            let mut pos = 0;
            let instr = Instr::decode(&bytes, &mut pos).unwrap();
            assert_eq!(pos, bytes.len());
            assert_eq!(instr.opcode(), *code);
            assert_eq!(instr.operands().iter().map(|o| o.2).collect::<Vec<_>>(), expect);
            let mut re = Vec::new();
            instr.encode(&mut re);
            assert_eq!(re, bytes);
        }
    }

    #[test]
    fn rejects_unknown_opcode_and_truncation() {
        let mut p = 0;
        assert!(matches!(Instr::decode(&[0xff], &mut p), Err(DecodeError::BadOpcode { .. })));
        let mut p = 0;
        assert!(matches!(Instr::decode(&[0xa0, 1, 2], &mut p), Err(DecodeError::Truncated { .. })));
    }
}

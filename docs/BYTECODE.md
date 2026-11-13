# Bytecode: instruction set and file format

## Execution model

Register machine. Every activation owns `register_count` registers;
parameters arrive in registers `0..param_count` and, with `has_rest`, the
rest array in register `param_count`. Calls pass arguments in a contiguous
register window (`args`, `argc`). Constants live in a module-wide pool
(numbers and strings). Exception handling is table-driven: each function
has `handlers: [(start, end, target)]` over byte offsets; the innermost
covering entry wins, and the thrown value is read with `Catch`.
Instructions are addressed by byte offset (`pc`).

## Encoding

An instruction is a one-byte opcode followed by its operands:

| operand kind | encoding                 | meaning                                  |
|--------------|--------------------------|------------------------------------------|
| `Reg`        | ULEB128                  | register index                           |
| `Const`      | ULEB128                  | constant-pool index (any constant)       |
| `Name`       | ULEB128                  | constant-pool index of a string          |
| `Func`       | ULEB128                  | function-table index                     |
| `Upval`      | ULEB128                  | index into the closure's upvalues        |
| `Count`      | ULEB128                  | element count                            |
| `Imm`        | SLEB128                  | signed 32-bit immediate                  |
| `U8`         | 1 byte                   | small enum / flags                       |
| `Target`     | 4 bytes little-endian    | absolute byte offset of an instruction   |

Jump targets are fixed-width so code is emitted in one pass and patched.

## File format (`.djbc`, version 1)

All integers are ULEB128 unless noted; strings are `uleb len` + UTF-8.

```
file      := "DJBC" u16le(version=1) u16le(flags: bit0 = has debug section)
             constants functions uleb(entry) [debug]
constants := uleb(n) { u8 tag (0 = f64le, 1 = string) payload }*
functions := uleb(n) { uleb(name) u8(kind) u8(flags: bit0 async, bit1 rest)
                       uleb(params) uleb(registers)
                       uleb(n) { u8(from_parent_register) uleb(index) }*      upvalues
                       uleb(len) code-bytes
                       uleb(n) { uleb(start) uleb(end) uleb(target) }* }*      handlers
debug     := str(source_name) u8(has_source) [str(source)]
             { per function, same order as the function table:
               pos(start) pos(end)
               uleb(n) { uleb(pc) pos u8(is_stmt) }*                   line table
               uleb(n) { uleb(parent+1) u8(kind) pos(start) pos(end) }* scopes
               uleb(n) { uleb(pc) uleb(scope) }*                        scope map
               uleb(n) { str(name) u8(kind) uleb(scope+1) u8(loc kind)
                         uleb(loc index) pos(decl)
                         uleb(n) { uleb(start) uleb(end) }* }*          variables
               uleb(n) { uleb(pc) uleb(block) uleb(inst+1) }* }         IR map
pos       := uleb(line) uleb(col)
```

The debug section is optional (`--strip-debug`) and never affects code:
compiling with and without it yields identical functions and constants.
Serialization is deterministic: the same source and options always give
the same bytes, and `Module::from_bytes(m.to_bytes()) == m`.

## Validation

`dj_bytecode::validate::validate(&module)` runs before a module is loaded
and rejects: unknown opcodes and truncated operands; register operands
`>= register_count` (call windows may end exactly at the register file
end); constant/name/function/upvalue indices out of range; name operands
that are not strings; jump and handler targets that are not instruction
starts; code that can fall off the end; handler ranges outside the code;
debug tables that are unsorted or reference unknown scopes. The loader
reports a `LoadError` instead of executing an invalid module.

## Disassembly

`djungle disasm file.ts` (or `dj_bytecode::disasm::disassemble`) prints
the constant pool, every function with its header, each instruction with
its byte offset, resolved constants/names/functions, and its source
position (`; line:col stmt`), followed by the handler table and the
variables with their live ranges.

## Instruction set

| opcode | name | operands | semantics |
|--------|------|----------|-----------|
| `0x00` | `Nop` | — | No operation. |
| `0x01` | `Mov` | `dst: Reg, src: Reg` | `dst = src` |
| `0x02` | `LoadConst` | `dst: Reg, k: Const` | `dst = constants[k]` |
| `0x03` | `LoadInt` | `dst: Reg, value: Imm` | `dst = value` (small integer) |
| `0x04` | `LoadUndef` | `dst: Reg` | `dst = undefined` |
| `0x05` | `LoadNull` | `dst: Reg` | `dst = null` |
| `0x06` | `LoadTrue` | `dst: Reg` | `dst = true` |
| `0x07` | `LoadFalse` | `dst: Reg` | `dst = false` |
| `0x08` | `LoadHole` | `dst: Reg` | `dst = <hole>` — uninitialized lexical binding (TDZ). |
| `0x09` | `CheckTdz` | `reg: Reg, name: Name` | Throw ReferenceError naming `name` when `reg` holds a hole. |
| `0x0a` | `Box` | `reg: Reg` | Replace the value in `reg` with a fresh cell containing that value. |
| `0x0b` | `NewCell` | `dst: Reg` | `dst = new cell(<hole>)` |
| `0x0c` | `CellGet` | `dst: Reg, cell: Reg` | `dst = cell.value` |
| `0x0d` | `CellGetChecked` | `dst: Reg, cell: Reg, name: Name` | `dst = cell.value`, ReferenceError naming `name` on a hole. |
| `0x0e` | `CellSet` | `cell: Reg, src: Reg` | `cell.value = src` |
| `0x0f` | `CellCopy` | `cell: Reg` | `cell = new cell(cell.value)` — per-iteration binding copy. |
| `0x10` | `UpvalGet` | `dst: Reg, idx: Upval` | `dst = upvalues[idx].value` |
| `0x11` | `UpvalGetChecked` | `dst: Reg, idx: Upval, name: Name` | `dst = upvalues[idx].value`, ReferenceError naming `name` on a hole. |
| `0x12` | `UpvalSet` | `idx: Upval, src: Reg` | `upvalues[idx].value = src` |
| `0x13` | `GetGlobal` | `dst: Reg, name: Name` | `dst = global[name]`, ReferenceError when absent. |
| `0x14` | `SetGlobal` | `name: Name, src: Reg` | `global[name] = src`, ReferenceError when absent (strict mode). |
| `0x15` | `TypeofGlobal` | `dst: Reg, name: Name` | `dst = typeof global[name]` ("undefined" when absent). |
| `0x16` | `LoadThis` | `dst: Reg` | `dst = this` of the activation. |
| `0x17` | `LoadNewTarget` | `dst: Reg` | `dst = new.target` of the activation. |
| `0x18` | `LoadCallee` | `dst: Reg` | `dst = ` the running function object. |
| `0x20` | `Add` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs + rhs` |
| `0x21` | `Sub` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs - rhs` |
| `0x22` | `Mul` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs * rhs` |
| `0x23` | `Div` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs / rhs` |
| `0x24` | `Mod` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs % rhs` |
| `0x25` | `Exp` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs ** rhs` |
| `0x26` | `Shl` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs << rhs` |
| `0x27` | `Shr` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs >> rhs` |
| `0x28` | `UShr` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs >>> rhs` |
| `0x29` | `BitAnd` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs & rhs` |
| `0x2a` | `BitOr` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs \| rhs` |
| `0x2b` | `BitXor` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs ^ rhs` |
| `0x2c` | `Eq` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs == rhs` |
| `0x2d` | `Ne` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs != rhs` |
| `0x2e` | `StrictEq` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs === rhs` |
| `0x2f` | `StrictNe` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs !== rhs` |
| `0x30` | `Lt` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs < rhs` |
| `0x31` | `Le` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs <= rhs` |
| `0x32` | `Gt` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs > rhs` |
| `0x33` | `Ge` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs >= rhs` |
| `0x34` | `InstanceOf` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs instanceof rhs` |
| `0x35` | `In` | `dst: Reg, lhs: Reg, rhs: Reg` | `dst = lhs in rhs` |
| `0x40` | `Neg` | `dst: Reg, src: Reg` | `dst = -src` |
| `0x41` | `ToNumber` | `dst: Reg, src: Reg` | `dst = ToNumber(src)` |
| `0x42` | `BitNot` | `dst: Reg, src: Reg` | `dst = ~src` |
| `0x43` | `Not` | `dst: Reg, src: Reg` | `dst = !src` |
| `0x44` | `TypeOf` | `dst: Reg, src: Reg` | `dst = typeof src` |
| `0x45` | `ToStr` | `dst: Reg, src: Reg` | `dst = ToString(src)` |
| `0x46` | `ToPropKey` | `dst: Reg, src: Reg` | `dst = ToPropertyKey(src)` |
| `0x47` | `Inc` | `dst: Reg, src: Reg` | `dst = ToNumber(src) + 1` |
| `0x48` | `Dec` | `dst: Reg, src: Reg` | `dst = ToNumber(src) - 1` |
| `0x50` | `GetProp` | `dst: Reg, obj: Reg, key: Reg` | `dst = obj[key]` |
| `0x51` | `GetNamed` | `dst: Reg, obj: Reg, name: Name` | `dst = obj.name` |
| `0x52` | `SetProp` | `obj: Reg, key: Reg, value: Reg` | `obj[key] = value` |
| `0x53` | `SetNamed` | `obj: Reg, name: Name, value: Reg` | `obj.name = value` |
| `0x54` | `DeleteProp` | `dst: Reg, obj: Reg, key: Reg` | `dst = delete obj[key]` |
| `0x55` | `DefineField` | `obj: Reg, key: Reg, value: Reg` | CreateDataProperty(obj, key, value) |
| `0x56` | `DefineNamed` | `obj: Reg, name: Name, value: Reg` | CreateDataProperty(obj, name, value) |
| `0x57` | `DefineMethod` | `obj: Reg, key: Reg, func: Reg, flags: U8` | Define method/getter/setter `func` on `obj[key]` and set its home object. `flags`: bits 0-1 kind (0 method, 1 getter, 2 setter), bit 2 enumerable. |
| `0x58` | `CopyDataProps` | `target: Reg, source: Reg, excl: Reg, count: Count` | Copy own enumerable properties of `source` to `target` except the keys in registers `excl..excl+count`. |
| `0x59` | `NewObject` | `dst: Reg` | `dst = {}` |
| `0x5a` | `NewArray` | `dst: Reg` | `dst = []` |
| `0x5b` | `ArrayPush` | `array: Reg, value: Reg` | `array.push(value)` (no user code) |
| `0x5c` | `ArraySpread` | `array: Reg, iterable: Reg` | Append all values produced by iterating `iterable`. |
| `0x5d` | `ArrayHole` | `array: Reg` | Append a hole (elision). |
| `0x70` | `Closure` | `dst: Reg, func: Func` | `dst = closure(functions[func])`, capturing upvalues per the function's descriptors. |
| `0x71` | `MakeClass` | `dst: Reg, ctor: Reg, parent: Reg, has_parent: U8` | Make closure `ctor` a class constructor. `has_parent` = 1 → inherit from `parent`. |
| `0x72` | `SetFieldInit` | `class: Reg, init: Reg` | Attach instance field initializer `init` to class `class`. |
| `0x73` | `InitFields` | — | Run the running constructor's field initializer on `this`. |
| `0x74` | `Call` | `dst: Reg, callee: Reg, this: Reg, args: Reg, argc: Count` | `dst = callee.call(this, args..args+argc)` |
| `0x75` | `CallFn` | `dst: Reg, callee: Reg, args: Reg, argc: Count` | `dst = callee(args..args+argc)` with `this = undefined` |
| `0x76` | `CallSpread` | `dst: Reg, callee: Reg, this: Reg, array: Reg` | `dst = callee.apply(this, array)` |
| `0x77` | `CallFnSpread` | `dst: Reg, callee: Reg, array: Reg` | `dst = callee.apply(undefined, array)` |
| `0x78` | `New` | `dst: Reg, callee: Reg, args: Reg, argc: Count` | `dst = new callee(args..args+argc)` |
| `0x79` | `NewSpread` | `dst: Reg, callee: Reg, array: Reg` | `dst = new callee(...array)` |
| `0x7a` | `SuperCall` | `dst: Reg, args: Reg, argc: Count` | `dst = super(args..args+argc)`; binds `this`, runs field initializers. |
| `0x7b` | `SuperCallSpread` | `dst: Reg, array: Reg` | `dst = super(...array)` |
| `0x7c` | `SuperGet` | `dst: Reg, key: Reg` | `dst = super[key]` (receiver = this) |
| `0x7d` | `SuperSet` | `key: Reg, value: Reg` | `super[key] = value` (receiver = this) |
| `0x90` | `GetIter` | `dst: Reg, iterable: Reg` | `dst = iterable[Symbol.iterator]()` |
| `0x91` | `IterNext` | `done: Reg, value: Reg, iter: Reg` | `{done, value} = iter.next()` |
| `0x92` | `IterClose` | `iter: Reg` | IteratorClose(iter) |
| `0x93` | `ForInIter` | `dst: Reg, obj: Reg` | `dst` = iterator over the enumerable string keys of `obj` |
| `0xa0` | `Jmp` | `target: Target` | Unconditional jump. |
| `0xa1` | `JmpTrue` | `cond: Reg, target: Target` | Jump when `cond` is truthy. |
| `0xa2` | `JmpFalse` | `cond: Reg, target: Target` | Jump when `cond` is falsy. |
| `0xa3` | `Ret` | `src: Reg` | Return `src`. |
| `0xa4` | `Throw` | `src: Reg` | Throw `src`. |
| `0xa5` | `ThrowError` | `kind: U8, msg: Name` | Throw a new error. `kind`: 0 TypeError, 1 ReferenceError, 2 SyntaxError, 3 RangeError. |
| `0xa6` | `GetException` | `dst: Reg` | `dst` = the exception being handled. First instruction of every handler. |
| `0xa7` | `Await` | `dst: Reg, value: Reg` | Suspend the async function until `value` settles; `dst` = fulfilled value (or throw). |
| `0xa8` | `Debugger` | — | `debugger` statement. |

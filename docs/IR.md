# IR specification

The IR is independent of TypeScript and of the bytecode VM: a module of
functions, each a control-flow graph of basic blocks holding
single-assignment temporaries. Source code is reachable only through spans.

## Model

```
Module   { name, source, functions: [Function], entry: FuncId }
Function { id, name, kind, is_async, param_count, has_rest, span, parent,
           scopes: [Scope], locals: [Local], upvalues: [Upvalue],
           blocks: [Block], entry: BlockId, temp_count }
Block    { insts: [Inst], term: Terminator, handler: Option<BlockId> }
Inst     { kind: InstKind, span, scope }
Terminator { kind: TermKind, span, scope }
```

Identifiers print as `f0`, `bb3`, `t7`, `l2`, `u0`, `s1`.

* **Temps** (`t`) are assigned exactly once and used only after their
  definition on every path (checked by the verifier). They never cross a
  suspension point other than as the `dst` of `Await`.
* **Locals** (`l`) are named or synthetic variable slots with a kind
  (`param`, `var`, `let`, `const`, `function`, `class`, `catch`, `this`,
  synthetic), a declaring scope, and a `captured` flag. Captured locals live
  in heap cells so closures share them; others live in registers.
* **Upvalues** (`u`) describe a closure's captured bindings: either a
  local of the immediately enclosing function or an upvalue of it.
* **Scopes** (`s`) form a tree per function (`function`, `block`, `catch`,
  `class`, loop scopes) with source ranges. Every instruction records its
  innermost scope.
* **Function kinds**: `script`, `normal`, `arrow`, `method`, `getter`,
  `setter`, `class-ctor`, `derived-ctor`, `field-init`.

## Control flow

| terminator | meaning |
|------------|---------|
| `Jump(bb)` | unconditional |
| `Branch { cond, then_bb, else_bb }` | on the truthiness of `cond` |
| `Return(t)` | return from the function |
| `Throw(t)` | throw a value |
| `ThrowError { kind, message }` | throw a fresh error (e.g. assignment to `const`) |
| `Unreachable` | after diverging code |

**Exceptions.** A block's `handler` names the block that receives control
when any instruction in it throws; the handler's first instruction is
`CatchValue` to bind the thrown value. Nested `try` regions are separate
blocks with different handlers. `finally` is lowered with two synthetic
locals, `%fkind` (0 normal, 1 return, 2 throw, 3+ the pending
`break`/`continue` target) and `%fval`, and a dispatch after the finally
body, so there is no special finally construct in the IR.

**Async.** `Await { dst, value }` suspends the function; `dst` receives the
fulfilled value, or the rejection is thrown at that point (so the block's
handler applies). An `is_async` function returns a promise.

**TDZ.** `DeclareLocal` starts a binding (uninitialized for lexical kinds,
a fresh cell for captured ones, which gives per-iteration loop bindings).
`LoadLocal`/`LoadUpval` with `check_tdz` throw a `ReferenceError` on an
uninitialized binding. `CopyBinding` implements the `for (let ...)` copy
between iterations.

## Instructions

| instruction | semantics |
|-------------|-----------|
| `Stmt` | Statement boundary: a breakpoint / stepping location. Never removed by optimization passes (except together with its unreachable block). |
| `Debugger` | `debugger;` |
| `Const` |  |
| `LoadLocal` | Read a local. `check_tdz` throws a ReferenceError on an uninitialized binding. |
| `StoreLocal` | Write (and initialize) a local. |
| `DeclareLocal` | Start a fresh binding: lexical locals enter their TDZ; captured locals get a new cell (per-iteration bindings in loops). |
| `CopyBinding` | `for (let ...)` per-iteration copy: new cell holding the current value. |
| `LoadUpval` |  |
| `StoreUpval` |  |
| `LoadGlobal` | Global object property read; ReferenceError when absent. |
| `StoreGlobal` |  |
| `TypeofGlobal` | `typeof name` for an unresolved identifier (never throws). |
| `LoadThis` | Raw `this` of the current activation. |
| `LoadNewTarget` |  |
| `LoadCallee` | The function object being executed (named function expressions). |
| `Unary` |  |
| `Binary` |  |
| `GetProp` |  |
| `GetNamed` |  |
| `SetProp` |  |
| `SetNamed` |  |
| `DeleteProp` |  |
| `NewObject` |  |
| `NewArray` |  |
| `ArrayPush` |  |
| `ArraySpread` | Append every value produced by iterating `iterable`. |
| `ArrayHole` | Append an elision (`[1, , 3]`). |
| `DefineField` | CreateDataProperty (object literals, class fields). |
| `DefineNamed` |  |
| `DefineMethod` | Define a method / accessor and set the function's home object. |
| `CopyDataProps` | Copy own enumerable properties of `source` onto `target`, skipping the keys in `excluded` (object spread / object rest). |
| `MakeClosure` |  |
| `MakeClass` | Turn closure `ctor` into a class constructor with a fresh prototype object inheriting from `parent.prototype` (or `Object.prototype`). |
| `SetFieldInit` | Attach a field-initializer function to class constructor `class`. |
| `InitFields` | Run the current class constructor's field initializer on `this` (base classes; derived classes run it inside `SuperCall`). |
| `Call` |  |
| `CallSpread` | `args` is an array holding the argument list. |
| `New` |  |
| `NewSpread` |  |
| `SuperCall` | `super(...)`: constructs the parent, binds `this`, runs field initializers. |
| `SuperCallSpread` |  |
| `SuperGet` | `super[key]` with the current `this` as receiver. |
| `SuperSet` |  |
| `GetIterator` |  |
| `IterNext` | Advance an iterator: `done` receives a boolean, `value` the value. |
| `IterClose` | IteratorClose (calls `return` when present). |
| `ForInIterator` | Iterator over enumerable string keys (`for ... in`). |
| `Await` | Suspend the async function until `value` settles. |
| `CatchValue` | First instruction of an exception handler block: the thrown value. |
| `Branch` |  |

Binary/unary operators follow ECMAScript semantics (`+` with `ToPrimitive`,
loose/strict equality, relational comparison, `in`, `instanceof`, bit
operations on `ToInt32`/`ToUint32`).

## Verification

`dj_ir::verify::verify_module` checks: every block ends in a terminator
with valid successors; temps are defined once and dominate their uses;
locals, upvalues, scopes and functions referenced exist; handlers start with
`CatchValue`; `Await` occurs only in async functions; `SuperCall` only in
derived constructors. The compiler verifies after lowering and after
optimization.

## Passes

`dj_ir::passes::optimize(&mut module, OptLevel::{None, Default})` runs, to a
fixed point: constant folding over primitive operands (no conversions that
could run user code, no number-to-string), branch folding, copy/temp
forwarding, dead code elimination of side-effect-free instructions, and
CFG simplification (jump threading, merging, unreachable-block removal).

Debug-info contract every pass upholds:

* instructions keep their `span` and `scope`;
* `Stmt` markers are never removed, reordered across each other or moved to
  another scope, except together with an unreachable block;
* stores to user-visible locals are never removed, so variable inspection at
  any statement boundary observes the same values as unoptimized code.

The debugger test `stops_are_stable_across_opt_levels_and_gc` checks this
end to end: the same stepping session stops at the same places, with the
same values, at `-O0`, optimized and under GC stress.

## Printing

`djungle ir file.ts` prints the module after optimization; `-O0` prints it
as lowered.

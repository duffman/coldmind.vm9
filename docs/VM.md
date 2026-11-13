# VM

`dj_vm::Vm` loads validated bytecode modules and interprets them.

```rust
let mut vm = Vm::new(VmOptions::default());
let id = vm.load(module)?;                // validates, resolves constants
let result: Result<(), Uncaught> = vm.run_module(id); // script + event loop
```

`VmOptions`: `max_frames` (call depth; `RangeError: Maximum call stack size
exceeded` beyond), `max_native_depth` (native → JS re-entry), `gc_stress`
(collect at every safepoint), `capture_output` (buffer console output
instead of writing to stdout/stderr).

## Values and objects

`Value` = `Undefined | Null | Bool | Number(f64) | String(JsStr) |
Symbol | Object(ObjRef)`, plus the internal `Hole` for array holes.
Strings are UTF-16 code-unit sequences. `ObjRef` is a generational handle
(slot index + generation), so a stale handle is detected rather than
aliased.

`JsObject { proto, extensible, properties: ordered map PropKey → Property,
kind }`. Property keys are strings or symbols; own keys enumerate in spec
order (integer indices ascending, then strings, then symbols, each in
insertion order). Properties are data (`value`, flags) or accessors
(`get`/`set`). `kind` selects exotic behaviour and internal slots: ordinary,
`Array` (dense element vector with virtual `length` and index properties),
closure, native function, bound function, error, `Map`, `Set`, promise,
iterators, primitive wrappers, coroutine (suspended async frame).

All operations follow ECMAScript abstract operations: `ToPrimitive`,
`ToNumber`, `ToString` (shortest round-trip number formatting),
`ToPropertyKey`, `[[Get]]`/`[[Set]]` along the prototype chain with
receivers, `[[DefineOwnProperty]]`, `[[Delete]]`, loose/strict equality,
relational comparison, `instanceof` (with `Symbol.hasInstance`), the
iterator protocol (with fast paths for array iterators and for-in).
Mutation of frozen objects or non-writable properties throws `TypeError`
(strict semantics).

## Execution

* **Frames.** `vm.frames` holds activations (`func`, `pc`, register base,
  `this`, callee, new-target, a stable `id`); registers live in one shared
  `vm.stack`. Calls from native code into JS push a *boundary* frame and run
  a nested interpreter loop, so natives (e.g. `Array.prototype.map`) can
  call back into JavaScript.
* **Exceptions.** A throw searches the current function's handler table for
  the innermost range covering the throwing instruction, then unwinds
  frames. Unwinding an async frame rejects its promise instead. Errors
  capture a stack trace (`name (file:line:col)` per frame, `(native)` for
  natives) at construction.
* **Closures.** Captured bindings live in heap cells. `MakeClosure`
  collects cells from the enclosing frame's registers or upvalues.
* **Classes.** `MakeClass` wires the prototype chain and home objects.
  Instance fields run in a field-init function invoked when `this` becomes
  available (base constructor entry, or after `super()` returns).

## Async scheduling

* **Async functions.** On `Await` the frame's registers and pc move into a
  heap `Coroutine`; resumption happens from a promise reaction job.
* **Jobs.** A FIFO microtask queue holds promise reactions, thenable
  resolution and `queueMicrotask` callbacks. Timers sit in a
  `BTreeMap<(due time, sequence), Timer>`, so equal deadlines fire in
  creation order.
* **Event loop.** After the script, `run_event_loop` drains microtasks. It
  then advances to the earliest timer, runs it and drains again, until
  nothing is pending.
* **Clocks.** Time comes from a `Scheduler` trait (`now`, `wait_until`).
  `VirtualClock` (the default) jumps straight to the next deadline, so
  timer programs run instantly and deterministically. `SystemClock`
  (`--real-time`) sleeps.
* **Unhandled rejections.** A promise still rejected and unhandled after a
  macrotask turn is reported as `Uncaught (in promise) ...`.

## Garbage collection

Precise mark-and-sweep over the handle heap. Collection happens only at
interpreter safepoints (instruction boundaries), when the allocation budget
is exceeded, or at every safepoint with `gc_stress`. Roots are:
- frames and registers;
- coroutines and the job queue;
- timers and the realm (intrinsics, global object);
- debugger references;
- an explicit root stack.

Native functions push anything they hold across a call into JavaScript
onto that root stack (`push_root` / `pop_roots(mark)`). Freed slots bump
their generation. `vm.gc_stats()` reports collections, live objects and
freed objects. The whole end-to-end corpus also runs under `gc_stress`.

## Runtime errors

Every runtime failure is a catchable JavaScript exception with the
standard error type and a V8-style message: `TypeError` (calling a
non-function, property access on null/undefined, frozen writes, invalid
`new`, ...), `ReferenceError` (unresolved globals, TDZ), `RangeError`
(invalid array length, stack overflow, `toFixed` range, ...), and
`SyntaxError` (`JSON.parse`). Loading an invalid module returns
`LoadError`; the VM never executes unvalidated code.

# Debugger architecture

Three layers:
1. **Metadata** emitted by the compiler (`dj_bytecode::debug`).
2. **Hooks and an inspection API** in the VM (`dj_vm::debugger`).
3. **A DAP-shaped session** (`dj_debug`). A wire-level DAP server is a
   JSON codec over layer 3.

## 1. Metadata model

`Module::debug: Option<DebugInfo>` holds `source_name`, the optional
embedded `source`, and one `FunctionDebug` per function. Positions are
`Pos { line, col }`, 1-based, with UTF-16 columns.

| map | content | used for |
|-----|---------|----------|
| `lines: [LineEntry { pc, pos, is_stmt }]` | pc → source position, sorted. `is_stmt` marks statement starts and carries the statement's own position (declarations: one per declarator; loop tests and updates are their own statements). | stack traces, stop locations, breakpoints, stepping |
| `scopes: [ScopeDebug { parent, kind, start, end }]` and `scope_map: [(pc, scope)]` | lexical scope tree with source ranges; innermost scope from each pc onward | scope chains in the Scopes view |
| `variables: [VarDebug { name, kind, scope, location, decl, ranges }]` | `kind`: param / var / let / const / function / class / catch / this. `location`: `Register(r)`, `Cell(r)` (captured; the register holds the cell), `Upvalue(i)`. `ranges`: half-open pc live ranges. | variable inspection, visibility |
| `ir_map: [IrPos { pc, block, inst }]` | bytecode → IR instruction (`inst = u32::MAX` for a terminator) | source ↔ IR ↔ bytecode correlation |

The chain is complete in both directions:
- **source ↔ IR:** every IR instruction carries a span and a scope.
- **IR ↔ bytecode:** `ir_map` links them.
- **bytecode ↔ source:** `lines` links them.

`source_maps.rs` verifies that a statement start's IR instruction has the
same source line.

Queries on the metadata:
- `FunctionDebug::pos_at`, `is_stmt_start`, `stmt_starts`, `scope_at`,
  `scope_chain`, `visible_variables`, `ir_pos_at`.
- `DebugInfo::resolve_breakpoint(line, col)`: the first statement at or
  after the requested position; on a tie the innermost function wins.
- `DebugInfo::breakpoint_locations(line)`.

Stripping (`--strip-debug`) removes the section without changing code.

## 2. VM interface

```rust
pub trait DebugHandler {
    fn on_stop(&mut self, view: &mut DebugView<'_>, reason: StopReason) -> ResumeAction;
    fn on_output(&mut self, text: &str) {}
}
```

### Host API on `Vm`

| call | purpose |
|------|---------|
| `set_debug_handler` / `clear_debug_handler` | install or remove the handler |
| `set_breakpoints(module, &[BreakpointSpec { line, column, condition }]) -> Vec<Breakpoint>` | replaces the module's breakpoints; each result is verified or not, with its resolved line, column and pc |
| `set_exception_filters(ExceptionFilters { all, uncaught })` | which throws stop |
| `set_stop_on_entry(bool)` | stop before the first statement |
| `pause_handle() -> Arc<AtomicBool>` | settable from any thread; the program pauses at the next instruction |

### Hooks

The interpreter calls a safepoint before each instruction while a handler
is installed. Stop reasons: `Entry`, `Breakpoint(ids)`, `Step`, `Pause`,
`DebuggerStatement`, `Exception { uncaught }`.

| action | where it stops |
|--------|----------------|
| `StepIn` | next statement start anywhere |
| `StepOver` | next statement start in the same activation, matched by frame id |
| `StepOut` | the caller's return point, reported at the call instruction |

When the stepped activation returns, any step stops at the caller's
return point. StepOver matches by frame *id*, so stepping over `await`
continues in the same async function once it resumes, while other code
runs in between.

Conditional breakpoints evaluate their condition in the top frame and stop
when it is truthy. A failing condition stops, so errors stay visible.

An exception counts as "uncaught" when no frame's handler covers its
current pc and no async function would turn it into a rejection. A
rethrow of the same value is reported once.

While stopped, `pc - 1` is the current instruction of every frame.

### Inspection: `DebugView`

| method | returns |
|--------|---------|
| `frames()` | `[FrameInfo { index, id, name, module, func, pc, source, pos, ir }]`, innermost first; `id` is stable across suspension |
| `scopes(frame)` | `[ScopeInfo { name, kind, variables_ref, range }]`: `Local`, `Block`, `Catch`, `Class`, `Closure` (upvalues), `Global` |
| `variables(ref)` | `[Variable { name, value, type_name, kind, variables_ref }]` |
| `exception()` | `ExceptionInfo { value, description, stack, uncaught }` |
| `evaluate(frame, expr)` / `evaluate_value` | side-effect-free expression result |
| `set_breakpoints` / `set_exception_filters` | the same as the host API, callable while stopped |

`variables(ref)` covers the bindings of a scope, the properties of an
object, array elements, `Map`/`Set` entries, and `[[Prototype]]`. A binding
in its TDZ shows as `<uninitialized>` with type `uninitialized`. Values use
the short inspect form (`Array(3)`, `Map(2)`, `#<Point>`, `"str"`).

`evaluate` resolves names, `this`, `.prop` and `[key]` against the frame's
visible variables, closure upvalues and globals. It never calls user code:
getters are not invoked.

## 3. Session API (`dj_debug`)

`Session::launch(module, LaunchConfig { stop_on_entry, gc_stress })` runs
the VM on its own thread. Requests are answered synchronously; events are
queued.

| DAP | here |
|-----|------|
| `setBreakpoints` | `Request::SetBreakpoints(specs)` → `Response::Breakpoints` |
| `setExceptionBreakpoints` | `Request::SetExceptionFilters` |
| `breakpointLocations` | `Request::BreakpointLocations(line)` → `Response::Locations` |
| `configurationDone` | `Request::ConfigurationDone` (the program starts) |
| `continue` / `next` / `stepIn` / `stepOut` | `Request::Continue` / `Next` / `StepIn` / `StepOut` |
| `pause` | `Session::pause()` (asynchronous flag) |
| `stackTrace` / `scopes` / `variables` | `Request::StackTrace` / `Scopes { frame }` / `Variables { reference }` |
| `evaluate` | `Request::Evaluate { frame, expression }` |
| `exceptionInfo` | `Request::ExceptionInfo` |
| `terminate` | `Request::Terminate` |
| `stopped` / `output` / `exited` events | `Event::Stopped { reason, frame }` / `Output(text)` / `Exited { error }` |

Inspection requests are served only while stopped; otherwise they return
`Response::Error`. Dropping the session terminates the program.

## Validation

`crates/dj_cli/tests/debugger.rs` covers:
- breakpoints with stack, scopes and TDZ values;
- conditional breakpoints and evaluate;
- step in, over and out, including the return point;
- closure variables;
- the `debugger` statement and object expansion;
- both exception filters;
- pause of an infinite loop;
- stepping over `await`;
- breakpoint locations and terminate;
- an identical stepping session at `-O0`, optimized and under GC stress.

`source_maps.rs` covers the metadata itself.

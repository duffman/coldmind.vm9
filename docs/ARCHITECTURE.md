# djungle — TypeScript → IR → bytecode → VM

```
source.ts ──dj_frontend──▶ IR (dj_ir) ──passes──▶ IR ──dj_codegen──▶ bytecode (dj_bytecode) ──dj_vm──▶ execution
   oxc parse,             CFG, scopes,     const fold,      register alloc,       .djbc file,           interpreter, GC,
   checks, lowering       debug spans      DCE, forward,    line/scope/var/IR     validator,            promises/timers,
   (types erased)                          simplify-cfg     maps                  disassembler          debugger hooks
                                                                                                         │
                                                                                     dj_debug (DAP-shaped session API)
```

| crate          | role |
|----------------|------|
| `dj_frontend`  | Parses with oxc, rejects unsupported constructs, performs semantic checks, erases types, lowers to IR. |
| `dj_ir`        | IR data model, printer, verifier, optimization passes. See [IR.md](IR.md). |
| `dj_codegen`   | IR → bytecode: register allocation, jump patching, exception tables, all debug maps. |
| `dj_bytecode`  | Instruction set, module model, binary format, validator, disassembler. See [BYTECODE.md](BYTECODE.md). |
| `dj_vm`        | Interpreter, values, heap and GC, builtins, job queue and timers, debugger hooks. See [VM.md](VM.md). |
| `dj_debug`     | Debug session over channels, shaped after the Debug Adapter Protocol. See [DEBUGGER.md](DEBUGGER.md). |
| `dj_cli`       | Library glue (`compile`, `run`) and the `djungle` binary. |

## CLI

```
djungle run file.ts [-O0] [--strip-debug] [--gc-stress] [--real-time]
djungle check file.ts          # diagnostics only
djungle ir file.ts [-O0]       # print IR after optimization
djungle disasm file.ts         # disassemble the bytecode
djungle compile file.ts -o out.djbc [--strip-debug]
djungle exec out.djbc
```

Exit code 1 on diagnostics or an uncaught exception. Uncaught errors print
`Uncaught Name: message` followed by `at fn (file:line:col)` frames; native
frames show as `at name (native)`.

## Language

Programs are scripts with strict-mode semantics (the semantics of an ES
module without imports/exports). Type annotations, interfaces, type aliases,
`declare` forms, `import type`/`export type`, `as`/`<T>`/`!`/`satisfies`
assertions are erased. TS runtime constructs supported: enums (numeric
with reverse mapping, string, computed-from-members), parameter
properties (defined before other fields, as with `useDefineForClassFields`),
abstract classes, `declare` fields, overload signatures.

Supported JavaScript: functions, arrows, closures, `var`/`let`/`const`
with TDZ, per-iteration loop bindings, destructuring with defaults and
rest, spread, classes (fields, static fields, accessors, `super`, computed
keys, derived constructors), getters/setters, optional chaining, nullish
coalescing, logical assignment, labeled `break`/`continue`, `switch`,
`for-in`/`for-of` with iterator closing, `try`/`catch`/`finally`,
template literals, `async`/`await`, `debugger`.

Builtins: `Object`, `Function.prototype` (`call`/`apply`/`bind`), `Array`,
`String`, `Number`, `Boolean`, `Symbol`, `Math` (deterministic `random`),
`JSON`, `Map`, `Set`, `Promise` (+ `all`/`allSettled`/`any`/`race`), the
error constructors, `setTimeout`/`setInterval`/`clear*`, `queueMicrotask`,
`globalThis`, `parseInt`/`parseFloat`/`isNaN`/`isFinite`, `console`
(`log`/`info`/`debug`/`trace`/`warn`/`error`/`dir`, output formatted like
Node's `util.inspect`).

### Rejected constructs

Every rejection is a diagnostic `file:line:col: error[code]: message` with
a caret line; all diagnostics of a file are reported together.

`error[unsupported]`: generators and async generators, `yield`,
`for await`, regular-expression and BigInt literals, decorators,
`accessor` fields, static blocks, `#private` members, namespaces and
non-`declare` module declarations, runtime `import`/`export`, `import =`,
`export =`, tagged templates, `arguments`, `with`, `import.meta`, dynamic
`import()`, `using`, JSX, `super` inside arrows or field initializers,
labeled function declarations, `delete` of an identifier / optional chain /
super property, `__proto__: v` in object literals, strings with lone
surrogates. `eval` and `new Function` exist but throw at run time.

`error[semantic]`: duplicate lexical declarations, unknown labels, illegal
`break`/`continue`, `super()` outside derived constructors, enum members
without an initializer after a string member, misplaced parameter
properties, lexical declarations in single-statement positions.

`error[parse]`: syntax errors and the early errors oxc reports (`return`
outside functions, `await` outside async functions, invalid assignment
targets, ...).

## Tests

`cargo test --workspace`. The end-to-end corpus `crates/dj_cli/tests/cases/*.ts`
is compared against `*.out`, recorded once from Node
(`crates/dj_cli/tests/gen_expected.sh`, run as strict ES modules) and run at
`-O0`, optimized, and optimized under GC stress. Separate suites cover
determinism and format round trips, diagnostics, stack traces, source/IR
maps and the debugger.

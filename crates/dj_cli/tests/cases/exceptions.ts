// try/catch/finally semantics and error objects.
function f1() {
  try { return "try"; } finally { console.log("finally runs"); }
}
console.log(f1());

function f2(): string {
  try { throw new Error("boom"); } catch (e) { return "caught " + (e as Error).message; } finally { console.log("f2 finally"); }
}
console.log(f2());

function f3() {
  // finally overrides return
  try { return 1; } finally { return 2; }
}
console.log(f3());

function f4() {
  for (let i = 0; i < 5; i++) {
    try {
      if (i === 1) continue;
      if (i === 3) break;
      console.log("body", i);
    } finally {
      console.log("finally", i);
    }
  }
  return "done";
}
console.log(f4());

function f5() {
  try {
    try { throw new TypeError("inner"); } finally { console.log("inner finally"); }
  } catch (e) {
    console.log("outer caught", (e as Error).name, (e as Error).message);
    try { throw e; } catch (e2) { console.log("rethrown same:", e === e2); }
  }
}
f5();

class ValidationError extends Error {
  constructor(message: string, public field: string) {
    super(message);
    this.name = "ValidationError";
  }
}
try {
  throw new ValidationError("bad input", "email");
} catch (e) {
  const v = e as ValidationError;
  console.log(v instanceof ValidationError, v instanceof Error, v.name, v.message, v.field, String(v));
}

try { throw { custom: true }; } catch (e) { console.log(e); }
try { throw 42; } catch { console.log("optional catch binding"); }

const errs = [new Error("e"), new TypeError("t"), new RangeError("r"), new ReferenceError("f"), new SyntaxError("s")];
console.log(errs.map(e => `${e.name}:${e.message}:${e instanceof Error}`).join(" "));
console.log(new Error("with cause", { cause: "root" }).cause, new Error().message === "", Error("no new").message);

function deep(n: number): number { return deep(n + 1); }
try { deep(0); } catch (e) { console.log("stack overflow ->", (e as Error).name, (e as Error).message); }

try { (undefined as any).foo; } catch (e) { console.log((e as Error).name, (e as Error).message); }
try { (null as any).foo = 1; } catch (e) { console.log((e as Error).name, (e as Error).message); }
try { (1 as any)(); } catch (e) { console.log((e as Error).name); }
try { new (class X { constructor() { throw new Error("ctor failed"); } })(); } catch (e) { console.log((e as Error).message); }
try { Object.freeze({ a: 1 }).a = 2; } catch (e) { console.log("frozen write:", (e as Error).name); }
try { const k = 1; (k as any) = 2; } catch (e) { console.log("const assign:", (e as Error).name); }
try { [].reduce((a: any, b: any) => a); } catch (e) { console.log((e as Error).message); }
try { JSON.parse("{bad"); } catch (e) { console.log((e as Error).name); }
try { new Array(-1); } catch (e) { console.log((e as Error).name, (e as Error).message); }
try { null!; undefinedVariable; } catch (e) { console.log((e as Error).name, (e as Error).message); }
declare var undefinedVariable: any;

let order: string[] = [];
function nested() {
  try {
    try {
      order.push("t1");
      throw 1;
    } finally {
      order.push("f1");
    }
  } catch {
    order.push("c2");
    return "r";
  } finally {
    order.push("f2");
  }
}
console.log(nested(), order.join(","));

// Closures, scoping, hoisting, TDZ.
function counter() {
  let n = 0;
  return { inc: () => ++n, get: () => n };
}
const c1 = counter(), c2 = counter();
c1.inc(); c1.inc(); c2.inc();
console.log(c1.get(), c2.get());

const fns: Array<() => number> = [];
for (let i = 0; i < 3; i++) fns.push(() => i);
console.log(fns.map(f => f()));

const vfns: Array<() => number> = [];
for (var j = 0; j < 3; j++) vfns.push(() => j);
console.log(vfns.map(f => f()));

const ofns: Array<() => string> = [];
for (const k of ["a", "b"]) ofns.push(() => k);
for (const k in { x: 1, y: 2 }) ofns.push(() => k);
console.log(ofns.map(f => f()));

function outer(x: number) {
  return function middle(y: number) {
    return function inner(z: number) {
      x++;
      return x + y + z;
    };
  };
}
const inner = outer(1)(10);
console.log(inner(100), inner(100));

console.log(hoisted(), typeof notYet);
function hoisted() { return "hoisted"; }
var notYet = 1;

try {
  console.log(tdz);
} catch (e) {
  console.log(e instanceof ReferenceError, (e as Error).message);
}
let tdz = 1;

function readLater() { return later; }
try { readLater(); } catch (e) { console.log("tdz via closure:", (e as Error).name); }
let later = "ok";
console.log(readLater());

const fact = function f(n: number): number { return n <= 1 ? 1 : n * f(n - 1); };
console.log(fact(10));

let shadow = "outer";
{
  let shadow = "block";
  console.log(shadow);
}
console.log(shadow);

function makeAdders() {
  const out = [];
  for (let i = 0; i < 3; i++) {
    let base = i * 10;
    out.push((v: number) => { base += v; return base; });
  }
  return out;
}
const adders = makeAdders();
console.log(adders[0](1), adders[0](1), adders[2](5));

let x = 1;
function setX() { x = 2; }
setX();
console.log(x);

const self = { name: "obj", arrow() { return [1].map(() => this.name); } };
console.log(self.arrow());

function defaults(a: number, b = a * 2, c = () => a + b) { a = 100; return [a, b, c()]; }
console.log(defaults(1), defaults(1, 5));

function rest(first: number, ...others: number[]) { return [first, others.length, others]; }
console.log(rest(1), rest(1, 2, 3));

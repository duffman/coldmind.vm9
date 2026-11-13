// Loops, labels, switch, for-in order, iterator protocol and closing.
outer: for (let i = 0; i < 3; i++) {
  for (let j = 0; j < 3; j++) {
    if (j === 1) continue outer;
    if (i === 2) break outer;
    console.log("pair", i, j);
  }
}

let n = 0;
while (true) { if (++n > 3) break; }
do { n--; } while (n > 0);
console.log("n =", n);

function sw(v: any) {
  const out: string[] = [];
  switch (v) {
    case 1: out.push("one");
    case 2: out.push("two"); break;
    default: out.push("default");
    case "3": out.push("three");
  }
  return out.join("+");
}
console.log(sw(1), sw(2), sw(3), sw("3"), sw(null));

const obj: any = { b: 1, a: 2, 10: "x", 2: "y", [Symbol("s")]: 3 };
obj.c = 4;
delete obj.a;
const keys: string[] = [];
for (const k in obj) keys.push(k);
console.log(keys);

class Proto { inherited = 1; }
const child = Object.create({ fromProto: true });
child.own = 1;
const ck: string[] = [];
for (const k in child) ck.push(k);
console.log(ck);

const iterable = {
  from: 1,
  to: 5,
  [Symbol.iterator]() {
    let cur = this.from;
    const to = this.to;
    return {
      next: () => (cur <= to ? { value: cur++, done: false } : { value: undefined, done: true }),
      return: () => { console.log("iterator closed at", cur); return { done: true, value: undefined }; },
    };
  },
};
for (const v of iterable) { if (v === 3) break; console.log("v", v); }
console.log([...iterable], Array.from(iterable, x => x * 10));
const [first, second, ...restItems] = iterable;
console.log(first, second, restItems);
const [only] = iterable;
console.log(only);

try {
  for (const v of iterable) { if (v === 2) throw new Error("stop"); }
} catch (e) { console.log("caught", (e as Error).message); }

function findFirst(): number {
  for (const v of iterable) { if (v > 1) return v; }
  return -1;
}
console.log(findFirst());

for (const ch of "héllo👍") show(ch);
function show(ch: string) { console.log(ch, ch.length); }

const m = new Map<string, number>([["x", 1]]);
m.set("y", 2);
for (const [k, v] of m) console.log(k, v);
for (const v of new Set([3, 3, 4])) console.log("set", v);
for (const [i, v] of ["a", "b"].entries()) console.log(i, v);

let count = 0;
for (;;) { if (count++ >= 2) break; }
console.log("count", count);

const arr = [1, 2, 3];
for (const x of arr) { if (x === 1) arr.push(4); }
console.log(arr);

for (var vi = 0; vi < 2; vi++) {}
console.log("vi after loop", vi);

let comma = (1, 2, 3);
console.log(comma, void 0);

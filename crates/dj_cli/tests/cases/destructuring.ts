// Destructuring, spread, rest, defaults.
const { a, b: renamed, c = "default", ...others } = { a: 1, b: 2, d: 4, e: 5 } as any;
console.log(a, renamed, c, others);

const [x, , y = 10, ...tail] = [1, 2, undefined, 4, 5];
console.log(x, y, tail);

let p = 1, q = 2;
[p, q] = [q, p];
console.log(p, q);

const { nested: { deep: [, second] } } = { nested: { deep: ["first", "second"] } };
console.log(second);

function params({ name, age = 30 }: { name: string; age?: number }, [h, ...t]: number[]) {
  return `${name}/${age}/${h}/${t}`;
}
console.log(params({ name: "ann" }, [1, 2, 3]));

const merged = { ...{ a: 1, b: 2 }, ...{ b: 3, c: 4 }, ...null, ...undefined, ..."hi" };
console.log(merged);

const arr = [..."abc", ...[1, 2], ...new Set([3])];
console.log(arr, Math.max(...[1, 5, 3]), [...[,1]]);

function sum(...nums: number[]) { return nums.reduce((s, n) => s + n, 0); }
console.log(sum(...[1, 2], 3, ...[4]));

const key = "dyn";
const { [key]: dynamic } = { dyn: "dynamic value" } as any;
console.log(dynamic);

let o: any = {};
({ a: o.first, b: o["second"] } = { a: "A", b: "B" });
console.log(o);

const { length } = "hello";
console.log(length);

try {
  const { z } = null as any;
} catch (e) {
  console.log((e as Error).name);
}

const users = [{ id: 1, tags: ["a"] }, { id: 2, tags: [] as string[] }];
for (const { id, tags: [firstTag = "none"] } of users) console.log(id, firstTag);

const obj = { method() { return "m"; }, get g() { return "getter"; }, ["comp" + "uted"]: true, 1: "one", shorthand: x };
console.log(obj, obj.method(), obj.g);

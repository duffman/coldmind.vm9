// console.log formatting (util.inspect compatible).
console.log(1, "str", true, null, undefined, -0, NaN, Symbol("sym"), Symbol());
console.log(["str", 1, null, undefined, true, -0]);
console.log({ a: 1, "b-c": 2, 3: "num", "valid_id": 4, "": 5, [Symbol("k")]: 6 });
console.log({ nested: { deeper: { deepest: { gone: 1 } } }, arr: [[[[1]]]] });
console.log(Array.from({ length: 30 }, (_, i) => i * 7));
console.log(Array.from({ length: 120 }, (_, i) => i));
console.log(["apple", "banana", "cherry", "date", "elderberry", "fig", "grape", "honeydew", "kiwi", "lemon"]);
console.log([1, , 3, , , 6], new Array(5), [undefined, , null]);
const longObj: any = {};
for (let i = 0; i < 8; i++) longObj["property" + i] = "value number " + i;
console.log(longObj);
console.log({ short: 1, list: [1, 2, 3], obj: { x: 1 } });
const circ: any = { name: "root", children: [] };
circ.self = circ;
circ.children.push({ parent: circ });
console.log(circ);
console.log(function named() {}, () => {}, class Foo {}, class extends Array {}, async function af() {}, Math.max);
const fnWithProps = Object.assign(function tagged() {}, { extra: 1 });
console.log(fnWithProps);
class Pt { constructor(public x: number, public y: number) {} }
class Empty {}
console.log(new Pt(1, 2), new Empty(), [new Pt(0, 0)], Object.create(null), Object.assign(Object.create(null), { a: 1 }));
console.log(new Map([["k", { v: [1, 2] }], [{ o: 1 } as any, "objkey"]]), new Set(["a", { b: 1 }]), new Map(), new Set());
console.log({ get g() { return 1; }, set s(v: any) {}, get gs() { return 1; }, set gs(v: any) {} });
console.log([new Error("in array").message], { e: new TypeError("t").name });
console.log("%s is %d years and %i, %f", "Bob", 42.5, 42.9, "3.5", "extra");
console.log("%j %o %%", 1, [1], "after");
console.log("%s", { a: 1 }, "%s");
console.log("a\nb", ["a\nb", "it's", 'say "hi"', `both ' and "`, "back`tick"]);
console.log([1.5, -2, 1e21, 1e-7, 0.000001, 123e-20, 2 ** 64]);
console.log({ arr: Array(200).fill(0).length, big: Array(101).fill(1) });
console.log([[1, 2, [3, 4, [5, [6]]]], { a: { b: { c: { d: {} } } } }]);
console.log({ s: "x".repeat(100) });
console.log(Object.assign([1, 2], { extra: "prop" }), Object.assign(new Map([[1, 2]]), {}));
console.log([Promise.resolve(1), new Promise(() => {}), Promise.reject(3).catch(() => {}) && 0]);
console.log(globalThis === globalThis, [true, false].map(String), String({}), `${[1, [2, 3]]}`, String(() => 1).length > 0);
console.log({ a: [{ b: 1, c: [1, 2, 3] }, { d: "x" }], e: new Map([[1, new Set([1])]]) });
console.log(Array.from({ length: 7 }, (_, i) => "item" + i), Array.from({ length: 27 }, (_, i) => "w" + i));

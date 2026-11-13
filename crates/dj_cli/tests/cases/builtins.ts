// Standard library behaviour.
const nums = [5, 1, 10, 2, 33];
console.log([...nums].sort(), [...nums].sort((a, b) => a - b), nums.indexOf(10), nums.includes(NaN), [NaN].includes(NaN), [NaN].indexOf(NaN));
console.log(nums.slice(1, -1), nums.slice(-2), [1, 2, 3, 4].splice(1, 2), nums.concat([6], 7));
console.log(nums.map(String), nums.filter(n => n > 4), nums.find(n => n > 5), nums.findIndex(n => n > 100), nums.findLast(n => n < 10), nums.findLastIndex(n => n > 1000));
console.log(nums.every(n => n > 0), nums.some(n => n > 30), nums.reduce((a, b) => a + b), nums.reduceRight((a, b) => a + "," + b, ""));
console.log([1, [2, [3, [4]]]].flat(), [1, [2, [3, [4]]]].flat(Infinity), [1, 2].flatMap(x => [x, x * 2]));
console.log(Array.from({ length: 3 }, (_, i) => i * i), Array.of(7, 8), Array.isArray([]), new Array(3).fill(0), [1, 2, 3].fill(9, 1));
console.log([3, 1, 2].reverse(), ["a", "b"].join(), [1, null, undefined, 2].join("-"), [1, 2, 3].at(-1), [..."ab"].keys().next());
console.log([1, 2, 3, 4, 5].copyWithin(0, 3), [1, 2, 3].lastIndexOf(3), [1, 2, 3].toString(), String([1, [2, 3]]));
const sorted = [{ k: 2, v: "b" }, { k: 1, v: "a" }, { k: 2, v: "c" }, { k: 1, v: "d" }].sort((x, y) => x.k - y.k);
console.log(sorted.map(o => o.v).join(""), [3, 1, 2].toSorted(), [1, 2, 3].toReversed(), [1, 2, 3].with(1, 9));
const arr: number[] = [1, 2, 3];
arr.length = 1;
arr[4] = 5;
console.log(arr, arr.length, arr.push(6), arr.pop(), arr.shift(), arr.unshift(0), arr);

const s = "Hello, World";
console.log(s.toUpperCase(), s.toLowerCase(), s.slice(-5), s.substring(7, 2), s.indexOf("o"), s.lastIndexOf("o"), s.charAt(4), s.charCodeAt(0), s.at(-1));
console.log(s.split(", "), s.split(""), "a,b,,c".split(",", 2), s.replace("l", "L"), s.replaceAll("l", "L"), s.startsWith("Hell"), s.endsWith("x"));
console.log("  pad ".trim() + "|", "x".padStart(3, "ab"), "x".padEnd(4, "-"), "ab".repeat(3), "abc".includes("bc"), String.fromCharCode(72, 105), "😀".codePointAt(0), String.fromCodePoint(128512));
console.log("b" > "a", "B" < "a", "10" < "9", 10 < 9, "abc".localeCompare("abd"), `${1 + 1}${"x"}`, "a-b-c".split("-").map(p => p.toUpperCase()).join(""));
console.log(s.concat("!", "?"), "Straße".length, "%", "a\tb\\n", JSON.stringify("q\"uote\n"));

console.log(0.1 + 0.2, 1 / 3, 1e21, 1e-7, 123456789012345680000, -0, 0 === -0, Object.is(0, -0), 2 ** 53, 2 ** 53 + 1, 5 % 3, -5 % 3, 7 / 0, -7 / 0, 0 / 0);
console.log((255).toString(16), (255).toString(2), (0.5).toString(2), (3.14159).toFixed(2), (1.005).toFixed(2), (1234.5678).toFixed(0), (0).toFixed(1), Number("  42  "), Number(""), Number("0x1f"), Number("1e3"), Number("abc"));
console.log(parseInt("42px"), parseInt("ff", 16), parseInt("0x10"), parseFloat("3.14abc"), parseFloat(".5"), Number.isInteger(5.0), Number.isSafeInteger(2 ** 53), Number.MAX_SAFE_INTEGER, Number.EPSILON > 0);
console.log(Math.max(), Math.min(1, 2), Math.round(2.5), Math.round(-2.5), Math.round(0.49999999999999994), Math.floor(-1.5), Math.ceil(1.1), Math.trunc(-1.9), Math.sign(-3), Math.abs(-2), Math.sqrt(16), Math.cbrt(27), Math.hypot(3, 4), Math.PI);
console.log(1 + "2", "3" * ("4" as any), true + 1, [] + [], [] + {}, null + 1, undefined + 1, "5" - 2, +"", +[], +{}, !!"", !!"0", null == undefined, null === undefined, NaN == NaN, "1" == 1 as any);
console.log(typeof null, typeof undefined, typeof 1, typeof "", typeof {}, typeof [], typeof (() => 1), typeof Symbol(), 7 & 3, 7 | 8, 5 ^ 1, ~5, 1 << 31, -1 >>> 0, -16 >> 2);

const o = { a: 1, b: { c: 2 } };
console.log(Object.keys(o), Object.values(o), Object.entries(o), Object.assign({}, o, { d: 3 }), Object.fromEntries([["x", 1], ["y", 2]]));
const frozen = Object.freeze({ f: 1 });
console.log(Object.isFrozen(frozen), Object.isFrozen(o), "a" in o, o.hasOwnProperty("b"), Object.hasOwn(o, "z"), Object.getOwnPropertyNames([1, 2]));
const withProps = Object.defineProperty({}, "hidden", { value: 42, enumerable: false });
console.log(withProps, withProps.hidden, Object.keys(withProps), JSON.stringify(Object.getOwnPropertyDescriptor(withProps, "hidden")));
const proto = { greet() { return "hi from proto"; } };
const inst = Object.create(proto);
console.log(inst.greet(), Object.getPrototypeOf(inst) === proto, proto.isPrototypeOf(inst), Object.prototype.toString.call([]), Object.prototype.toString.call(null), String(Symbol("desc")), Symbol("d").description);

console.log(JSON.stringify({ a: [1, "two", null, undefined, () => 1], b: undefined, c: 0,d: { e: NaN, f: Infinity } }));
console.log(JSON.stringify({ z: 1, y: [1, { x: 2 }] }, null, 2));
console.log(JSON.stringify([1, 2], null, "--"), JSON.stringify({ a: 1, b: 2, c: 3 }, ["a", "c"]), JSON.stringify({ a: 5 }, (k, v) => typeof v === "number" ? v * 2 : v));
console.log(JSON.stringify({ toJSON() { return "custom"; } }), JSON.stringify(" \u0007"), JSON.stringify(undefined), JSON.stringify(null));
console.log(JSON.parse('{"a":[1,2,{"b":null}],"c":"\\u0041"}'), JSON.parse("1e3"), JSON.parse(' "s" '), JSON.parse('[1,2]', (k, v) => Array.isArray(v) ? v.length : v * 10));

const map = new Map<any, any>([[1, "one"], ["1", "string one"]]);
const keyObj = {};
map.set(keyObj, "object").set(NaN, "nan");
console.log(map.get(1), map.get("1"), map.get(keyObj), map.get(NaN), map.size, map.has({}), map.delete(1), map.size, [...map.keys()]);
const set = new Set([1, 2, 2, "2", NaN, NaN]);
console.log(set.size, set.has("2"), [...set], set.delete(1), [...set.entries()][0]);
map.forEach((v, k) => { if (k === "1") map.delete(keyObj); });
console.log([...map.values()]);

console.log(isNaN("x" as any), isFinite("12" as any), Number.isNaN("x"), Boolean(0), Boolean([]), String(null), String([] === undefined));

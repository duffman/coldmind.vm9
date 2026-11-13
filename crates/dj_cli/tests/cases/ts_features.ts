// TypeScript-only syntax: erased types, enums, parameter properties, abstract classes.
interface Shape { area(): number; readonly name: string }
type Pair<T> = [T, T];
type Union = "a" | "b";

enum Color { Red, Green = 5, Blue }
enum Dir { Up = "UP", Down = "DOWN" }
enum Mixed { A = 1, B = A * 2, C = B + 1 }
console.log(Color.Red, Color.Green, Color.Blue, Color[6], Dir.Up, Mixed.C, Color);
console.log(Dir, Object.keys(Color));

abstract class Base implements Shape {
  abstract area(): number;
  abstract readonly name: string;
  describe(): string { return `${this.name}: ${this.area().toFixed(2)}`; }
}
class Circle extends Base {
  readonly name = "circle";
  constructor(private readonly r: number) { super(); }
  area() { return Math.PI * this.r ** 2; }
}
class Square extends Base {
  name = "square";
  constructor(protected side: number, public label?: string) { super(); }
  override area() { return this.side * this.side; }
}
const shapes: Shape[] = [new Circle(1), new Square(2, "sq")];
for (const s of shapes) console.log((s as Base).describe());
console.log(shapes[1]);

function identity<T>(v: T): T { return v; }
function first<T extends unknown[]>(xs: T): T[0] { return xs[0]; }
const pair: Pair<number> = [1, 2];
console.log(identity<string>("generic"), first(pair), identity(pair) satisfies Pair<number>);

function overloaded(x: string): string;
function overloaded(x: number): number;
function overloaded(x: any) { return x; }
console.log(overloaded(1), overloaded("s"));

declare const declared: number;
declare function declaredFn(): void;
declare class DeclaredClass {}
declare namespace DeclaredNs { const x: number; }
declare module "some-module" { export const y: number; }
declare global { interface Window { custom: string } }

const maybe: { a?: { b?: { c: number } }; f?: () => number } = { a: { b: { c: 3 } } };
console.log(maybe.a?.b?.c, maybe.a?.["b"]?.c, maybe.f?.(), (maybe as any).x?.y.z, maybe.a!.b!.c);
const val = null ?? "fallback";
let assign: number | null = null;
assign ??= 10;
assign ||= 20;
assign &&= assign + 1;
console.log(val, assign, 0 ?? 1, 0 || 1, "" && "x");

const tuple = [1, "two"] as const;
const cfg = <const>{ mode: "dev" };
const casted = <number>(42 as unknown);
console.log(tuple, cfg, casted, typeof (null as unknown as string));

let definite!: string;
definite = "assigned";
console.log(definite);

type Fn = (this: { v: number }, add: number) => number;
const withThis: Fn = function (add) { return this.v + add; };
console.log(withThis.call({ v: 1 }, 2));

class Getters {
  private _v = 0;
  get v(): number { return this._v; }
  set v(n: number) { this._v = n * 2; }
  static readonly CONST: number = 7;
  declare declaredField: string;
}
const g = new Getters();
g.v = 4;
console.log(g.v, Getters.CONST, Object.keys(g), "declaredField" in g);

function assertIsString(v: unknown): asserts v is string {
  if (typeof v !== "string") throw new TypeError("not a string");
}
function isNum(v: unknown): v is number { return typeof v === "number"; }
const unk: unknown = "text";
assertIsString(unk);
console.log(unk.toUpperCase(), isNum(3), isNum("3"));

const obj = { k: 1 } satisfies Record<string, number>;
const keyOf: keyof typeof obj = "k";
console.log(obj[keyOf]);

import type { Foo } from "./nonexistent";
export type { Shape };
let typed: Foo | undefined;
console.log(typed);

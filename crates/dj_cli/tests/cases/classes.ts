// Classes, inheritance, super, accessors, statics, fields.
class Animal {
  static count = 0;
  legs = 4;
  constructor(public name: string) { Animal.count++; }
  speak() { return `${this.name} makes a sound`; }
  get description() { return `${this.name} (${this.legs} legs)`; }
  set nickname(v: string) { this.name = v.toUpperCase(); }
  static create(n: string) { return new this(n); }
  toString() { return `Animal(${this.name})`; }
}

class Bird extends Animal {
  legs = 2;
  constructor(name: string, public canFly = true) { super(name); }
  speak() { return super.speak() + " (tweet)"; }
  get description() { return "bird: " + super.description; }
}

const a = new Animal("cat");
const b = new Bird("tweety");
console.log(a.speak(), "|", b.speak());
console.log(a.description, "|", b.description);
b.nickname = "polly";
console.log(b.name, `${a}`, String(b), Animal.count);
console.log(b instanceof Bird, b instanceof Animal, a instanceof Bird, Object.getPrototypeOf(b) === Bird.prototype);
console.log(a, b);
const c = Bird.create("sparrow");
console.log(c instanceof Bird, c.legs, Animal.count);
console.log(Object.keys(b), typeof Animal, Animal.name, Bird.name);

class Point {
  constructor(public x: number, public y: number) {}
  static origin = new Point(0, 0);
  ["computed" + "Name"]() { return "computed"; }
  add(o: Point) { return new Point(this.x + o.x, this.y + o.y); }
}
const p = new Point(1, 2).add(new Point(3, 4));
console.log(p, Point.origin, p.computedName());

class Base {
  constructor() { this.init(); }
  init() { console.log("Base.init"); }
}
class Derived extends Base {
  field = "set";
  init() { console.log("Derived.init, field =", this.field); }
}
new Derived();

try {
  // @ts-ignore
  Animal("x");
} catch (e) { console.log(e instanceof TypeError); }

class NoSuper extends Base {
  constructor() {
    try {
      // @ts-ignore
      this.x = 1;
    } catch (e) {
      console.log("before super:", (e as Error).name);
    }
    super();
  }
}
new NoSuper();

const Anon = class {};
class Ext extends Anon {}
console.log(Anon, Ext, Anon.name, new Ext());

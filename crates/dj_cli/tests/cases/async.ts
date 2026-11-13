// Promises, async/await, microtask and timer ordering.
const log: string[] = [];

setTimeout(() => log.push("timeout 0"), 0);
setTimeout(() => log.push("timeout 10"), 10);
setTimeout(() => log.push("timeout 5"), 5);
queueMicrotask(() => log.push("microtask"));
Promise.resolve().then(() => log.push("then 1")).then(() => log.push("then 2"));
log.push("sync");

async function a1() {
  log.push("a1 start");
  await null;
  log.push("a1 after await");
  return "a1 result";
}
a1().then(v => log.push(v));

async function thrower() {
  await 1;
  throw new Error("async boom");
}
thrower().catch(e => log.push("caught: " + e.message));

async function tryAwait() {
  try {
    await Promise.reject(new TypeError("rejected"));
  } catch (e) {
    return "handled " + (e as Error).name;
  } finally {
    log.push("tryAwait finally");
  }
}
tryAwait().then(v => log.push(v!));

const delay = (ms: number, v: string) => new Promise<string>(res => setTimeout(() => res(v), ms));

async function main() {
  const r = await Promise.all([delay(20, "x"), "plain", Promise.resolve("p")]);
  console.log("all:", r);
  console.log("race:", await Promise.race([delay(30, "slow"), delay(15, "fast")]));
  const settled = await Promise.allSettled([Promise.reject(new Error("no")), delay(1, "yes")]);
  console.log("allSettled:", settled.map(s => s.status === "fulfilled" ? s.value : (s.reason as Error).message));
  try {
    await Promise.any([Promise.reject(new Error("a")), Promise.reject(new Error("b"))]);
  } catch (e) {
    console.log("any:", (e as any).name, (e as any).errors.length);
  }
  console.log("any ok:", await Promise.any([Promise.reject(1), delay(2, "winner")]));

  const order: number[] = [];
  await Promise.all([3, 1, 2].map(async n => { await delay(n * 3, ""); order.push(n); }));
  console.log("order:", order);

  let resolveOut!: (v: number) => void;
  const pending = new Promise<number>(r => { resolveOut = r; });
  setTimeout(() => resolveOut(99), 1);
  console.log("deferred:", await pending);

  const thenable = { then(res: (v: string) => void) { res("thenable value"); } };
  console.log(await thenable);

  const p = Promise.resolve(1);
  console.log(Promise.resolve(p) === p, await p.finally(() => "ignored"));

  class Api {
    base = "api";
    async get(path: string) { await null; return `${this.base}/${path}`; }
    static async make() { return new Api(); }
  }
  const api = await Api.make();
  console.log(await api.get("users"));

  const arrow = async (x: number) => x * 2;
  console.log(await arrow(21));

  let ticks = 0;
  const id = setInterval(() => { if (++ticks === 3) clearInterval(id); }, 2);
  await delay(50, "");
  console.log("interval ticks:", ticks);

  const cleared = setTimeout(() => console.log("SHOULD NOT RUN"), 1);
  clearTimeout(cleared);

  console.log("log:", log);
  console.log(await Promise.resolve(Promise.resolve("nested")));
  try {
    await new Promise((_, rej) => rej("string reason"));
  } catch (e) { console.log("reason:", e); }
  const chained = await Promise.reject(new Error("x")).then(() => "no", e => "recovered " + e.message);
  console.log(chained);
}
main().then(() => console.log("main done"));
console.log("end of script");

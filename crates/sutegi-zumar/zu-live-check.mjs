// Drives the .zu-authored live server (scaffolded by demo-zu-live.sh)
// through the real zumar-live.js client. Proves the program compiled from
// counter.zu behaves live: clicks, a server-side `every` tick, and
// reconnect-by-replay — none of which is hand-written Rust.
import { join } from "node:path";

const zumar = process.env.ZUMAR;
const PORT = process.env.PORT || "8798";
const { connect } = await import(join(zumar, "www/zumar-live.js"));
const { decodeInit, decodeUpdate } = await import(join(zumar, "www/zumar-wire.js"));

// Fresh session id per run — the journal is persistent (SQLite), so reusing
// a fixed id across runs would replay a prior run's inputs on mount. The
// reconnect test below reuses THIS run's id on purpose.
const SID = "zu-" + Math.abs(Date.now() ^ (Math.random() * 1e9)).toString(36);

let passed = 0;
let failed = 0;
const check = (n, c) => {
  c ? passed++ : failed++;
  console.log(`${c ? "ok" : "FAIL"} - ${n}`);
};
const nodeAt = (root, p) => p.reduce((n, i) => n.children[i], root);
const COUNT = [2, 1, 0]; // span.count in counter.zu's view

async function session(id) {
  const app = await connect(`ws://127.0.0.1:${PORT}/live?session=${id}`);
  const init = decodeInit(app.init());
  const state = { count: nodeAt(init.root, COUNT).text };
  const waiters = [];
  app.onUpdate((b) => {
    for (const p of decodeUpdate(b).patches)
      if (p.op === "setText" && p.path.join() === COUNT.join()) state.count = p.text;
    waiters.splice(0).forEach((w) => w());
  });
  const settle = (pred, what, ms = 5000) =>
    new Promise((res, rej) => {
      const t0 = Date.now();
      const look = () =>
        pred() ? res() : Date.now() - t0 > ms ? rej(new Error(what)) : waiters.push(look);
      look();
    });
  const click = (p) =>
    app.dispatch(Uint32Array.from(p), "click", undefined, undefined, undefined);
  return { app, state, settle, click };
}

const a = await session(SID);
check("counter.zu mounts live: count 0", a.state.count === "0");

a.click([2, 2]); // +
a.click([2, 2]);
a.click([2, 2]);
await a.settle(() => a.state.count === "3", "3 clicks");
check("clicks drive the .zu program server-side to 3", true);

a.click([3, 0]); // start ticker — a server-side every(1000) from the .zu `sub`
const base = Number(a.state.count);
await a.settle(() => Number(a.state.count) >= base + 2, "two server ticks");
a.click([3, 0]); // stop
check("server-side `every` (from .zu sub) advanced the count", true);

const at = a.state.count;
a.app.close();
await a.app.closed;
await new Promise((r) => setTimeout(r, 200));
const b = await session(SID); // reconnect, same session
check(`reconnect: journal replay restored count ${at}`, b.state.count === at);

a.app.close?.();
b.app.close();
console.log(`\n${passed}/${passed + failed} checks passed`);
process.exit(failed ? 1 : 0);

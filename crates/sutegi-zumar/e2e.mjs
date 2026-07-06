// E2E for the live bridge: spawns the counter_live example and drives it
// through the REAL client stack (zumar-live.js + zumar-wire.js from the
// sibling zumar checkout). Proves the P3 claims:
//   - server-side delay / every / httpGet (unsolicited updates arrive)
//   - cmds/subs never reach the client (stripped from every frame)
//   - reconnect with the same session id replays the journal → same state
//
//   node e2e.mjs

import { spawn, spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const zumarWww = join(here, "../../../zumar/www");
const { connect } = await import(join(zumarWww, "zumar-live.js"));
const { decodeInit, decodeUpdate } = await import(join(zumarWww, "zumar-wire.js"));

const PORT = process.env.PORT || "8796";
const DB = `/tmp/counter-live-e2e-${process.pid}.db`;

let passed = 0;
let failed = 0;
const check = (name, cond) => {
  cond ? passed++ : failed++;
  console.log(`${cond ? "ok" : "FAIL"} - ${name}`);
};

const build = spawnSync("cargo", ["build", "-q", "--example", "counter_live"], {
  cwd: here,
  stdio: "inherit",
});
if (build.status !== 0) process.exit(1);

const server = spawn(join(here, "target/debug/examples/counter_live"), [], {
  env: { ...process.env, HOST: "127.0.0.1", PORT, LIVE_DB: DB },
  stdio: "ignore",
});
process.on("exit", () => server.kill("SIGTERM"));
for (let i = 0; ; i++) {
  try {
    if ((await fetch(`http://127.0.0.1:${PORT}/api/hello`)).ok) break;
  } catch {}
  if (i > 100) throw new Error("server never came up");
  await new Promise((r) => setTimeout(r, 50));
}

// paths in counter_live's view: count text [2,1,0] · greeting [4,0]
// buttons: - [2,0] · + [2,2] · lazy [3,0] · ticker [3,1] · fetch [3,2]
const COUNT = [2, 1, 0];
const GREET = [4, 0];

// A session that tracks state from decoded frames like the shim would.
async function session(id) {
  const app = await connect(`ws://127.0.0.1:${PORT}/live?session=${id}`);
  const init = decodeInit(app.init());
  const state = {
    count: nodeAt(init.root, COUNT).text,
    greeting: nodeAt(init.root, GREET).text,
    strippedEverywhere: init.cmds.length === 0 && init.subs.length === 0,
    updates: 0,
  };
  const waiters = [];
  app.onUpdate((bytes) => {
    const u = decodeUpdate(bytes);
    state.updates++;
    if (u.cmds.length || u.subs.length) state.strippedEverywhere = false;
    for (const p of u.patches) {
      if (p.op === "setText" && p.path.join() === COUNT.join()) state.count = p.text;
      if (p.op === "setText" && p.path.join() === GREET.join()) state.greeting = p.text;
    }
    for (const w of waiters.splice(0)) w();
  });
  const settle = (pred, what, ms = 3000) =>
    new Promise((resolve, reject) => {
      const t0 = Date.now();
      const look = () => {
        if (pred()) return resolve();
        if (Date.now() - t0 > ms) return reject(new Error(`timeout: ${what}`));
        waiters.push(look);
        setTimeout(look, 100);
      };
      look();
    });
  const click = (path) =>
    app.dispatch(Uint32Array.from(path), "click", undefined, undefined, undefined);
  return { app, state, click, settle };
}

const nodeAt = (root, path) => path.reduce((n, i) => n.children[i], root);

// --- a full session ------------------------------------------------------

const a = await session("e2e-alpha");
check("mount: count 0", a.state.count === "0");

a.click([2, 2]);
a.click([2, 2]);
a.click([2, 2]);
await a.settle(() => a.state.count === "3", "3 clicks");
check("clicks: server patches to 3", true);

a.click([3, 0]); // lazy +10: a server-side delay(1000)
await a.settle(() => a.state.count === "13", "server delay fired", 4000);
check("server-side delay: unsolicited +10 update arrived", true);

a.click([3, 2]); // fetch: server-side httpGet against its own /api
await a.settle(() => a.state.greeting.includes("fetched server-side"), "server httpGet");
check("server-side httpGet: greeting from the app's own API", true);

a.click([3, 1]); // ticker on: server-side every(1000)
const before = Number(a.state.count);
await a.settle(() => Number(a.state.count) >= before + 2, "two ticks", 5000);
a.click([3, 1]); // ticker off
check("server-side every: ticker advanced the count", true);
await new Promise((r) => setTimeout(r, 150));
const frozen = a.state.count;
await new Promise((r) => setTimeout(r, 1500));
check("sub stop: ticker actually stopped", a.state.count === frozen);
check("protocol: cmds/subs stripped from every frame", a.state.strippedEverywhere);

// --- reconnect: the novel bit --------------------------------------------

const finalCount = a.state.count;
a.app.close();
await a.app.closed;
await new Promise((r) => setTimeout(r, 200));

const b = await session("e2e-alpha"); // same session id → journal replay
check(
  `reconnect: journal replay restores count ${finalCount}`,
  b.state.count === finalCount
);
check("reconnect: greeting survived too", b.state.greeting.includes("fetched server-side"));
b.click([2, 2]);
await b.settle(() => Number(b.state.count) === Number(finalCount) + 1, "post-replay click");
check("reconnected session keeps working", true);

const c = await session("e2e-other"); // different session → fresh state
check("a different session starts fresh at 0", c.state.count === "0");

b.app.close();
c.app.close();
server.kill("SIGTERM");
console.log(`\n${passed}/${passed + failed} checks passed`);
process.exit(failed ? 1 : 0);

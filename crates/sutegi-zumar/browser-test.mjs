// Real-browser check for live mode: headless Chrome over CDP (no deps).
// The one thing e2e.mjs can't prove: mountLive's localStorage session +
// reconnect loop in a real page — click to a count, RELOAD THE TAB, and
// the journal replay brings the state back.
//
//   node browser-test.mjs

import { spawn, spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const PORT = process.env.PORT || "8797";
const CDP = "9225";
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const DB = `/tmp/counter-live-browser-${process.pid}.db`;

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
const chrome = spawn(
  CHROME,
  [
    "--headless=new",
    `--remote-debugging-port=${CDP}`,
    `--user-data-dir=/tmp/sutegi-zumar-chrome-${process.pid}`,
    "--no-first-run",
    "about:blank",
  ],
  { stdio: "ignore" }
);
const cleanup = () => {
  server.kill("SIGTERM");
  chrome.kill("SIGTERM");
};
process.on("exit", cleanup);

const until = async (f, what, tries = 100) => {
  for (let i = 0; i < tries; i++) {
    try {
      const v = await f();
      if (v) return v;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`timed out waiting for ${what}`);
};

await until(() => fetch(`http://127.0.0.1:${PORT}/api/hello`).then((r) => r.ok), "server");
const target = await until(
  () =>
    fetch(`http://127.0.0.1:${CDP}/json/new?http://127.0.0.1:${PORT}/`, { method: "PUT" }).then(
      (r) => r.json()
    ),
  "chrome CDP"
);
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r, j) => {
  ws.onopen = r;
  ws.onerror = j;
});
let msgId = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
  }
};
const evaluate = (expression) =>
  new Promise((resolve) => {
    const id = ++msgId;
    pending.set(id, (m) => resolve(m.result?.result?.value));
    ws.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression } }));
  });
const count = () => evaluate(`document.querySelector(".count")?.textContent`);
const click = (sel) =>
  evaluate(
    `(() => { const b = document.querySelector(${JSON.stringify(sel)}); if (b) b.click(); return !!b; })()`
  );

await until(async () => (await count()) === "0", "live mount");
check("page mounts: count 0, state on the server", true);

for (let i = 0; i < 4; i++) await click(".row button:last-child");
await until(async () => (await count()) === "4", "count 4");
check("real clicks drive the server-side model to 4", true);

await click("button.lazy");
await until(async () => (await count()) === "14", "server delay lands in the DOM", 40);
check("server-side delay patches the real DOM (+10 after 1s)", true);

// THE test: reload the tab. Same localStorage session → journal replay.
await evaluate("location.reload()");
await until(async () => (await count()) === "14", "state back after reload", 60);
check("tab reload: journal replay restores count 14", true);

await click(".row button:last-child");
await until(async () => (await count()) === "15", "post-reload click");
check("replayed session keeps working (15)", true);

ws.close();
cleanup();
console.log(`\n${passed}/${passed + failed} browser checks passed`);
process.exit(failed ? 1 : 0);

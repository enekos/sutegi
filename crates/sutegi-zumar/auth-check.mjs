// auth-check.mjs — the P5 claims, proven over the real wire (no browser).
//
// The one thing unit tests can't show: the session cookie riding the
// WebSocket upgrade, `Live::guard` refusing an anonymous socket, the factory
// seeding the logged-in user's name into the mounted program, and a live
// form round-tripping via a server-side httpPost — all with no CSRF token.
//
// Uses node's global fetch + WebSocket (undici); WebSocket takes a `headers`
// option, so we can put the login cookie on the upgrade exactly as a browser
// would. Frames are decoded with the real zumar-wire.js.
//
//   ZUMAR=/path/to/zumar PORT=8794 node auth-check.mjs

import { join } from "node:path";

const zumar = process.env.ZUMAR;
const PORT = process.env.PORT || "8794";
const { decodeInit, decodeUpdate } = await import(join(zumar, "www/zumar-wire.js"));

let passed = 0;
let failed = 0;
const check = (n, c) => {
  c ? passed++ : failed++;
  console.log(`${c ? "ok" : "FAIL"} - ${n}`);
};

// --- outbound frame encoder (mirror of www/zumar-live.js) ---
class Writer {
  constructor() {
    this.b = [];
  }
  u8(n) {
    this.b.push(n & 0xff);
    return this;
  }
  vu(n) {
    while (n >= 0x80) {
      this.b.push((n & 0x7f) | 0x80);
      n >>>= 7;
    }
    this.b.push(n);
    return this;
  }
  str(s) {
    const bytes = new TextEncoder().encode(s);
    this.vu(bytes.length);
    for (const x of bytes) this.b.push(x);
    return this;
  }
  bytes() {
    return Uint8Array.from(this.b);
  }
}
const dispatchFrame = (path, name, value) => {
  const w = new Writer().u8(1).u8(0).vu(path.length);
  for (const p of path) w.vu(p);
  w.str(name);
  const flags = typeof value === "string" ? 1 : 0;
  w.u8(flags);
  if (flags & 1) w.str(value);
  return w.bytes();
};

// --- helpers ---
const textsOf = (node, out) => {
  if (!node) return out;
  if (node.kind === "text") out.push(node.text);
  for (const c of node.children ?? []) textsOf(c, out);
  return out;
};
const patchTexts = (patches) => {
  const out = [];
  for (const p of patches) {
    if (p.op === "setText") out.push(p.text);
    if (p.op === "replace") textsOf(p.node, out);
    if (p.op === "insertChild") textsOf(p.node, out);
    if (p.op === "appendChildren") for (const n of p.nodes) textsOf(n, out);
  }
  return out;
};
const open = (cookie) =>
  new WebSocket(
    `ws://127.0.0.1:${PORT}/account/live?session=false`,
    cookie ? { headers: { Cookie: cookie } } : undefined
  );

const base = `http://127.0.0.1:${PORT}`;

// 1. anonymous socket → the guard must close it before any mount.
{
  const ws = open(null);
  ws.binaryType = "arraybuffer";
  let mounted = false;
  const code = await new Promise((resolve) => {
    ws.onmessage = () => (mounted = true);
    ws.onclose = (e) => resolve(e.code);
    ws.onerror = () => {};
    setTimeout(() => resolve(-1), 4000);
  });
  check("anonymous socket is closed by Live::guard (1008)", code === 1008);
  check("anonymous socket never mounts a program", !mounted);
}

// 2. log in — the signed session cookie comes back on the response.
const login = await fetch(`${base}/login`, {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ email: "eneko@join.com", password: "password1" }),
});
const setCookie = login.headers.getSetCookie?.() ?? [];
const cookie = setCookie.map((c) => c.split(";")[0]).join("; ");
check("login succeeds and returns a session cookie", login.ok && cookie.includes("sutegi_session"));

// 3. authenticated socket → mounts, greeted by the name the factory read
//    from the cookie (session in the live mount).
const ws = open(cookie);
ws.binaryType = "arraybuffer";
const updates = [];
const waiters = [];
let initTexts = [];
await new Promise((resolve, reject) => {
  ws.onmessage = (e) => {
    const bytes = new Uint8Array(e.data);
    if (initTexts.length === 0 && !ws.__mounted) {
      ws.__mounted = true;
      initTexts = textsOf(decodeInit(bytes).root, []);
      resolve();
    } else {
      updates.push(...patchTexts(decodeUpdate(bytes).patches));
      waiters.splice(0).forEach((w) => w());
    }
  };
  ws.onclose = (e) => reject(new Error(`socket closed ${e.code}`));
  ws.onerror = () => reject(new Error("socket error"));
  setTimeout(() => reject(new Error("no initial render")), 5000);
});
check(
  "authenticated socket mounts, greeted by name (session in live mount)",
  initTexts.some((t) => t.includes("hi Eneko"))
);

// 4. live form: set the note, submit. onSubmit dispatches over the
//    authenticated socket; the bridge POSTs it server-side; the echo comes
//    back as a patch. No CSRF token anywhere.
const seen = (needle) => updates.some((t) => t.includes(needle));
const settle = (needle, ms = 5000) =>
  new Promise((res, rej) => {
    if (seen(needle)) return res();
    const timer = setTimeout(() => rej(new Error(`never saw "${needle}"`)), ms);
    const look = () => {
      if (seen(needle)) {
        clearTimeout(timer);
        res();
      } else waiters.push(look);
    };
    waiters.push(look);
  });
ws.send(dispatchFrame([1, 0], "input", "buy milk")); // type into .note
ws.send(dispatchFrame([1], "submit")); // submit .noteform
await settle("buy milk");
check("live form save round-trips via httpPost (CSRF-free by construction)", true);

ws.close();
console.log(`\n${passed}/${passed + failed} wire checks passed`);
process.exit(failed ? 1 : 0);

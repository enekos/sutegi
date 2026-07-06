// Two clients on the chat.zu live server: Alice sends, BOB must receive.
// That cross-connection delivery is the whole P4 payoff — a `publish` from
// one connection's program reaches every connection subscribed to the topic.
//
// Rather than mirror the DOM, each client accumulates every text string that
// appears in any patch it receives — enough to assert "the message arrived".
import { join } from "node:path";

const zumar = process.env.ZUMAR;
const PORT = process.env.PORT || "8799";
const { connect } = await import(join(zumar, "www/zumar-live.js"));
const { decodeInit, decodeUpdate } = await import(join(zumar, "www/zumar-wire.js"));

let passed = 0;
let failed = 0;
const check = (n, c) => {
  c ? passed++ : failed++;
  console.log(`${c ? "ok" : "FAIL"} - ${n}`);
};

// Pull every text string out of a SerNode subtree (text nodes + children).
function textsOf(node, out) {
  if (!node) return out;
  if (node.kind === "text") out.push(node.text);
  for (const c of node.children ?? []) textsOf(c, out);
  return out;
}
// Every text a patch introduces: setText payloads + text inside appended/
// inserted/replaced nodes.
function patchTexts(patches) {
  const out = [];
  for (const p of patches) {
    if (p.op === "setText") out.push(p.text);
    if (p.op === "replace") textsOf(p.node, out);
    if (p.op === "insertChild") textsOf(p.node, out);
    if (p.op === "appendChildren") for (const n of p.nodes) textsOf(n, out);
  }
  return out;
}

async function client() {
  const app = await connect(`ws://127.0.0.1:${PORT}/live?session=false`);
  const received = [];
  textsOf(decodeInit(app.init()).root, received); // initial texts (none, empty log)
  const waiters = [];
  app.onUpdate((b) => {
    received.push(...patchTexts(decodeUpdate(b).patches));
    waiters.splice(0).forEach((w) => w());
  });
  const saw = (needle) => received.some((t) => t.includes(needle));
  const settle = (needle, ms = 4000) =>
    new Promise((res, rej) => {
      if (saw(needle)) return res();
      const timer = setTimeout(() => rej(new Error(`never saw "${needle}"`)), ms);
      const look = () => {
        if (saw(needle)) {
          clearTimeout(timer);
          res();
        } else {
          waiters.push(look); // re-armed on the next update
        }
      };
      waiters.push(look);
    });
  const type = (path, value) =>
    app.dispatch(Uint32Array.from(path), "input", value, undefined, undefined);
  const send = () => app.dispatch(Uint32Array.from([3, 2]), "click", undefined, undefined, undefined);
  return { app, saw, settle, type, send };
}

const alice = await client();
const bob = await client();
check("two clients mount chat.zu", true);

// name field [3,0], msg field [3,1], send button [3,2]
alice.type([3, 0], "alice");
alice.type([3, 1], "hello bob");
alice.send();

await alice.settle("alice: hello bob");
check("sender sees the message", true);
await bob.settle("alice: hello bob");
check("OTHER client receives it (cross-connection pubsub fan-out)", true);

// Bob replies, Alice hears it — bidirectional.
bob.type([3, 0], "bob");
bob.type([3, 1], "hi alice");
bob.send();
await alice.settle("bob: hi alice");
check("fan-out is bidirectional", true);

alice.app.close();
bob.app.close();
console.log(`\n${passed}/${passed + failed} checks passed`);
process.exit(failed ? 1 : 0);

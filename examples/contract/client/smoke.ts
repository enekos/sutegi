import { channels, ContractError, createClient, type ChannelSocket, type NoteChange } from "./api.ts";

const base = (globalThis as { process?: { argv: string[] } }).process?.argv[2] ?? "http://127.0.0.1:4790";
const api = createClient({ base });

let failures = 0;
function check(label: string, ok: boolean, detail?: unknown) {
  if (ok) {
    console.log(`ok   ${label}`);
  } else {
    failures++;
    console.log(`FAIL ${label}`, detail ?? "");
  }
}

async function rejects(p: Promise<unknown>): Promise<ContractError | null> {
  try {
    await p;
    return null;
  } catch (e) {
    return e instanceof ContractError ? e : null;
  }
}

function waitFor<T>(register: (resolve: (v: T) => void) => void, ms = 2000): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out waiting")), ms);
    register((v) => {
      clearTimeout(timer);
      resolve(v);
    });
  });
}

async function http() {
  const empty = await api.notes.list();
  check("list starts empty", empty.total === 0 && empty.items.length === 0, empty);

  const bad = await rejects(api.notes.create({ title: "" }));
  check("422 carries field errors", bad?.status === 422 && !!bad.errors?.title, bad);

  const a = await api.notes.create({ title: "Ship contracts", body: "typed routes and streams for sutegi", tags: ["work"] });
  const b = await api.notes.create({ title: "Groceries", tags: ["home"] });
  check("create returns the note", a.id === 1 && b.id === 2 && b.body === "" && !a.pinned, [a, b]);

  const got = await api.notes.get({ id: a.id });
  check("get binds the path param", got.title === "Ship contracts", got);

  const work = await api.notes.list({ tag: "work" });
  check("query input filters", work.total === 1 && work.items[0]?.id === a.id, work);

  const one = await api.notes.list({ limit: 1 });
  check("query numbers coerce", one.items.length === 1 && one.total === 2, one);

  const tooMany = await rejects(api.notes.list({ limit: 500 }));
  check("query rules apply", tooMany?.status === 422 && !!tooMany.errors?.limit, tooMany);

  const missing = await rejects(api.notes.get({ id: 99 }));
  check("404 is a ContractError", missing?.status === 404 && missing.message === "note 99 does not exist", missing);

  const s = api.notes.summarize({ id: a.id, words: 4 });
  const seen: string[] = [];
  for await (const ev of s) {
    if (ev.t === "started") seen.push(`started:${ev.words}`);
    else seen.push(ev.text);
  }
  const summary = await s.result;
  check(
    "stream yields typed events then the result",
    seen.join("|") === "started:4|Ship|contracts|typed|routes" && summary.text === "Ship contracts typed routes" && summary.words === 4,
    { seen, summary },
  );

  const gone = api.notes.summarize({ id: 99 });
  let iterErr: unknown = null;
  try {
    for await (const _ of gone) {
    }
  } catch (e) {
    iterErr = e;
  }
  check(
    "stream refused before opening",
    iterErr instanceof ContractError && iterErr.status === 404 && (await rejects(gone.result))?.status === 404,
    iterErr,
  );

  const long = api.notes.summarize({ id: a.id, words: 20 });
  let n = 0;
  let aborted = false;
  try {
    for await (const _ of long) {
      if (++n === 2) long.abort();
    }
  } catch (e) {
    aborted = (e as Error).name === "AbortError";
  }
  check("abort stops the stream", aborted && n === 2, { n, aborted });

  await api.notes.delete({ id: b.id });
  const after = await api.notes.list();
  check("delete returns null and removes", after.total === 1, after);
}

async function live() {
  const src = await (await fetch(`${base}/channels.js`)).text();
  const mod = { exports: {} as { SutegiSocket: new (path: string, opts?: unknown) => ChannelSocket & { connect(): void; disconnect(): void } } };
  const g = globalThis as { location?: { protocol: string; host: string } };
  g.location ??= { protocol: new URL(base).protocol, host: new URL(base).host };
  new Function("module", src)(mod);
  const socket = new mod.exports.SutegiSocket(base.replace(/^http/, "ws") + "/live");
  socket.connect();

  const refused = await rejects(channels.notes(socket, "main", { viewer: "" }).joined);
  check("typed join validates", !!refused?.errors?.viewer, refused);

  const ch = channels.notes(socket, "main", { viewer: "ada" });
  const joined = await ch.joined;
  check("typed join replies", joined.notes === 1, joined);

  const created = waitFor<NoteChange>((r) => ch.on("change", r));
  await api.notes.create({ title: "From HTTP" });
  const c = await created;
  check("HTTP broadcast reaches the typed channel", c.kind === "created" && c.title === "From HTTP", c);

  const pinned = waitFor<NoteChange>((r) =>
    ch.on("change", (p) => {
      if (p.kind === "pinned") r(p);
    }),
  );
  const note = await ch.push("pin", { id: 1, pinned: true });
  const p = await pinned;
  check("typed push replies and emits", note.pinned && p.kind === "pinned" && p.by === "ada", { note, p });

  const nope = await rejects(ch.push("pin", { id: 404, pinned: true }));
  check("typed push errors reject", nope?.message === "note 404 does not exist", nope);

  ch.leave();
  socket.disconnect();
}

await http();
await live();
console.log(failures === 0 ? "all checks passed" : `${failures} check(s) failed`);
if (failures > 0) throw new Error("smoke failed");

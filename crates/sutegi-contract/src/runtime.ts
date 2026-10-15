export interface ContractErrorBody {
  error: string;
  errors?: Record<string, string[]>;
}

export class ContractError extends Error {
  readonly status: number;
  readonly errors: Record<string, string[]> | undefined;
  constructor(status: number, body: ContractErrorBody) {
    super(body.error);
    this.name = "ContractError";
    this.status = status;
    this.errors = body.errors;
  }
}

export interface ClientOptions {
  base?: string;
  fetch?: typeof fetch;
  headers?: () => Record<string, string> | Promise<Record<string, string>>;
  credentials?: RequestCredentials;
}

export interface Streamed<E, O> extends AsyncIterable<E> {
  readonly result: Promise<O>;
  abort(): void;
}

type Bag = Record<string, unknown>;

function fillPath(path: string, params: readonly string[], input: unknown): [string, unknown] {
  if (params.length === 0) return [path, input];
  const rest: Bag = { ...((input ?? {}) as Bag) };
  const url = path
    .split("/")
    .map((seg) => {
      if (!seg.startsWith(":")) return seg;
      const name = seg.slice(1);
      const value = rest[name];
      delete rest[name];
      if (value === undefined || value === null) {
        throw new ContractError(0, { error: `missing path parameter "${name}"` });
      }
      return encodeURIComponent(String(value));
    })
    .join("/");
  return [url, rest];
}

function queryString(input: unknown): string {
  if (input === undefined || input === null || typeof input !== "object") return "";
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(input as Bag)) {
    if (v === undefined || v === null) continue;
    q.set(k, typeof v === "object" ? JSON.stringify(v) : String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

async function failure(res: Response): Promise<ContractError> {
  const text = await res.text();
  let body: ContractErrorBody = { error: text || `request failed (${res.status})` };
  try {
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed.error === "string") body = parsed;
  } catch {}
  return new ContractError(res.status, body);
}

function runtime(options: ClientOptions) {
  const base = options.base ?? "";
  const doFetch: typeof fetch = options.fetch ?? ((input, init) => fetch(input, init));

  async function send(
    method: string,
    path: string,
    params: readonly string[],
    input: unknown,
    accept: string,
    signal?: AbortSignal,
  ): Promise<Response> {
    const [url, rest] = fillPath(path, params, input);
    const headers: Record<string, string> = {
      accept,
      ...(options.headers ? await options.headers() : {}),
    };
    let target = base + url;
    let body: string | undefined;
    if (method === "GET") {
      target += queryString(rest);
    } else if (rest !== undefined) {
      headers["content-type"] = "application/json";
      body = JSON.stringify(rest);
    }
    const res = await doFetch(target, { method, headers, body, credentials: options.credentials, signal });
    if (!res.ok) throw await failure(res);
    return res;
  }

  async function call<O>(method: string, path: string, params: readonly string[], input: unknown): Promise<O> {
    const res = await send(method, path, params, input, "application/json");
    const text = await res.text();
    return (text ? JSON.parse(text) : null) as O;
  }

  function stream<E, O>(method: string, path: string, params: readonly string[], input: unknown): Streamed<E, O> {
    const controller = new AbortController();
    const queue: E[] = [];
    let wake: (() => void) | null = null;
    let finished = false;
    let failed: unknown = undefined;
    let resolveResult!: (o: O) => void;
    let rejectResult!: (e: unknown) => void;
    const result = new Promise<O>((resolve, reject) => {
      resolveResult = resolve;
      rejectResult = reject;
    });
    result.catch(() => {});
    const poke = () => {
      const w = wake;
      wake = null;
      if (w) w();
    };

    (async () => {
      let done = false;
      try {
        const res = await send(method, path, params, input, "text/event-stream", controller.signal);
        if (!res.body) throw new ContractError(res.status, { error: "response has no body" });
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buf = "";
        for (;;) {
          const chunk = await reader.read();
          if (chunk.done) break;
          buf += decoder.decode(chunk.value, { stream: true }).replace(/\r\n/g, "\n");
          let cut: number;
          while ((cut = buf.indexOf("\n\n")) >= 0) {
            const frame = buf.slice(0, cut);
            buf = buf.slice(cut + 2);
            let event = "message";
            const data: string[] = [];
            for (const line of frame.split("\n")) {
              if (line.startsWith("event:")) event = line.slice(6).trim();
              else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
            }
            if (data.length === 0) continue;
            const payload = JSON.parse(data.join("\n"));
            if (event === "item") {
              queue.push(payload as E);
              poke();
            } else if (event === "done") {
              done = true;
              resolveResult(payload as O);
            } else if (event === "error") {
              throw new ContractError(typeof payload.status === "number" ? payload.status : 500, payload);
            }
          }
        }
        if (!done) throw new ContractError(0, { error: "stream ended before it finished" });
      } catch (e) {
        failed = e;
        rejectResult(e);
      } finally {
        finished = true;
        poke();
      }
    })();

    return {
      result,
      abort: () => controller.abort(),
      async *[Symbol.asyncIterator]() {
        for (;;) {
          const next = queue.shift();
          if (next !== undefined) {
            yield next;
            continue;
          }
          if (finished) {
            if (failed !== undefined) throw failed;
            return;
          }
          await new Promise<void>((resolve) => {
            wake = resolve;
          });
        }
      },
    };
  }

  return { call, stream };
}


export interface ChannelPush {
  receive(status: "ok" | "error" | "timeout", cb: (response: any) => void): ChannelPush;
}

export interface ChannelHandle {
  on(event: string, cb: (payload: any) => void): unknown;
  join(): ChannelPush;
  push(event: string, payload: unknown): ChannelPush;
  leave(): unknown;
}

export interface ChannelSocket {
  channel(topic: string, params?: unknown): ChannelHandle;
}

export interface TypedChannel<J, Emits, Handles extends { [K in keyof Handles]: [unknown, unknown] }> {
  readonly joined: Promise<J>;
  readonly raw: ChannelHandle;
  on<K extends keyof Emits & string>(event: K, cb: (payload: Emits[K]) => void): void;
  push<K extends keyof Handles & string>(event: K, payload: Handles[K][0]): Promise<Handles[K][1]>;
  leave(): void;
}

function refusal(response: any): ContractError {
  const body: ContractErrorBody =
    response && typeof response.error === "string" ? response : { error: String(response?.reason ?? "refused") };
  return new ContractError(0, body);
}

function settlePush<T>(push: ChannelPush): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    push
      .receive("ok", (response) => resolve(response as T))
      .receive("error", (response) => reject(refusal(response)))
      .receive("timeout", () => reject(new ContractError(0, { error: "timed out" })));
  });
}

function typedChannel<J, Emits, Handles extends { [K in keyof Handles]: [unknown, unknown] }>(
  handle: ChannelHandle,
): TypedChannel<J, Emits, Handles> {
  let settled = false;
  let resolveJoin!: (j: J) => void;
  let rejectJoin!: (e: unknown) => void;
  const joined = new Promise<J>((resolve, reject) => {
    resolveJoin = resolve;
    rejectJoin = reject;
  });
  joined.catch(() => {});
  const join = handle.join.bind(handle);
  handle.join = () =>
    join()
      .receive("ok", (response) => {
        if (!settled) {
          settled = true;
          resolveJoin(response as J);
        }
      })
      .receive("error", (response) => {
        if (!settled) {
          settled = true;
          rejectJoin(refusal(response));
        }
      });
  handle.join();
  return {
    joined,
    raw: handle,
    on: (event, cb) => {
      handle.on(event, cb);
    },
    push: (event, payload) => settlePush(handle.push(event, payload)),
    leave: () => {
      handle.leave();
    },
  };
}

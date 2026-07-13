/* sutegi-channels — dependency-free browser client.
 *
 * Mirrors the wire protocol documented at /__channels: one JSON envelope
 * {topic, event, ref, join_ref, payload} per WebSocket text frame; control
 * events under the "stg:" prefix; heartbeats on the "stg" topic.
 *
 *   const socket = new SutegiSocket("/channels");
 *   socket.connect();
 *   const room = socket.channel("room:1", {nick: "ada"});
 *   room.on("new_msg", (payload) => render(payload));
 *   room.join()
 *     .receive("ok",    (resp) => console.log("joined", resp))
 *     .receive("error", (resp) => console.log("refused", resp));
 *   room.push("new_msg", {body: "hello"}).receive("ok", () => {});
 *
 * Reconnect: automatic with capped exponential backoff; every joined
 * channel rejoins (fresh join_ref, so frames from the previous life are
 * discarded). Delivery is at-most-once — messages sent while disconnected
 * are gone; design for it.
 */
"use strict";

const STG = {
  join: "stg:join",
  leave: "stg:leave",
  reply: "stg:reply",
  error: "stg:error",
  close: "stg:close",
};

class Push {
  constructor(timeoutMs) {
    this._callbacks = {};
    this._received = null;
    this._timer = timeoutMs
      ? setTimeout(() => this._resolve("timeout", {}), timeoutMs)
      : null;
  }
  receive(status, cb) {
    if (this._received && this._received.status === status) cb(this._received.response);
    (this._callbacks[status] = this._callbacks[status] || []).push(cb);
    return this;
  }
  _resolve(status, response) {
    if (this._received) return;
    if (this._timer) clearTimeout(this._timer);
    this._received = { status, response };
    (this._callbacks[status] || []).forEach((cb) => cb(response));
  }
}

class Channel {
  constructor(socket, topic, params) {
    this.socket = socket;
    this.topic = topic;
    this.params = params || {};
    this.joinRef = null;
    this.state = "closed"; // closed | joining | joined | leaving
    this._bindings = {};
    this._joinPush = null;
    this._pending = []; // pushes queued until joined
  }

  on(event, cb) {
    (this._bindings[event] = this._bindings[event] || []).push(cb);
    return this;
  }

  join() {
    this.state = "joining";
    this.joinRef = this.socket._nextRef();
    this._joinPush = this.socket._push(
      this.topic, STG.join, this.params, this.joinRef, this.joinRef
    );
    this._joinPush
      .receive("ok", () => {
        this.state = "joined";
        this._pending.splice(0).forEach((send) => send());
      })
      .receive("error", () => (this.state = "closed"))
      .receive("timeout", () => (this.state = "closed"));
    return this._joinPush;
  }

  push(event, payload) {
    const ref = this.socket._nextRef();
    const push = new Push(this.socket.timeoutMs);
    const send = () =>
      this.socket._send(this.topic, event, payload, ref, this.joinRef, push);
    if (this.state === "joined") send();
    else if (this.state === "joining") this._pending.push(send);
    else push._resolve("error", { reason: "not joined" });
    return push;
  }

  leave() {
    const push = this.socket._push(
      this.topic, STG.leave, {}, this.socket._nextRef(), this.joinRef
    );
    this.state = "closed";
    this._joinPush = null; // an intentional leave must not auto-rejoin
    return push;
  }

  _handle(env) {
    // Discard frames from a previous membership instance.
    if (env.join_ref && this.joinRef && env.join_ref !== this.joinRef) return;
    if (env.event === STG.close) {
      this.state = "closed";
      (this._bindings["stg:close"] || []).forEach((cb) => cb(env.payload));
      return;
    }
    (this._bindings[env.event] || []).forEach((cb) => cb(env.payload));
  }

  _rejoin() {
    if (this._joinPush) this.join(); // joined (or joining) before the drop
  }
}

class SutegiSocket {
  constructor(path, opts) {
    opts = opts || {};
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    this.url = path.startsWith("ws")
      ? path
      : `${proto}//${location.host}${path}`;
    this.timeoutMs = opts.timeout || 10000;
    this.heartbeatMs = opts.heartbeat || 30000;
    this.channels = [];
    this.ws = null;
    this._ref = 0;
    this._replies = {}; // ref -> Push
    this._backoff = 250;
    this._heartbeatTimer = null;
    this._closedByUser = false;
    this.onError = opts.onError || (() => {});
  }

  connect() {
    this._closedByUser = false;
    this.ws = new WebSocket(this.url);
    this.ws.onopen = () => {
      this._backoff = 250;
      this._startHeartbeat();
      this.channels.forEach((ch) => ch._rejoin());
    };
    this.ws.onmessage = (e) => this._onFrame(e.data);
    this.ws.onclose = () => {
      clearInterval(this._heartbeatTimer);
      this.channels.forEach((ch) => {
        if (ch.state === "joined" || ch.state === "joining") ch.state = "closed";
      });
      if (this._closedByUser) return;
      const delay = this._backoff;
      this._backoff = Math.min(this._backoff * 2, 10000);
      setTimeout(() => this.connect(), delay);
    };
    return this;
  }

  disconnect() {
    this._closedByUser = true;
    if (this.ws) this.ws.close();
  }

  channel(topic, params) {
    const ch = new Channel(this, topic, params);
    this.channels.push(ch);
    return ch;
  }

  _startHeartbeat() {
    clearInterval(this._heartbeatTimer);
    this._heartbeatTimer = setInterval(() => {
      this._push("stg", "heartbeat", {}, this._nextRef(), null).receive(
        "timeout",
        () => this.ws && this.ws.close() // dead link: force the reconnect path
      );
    }, this.heartbeatMs);
  }

  _nextRef() {
    return String(++this._ref);
  }

  _push(topic, event, payload, ref, joinRef) {
    const push = new Push(this.timeoutMs);
    this._send(topic, event, payload, ref, joinRef, push);
    return push;
  }

  _send(topic, event, payload, ref, joinRef, push) {
    if (push && ref) this._replies[ref] = push;
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      if (push) push._resolve("timeout", { reason: "socket not open" });
      return;
    }
    const env = { topic, event, payload };
    if (ref) env.ref = ref;
    if (joinRef) env.join_ref = joinRef;
    this.ws.send(JSON.stringify(env));
  }

  _onFrame(data) {
    let env;
    try {
      env = JSON.parse(data);
    } catch (_e) {
      return;
    }
    if (env.event === STG.reply && env.ref) {
      const push = this._replies[env.ref];
      delete this._replies[env.ref];
      if (push) push._resolve(env.payload.status, env.payload.response);
      return;
    }
    if (env.event === STG.error) {
      this.onError(env);
      return;
    }
    this.channels
      .filter((ch) => ch.topic === env.topic)
      .forEach((ch) => ch._handle(env));
  }
}

/* Usable as a plain <script> (globals) or as an ES module via a wrapper. */
if (typeof window !== "undefined") {
  window.SutegiSocket = SutegiSocket;
  window.SutegiChannel = Channel;
}
if (typeof module !== "undefined" && module.exports) {
  module.exports = { SutegiSocket, Channel, Push };
}

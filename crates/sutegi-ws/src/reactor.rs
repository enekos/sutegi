//! The connection engine: sharded event loops that own every upgraded
//! socket in the process.
//!
//! ## Shape
//!
//! `WsRuntime::start` spawns one reactor thread per shard (default: one per
//! core). Each shard owns a poller (kqueue/epoll), a slab of connection
//! states, and a self-pipe for cross-thread wake-ups. An adopted socket is
//! made non-blocking, registered with the shard's poller, and from then on
//! costs **no thread** — an idle connection is a slab slot of a couple
//! hundred bytes plus its kernel socket buffers. That, and nothing else, is
//! what makes very large fleets (hundreds of thousands to millions of
//! sockets per process) feasible.
//!
//! ## Threading contract
//!
//! `on_open` / `on_message` / `on_close` run **inline on the shard thread**,
//! like a Phoenix channel process: per-connection ordering is guaranteed and
//! there is no cross-thread handoff on the hot path. The flip side: a
//! blocking handler stalls every connection on that shard. Do CPU-quick work
//! inline; push slow work (DB, upstream calls) to your own threads and reply
//! through the cloned [`Conn`] handle, which is `Send + Sync` and safe to
//! use from anywhere.
//!
//! ## Backpressure
//!
//! Outbound bytes queue per connection up to `max_buffered`; a consumer that
//! can't keep up is disconnected rather than allowed to pin process memory.
//! Broadcasts share one encoded buffer (`Arc`) across all queues, so fanning
//! a frame to a million sockets allocates it once.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sutegi_http::Request;

use crate::poller::{raise_nofile_limit, Poller, WakePipe};
use crate::protocol::{
    decode_frame, encode_close, encode_frame, parse_close, Opcode, ProtocolError,
};

/// Reserved poller token for the shard's wake pipe.
const WAKE_TOKEN: usize = usize::MAX;
/// How long to wait for a close handshake to complete before dropping.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// Cap on bytes read per connection per loop iteration, so one firehose
/// client cannot starve the rest of the shard (level-triggered polling
/// re-fires for whatever stays in the kernel buffer).
const READ_QUANTUM: usize = 128 * 1024;
/// Cap on frames *parsed* per connection per loop iteration. Bytes alone
/// don't bound work: a client can pack thousands of tiny control frames into
/// one read, and answering them all inline (e.g. a PONG per PING) would
/// starve every other connection on the shard. Past this the connection is
/// parked on `pending_parse` and resumed round-robin next iteration.
const FRAMES_PER_WAKE: usize = 256;
/// Floor the inbound buffer keeps after a burst drains — enough to hold the
/// next few frames without reallocating, without pinning a large spike.
const RBUF_FLOOR: usize = 16 * 1024;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Runtime tuning knobs. `Default` is production-shaped.
#[derive(Clone, Debug)]
pub struct WsConfig {
    /// Reactor threads. 0 = one per available core.
    pub shards: usize,
    /// Largest single frame accepted (close code 1009 past this). Default 1 MiB.
    pub max_frame: usize,
    /// Largest assembled message across fragments. Default 1 MiB.
    pub max_message: usize,
    /// Idle time before the server pings. Default 30s.
    pub ping_interval: Duration,
    /// Idle time (no bytes at all) before the connection is dropped.
    /// Must exceed `ping_interval`. Default 75s.
    pub idle_timeout: Duration,
    /// Per-connection outbound queue cap; a slower consumer is dropped.
    /// Default 1 MiB.
    pub max_buffered: usize,
    /// Process-wide connection cap. Default 1_048_576.
    pub max_connections: usize,
    /// Max concurrent connections from a single source IP (0 = unlimited).
    /// Bounds single-source connection-exhaustion — one IP can otherwise eat
    /// the whole `max_connections` pool. Default 1024 (generous enough for a
    /// shared NAT, tight enough that exhausting the pool needs 1000+ hosts).
    pub max_connections_per_ip: u32,
    /// Raise `RLIMIT_NOFILE` toward the hard cap at startup. Default true.
    pub raise_nofile: bool,
}

impl Default for WsConfig {
    fn default() -> WsConfig {
        WsConfig {
            shards: 0,
            max_frame: 1 << 20,
            max_message: 1 << 20,
            ping_interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(75),
            max_buffered: 1 << 20,
            max_connections: 1 << 20,
            max_connections_per_ip: 1024,
            raise_nofile: true,
        }
    }
}

/// A complete inbound message, fragments already assembled and text already
/// UTF-8-validated.
#[derive(Debug)]
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
}

/// `on_open(conn, upgrade_request)` callback.
pub type OnOpen = Arc<dyn Fn(&Conn, &Request) + Send + Sync>;
/// `on_message(conn, msg)` callback.
pub type OnMessage = Arc<dyn Fn(&Conn, Msg) + Send + Sync>;
/// `on_close(conn, close_code)` callback.
pub type OnClose = Arc<dyn Fn(&Conn, u16) + Send + Sync>;
/// `authorize(upgrade_request) -> allow` gate, run in the HTTP worker
/// **before** the `101` is written (unlike `on_open`, which runs after the
/// upgrade is already committed). This is where cross-site protection lives.
pub type Authorize = Arc<dyn Fn(&Request) -> bool + Send + Sync>;

/// Per-endpoint callbacks. `on_*` run inline on the shard thread (see module
/// docs for the threading contract); `authorize`/`allowed_origins` run in the
/// HTTP worker during the handshake.
#[derive(Clone, Default)]
pub struct Handlers {
    /// Connection adopted. Receives the upgrade [`Request`] (path params,
    /// headers, cookies) for identity decisions. Runs *after* the 101 — for
    /// admission control use `authorize`/`allowed_origins` instead.
    pub on_open: Option<OnOpen>,
    /// A complete text/binary message arrived.
    pub on_message: Option<OnMessage>,
    /// Connection gone (clean close code, or 1006 for a dirty drop). The
    /// handle is already dead: sends from inside `on_close` are no-ops.
    pub on_close: Option<OnClose>,
    /// If set, the handshake is refused with `403` unless this returns `true`.
    /// Checked before the 101 so a rejected client never becomes a socket.
    pub authorize: Option<Authorize>,
    /// If set, the `Origin` header must be present and exactly match one of
    /// these — the built-in defense against Cross-Site WebSocket Hijacking
    /// (a cookie-authenticated socket opened by an attacker's page). Checked
    /// before `authorize`.
    pub allowed_origins: Option<Vec<String>>,
}

/// Pre-encode a text frame for fan-out: `Conn::send_shared` clones only the
/// `Arc`, so a million-connection broadcast encodes the payload exactly once.
pub fn text_frame(text: &str) -> Arc<Vec<u8>> {
    Arc::new(encode_frame(Opcode::Text, text.as_bytes(), true))
}

/// Pre-encode a binary frame for fan-out (see [`text_frame`]).
pub fn binary_frame(data: &[u8]) -> Arc<Vec<u8>> {
    Arc::new(encode_frame(Opcode::Binary, data, true))
}

// ---------------------------------------------------------------------------
// Cross-thread command surface
// ---------------------------------------------------------------------------

/// Everything needed to take over an upgraded socket. Boxed inside
/// [`Cmd::Adopt`] so the rare, large adopt payload doesn't bloat the
/// per-message `Send` variant that dominates the inbox on a broadcast.
struct AdoptData {
    stream: TcpStream,
    leftover: Vec<u8>,
    handlers: Arc<Handlers>,
    req: Request,
    /// Source IP (for per-IP release), already reserved by the runtime.
    ip: Option<String>,
}

enum Cmd {
    Adopt(Box<AdoptData>),
    Send {
        token: u32,
        gen: u32,
        buf: Arc<Vec<u8>>,
    },
    /// One frame to many connections on the same shard — a broadcast that
    /// takes the shard lock once instead of once per target.
    SendMany {
        targets: Vec<(u32, u32)>,
        buf: Arc<Vec<u8>>,
    },
    Close {
        token: u32,
        gen: u32,
        code: u16,
        reason: String,
    },
}

/// The half of a shard visible to other threads: an injection queue plus a
/// wake pipe. `wake_pending` dedups wake bytes so a hot sender can't fill
/// the pipe.
struct ShardShared {
    inbox: Mutex<Vec<Cmd>>,
    pipe: WakePipe,
    wake_pending: AtomicBool,
}

impl ShardShared {
    fn inject(&self, cmd: Cmd) {
        self.inbox.lock().unwrap().push(cmd);
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            self.pipe.wake();
        }
    }
}

/// A cheap, cloneable, thread-safe handle to one live connection.
///
/// Sends are fire-and-forget: if the connection has closed (or closes before
/// the reactor drains the queue), the frame is dropped silently — exactly
/// the semantics a broadcast loop wants. Generation counters make a stale
/// handle harmless even after its slab slot is reused.
#[derive(Clone)]
pub struct Conn {
    id: u64,
    token: u32,
    gen: u32,
    shard: Arc<ShardShared>,
}

impl Conn {
    /// Process-unique id, stable for the life of the connection. Key your
    /// rosters/maps with this.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn send_text(&self, text: &str) {
        self.send_shared(&text_frame(text));
    }

    pub fn send_binary(&self, data: &[u8]) {
        self.send_shared(&binary_frame(data));
    }

    /// Enqueue a pre-encoded frame (from [`text_frame`]/[`binary_frame`]) to
    /// this one connection. To fan a frame out to many, prefer [`broadcast`],
    /// which takes each shard's lock once instead of once per connection.
    pub fn send_shared(&self, frame: &Arc<Vec<u8>>) {
        self.shard.inject(Cmd::Send {
            token: self.token,
            gen: self.gen,
            buf: Arc::clone(frame),
        });
    }

    /// Start a graceful close: sends a close frame, then drops the socket
    /// once the peer acknowledges (or after a grace period).
    pub fn close(&self, code: u16, reason: &str) {
        self.shard.inject(Cmd::Close {
            token: self.token,
            gen: self.gen,
            code,
            reason: reason.to_string(),
        });
    }
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn").field("id", &self.id).finish()
    }
}

/// Fan one pre-encoded frame out to many connections, grouping targets by
/// shard so each shard's inbox lock is taken **once** — the difference
/// between `shards` lock cycles and one-per-connection at a million sockets.
/// Encode the frame once with [`text_frame`]/[`binary_frame`] and pass it here.
///
/// ```ignore
/// let frame = text_frame(&msg);
/// broadcast(roster.values(), &frame);   // roster: HashMap<u64, Conn>
/// ```
pub fn broadcast<'a, I>(conns: I, frame: &Arc<Vec<u8>>)
where
    I: IntoIterator<Item = &'a Conn>,
{
    // Group by shard identity (the Arc's pointer), carrying one Arc clone of
    // the shard per group so we can inject after the grouping pass.
    type ShardGroup = (Arc<ShardShared>, Vec<(u32, u32)>);
    let mut by_shard: HashMap<usize, ShardGroup> = HashMap::new();
    for conn in conns {
        let key = Arc::as_ptr(&conn.shard) as usize;
        by_shard
            .entry(key)
            .or_insert_with(|| (Arc::clone(&conn.shard), Vec::new()))
            .1
            .push((conn.token, conn.gen));
    }
    for (_, (shard, targets)) in by_shard {
        shard.inject(Cmd::SendMany {
            targets,
            buf: Arc::clone(frame),
        });
    }
}

// ---------------------------------------------------------------------------
// Admission control
// ---------------------------------------------------------------------------

/// Global + per-IP admission, shared by the runtime (which *reserves* a slot
/// before the `101` is written) and the shards (which *release* on teardown).
/// Reserving up front makes the cap a hard bound: a burst of upgrades can't
/// overshoot it in the window between an advisory check and a deferred
/// increment, because the count moves atomically at admission time.
struct Admission {
    total: AtomicUsize,
    max_total: usize,
    per_ip: Mutex<HashMap<String, u32>>,
    max_per_ip: u32,
}

impl Admission {
    /// Try to reserve one slot for `ip`. Returns false (nothing reserved) if
    /// the global or per-IP cap is already met.
    fn reserve(&self, ip: Option<&str>) -> bool {
        let prev = self.total.fetch_add(1, Ordering::AcqRel);
        if prev >= self.max_total {
            self.total.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        if self.max_per_ip > 0 {
            if let Some(ip) = ip {
                let mut map = self.per_ip.lock().unwrap();
                let n = map.entry(ip.to_string()).or_insert(0);
                if *n >= self.max_per_ip {
                    drop(map);
                    self.total.fetch_sub(1, Ordering::AcqRel);
                    return false;
                }
                *n += 1;
            }
        }
        true
    }

    /// Release a previously reserved slot.
    fn release(&self, ip: Option<&str>) {
        self.total.fetch_sub(1, Ordering::AcqRel);
        if self.max_per_ip > 0 {
            if let Some(ip) = ip {
                let mut map = self.per_ip.lock().unwrap();
                if let Some(n) = map.get_mut(ip) {
                    *n -= 1;
                    if *n == 0 {
                        map.remove(ip);
                    }
                }
            }
        }
    }

    fn count(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    fn has_capacity(&self) -> bool {
        self.total.load(Ordering::Relaxed) < self.max_total
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// The process-wide WebSocket engine. Start once, adopt every upgraded
/// socket into it.
pub struct WsRuntime {
    shards: Vec<Arc<ShardShared>>,
    next_shard: AtomicUsize,
    admission: Arc<Admission>,
}

impl WsRuntime {
    /// Spawn the reactor threads and return the shared runtime handle.
    pub fn start(cfg: WsConfig) -> io::Result<Arc<WsRuntime>> {
        if cfg.raise_nofile {
            raise_nofile_limit();
        }
        let n = if cfg.shards == 0 {
            thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4)
        } else {
            cfg.shards
        };
        let admission = Arc::new(Admission {
            total: AtomicUsize::new(0),
            max_total: cfg.max_connections,
            per_ip: Mutex::new(HashMap::new()),
            max_per_ip: cfg.max_connections_per_ip,
        });
        let mut shards = Vec::with_capacity(n);
        for i in 0..n {
            let shared = Arc::new(ShardShared {
                inbox: Mutex::new(Vec::new()),
                pipe: WakePipe::new()?,
                wake_pending: AtomicBool::new(false),
            });
            let poller = Poller::new()?;
            poller.add(shared.pipe.read_fd, WAKE_TOKEN, false)?;
            let shard = Shard {
                shared: Arc::clone(&shared),
                poller,
                slab: Vec::new(),
                gens: Vec::new(),
                free: Vec::new(),
                pending_parse: Vec::new(),
                cfg: cfg.clone(),
                admission: Arc::clone(&admission),
                last_sweep: Instant::now(),
                scratch: vec![0u8; 64 * 1024],
            };
            thread::Builder::new()
                .name(format!("sutegi-ws-{i}"))
                .spawn(move || shard.run())?;
            shards.push(shared);
        }
        Ok(Arc::new(WsRuntime {
            shards,
            next_shard: AtomicUsize::new(0),
            admission,
        }))
    }

    /// Hand an upgraded socket to a shard. `leftover` is whatever the HTTP
    /// parser had already buffered past the upgrade request; `req` is the
    /// upgrade request itself, passed to `on_open`.
    ///
    /// Reserves the connection slot atomically here — before returning to the
    /// HTTP worker that wrote the 101 — so the global and per-IP caps are hard
    /// bounds, not counts a burst can race past. Returns `Err` (and adopts
    /// nothing) when a cap is met.
    pub fn adopt(
        &self,
        stream: TcpStream,
        leftover: Vec<u8>,
        handlers: Arc<Handlers>,
        req: Request,
    ) -> io::Result<()> {
        let ip = req.peer_ip();
        if !self.admission.reserve(ip.as_deref()) {
            return Err(io::Error::other("connection cap reached"));
        }
        let i = self.next_shard.fetch_add(1, Ordering::Relaxed) % self.shards.len();
        self.shards[i].inject(Cmd::Adopt(Box::new(AdoptData {
            stream,
            leftover,
            handlers,
            req,
            ip,
        })));
        Ok(())
    }

    /// Live connection count (admitted, not yet released) across all shards.
    pub fn connections(&self) -> usize {
        self.admission.count()
    }

    /// Whether a new connection would currently be admitted (advisory: the
    /// authoritative reservation happens in [`adopt`](Self::adopt)).
    pub fn has_capacity(&self) -> bool {
        self.admission.has_capacity()
    }
}

// ---------------------------------------------------------------------------
// Per-connection state
// ---------------------------------------------------------------------------

struct OutBuf {
    buf: Arc<Vec<u8>>,
    pos: usize,
}

struct ConnState {
    stream: TcpStream,
    id: u64,
    /// Unparsed inbound bytes (bounded: frames are refused from their length
    /// field, and reads are quantum-capped per loop).
    rbuf: Vec<u8>,
    /// In-flight fragmented message: (type, bytes so far).
    assembly: Option<(Opcode, Vec<u8>)>,
    out: VecDeque<OutBuf>,
    out_bytes: usize,
    want_write: bool,
    /// Set once we've sent a close frame; holds the drop deadline and the
    /// code to report to `on_close`.
    closing: Option<(Instant, u16)>,
    /// The peer already sent its close frame: drop the socket the moment our
    /// echo flushes (RFC 6455 §5.5.1 — the server closes first).
    peer_closed: bool,
    last_activity: Instant,
    awaiting_pong: bool,
    handlers: Arc<Handlers>,
    /// Source IP, for releasing the per-IP reservation on teardown.
    peer: Option<String>,
}

// ---------------------------------------------------------------------------
// Shard
// ---------------------------------------------------------------------------

struct Shard {
    shared: Arc<ShardShared>,
    poller: Poller,
    slab: Vec<Option<ConnState>>,
    /// Generation per slot, bumped on removal: stale `Conn` handles (and
    /// queued commands) for a reused slot are recognized and dropped.
    gens: Vec<u32>,
    free: Vec<u32>,
    /// Connections that hit the per-wakeup frame cap with bytes still
    /// buffered — resumed round-robin next iteration so no single peer can
    /// monopolize the shard thread.
    pending_parse: Vec<u32>,
    cfg: WsConfig,
    admission: Arc<Admission>,
    last_sweep: Instant,
    scratch: Vec<u8>,
}

impl Shard {
    fn run(mut self) {
        let mut events = Vec::with_capacity(1024);
        loop {
            // Don't block for a second when connections are mid-parse waiting
            // to resume (frame-cap fairness) — poll and come straight back.
            let timeout = if self.pending_parse.is_empty() {
                Duration::from_secs(1)
            } else {
                Duration::ZERO
            };
            if let Err(e) = self.poller.wait(&mut events, timeout) {
                // A failing poller is unrecoverable for this shard; don't
                // spin at 100% CPU on a persistent error.
                eprintln!("sutegi-ws: poller error: {e}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            let batch = std::mem::take(&mut events);
            for ev in &batch {
                if ev.token == WAKE_TOKEN {
                    self.shared.pipe.drain();
                    self.shared.wake_pending.store(false, Ordering::Release);
                    continue;
                }
                if ev.readable || ev.hup {
                    self.handle_readable(ev.token as u32);
                }
                if ev.writable {
                    self.flush_out(ev.token as u32);
                }
            }
            events = batch;
            self.drain_inbox();
            // Resume connections parked by the per-wakeup frame cap. One pass
            // per loop = round-robin fairness; re-parking is fine.
            if !self.pending_parse.is_empty() {
                let todo = std::mem::take(&mut self.pending_parse);
                for token in todo {
                    self.parse_frames(token);
                }
            }
            self.tick();
        }
    }

    fn drain_inbox(&mut self) {
        // Swap the queue out under the lock, process outside it.
        let cmds = {
            let mut inbox = self.shared.inbox.lock().unwrap();
            if inbox.is_empty() {
                return;
            }
            std::mem::take(&mut *inbox)
        };
        for cmd in cmds {
            match cmd {
                Cmd::Adopt(data) => self.adopt(*data),
                Cmd::Send { token, gen, buf } => {
                    if self.live(token, gen) {
                        self.enqueue(token, buf);
                    }
                }
                Cmd::SendMany { targets, buf } => {
                    for (token, gen) in targets {
                        if self.live(token, gen) {
                            self.enqueue(token, Arc::clone(&buf));
                        }
                    }
                }
                Cmd::Close {
                    token,
                    gen,
                    code,
                    reason,
                } => {
                    if self.live(token, gen) {
                        self.begin_close(token, code, &reason);
                    }
                }
            }
        }
    }

    fn live(&self, token: u32, gen: u32) -> bool {
        let t = token as usize;
        t < self.slab.len() && self.gens[t] == gen && self.slab[t].is_some()
    }

    fn conn_handle(&self, token: u32) -> Conn {
        let state = self.slab[token as usize].as_ref().unwrap();
        Conn {
            id: state.id,
            token,
            gen: self.gens[token as usize],
            shard: Arc::clone(&self.shared),
        }
    }

    fn adopt(&mut self, data: AdoptData) {
        let AdoptData {
            stream,
            leftover,
            handlers,
            req,
            ip,
        } = data;
        // The slot was already reserved by `WsRuntime::adopt`; any early
        // return here must release it or the count leaks.
        if stream.set_nonblocking(true).is_err() {
            self.admission.release(ip.as_deref());
            return;
        }
        let _ = stream.set_nodelay(true);

        let token = match self.free.pop() {
            Some(t) => t,
            None => {
                self.slab.push(None);
                self.gens.push(0);
                (self.slab.len() - 1) as u32
            }
        };
        if self
            .poller
            .add(stream.as_raw_fd(), token as usize, false)
            .is_err()
        {
            self.free.push(token);
            self.admission.release(ip.as_deref());
            return;
        }
        let state = ConnState {
            stream,
            id: NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed),
            rbuf: leftover,
            assembly: None,
            out: VecDeque::new(),
            out_bytes: 0,
            want_write: false,
            closing: None,
            peer_closed: false,
            last_activity: Instant::now(),
            awaiting_pong: false,
            handlers,
            peer: ip,
        };
        self.slab[token as usize] = Some(state);

        let conn = self.conn_handle(token);
        let handlers = Arc::clone(&self.slab[token as usize].as_ref().unwrap().handlers);
        if let Some(on_open) = &handlers.on_open {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_open(&conn, &req)));
        }
        // The client may have pipelined frames behind its handshake.
        self.parse_frames(token);
    }

    fn handle_readable(&mut self, token: u32) {
        let t = token as usize;
        if t >= self.slab.len() || self.slab[t].is_none() {
            return; // already removed this iteration
        }
        let mut read_total = 0usize;
        let mut eof = false;
        loop {
            let state = match self.slab[t].as_mut() {
                Some(s) => s,
                None => return,
            };
            match (&state.stream).read(&mut self.scratch) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    state.rbuf.extend_from_slice(&self.scratch[..n]);
                    state.last_activity = Instant::now();
                    state.awaiting_pong = false;
                    read_total += n;
                    if read_total >= READ_QUANTUM {
                        break; // fairness: poller re-fires for the rest
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.remove(token, 1006);
                    return;
                }
            }
        }
        self.parse_frames(token);
        if eof {
            // Peer sent FIN. If a close handshake was in flight this is the
            // expected completion; otherwise it's an abnormal drop.
            let code = match self.slab[t].as_ref() {
                Some(s) => s.closing.map(|(_, c)| c).unwrap_or(1006),
                None => return, // parse_frames already removed it
            };
            self.remove(token, code);
        }
    }

    fn parse_frames(&mut self, token: u32) {
        let t = token as usize;
        let mut consumed = 0usize;
        let mut frames = 0usize;
        // Set when we stop with parseable bytes still buffered (frame cap hit)
        // — the connection is parked and resumed next loop for fairness.
        let mut more = false;
        loop {
            let state = match self.slab[t].as_mut() {
                Some(s) => s,
                None => return, // removed by a handler mid-loop
            };
            if frames >= FRAMES_PER_WAKE {
                more = consumed < state.rbuf.len();
                break;
            }
            match decode_frame(&state.rbuf[consumed..], self.cfg.max_frame) {
                Ok(None) => break,
                Ok(Some((frame, used))) => {
                    consumed += used;
                    frames += 1;
                    self.handle_frame(token, frame);
                }
                Err(err) => {
                    self.protocol_error(token, err);
                    return; // rbuf is dead along with the connection state
                }
            }
        }
        if let Some(state) = self.slab[t].as_mut() {
            if consumed > 0 {
                state.rbuf.drain(..consumed);
            }
            // Reclaim a large post-burst buffer, but keep a working floor so
            // steady traffic doesn't free-and-regrow every read.
            if state.rbuf.len() < RBUF_FLOOR && state.rbuf.capacity() > RBUF_FLOOR {
                state.rbuf.shrink_to(RBUF_FLOOR);
            }
            if more {
                self.pending_parse.push(token);
            }
        }
    }

    fn handle_frame(&mut self, token: u32, frame: crate::protocol::Frame) {
        let t = token as usize;
        match frame.opcode {
            Opcode::Ping => {
                let pong = Arc::new(encode_frame(Opcode::Pong, &frame.payload, true));
                self.enqueue(token, pong);
            }
            Opcode::Pong => {
                if let Some(state) = self.slab[t].as_mut() {
                    state.awaiting_pong = false;
                }
            }
            Opcode::Close => match parse_close(&frame.payload) {
                Ok((code, _reason)) => {
                    let already_closing =
                        self.slab[t].as_ref().is_some_and(|s| s.closing.is_some());
                    if already_closing {
                        // Our close was acknowledged: drop now.
                        self.remove(token, code);
                    } else {
                        // Peer-initiated: echo the code, then drop once the
                        // echo is flushed (or on their FIN).
                        if let Some(state) = self.slab[t].as_mut() {
                            state.closing = Some((Instant::now() + CLOSE_GRACE, code));
                            state.peer_closed = true;
                        }
                        let echo = Arc::new(encode_close(code, ""));
                        self.enqueue(token, echo);
                    }
                }
                Err(err) => self.protocol_error(token, err),
            },
            Opcode::Text | Opcode::Binary => {
                let state = match self.slab[t].as_mut() {
                    Some(s) => s,
                    None => return,
                };
                if state.closing.is_some() {
                    return; // draining a closing connection: ignore data
                }
                if state.assembly.is_some() {
                    self.protocol_error(token, ProtocolError::BadFragmentation);
                    return;
                }
                if frame.fin {
                    self.deliver(token, frame.opcode, frame.payload);
                } else {
                    state.assembly = Some((frame.opcode, frame.payload));
                }
            }
            Opcode::Continuation => {
                let state = match self.slab[t].as_mut() {
                    Some(s) => s,
                    None => return,
                };
                if state.closing.is_some() {
                    return;
                }
                let Some((op, mut data)) = state.assembly.take() else {
                    self.protocol_error(token, ProtocolError::BadFragmentation);
                    return;
                };
                if data.len() + frame.payload.len() > self.cfg.max_message {
                    self.protocol_error(token, ProtocolError::TooBig);
                    return;
                }
                data.extend_from_slice(&frame.payload);
                if frame.fin {
                    self.deliver(token, op, data);
                } else {
                    state.assembly = Some((op, data));
                }
            }
        }
    }

    /// A complete message: validate text, run the user handler.
    fn deliver(&mut self, token: u32, opcode: Opcode, payload: Vec<u8>) {
        let msg = match opcode {
            Opcode::Text => match String::from_utf8(payload) {
                Ok(s) => Msg::Text(s),
                Err(_) => {
                    self.protocol_error(token, ProtocolError::BadUtf8);
                    return;
                }
            },
            _ => Msg::Binary(payload),
        };
        let handlers = match self.slab[token as usize].as_ref() {
            Some(s) => Arc::clone(&s.handlers),
            None => return,
        };
        if let Some(on_message) = &handlers.on_message {
            let conn = self.conn_handle(token);
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_message(&conn, msg)));
        }
    }

    /// Send a close frame for a violation and stop reading from the peer.
    fn protocol_error(&mut self, token: u32, err: ProtocolError) {
        let code = err.close_code();
        self.begin_close(token, code, "");
    }

    /// Server-initiated close: send the frame, then wait (bounded) for the
    /// peer's acknowledgment before dropping.
    fn begin_close(&mut self, token: u32, code: u16, reason: &str) {
        let t = token as usize;
        let already = match self.slab[t].as_ref() {
            Some(s) => s.closing.is_some(),
            None => return,
        };
        if already {
            return;
        }
        let frame = Arc::new(encode_close(code, reason));
        self.enqueue(token, frame);
        if let Some(state) = self.slab[t].as_mut() {
            state.closing = Some((Instant::now() + CLOSE_GRACE, code));
            state.assembly = None;
        }
    }

    /// Queue bytes and try to flush immediately (the hot path writes with a
    /// single syscall and never touches the poller).
    fn enqueue(&mut self, token: u32, buf: Arc<Vec<u8>>) {
        let t = token as usize;
        let state = match self.slab[t].as_mut() {
            Some(s) => s,
            None => return,
        };
        state.out_bytes += buf.len();
        state.out.push_back(OutBuf { buf, pos: 0 });
        if state.out_bytes > self.cfg.max_buffered {
            // Slow consumer: dropping it is the only bounded option.
            self.remove(token, 1006);
            return;
        }
        self.flush_out(token);
    }

    fn flush_out(&mut self, token: u32) {
        let t = token as usize;
        loop {
            let state = match self.slab.get_mut(t).and_then(|s| s.as_mut()) {
                Some(s) => s,
                None => return,
            };
            let Some(front) = state.out.front_mut() else {
                // Fully drained.
                if state.peer_closed {
                    // Close handshake complete on both sides and our echo is
                    // on the wire: the server closes the TCP connection.
                    let code = state.closing.map(|(_, c)| c).unwrap_or(1000);
                    self.remove(token, code);
                    return;
                }
                if state.want_write {
                    state.want_write = false;
                    let fd = state.stream.as_raw_fd();
                    let _ = self.poller.set_write(fd, t, false);
                }
                return;
            };
            match (&state.stream).write(&front.buf[front.pos..]) {
                Ok(n) => {
                    front.pos += n;
                    state.out_bytes -= n;
                    if front.pos == front.buf.len() {
                        state.out.pop_front();
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !state.want_write {
                        state.want_write = true;
                        let fd = state.stream.as_raw_fd();
                        let _ = self.poller.set_write(fd, t, true);
                    }
                    return;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.remove(token, 1006);
                    return;
                }
            }
        }
    }

    /// Tear down a connection: close the fd (which deregisters it), free the
    /// slot, bump the generation, and notify `on_close`.
    fn remove(&mut self, token: u32, code: u16) {
        let t = token as usize;
        let Some(state) = self.slab[t].take() else {
            return;
        };
        // Construct the handle before bumping the generation so `on_close`
        // sees the right id — its sends are dropped either way, because the
        // slot is already vacated.
        let conn = Conn {
            id: state.id,
            token,
            gen: self.gens[t],
            shard: Arc::clone(&self.shared),
        };
        self.gens[t] = self.gens[t].wrapping_add(1);
        self.free.push(token);
        let _ = self.poller.del(state.stream.as_raw_fd());
        drop(state.stream); // close(2): kqueue/epoll deregister with the fd
        self.admission.release(state.peer.as_deref());
        if let Some(on_close) = &state.handlers.on_close {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_close(&conn, code)));
        }
    }

    /// Once a second: ping idle connections, drop dead ones, expire close
    /// handshakes that never completed. A linear slab sweep is deliberate —
    /// at 125k connections per shard it's a sub-millisecond pass, and it
    /// keeps per-connection state at two timestamps instead of a timer wheel.
    fn tick(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_sweep) < Duration::from_secs(1) {
            return;
        }
        self.last_sweep = now;
        let mut expired: Vec<(u32, u16)> = Vec::new();
        let mut ping: Vec<u32> = Vec::new();
        for (t, slot) in self.slab.iter().enumerate() {
            let Some(state) = slot else { continue };
            if let Some((deadline, code)) = state.closing {
                if now >= deadline {
                    expired.push((t as u32, code));
                }
                continue;
            }
            let idle = now.duration_since(state.last_activity);
            if idle >= self.cfg.idle_timeout {
                expired.push((t as u32, 1006));
            } else if idle >= self.cfg.ping_interval && !state.awaiting_pong {
                ping.push(t as u32);
            }
        }
        for (token, code) in expired {
            self.remove(token, code);
        }
        if !ping.is_empty() {
            // One encode for the whole sweep.
            let frame = Arc::new(encode_frame(Opcode::Ping, b"", true));
            for token in ping {
                if let Some(state) = self.slab[token as usize].as_mut() {
                    state.awaiting_pong = true;
                }
                self.enqueue(token, Arc::clone(&frame));
            }
        }
        self.reclaim_slab();
    }

    /// Give back the trailing free slots so a shard that spiked to a million
    /// connections and drained doesn't keep hundreds of MB of empty slab (and
    /// a million-entry sweep) forever. Only touches the free tail — live slots
    /// keep their tokens, so in-flight `Conn` handles stay valid. A truncated
    /// slot that later regrows restarts at generation 0, which can only be
    /// aliased by a stale handle whose gen is also 0 — impossible, since any
    /// freed slot was removed at least once and so has gen ≥ 1.
    fn reclaim_slab(&mut self) {
        let mut new_len = self.slab.len();
        while new_len > 0 && self.slab[new_len - 1].is_none() {
            new_len -= 1;
        }
        if new_len == self.slab.len() {
            return;
        }
        self.slab.truncate(new_len);
        self.gens.truncate(new_len);
        self.free.retain(|&t| (t as usize) < new_len);
        // Only pay the reallocation when the win is large.
        if self.slab.capacity() > 4 * new_len.max(1) {
            self.slab.shrink_to_fit();
            self.gens.shrink_to_fit();
        }
    }
}

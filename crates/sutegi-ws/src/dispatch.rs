//! Bounded, ordered callback execution away from reactor threads.
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::{Conn, Handlers, Msg};
use sutegi_http::Request;

/// Limits for one endpoint's callback pool. Running callbacks must cooperate
/// with cancellation (`Conn::is_closed`); Rust cannot forcibly stop a thread.
#[derive(Clone, Debug)]
pub struct DispatchConfig {
    pub workers: usize,
    pub max_connections: usize,
    pub max_pending_per_connection: usize,
    pub max_pending_bytes: usize,
}
impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            workers: 4,
            max_connections: 1024,
            max_pending_per_connection: 32,
            max_pending_bytes: 8 << 20,
        }
    }
}

enum Event {
    Open(Request),
    Message(Msg),
    Close(u16),
}
impl Event {
    fn bytes(&self) -> usize {
        match self {
            Self::Open(r) => {
                r.body.len()
                    + r.path.len()
                    + r.query.len()
                    + r.headers
                        .iter()
                        .map(|(k, v)| k.len() + v.len())
                        .sum::<usize>()
            }
            Self::Message(Msg::Text(s)) => s.len(),
            Self::Message(Msg::Binary(b)) => b.len(),
            Self::Close(_) => 0,
        }
    }
}
struct Lane {
    conn: Conn,
    queue: VecDeque<Event>,
    scheduled: bool,
    closed: bool,
}
#[derive(Default)]
struct State {
    lanes: HashMap<u64, Lane>,
    ready: VecDeque<u64>,
    bytes: usize,
    stopping: bool,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    handlers: Handlers,
    config: DispatchConfig,
}
struct Pool(Arc<Shared>);
impl Drop for Pool {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.stopping = true;
        for lane in state.lanes.values() {
            lane.conn.close(1001, "handler pool stopped");
        }
        self.0.wake.notify_all();
    }
}

impl Pool {
    fn enqueue(&self, conn: &Conn, event: Event) {
        let mut state = self.0.state.lock().unwrap();
        let id = conn.id();
        if state.stopping {
            conn.close(1013, "handler pool stopped");
            return;
        }
        let size = event.bytes();
        if matches!(event, Event::Open(_)) {
            if state.lanes.len() >= self.0.config.max_connections
                || size > self.0.config.max_pending_bytes.saturating_sub(state.bytes)
            {
                conn.close(1013, "handler capacity reached");
                return;
            }
            state.lanes.insert(
                id,
                Lane {
                    conn: conn.clone(),
                    queue: VecDeque::new(),
                    scheduled: false,
                    closed: false,
                },
            );
        }
        let Some(lane) = state.lanes.get(&id) else {
            return;
        };
        if lane.closed {
            return;
        }
        if matches!(event, Event::Message(_))
            && (lane.queue.len() >= self.0.config.max_pending_per_connection
                || size > self.0.config.max_pending_bytes.saturating_sub(state.bytes))
        {
            conn.close(1013, "handler queue full");
            return;
        }
        let lane = state.lanes.get_mut(&id).unwrap();
        let mut removed = 0;
        if matches!(event, Event::Close(_)) {
            lane.closed = true;
            // Preserve open/close ordering, cancel work which has not started.
            lane.queue.retain(|e| {
                if matches!(e, Event::Message(_)) {
                    removed += e.bytes();
                    false
                } else {
                    true
                }
            });
        }
        lane.queue.push_back(event);
        if !lane.scheduled {
            lane.scheduled = true;
            state.ready.push_back(id);
        }
        state.bytes = state.bytes - removed + size;
        self.0.wake.notify_one();
    }
}

impl Handlers {
    /// Wrap callbacks in a bounded pool. Call after setting callbacks. Handshake
    /// authorization stays in the HTTP worker. Connections retain event order;
    /// one blocked connection consumes one worker, never a reactor shard.
    pub fn dispatched(self, config: DispatchConfig) -> io::Result<Self> {
        if config.workers == 0
            || config.max_connections == 0
            || config.max_pending_per_connection == 0
            || config.max_pending_bytes == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dispatch limits must be positive",
            ));
        }
        let mut wrapped = self.clone();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            handlers: self,
            config,
        });
        let pool = Arc::new(Pool(Arc::clone(&shared)));
        for n in 0..shared.config.workers {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(format!("sutegi-ws-handler-{n}"))
                .spawn(move || worker(shared))?;
        }
        let open = Arc::clone(&pool);
        wrapped.on_open = Some(Arc::new(move |c, r| {
            open.enqueue(c, Event::Open(r.clone()))
        }));
        let message = Arc::clone(&pool);
        wrapped.on_message = Some(Arc::new(move |c, m| message.enqueue(c, Event::Message(m))));
        wrapped.on_close = Some(Arc::new(move |c, code| pool.enqueue(c, Event::Close(code))));
        Ok(wrapped)
    }
}

fn worker(shared: Arc<Shared>) {
    loop {
        let (id, conn, event) = {
            let mut state = shared.state.lock().unwrap();
            while state.ready.is_empty() {
                if state.stopping {
                    return;
                }
                state = shared.wake.wait(state).unwrap();
            }
            let id = state.ready.pop_front().unwrap();
            let lane = state.lanes.get_mut(&id).unwrap();
            let event = lane.queue.pop_front().unwrap();
            let conn = lane.conn.clone();
            state.bytes -= event.bytes();
            (id, conn, event)
        };
        let closing = matches!(event, Event::Close(_));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match event {
            Event::Open(req) => {
                if let Some(f) = &shared.handlers.on_open {
                    f(&conn, &req);
                }
            }
            Event::Message(msg) => {
                if !conn.is_closed()
                    && let Some(f) = &shared.handlers.on_message
                {
                    f(&conn, msg);
                }
            }
            Event::Close(code) => {
                if let Some(f) = &shared.handlers.on_close {
                    f(&conn, code);
                }
            }
        }));
        if result.is_err() {
            conn.close(1011, "handler panicked");
        }
        let mut state = shared.state.lock().unwrap();
        if closing {
            state.lanes.remove(&id);
        } else {
            let lane = state.lanes.get_mut(&id).unwrap();
            if lane.queue.is_empty() {
                lane.scheduled = false;
            } else {
                state.ready.push_back(id);
                shared.wake.notify_one();
            }
        }
    }
}

//! The bridge's timer wheel: one thread, a deadline heap, a condvar. Serves
//! both effect kinds the runtime can request — `delay` (one-shot) and
//! `every` (repeating; reschedules itself until the program's sub diff
//! stops it or the connection closes). Fires happen outside all locks.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fire {
    Delay { id: u32 },
    Every { id: u32 },
}

#[derive(PartialEq, Eq)]
struct Entry {
    at: Instant,
    conn: u64,
    fire: Fire,
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.at.cmp(&other.at)
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct Inner {
    queue: Mutex<BinaryHeap<Reverse<Entry>>>,
    /// Live repeating timers, (conn, sub id) → period ms. A fire for a key
    /// no longer here neither notifies nor reschedules — this is how Stop
    /// and connection close cancel; stale heap entries drain silently.
    every: Mutex<HashMap<(u64, u32), u32>>,
    cv: Condvar,
}

pub(crate) struct Scheduler {
    inner: Arc<Inner>,
}

impl Scheduler {
    /// `on_fire(conn, fire)` runs on the scheduler thread — keep it to one
    /// program step (lock, apply, send); heavier work belongs elsewhere.
    pub(crate) fn new(on_fire: impl Fn(u64, Fire) + Send + 'static) -> Scheduler {
        let inner = Arc::new(Inner {
            queue: Mutex::new(BinaryHeap::new()),
            every: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
        });
        let thread_inner = Arc::clone(&inner);
        std::thread::spawn(move || run(thread_inner, on_fire));
        Scheduler { inner }
    }

    pub(crate) fn delay(&self, conn: u64, id: u32, ms: u32) {
        self.push(conn, Fire::Delay { id }, ms);
    }

    pub(crate) fn start_every(&self, conn: u64, id: u32, ms: u32) {
        let ms = ms.max(1);
        self.inner.every.lock().unwrap().insert((conn, id), ms);
        self.push(conn, Fire::Every { id }, ms);
    }

    pub(crate) fn stop_every(&self, conn: u64, id: u32) {
        self.inner.every.lock().unwrap().remove(&(conn, id));
    }

    pub(crate) fn drop_conn(&self, conn: u64) {
        self.inner
            .every
            .lock()
            .unwrap()
            .retain(|(c, _), _| *c != conn);
        // one-shot delays for a dead conn fire into a missing session — a
        // no-op — so the heap needs no surgery
    }

    fn push(&self, conn: u64, fire: Fire, ms: u32) {
        let entry = Entry {
            at: Instant::now() + Duration::from_millis(u64::from(ms)),
            conn,
            fire,
        };
        self.inner.queue.lock().unwrap().push(Reverse(entry));
        self.inner.cv.notify_one();
    }
}

fn run(inner: Arc<Inner>, on_fire: impl Fn(u64, Fire)) {
    loop {
        let due = {
            let mut queue = inner.queue.lock().unwrap();
            loop {
                match queue.peek() {
                    None => {
                        queue = inner.cv.wait(queue).unwrap();
                    }
                    Some(Reverse(head)) => {
                        let now = Instant::now();
                        if head.at <= now {
                            break queue.pop().unwrap().0;
                        }
                        let wait = head.at - now;
                        queue = inner.cv.wait_timeout(queue, wait).unwrap().0;
                    }
                }
            }
        };
        match due.fire {
            Fire::Delay { .. } => on_fire(due.conn, due.fire),
            Fire::Every { id } => {
                let period = inner.every.lock().unwrap().get(&(due.conn, id)).copied();
                if let Some(ms) = period {
                    on_fire(due.conn, due.fire);
                    // reschedule (unless a Stop landed during the fire)
                    if inner.every.lock().unwrap().contains_key(&(due.conn, id)) {
                        let entry = Entry {
                            at: Instant::now() + Duration::from_millis(u64::from(ms)),
                            conn: due.conn,
                            fire: due.fire,
                        };
                        inner.queue.lock().unwrap().push(Reverse(entry));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn delay_fires_once() {
        let (tx, rx) = mpsc::channel();
        let sched = Scheduler::new(move |conn, fire| {
            tx.send((conn, fire)).unwrap();
        });
        sched.delay(1, 10, 5);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (1, Fire::Delay { id: 10 })
        );
        assert!(rx.recv_timeout(Duration::from_millis(60)).is_err());
    }

    #[test]
    fn every_repeats_until_stopped() {
        let (tx, rx) = mpsc::channel();
        let sched = Scheduler::new(move |conn, fire| {
            let _ = tx.send((conn, fire));
        });
        sched.start_every(2, 20, 5);
        for _ in 0..3 {
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                (2, Fire::Every { id: 20 })
            );
        }
        sched.stop_every(2, 20);
        while rx.recv_timeout(Duration::from_millis(60)).is_ok() {} // drain in-flight
        assert!(rx.recv_timeout(Duration::from_millis(60)).is_err());
    }

    #[test]
    fn drop_conn_cancels_repeaters() {
        let (tx, rx) = mpsc::channel();
        let sched = Scheduler::new(move |conn, fire| {
            let _ = tx.send((conn, fire));
        });
        sched.start_every(7, 1, 5);
        sched.drop_conn(7);
        std::thread::sleep(Duration::from_millis(20));
        while rx.try_recv().is_ok() {} // drain anything that raced the drop
        assert!(rx.recv_timeout(Duration::from_millis(60)).is_err());
    }
}

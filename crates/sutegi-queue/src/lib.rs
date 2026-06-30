//! A small, dependency-free background job queue — sutegi's answer to Laravel
//! queues, scoped to a single process.
//!
//! Jobs implement [`Job`] and are dispatched to a pool of worker threads. A
//! failing job is retried up to `tries()` times. The queue tracks counters
//! ([`Stats`]) that can be surfaced as JSON for introspection. Delayed
//! dispatch is supported via a per-job timer thread.
//!
//! For durable, cross-process queues you'd back this with the `sqlite` layer;
//! this in-process version covers the common "do it after the response"
//! case (emails, webhooks, cache warming) with zero dependencies.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use sutegi_json::Json;

/// A unit of background work.
pub trait Job: Send + 'static {
    /// A label for logging/introspection.
    fn name(&self) -> &str;

    /// Run the job. Returning `Err` triggers a retry (up to `tries()`).
    fn handle(&self) -> Result<(), String>;

    /// Total attempts before the job is considered failed (default 1).
    fn tries(&self) -> u32 {
        1
    }
}

/// Live counters for the queue.
#[derive(Default)]
struct Counters {
    dispatched: AtomicU64,
    processed: AtomicU64,
    failed: AtomicU64,
    retried: AtomicU64,
}

/// A snapshot of queue counters.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub dispatched: u64,
    pub processed: u64,
    pub failed: u64,
    pub retried: u64,
}

impl Stats {
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("dispatched", Json::int(self.dispatched as i64)),
            ("processed", Json::int(self.processed as i64)),
            ("failed", Json::int(self.failed as i64)),
            ("retried", Json::int(self.retried as i64)),
        ])
    }
}

type BoxedJob = Box<dyn Job>;

/// A background queue with a fixed pool of workers.
pub struct Queue {
    sender: Option<mpsc::Sender<BoxedJob>>,
    workers: Vec<thread::JoinHandle<()>>,
    counters: Arc<Counters>,
}

impl Queue {
    /// Create a queue with `workers` background threads.
    pub fn new(workers: usize) -> Arc<Queue> {
        let (sender, receiver) = mpsc::channel::<BoxedJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let counters = Arc::new(Counters::default());

        let mut handles = Vec::with_capacity(workers.max(1));
        for _ in 0..workers.max(1) {
            let receiver = Arc::clone(&receiver);
            let counters = Arc::clone(&counters);
            handles.push(thread::spawn(move || loop {
                let job = {
                    let lock = receiver.lock().unwrap();
                    lock.recv()
                };
                match job {
                    Ok(job) => run_job(job, &counters),
                    Err(_) => break, // channel closed
                }
            }));
        }

        Arc::new(Queue {
            sender: Some(sender),
            workers: handles,
            counters,
        })
    }

    /// Enqueue a job for background processing.
    pub fn dispatch(&self, job: impl Job) {
        self.counters.dispatched.fetch_add(1, Ordering::Relaxed);
        if let Some(sender) = &self.sender {
            let _ = sender.send(Box::new(job));
        }
    }

    /// Enqueue a job to run after `delay`. Spawns a lightweight timer thread.
    pub fn dispatch_after(self: &Arc<Self>, delay: Duration, job: impl Job) {
        self.counters.dispatched.fetch_add(1, Ordering::Relaxed);
        let queue = Arc::clone(self);
        let mut boxed: Option<BoxedJob> = Some(Box::new(job));
        thread::spawn(move || {
            thread::sleep(delay);
            if let (Some(sender), Some(job)) = (queue.sender.as_ref(), boxed.take()) {
                let _ = sender.send(job);
            }
        });
    }

    /// Current counter snapshot.
    pub fn stats(&self) -> Stats {
        Stats {
            dispatched: self.counters.dispatched.load(Ordering::Relaxed),
            processed: self.counters.processed.load(Ordering::Relaxed),
            failed: self.counters.failed.load(Ordering::Relaxed),
            retried: self.counters.retried.load(Ordering::Relaxed),
        }
    }
}

fn run_job(job: BoxedJob, counters: &Counters) {
    let tries = job.tries().max(1);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match job.handle() {
            Ok(()) => {
                counters.processed.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(err) => {
                if attempt >= tries {
                    counters.failed.fetch_add(1, Ordering::Relaxed);
                    eprintln!(
                        "[queue] job '{}' failed after {} attempt(s): {}",
                        job.name(),
                        attempt,
                        err
                    );
                    return;
                }
                counters.retried.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // Closing the channel lets idle workers exit; join to finish in-flight jobs.
        drop(self.sender.take());
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    struct Counting(Arc<AtomicU32>);
    impl Job for Counting {
        fn name(&self) -> &str {
            "counting"
        }
        fn handle(&self) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct AlwaysFails;
    impl Job for AlwaysFails {
        fn name(&self) -> &str {
            "always-fails"
        }
        fn handle(&self) -> Result<(), String> {
            Err("nope".into())
        }
        fn tries(&self) -> u32 {
            3
        }
    }

    #[test]
    fn processes_dispatched_jobs() {
        let counter = Arc::new(AtomicU32::new(0));
        let queue = Queue::new(4);
        for _ in 0..20 {
            queue.dispatch(Counting(Arc::clone(&counter)));
        }
        // Dropping the queue drains and joins all workers.
        let stats = queue.stats();
        drop(queue);
        assert!(stats.dispatched == 20);
        assert_eq!(counter.load(Ordering::Relaxed), 20);
    }

    /// Poll `cond` until true or ~1s elapses; returns whether it became true.
    fn wait_until(cond: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        cond()
    }

    #[test]
    fn retries_then_marks_failed() {
        let queue = Queue::new(1);
        queue.dispatch(AlwaysFails); // tries() == 3
        assert!(
            wait_until(|| queue.stats().failed == 1),
            "job should fail terminally"
        );
        let s = queue.stats();
        assert_eq!(s.failed, 1);
        assert_eq!(s.retried, 2); // 3 attempts → 2 retries before the final failure
        assert_eq!(s.processed, 0);
        assert_eq!(s.dispatched, 1);
    }

    #[test]
    fn delayed_dispatch_runs_after_delay() {
        let counter = Arc::new(AtomicU32::new(0));
        let queue = Queue::new(2);
        queue.dispatch_after(Duration::from_millis(10), Counting(Arc::clone(&counter)));
        // Counted as dispatched immediately, but only processed after the delay.
        assert_eq!(queue.stats().dispatched, 1);
        assert!(wait_until(|| counter.load(Ordering::Relaxed) == 1));
        assert!(wait_until(|| queue.stats().processed == 1));
    }

    #[test]
    fn stats_serialize_to_json() {
        let s = Stats {
            dispatched: 5,
            processed: 3,
            failed: 1,
            retried: 4,
        };
        let j = s.to_json();
        assert_eq!(j.get("dispatched").and_then(Json::as_i64), Some(5));
        assert_eq!(j.get("failed").and_then(Json::as_i64), Some(1));
        assert_eq!(j.get("retried").and_then(Json::as_i64), Some(4));
    }
}

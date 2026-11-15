//! Shared admission/drain state for HTTP, upgraded sockets and background work.
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Draining,
    Stopped,
}
type Wake = Arc<dyn Fn() + Send + Sync>;
struct Inner {
    phase: AtomicU8,
    deadline: Mutex<Option<Instant>>,
    wakes: Mutex<Vec<Wake>>,
}
#[derive(Clone)]
pub struct Lifecycle(Arc<Inner>);
impl Default for Lifecycle {
    fn default() -> Self {
        Self(Arc::new(Inner {
            phase: AtomicU8::new(0),
            deadline: Mutex::new(None),
            wakes: Mutex::new(Vec::new()),
        }))
    }
}
impl Lifecycle {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn phase(&self) -> Phase {
        match self.0.phase.load(Ordering::Acquire) {
            0 => Phase::Running,
            1 => {
                if self.deadline().is_some_and(|d| Instant::now() >= d) {
                    self.stop();
                    Phase::Stopped
                } else {
                    Phase::Draining
                }
            }
            _ => Phase::Stopped,
        }
    }
    /// Starts draining once; repeated requests never extend the deadline.
    pub fn drain(&self, grace: Duration) {
        let mut deadline = self.0.deadline.lock().unwrap();
        if self.0.phase.load(Ordering::Acquire) != 0 {
            return;
        }
        *deadline = Some(
            Instant::now()
                .checked_add(grace)
                .unwrap_or_else(Instant::now),
        );
        // stop() can race with this request. Never resurrect a stopped
        // lifecycle after publishing the deadline.
        let changed = self
            .0
            .phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        drop(deadline);
        if changed {
            self.wake();
        }
    }
    pub fn stop(&self) {
        if self.0.phase.swap(2, Ordering::AcqRel) != 2 {
            self.wake();
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        *self.0.deadline.lock().unwrap()
    }
    /// A nonblocking notification hook, for waking an event loop. Runs inline;
    /// perform slow cleanup in a listener observing `phase()` instead.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.0.wakes.lock().unwrap().push(Arc::new(wake));
    }
    fn wake(&self) {
        let wakes = self.0.wakes.lock().unwrap().clone();
        for wake in wakes {
            // One application hook must not suppress notifications to other
            // transports or unwind through an event loop polling phase().
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wake()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_state_wins_a_concurrent_drain() {
        for _ in 0..100 {
            let lifecycle = Lifecycle::new();
            let drain = lifecycle.clone();
            let thread = std::thread::spawn(move || drain.drain(Duration::from_secs(60)));
            lifecycle.stop();
            thread.join().unwrap();
            assert_eq!(lifecycle.phase(), Phase::Stopped);
        }
    }
    #[test]
    fn a_panicking_hook_does_not_suppress_other_notifications() {
        use std::sync::atomic::AtomicBool;
        let lifecycle = Lifecycle::new();
        let called = Arc::new(AtomicBool::new(false));
        lifecycle.on_change(|| panic!("application hook"));
        let flag = called.clone();
        lifecycle.on_change(move || {
            flag.store(true, Ordering::Release);
        });
        lifecycle.stop();
        assert!(called.load(Ordering::Acquire));
    }
    #[test]
    fn repeated_drain_does_not_extend_the_deadline() {
        let state = Lifecycle::new();
        state.drain(Duration::from_millis(20));
        let deadline = state.deadline();
        state.drain(Duration::from_secs(3600));
        assert_eq!(state.deadline(), deadline);
        assert_eq!(state.phase(), Phase::Draining);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(state.phase(), Phase::Stopped);
        state.drain(Duration::from_secs(1));
        assert_eq!(state.phase(), Phase::Stopped);
    }
}

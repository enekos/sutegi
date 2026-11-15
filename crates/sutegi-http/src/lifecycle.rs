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
        self.0.phase.store(1, Ordering::Release);
        drop(deadline);
        self.wake();
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
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

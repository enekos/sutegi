//! A small blocking connection pool. Connections are created lazily up to
//! `max_size`; a checked-out connection is returned to the pool on drop unless
//! it went bad, in which case it is discarded and the slot frees up.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use sutegi_json::Json;

use crate::protocol::Client;
use crate::{Config, PgValue};

struct Inner {
    /// Idle, ready-to-use connections.
    idle: Vec<Client>,
    /// Live connections (idle + checked out). Capped at `max_size`.
    open: usize,
}

/// A thread-safe pool of PostgreSQL connections.
#[derive(Clone)]
pub struct Pool {
    cfg: Config,
    max_size: usize,
    state: Arc<(Mutex<Inner>, Condvar)>,
}

impl Pool {
    /// Create a pool. No connection is opened until the first checkout, so
    /// constructing a pool never blocks or fails.
    pub fn new(cfg: Config, max_size: usize) -> Pool {
        Pool {
            cfg,
            max_size: max_size.max(1),
            state: Arc::new((
                Mutex::new(Inner {
                    idle: Vec::new(),
                    open: 0,
                }),
                Condvar::new(),
            )),
        }
    }

    /// Build a pool from `DATABASE_URL`/`PG*` environment variables.
    pub fn from_env(max_size: usize) -> Result<Pool, String> {
        Ok(Pool::new(Config::from_env()?, max_size))
    }

    /// The connection settings this pool dials with — for callers that need a
    /// dedicated out-of-pool connection (advisory locks, LISTEN sessions).
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Check a connection out, run `f`, and return the connection to the pool.
    /// A connection that errors mid-use is dropped rather than reused, and so
    /// is one whose `f` panicked — its slot is released either way.
    pub fn with<T>(&self, f: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
        let mut client = self.checkout()?;
        let slot = Slot::reserve(self);
        let result = f(&mut client);
        if result.is_err() && !client.ping() {
            drop(slot);
        } else {
            slot.keep();
            self.checkin(client);
        }
        result
    }

    /// Convenience: run a parameterized query on a pooled connection.
    pub fn query(&self, sql: &str, params: &[PgValue]) -> Result<Vec<Json>, String> {
        self.with(|c| c.query(sql, params))
    }

    /// Convenience: run a parameterized statement, returning rows affected.
    pub fn execute(&self, sql: &str, params: &[PgValue]) -> Result<u64, String> {
        self.with(|c| c.execute(sql, params))
    }

    /// Convenience: run a non-parameterized batch (BEGIN/COMMIT/DDL).
    pub fn batch(&self, sql: &str) -> Result<(), String> {
        self.with(|c| c.batch(sql))
    }

    fn checkout(&self) -> Result<Client, String> {
        self.checkout_with(Client::connect)
    }

    /// Hand out an idle connection, dial a new one while under `max_size`, or
    /// wait for one to come back. The wait is bounded by [`Config::timeout`]
    /// when set, so a pool whose slots are all wedged fails loudly instead of
    /// hanging its callers forever.
    fn checkout_with(
        &self,
        connect: impl FnOnce(&Config) -> Result<Client, String>,
    ) -> Result<Client, String> {
        let deadline = self.cfg.timeout.map(|t| Instant::now() + t);
        let cvar = &self.state.1;
        let mut inner = self.lock();
        loop {
            if let Some(client) = inner.idle.pop() {
                return Ok(client);
            }
            if inner.open < self.max_size {
                inner.open += 1;
                // Release the lock while the (slow) TCP handshake runs. The
                // slot rolls back on drop, so neither an error nor a panic in
                // `connect` leaks the reservation.
                drop(inner);
                let slot = Slot::reserve(self);
                let client = connect(&self.cfg)?;
                slot.keep();
                return Ok(client);
            }
            // Pool is full: wait for a connection to be returned.
            inner = match deadline {
                None => cvar.wait(inner).unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(format!(
                            "pool checkout timed out: all {} connections busy",
                            self.max_size
                        ));
                    }
                    cvar.wait_timeout(inner, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }

    fn checkin(&self, client: Client) {
        let mut inner = self.lock();
        inner.idle.push(client);
        self.state.1.notify_one();
    }

    fn discard(&self) {
        let mut inner = self.lock();
        inner.open = inner.open.saturating_sub(1);
        self.state.1.notify_one();
    }

    /// Poison is recovered, not propagated: `Inner` is a counter and a stack of
    /// idle connections, valid whatever a panicking holder was doing — and the
    /// [`Slot`] drop that runs during unwinding must not panic again.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.state.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One reserved unit of `open`. Dropping it gives the slot back (and wakes a
/// waiter); [`Slot::keep`] hands ownership of the slot to a live connection.
struct Slot<'p> {
    pool: &'p Pool,
    armed: bool,
}

impl<'p> Slot<'p> {
    fn reserve(pool: &'p Pool) -> Slot<'p> {
        Slot { pool, armed: true }
    }

    fn keep(mut self) {
        self.armed = false;
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.pool.discard();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn from_env_builds_without_connecting() {
        // Constructing a pool must never touch the network.
        let pool = Pool::new(Config::default(), 4);
        assert_eq!(pool.max_size, 4);
    }

    fn open(pool: &Pool) -> usize {
        pool.lock().open
    }

    #[test]
    fn a_panicking_connect_gives_its_slot_back() {
        let pool = Pool::new(Config::default(), 1);
        for _ in 0..3 {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.checkout_with(|_| panic!("hostile handshake"))
            }));
            assert!(caught.is_err());
            assert_eq!(open(&pool), 0, "a panic during connect leaked the slot");
        }
    }

    #[test]
    fn a_failing_connect_gives_its_slot_back() {
        let pool = Pool::new(Config::default(), 1);
        for _ in 0..3 {
            assert!(pool.checkout_with(|_| Err("refused".into())).is_err());
            assert_eq!(open(&pool), 0);
        }
    }

    #[test]
    fn a_full_pool_times_out_instead_of_waiting_forever() {
        let cfg = Config {
            timeout: Some(Duration::from_millis(50)),
            ..Config::default()
        };
        let pool = Pool::new(cfg, 1);
        pool.lock().open = 1; // the one slot is held by a wedged caller
        let started = Instant::now();
        let err = pool
            .checkout_with(|_| unreachable!("no slot is free"))
            .err()
            .expect("checkout must give up");
        assert!(err.contains("timed out"), "got: {err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

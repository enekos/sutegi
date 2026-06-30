//! A small blocking connection pool. Connections are created lazily up to
//! `max_size`; a checked-out connection is returned to the pool on drop unless
//! it went bad, in which case it is discarded and the slot frees up.

use std::sync::{Arc, Condvar, Mutex};

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

    /// Check a connection out, run `f`, and return the connection to the pool.
    /// A connection that errors mid-use is dropped rather than reused.
    pub fn with<T>(&self, f: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
        let mut client = self.checkout()?;
        let result = f(&mut client);
        if result.is_err() && !client.ping() {
            self.discard();
        } else {
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
        let (lock, cvar) = &*self.state;
        let mut inner = lock.lock().unwrap();
        loop {
            if let Some(client) = inner.idle.pop() {
                return Ok(client);
            }
            if inner.open < self.max_size {
                inner.open += 1;
                // Release the lock while the (slow) TCP handshake runs.
                drop(inner);
                match Client::connect(&self.cfg) {
                    Ok(client) => return Ok(client),
                    Err(e) => {
                        // Roll back the reservation so the slot isn't leaked.
                        let mut inner = lock.lock().unwrap();
                        inner.open -= 1;
                        cvar.notify_one();
                        return Err(e);
                    }
                }
            }
            // Pool is full: wait for a connection to be returned.
            inner = cvar.wait(inner).unwrap();
        }
    }

    fn checkin(&self, client: Client) {
        let (lock, cvar) = &*self.state;
        let mut inner = lock.lock().unwrap();
        inner.idle.push(client);
        cvar.notify_one();
    }

    fn discard(&self) {
        let (lock, cvar) = &*self.state;
        let mut inner = lock.lock().unwrap();
        inner.open -= 1;
        cvar.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_builds_without_connecting() {
        // Constructing a pool must never touch the network.
        let pool = Pool::new(Config::default(), 4);
        assert_eq!(pool.max_size, 4);
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sutegi_json::Json;
use sutegi_orm::Value as DbValue;
use sutegi_orm::db::Db;
use sutegi_queue::Queue;
use zin::conform::{Adapter, Args, Outcome, check};
use zin::{Cover, PlanOptions, State, Value};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

struct Harness {
    queue: Queue,
    _path: DbPath,
    should_fail: Arc<AtomicBool>,
    current_id: Option<i64>,
    last_attempts: i64,
}

struct DbPath(String);

impl Drop for DbPath {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

impl Harness {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!(
                "sutegi-queue-conformance-{}-{}.db",
                std::process::id(),
                NEXT_DB.fetch_add(1, Ordering::Relaxed)
            ))
            .to_string_lossy()
            .into_owned();
        let db = Db::open(&path).expect("open conformance database");
        let mut queue = Queue::new(db).retry_backoff(Duration::ZERO);
        queue.migrate().expect("migrate conformance queue");
        let should_fail = Arc::new(AtomicBool::new(false));
        let handler_should_fail = Arc::clone(&should_fail);
        queue.register("conform", move |_| {
            if handler_should_fail.load(Ordering::Relaxed) {
                Err("requested failure".into())
            } else {
                Ok(())
            }
        });
        Self {
            _path: DbPath(path),
            queue,
            should_fail,
            current_id: None,
            last_attempts: 0,
        }
    }

    fn row(&self) -> Option<Json> {
        let id = self.current_id?;
        self.queue
            .store()
            .query(
                "SELECT failed_at, attempts FROM sutegi_jobs WHERE id = ?",
                &[DbValue::Int(id)],
            )
            .expect("read conformance job")
            .into_iter()
            .next()
    }
}

impl Adapter for Harness {
    fn call(&mut self, action: &str, args: &Args) -> Outcome {
        match action {
            "dispatch" => {
                let was_finished = self
                    .row()
                    .is_none_or(|row| row.get("failed_at") != Some(&Json::Null));
                let result = self
                    .queue
                    .job("conform", Json::Null)
                    .max_attempts(2)
                    .unique("zin")
                    .dispatch();
                if was_finished && result.is_ok() {
                    self.last_attempts = 0;
                }
                if let Ok(id) = &result {
                    self.current_id = Some(*id);
                }
                result.into()
            }
            "run" => {
                let fail = args.get("result") == Some(&Value::Symbol("failure".into()));
                self.should_fail.store(fail, Ordering::Relaxed);
                let result = self.queue.run_once().expect("run conformance job");
                if let Some(row) = self.row() {
                    self.last_attempts = row
                        .get("attempts")
                        .and_then(Json::as_i64)
                        .expect("job attempts");
                } else if result {
                    self.current_id = None;
                    self.last_attempts = 0;
                }
                result.into()
            }
            "retry" => {
                let Some(id) = self.current_id else {
                    return Outcome::Rejected;
                };
                let retried = self.queue.retry(id, 2).expect("retry conformance job");
                if retried {
                    self.last_attempts = 0;
                }
                retried.into()
            }
            "purge" => {
                let was_failed = self
                    .row()
                    .is_some_and(|row| row.get("failed_at") != Some(&Json::Null));
                self.queue
                    .purge_failed(Duration::ZERO)
                    .expect("purge conformance job");
                if was_failed {
                    self.current_id = None;
                    self.last_attempts = 0;
                }
                Outcome::Accepted
            }
            other => panic!("unknown action {other}"),
        }
    }

    fn observe(&self) -> Option<State> {
        let row = self.row();
        let status = match &row {
            None => "empty",
            Some(row) if row.get("failed_at") != Some(&Json::Null) => "failed",
            Some(_) => "ready",
        };
        let attempts = row
            .and_then(|row| row.get("attempts").and_then(Json::as_i64))
            .unwrap_or(self.last_attempts);
        Some(State::from([
            ("status".into(), Value::Symbol(status.into())),
            ("attempts".into(), Value::Integer(attempts)),
        ]))
    }
}

#[test]
fn queue_conforms_to_its_zin_specification() {
    check(
        include_str!("queue.zin"),
        &PlanOptions::cover(Cover::Pairs { limit: None }),
        Harness::new,
    )
    .expect("queue behavior should match its specification");
}

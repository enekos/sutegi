//! An **event-sourced account ledger**: nothing is ever updated in place —
//! deposits and withdrawals are appended to per-account streams, the balance
//! is folded from the stream on demand, and a background projection maintains
//! a queryable `account_balances` read model (rebuildable from the log).
//!
//! ```text
//! curl -X POST localhost:8080/accounts/42/deposit  -d '{"amount": 100}'
//! curl -X POST localhost:8080/accounts/42/withdraw -d '{"amount": 30}'
//! curl         localhost:8080/accounts/42           # folded: {"balance":70,"version":2}
//! curl         localhost:8080/accounts/42/history   # the raw events
//! curl         localhost:8080/accounts              # the projected read model
//! curl         localhost:8080/__events              # log head + projection lag
//! ```

use std::sync::Arc;
use std::time::Duration;

use sutegi::events::{
    event, Aggregate, EventError, EventStore, Expected, Projections, StoredEvent,
};
use sutegi::prelude::*;

type Store = EventStore<Db>;

/// Current account state, folded from its stream. `apply` is the only place
/// event names are interpreted — decisions and read models both go through it.
#[derive(Default)]
struct Account {
    balance: i64,
}

impl Aggregate for Account {
    fn apply(&mut self, e: &StoredEvent) {
        let amount = e.payload.get("amount").and_then(Json::as_i64).unwrap_or(0);
        match e.name.as_str() {
            "deposited" => self.balance += amount,
            "withdrawn" => self.balance -= amount,
            _ => {}
        }
    }
}

fn main() -> std::io::Result<()> {
    let db = Db::open(&sutegi::env_or("EVENTS_PATH", "events.db")).expect("open db");
    let store = EventStore::new(db.clone());
    store.migrate().expect("migrate event store");

    // The read model lives in an ordinary table, owned by its projection.
    db.execute(
        "CREATE TABLE IF NOT EXISTS account_balances (\
            stream TEXT PRIMARY KEY, balance BIGINT NOT NULL DEFAULT 0)",
        &[],
    )
    .expect("create read model");
    let mut projections = Projections::new(db.clone()).poll_interval(Duration::from_millis(200));
    projections.register("account_balances", |e, tx| {
        let amount = e.payload.get("amount").and_then(Json::as_i64).unwrap_or(0);
        let delta = match e.name.as_str() {
            "deposited" => amount,
            "withdrawn" => -amount,
            _ => return Ok(()),
        };
        tx.execute(
            "INSERT INTO account_balances (stream, balance) VALUES (?, ?) \
             ON CONFLICT (stream) DO UPDATE SET \
             balance = account_balances.balance + excluded.balance",
            &[Value::Text(e.stream.clone()), Value::Int(delta)],
        )?;
        Ok(())
    });
    projections.migrate().expect("migrate projections");
    let _workers = Arc::new(projections).start(); // held: dropping it stops the workers

    App::new("ledger-demo")
        .state(store)
        .post("/accounts/:id/deposit", "Deposit into an account.", |c| {
            let amount = amount_from(c)?;
            let version = command(c, |_| Ok(event("deposited", amount_payload(amount))))?;
            Ok::<_, Error>(json(200, &Json::obj(vec![("version", Json::int(version))])))
        })
        .post(
            "/accounts/:id/withdraw",
            "Withdraw — rejected if it would overdraw.",
            |c| {
                let amount = amount_from(c)?;
                let version = command(c, |account| {
                    if account.balance < amount {
                        return Err(Error::unprocessable(format!(
                            "insufficient funds: balance is {}",
                            account.balance
                        )));
                    }
                    Ok(event("withdrawn", amount_payload(amount)))
                })?;
                Ok::<_, Error>(json(200, &Json::obj(vec![("version", Json::int(version))])))
            },
        )
        .get(
            "/accounts/:id",
            "Balance + version, folded live from the stream.",
            |c| {
                let (account, version) = c.state::<Store>().load::<Account>(&stream(c))?;
                Ok::<_, Error>(json(
                    200,
                    &Json::obj(vec![
                        ("balance", Json::int(account.balance)),
                        ("version", Json::int(version)),
                    ]),
                ))
            },
        )
        .get("/accounts/:id/history", "The account's raw events.", |c| {
            let events = c.state::<Store>().read_stream(&stream(c), 0)?;
            Ok::<_, Error>(json(
                200,
                &Json::Arr(events.iter().map(StoredEvent::to_json).collect()),
            ))
        })
        .get(
            "/accounts",
            "All balances from the projected read model (eventually consistent).",
            |c| {
                let rows = c.state::<Store>().backend().query(
                    "SELECT stream, balance FROM account_balances ORDER BY stream",
                    &[],
                )?;
                Ok::<_, Error>(json(200, &Json::Arr(rows)))
            },
        )
        .get(
            "/__events",
            "Event store stats: head, streams, projection lag.",
            |c| Ok::<_, Error>(json(200, &c.state::<Store>().stats()?)),
        )
        .serve()
}

/// Load → decide → append with optimistic concurrency, retrying the decision
/// when a concurrent command wins the version race.
fn command(
    c: &Ctx,
    decide: impl Fn(&Account) -> Result<sutegi::events::NewEvent, Error>,
) -> Result<i64, Error> {
    let store = c.state::<Store>();
    let stream = stream(c);
    for _ in 0..3 {
        let (account, version) = store.load::<Account>(&stream)?;
        let event = decide(&account)?;
        match store.append(&stream, Expected::Version(version), &[event]) {
            Ok(version) => return Ok(version),
            Err(EventError::Conflict { .. }) => continue, // someone got there first: re-decide
            Err(EventError::Store(e)) => return Err(Error::internal(e)),
        }
    }
    Err(Error::new(409, "too much contention, retry"))
}

fn stream(c: &Ctx) -> String {
    format!("account:{}", c.param("id").unwrap_or(""))
}

fn amount_from(c: &Ctx) -> Result<i64, Error> {
    match c.json()?.get("amount").and_then(Json::as_i64) {
        Some(n) if n > 0 => Ok(n),
        _ => Err(Error::unprocessable("amount must be a positive integer")),
    }
}

fn amount_payload(amount: i64) -> Json {
    Json::obj(vec![("amount", Json::int(amount))])
}

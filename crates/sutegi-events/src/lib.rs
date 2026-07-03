//! **Event sourcing** for sutegi: state as an append-only log of facts.
//!
//! Instead of overwriting rows, an event-sourced app appends immutable events
//! (`"deposited"`, `"order.shipped"`) to per-entity **streams**, and derives
//! every current-state view from them: fold a stream into an [`Aggregate`] to
//! make a decision, or run [`Projections`] in the background to maintain
//! queryable read models. The log is the source of truth; everything else can
//! be rebuilt from it.
//!
//! Three pieces, all riding on the ORM's [`Backend`](sutegi_orm::Backend) seam
//! so the same code runs on SQLite (single-node) or Postgres (multi-pod):
//!
//! - [`EventStore`] — append events with **optimistic concurrency**
//!   ([`Expected`]), read streams, fold aggregates. Appends are transactional
//!   and safe across pods: conflicts surface as [`EventError::Conflict`].
//! - [`Aggregate`] — a `Default` state plus `apply(event)`; [`EventStore::load`]
//!   folds a stream into `(state, version)` for the load → decide → append loop.
//! - [`Projections`] — named background consumers with durable checkpoints.
//!   Each batch runs **in one transaction with the checkpoint update**, so a
//!   read model is exactly-once per projection and rebuildable via
//!   [`Projections::reset`].
//!
//! ```no_run
//! use sutegi_events::{event, EventStore, Expected};
//! use sutegi_json::Json;
//! use sutegi_orm::db::Db;
//!
//! let store = EventStore::new(Db::open("app.db").unwrap());
//! store.migrate().unwrap();
//! store
//!     .append(
//!         "account:42",
//!         Expected::NoStream,
//!         &[event("opened", Json::obj(vec![("owner", Json::str("eneko"))]))],
//!     )
//!     .unwrap();
//! ```
//!
//! ## Ordering and the global log
//!
//! Every event gets a gap-free global `position`, assigned inside the append
//! transaction as `MAX(position) + 1` and protected by the primary key: two
//! racing appends can't both commit the same position, so the loser retries
//! with a fresh one. Readers paging `position > checkpoint` therefore never
//! skip an event — the property projections depend on. The cost is that
//! appends serialize at the head of the log (writes to *different* streams
//! still conflict on `position`); plenty for a typical app, and the honest
//! trade against gap-scanning complexity.
//!
//! A projection handler that keeps failing halts its projection (the batch
//! rolls back and is retried each poll) — deliberate, because ordered
//! processing cannot skip an event. Fix the handler; the checkpoint hasn't
//! moved.

mod projection;
mod store;

pub use projection::{ProjectionFn, ProjectionWorkers, Projections};
pub use store::{append_tx, event, EventError, EventStore, Expected, NewEvent, StoredEvent};

/// Current state folded from a stream: start at `Default`, [`apply`]
/// (Aggregate::apply) each stored event in order. Keep `apply` pure — it runs
/// on every load and on any future replay.
///
/// Decision logic (command handling) stays outside the trait: load the
/// aggregate, decide, then [`EventStore::append`] with the loaded version as
/// [`Expected::Version`] so a concurrent writer surfaces as a conflict.
pub trait Aggregate: Default {
    fn apply(&mut self, event: &StoredEvent);
}

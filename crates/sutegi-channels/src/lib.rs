#![forbid(unsafe_code)]
//! Phoenix-style channels for sutegi: topics, joins, replies, broadcasts —
//! multiplexed over one WebSocket, fanned out across pods via the pubsub
//! [`Broker`](sutegi_pubsub::Broker) seam, and introspectable by agents at
//! `/__channels`.
//!
//! ## The pieces
//!
//! - [`Channel`] — declare a topic pattern (`"room:*"`), a join callback
//!   (auth + setup), event handlers, and the schemas the manifest advertises.
//! - [`Channels`] — the builder: register channels, pick a broker
//!   (in-process by default; `PgPubSub` for cross-pod), set handshake gates.
//! - [`ChannelHub`] — the runtime: wire it to a WebSocket endpoint
//!   (`App::channels` in the facade does this), broadcast from anywhere.
//! - [`Socket`] — a member's view inside callbacks: `push`, `broadcast`,
//!   `broadcast_from`, `assign`, `kick`.
//!
//! ## The contract, honestly
//!
//! Delivery is **at-most-once, fire-and-forget** — Phoenix's contract too.
//! A slow consumer is disconnected at the transport's buffer cap; a client
//! that reconnects rejoins its topics (the bundled JS client does) and
//! missed messages are gone. If a message must survive that, put it in a
//! table and let the channel announce *that there is news*, not the news.
//!
//! Handlers run inline on reactor shards (the `sutegi-ws` contract): quick
//! CPU work only, push anything blocking to your own threads, reply via the
//! cheap-to-clone [`Socket`]/[`ChannelHub`] handles.
//!
//! ## Agent-native
//!
//! `/__channels` (mounted by `App::channels`) describes the envelope
//! protocol, every channel pattern, and per-event payload schemas — an LLM
//! can read it and speak the protocol over a raw WebSocket with no SDK. The
//! same manifest is where the bundled human client comes from:
//! [`JS_CLIENT`] is a dependency-free browser client (~4 KB) served however
//! you serve assets.

mod channel;
mod hub;
#[cfg(feature = "presence")]
mod presence;
pub mod protocol;

pub use channel::{Channel, LeaveReason, Reply};
pub use hub::{ChannelHub, Channels, Socket};
#[cfg(feature = "presence")]
pub use presence::Presence;

/// The bundled dependency-free browser client (`Socket`/`Channel`/`Push`
/// classes, auto-rejoin with backoff, heartbeats). Serve it as a static
/// asset:
///
/// ```ignore
/// app.get("/channels.js", "The channels client.", |_c| {
///     Response::new(200)
///         .with_header("content-type", "application/javascript")
///         .with_body(sutegi_channels::JS_CLIENT.as_bytes())
/// })
/// ```
pub const JS_CLIENT: &str = include_str!("../client/sutegi-channels.js");

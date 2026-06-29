//! Adapters — the outer ring. Inbound adapters drive the application (HTTP, AI);
//! outbound adapters are driven by it (persistence). All framework- and
//! IO-specific code lives here; the layers below stay clean.

pub mod inbound_ai;
pub mod inbound_http;
pub mod outbound_memory;
pub mod outbound_sqlite;

//! WebSockets for sutegi: an RFC 6455 codec plus a sharded reactor built on
//! `kqueue`/`epoll`, designed so one process can hold very large numbers of
//! live connections.
//!
//! ## The architecture split
//!
//! sutegi's HTTP side stays blocking thread-per-connection — that model is
//! simple, debuggable, and right for request/response. But a WebSocket is a
//! mostly-idle socket that lives for hours; pinning a thread per socket caps
//! a process at thousands. So upgraded sockets **detach**: the HTTP worker
//! writes the `101`, hands the raw socket to [`WsRuntime`], and goes back to
//! serving requests. From then on the socket belongs to a reactor shard and
//! costs a few hundred bytes of user-space state. Blocking where blocking is
//! right, evented where evented is right — and still no async runtime, no
//! futures, no third-party event loop; the reactor is a plain thread around
//! a poller syscall.
//!
//! ## Scale honesty
//!
//! User-space state is ~300 bytes per idle connection (measure with the
//! `ws-load` example; don't trust prose). The real budget at millions of
//! sockets is the kernel's: each TCP socket holds receive/send buffers
//! (tune `net.ipv4.tcp_rmem`/`tcp_wmem` floors on Linux), fd limits must be
//! raised (done automatically at startup), and a single broadcast to 1M
//! sockets is 1M `write(2)` calls — plan fan-out rates accordingly. macOS
//! dev boxes cap out far earlier than a tuned Linux host.
//!
//! This crate is transport only. Topics/rooms, presence, and rejoin
//! semantics are the channels layer built on top of it.

mod poller;
pub mod protocol;
mod reactor;

pub use poller::raise_nofile_limit;
pub use protocol::accept_key;
pub use reactor::{binary_frame, broadcast, text_frame, Conn, Handlers, Msg, WsConfig, WsRuntime};

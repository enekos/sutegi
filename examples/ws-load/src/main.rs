//! WebSocket stress harness: how many live sockets can one sutegi process
//! hold, and how fast does a broadcast reach all of them?
//!
//! Two roles in one binary:
//!
//! ```sh
//! # terminal 1 — the server. Listens on PORTS consecutive ports (all feeding
//! # ONE WsRuntime) so a loopback client can get past the ~16k ephemeral
//! # ports available per (src ip, dst ip, dst port) tuple.
//! ws-load server [base_port=9100] [ports=8]
//!
//! # terminal 2 — the client fleet. Opens CONNS sockets round-robin over the
//! # ports, holds them, and measures every broadcast's delivery latency.
//! ws-load client [base_port=9100] [ports=8] [conns=100000] [hold_secs=60]
//!
//! # terminal 3 — trigger fan-out + read stats.
//! curl -X POST localhost:9100/broadcast
//! curl localhost:9100/stats
//! ```
//!
//! Latency numbers include the client's own scan loop (a single thread
//! polling every socket), so p99 is an upper bound, not a pure server number.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("");
    let num = |i: usize, default: usize| -> usize {
        args.get(i).and_then(|s| s.parse().ok()).unwrap_or(default)
    };
    match role {
        "server" => server(num(2, 9100) as u16, num(3, 8) as u16),
        "client" => {
            client(
                num(2, 9100) as u16,
                num(3, 8) as u16,
                num(4, 100_000),
                num(5, 60) as u64,
            );
            Ok(())
        }
        _ => {
            eprintln!("usage: ws-load server [base_port] [ports]");
            eprintln!("       ws-load client [base_port] [ports] [conns] [hold_secs]");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------
// server
// ---------------------------------------------------------------------------

static ROSTER: Mutex<Option<HashMap<u64, Conn>>> = Mutex::new(None);

fn now_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros()
}

fn server(base_port: u16, ports: u16) -> std::io::Result<()> {
    *ROSTER.lock().unwrap() = Some(HashMap::new());

    let app = App::new("ws-load")
        .ws_config(WsConfig {
            // Long timers: idle *holding* is the point of the test, and the
            // client fleet doesn't answer pings promptly while it scans.
            ping_interval: Duration::from_secs(120),
            idle_timeout: Duration::from_secs(600),
            // The fleet comes from loopback; disable the per-IP cap so the
            // harness can actually reach six-figure connection counts.
            max_connections_per_ip: 0,
            ..WsConfig::default()
        })
        .ws(
            "/ws",
            "Load socket: joins the fleet, echoes nothing.",
            Ws::new()
                .on_open(|conn: &Conn, _req: &Request| {
                    if let Some(r) = ROSTER.lock().unwrap().as_mut() {
                        r.insert(conn.id(), conn.clone());
                    }
                })
                .on_close(|conn: &Conn, _code| {
                    if let Some(r) = ROSTER.lock().unwrap().as_mut() {
                        r.remove(&conn.id());
                    }
                }),
        )
        .get("/stats", "Live connection count.", |_c| {
            let conns = ROSTER
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.len())
                .unwrap_or(0);
            text(200, &format!("{{\"connections\":{conns}}}"))
        })
        .route(
            Method::Post,
            "/broadcast",
            "Fan one frame out to every connection; returns timings.",
            |_c| {
                let t0 = Instant::now();
                // The broadcast payload carries the send timestamp so each
                // client can compute its own delivery latency.
                let frame = text_frame(&format!("t:{}", now_micros()));
                let sent = {
                    let guard = ROSTER.lock().unwrap();
                    let roster = guard.as_ref().unwrap();
                    for conn in roster.values() {
                        conn.send_shared(&frame);
                    }
                    roster.len()
                };
                let enqueue_micros = t0.elapsed().as_micros();
                text(
                    200,
                    &format!("{{\"sent\":{sent},\"enqueue_micros\":{enqueue_micros}}}"),
                )
            },
        );

    // One service closure, N listeners, one shared reactor: this is only
    // needed on loopback (ephemeral-port math); a real deployment listens
    // once.
    let svc = Arc::new(app.service());
    let mut handles = Vec::new();
    for i in 0..ports {
        let svc = Arc::clone(&svc);
        let addr = format!("127.0.0.1:{}", base_port + i);
        println!("listening on {addr}");
        handles.push(thread::spawn(move || {
            sutegi::http::serve(&addr, 16, Limits::default(), move |req| svc(req))
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// client
// ---------------------------------------------------------------------------

struct Sock {
    stream: TcpStream,
    /// Carry-over bytes when a frame arrives split across reads.
    buf: Vec<u8>,
}

fn client(base_port: u16, ports: u16, conns: usize, hold_secs: u64) {
    let limit = sutegi_ws::raise_nofile_limit();
    println!("fd limit: {limit}");
    assert!(
        (limit as usize) > conns + 64,
        "fd limit {limit} too low for {conns} connections"
    );

    // ---- connect phase ----
    let t0 = Instant::now();
    let mut socks: Vec<Sock> = Vec::with_capacity(conns);
    let mut failures = 0usize;
    // Explicit source ports: the OS ephemeral allocator is global (~16k on
    // macOS), but a (src ip, src port, dst ip, dst port) tuple only needs to
    // be unique — so we walk our own src range and pair it with the dst
    // round-robin, giving src_range × ports capacity.
    let mut next_src: u16 = 20000;
    let bump = |src: &mut u16| {
        *src = if *src >= 65500 { 20000 } else { *src + 1 };
    };
    for i in 0..conns {
        let port = base_port + (i % ports as usize) as u16;
        let mut opened = None;
        for _attempt in 0..64 {
            let src = next_src;
            bump(&mut next_src);
            match open_ws(port, src) {
                Ok(stream) => {
                    opened = Some(stream);
                    break;
                }
                Err(e) => {
                    failures += 1;
                    if failures <= 5 {
                        eprintln!("connect {i} via src {src} failed: {e} (retrying)");
                    }
                }
            }
        }
        match opened {
            Some(stream) => socks.push(Sock {
                stream,
                buf: Vec::new(),
            }),
            None => {
                eprintln!(
                    "connect {i}: exhausted retries, stopping at {} sockets",
                    socks.len()
                );
                break;
            }
        }
        if (i + 1) % 10_000 == 0 {
            println!(
                "connected {}/{conns} ({:.0}/s)",
                i + 1,
                (i + 1) as f64 / t0.elapsed().as_secs_f64()
            );
        }
    }
    println!(
        "fleet up: {} sockets in {:.1}s ({} retries exhausted)",
        socks.len(),
        t0.elapsed().as_secs_f64(),
        conns - socks.len()
    );

    // ---- hold + measure phase ----
    // The fleet is split across reader threads so the client's own scan loop
    // doesn't dominate the delivery numbers (one thread draining N sockets
    // serializes the measurement).
    let deadline = Instant::now() + Duration::from_secs(hold_secs);
    let fleet = socks.len().max(1);
    let received = Arc::new(AtomicU64::new(0));
    let latencies: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let readers = 8usize;
    let chunk = fleet.div_ceil(readers);
    println!("holding for {hold_secs}s — POST /broadcast on the server to measure fan-out");
    let mut handles = Vec::new();
    while !socks.is_empty() {
        let mut mine: Vec<Sock> = socks.drain(..chunk.min(socks.len())).collect();
        let received = Arc::clone(&received);
        let latencies = Arc::clone(&latencies);
        handles.push(thread::spawn(move || {
            let mut local: Vec<u64> = Vec::new();
            while Instant::now() < deadline {
                let mut any = false;
                for sock in mine.iter_mut() {
                    if let Some(payload) = poll_frame(sock) {
                        any = true;
                        if let Some(t) = payload
                            .strip_prefix("t:")
                            .and_then(|s| s.parse::<u128>().ok())
                        {
                            local.push(now_micros().saturating_sub(t) as u64);
                        }
                    }
                }
                if !local.is_empty() {
                    received.fetch_add(local.len() as u64, Ordering::Relaxed);
                    latencies.lock().unwrap().extend_from_slice(&local);
                    local.clear();
                }
                if !any {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }));
    }

    // Coordinator: report a round once every socket has received one frame.
    let mut reported = 0u64;
    let mut round_started: Option<Instant> = None;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        let n = received.load(Ordering::Relaxed);
        if round_started.is_none() && n > reported {
            round_started = Some(Instant::now());
        }
        if n - reported >= fleet as u64 {
            let mut all = latencies.lock().unwrap();
            report(&mut all, round_started.take());
            reported = n;
        }
    }
    for h in handles {
        let _ = h.join();
    }
    let mut rest = latencies.lock().unwrap();
    if !rest.is_empty() {
        report(&mut rest, round_started.take());
    }
    println!("done; closing fleet");
}

fn report(latencies: &mut Vec<u64>, round_started: Option<Instant>) {
    latencies.sort_unstable();
    let n = latencies.len();
    let pct = |p: f64| latencies[((n as f64 * p) as usize).min(n - 1)];
    println!(
        "round complete: {n} frames | delivery latency µs: p50={} p90={} p99={} max={} | drain-to-report {:?}",
        pct(0.50),
        pct(0.90),
        pct(0.99),
        latencies[n - 1],
        round_started.map(|s| s.elapsed()).unwrap_or_default(),
    );
    latencies.clear();
}

/// Open one WebSocket from an explicit source port: TCP connect + RFC 6455
/// handshake, then non-blocking.
fn open_ws(port: u16, src_port: u16) -> std::io::Result<TcpStream> {
    let mut stream = connect_bound(src_port, port)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /ws HTTP/1.1\r\nHost: load\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )?;
    // Read the 101 head to its blank line, byte-wise (no over-read: frames
    // may follow immediately).
    let mut last4 = [0u8; 4];
    let mut b = [0u8; 1];
    loop {
        stream.read_exact(&mut b)?;
        last4.rotate_left(1);
        last4[3] = b[0];
        if &last4 == b"\r\n\r\n" {
            break;
        }
    }
    stream.set_nonblocking(true)?;
    Ok(stream)
}

/// Try to pull one complete server frame out of a non-blocking socket.
/// Answers pings inline; returns text payloads.
/// `socket(2)` + `SO_REUSEADDR`/`SO_REUSEPORT` + `bind(2)` to a chosen source
/// port + `connect(2)` — what `TcpStream::connect` can't express.
fn connect_bound(src_port: u16, dst_port: u16) -> std::io::Result<TcpStream> {
    use std::os::fd::FromRawFd;
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let one: libc::c_int = 1;
        for opt in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &one as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = src_port.to_be();
        addr.sin_addr.s_addr = u32::from_be_bytes([127, 0, 0, 1]).to_be();
        let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        if libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) != 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        addr.sin_port = dst_port.to_be();
        if libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len) != 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(TcpStream::from_raw_fd(fd))
    }
}

fn poll_frame(sock: &mut Sock) -> Option<String> {
    let mut tmp = [0u8; 512];
    match sock.stream.read(&mut tmp) {
        Ok(0) => return None, // closed; leave the corpse to the hold loop
        Ok(n) => sock.buf.extend_from_slice(&tmp[..n]),
        Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
        Err(_) => return None,
    }
    if sock.buf.len() < 2 {
        return None;
    }
    let opcode = sock.buf[0] & 0x0F;
    let len = (sock.buf[1] & 0x7F) as usize;
    if len > 125 || sock.buf.len() < 2 + len {
        return None; // stress frames are small; anything else waits
    }
    let payload: Vec<u8> = sock.buf[2..2 + len].to_vec();
    sock.buf.drain(..2 + len);
    match opcode {
        0x9 => {
            // Pong (masked, client-to-server) so long holds survive pings.
            let mask = [3u8, 1, 4, 1];
            let mut pong = vec![0x8A, 0x80 | payload.len() as u8];
            pong.extend_from_slice(&mask);
            pong.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
            let _ = sock.stream.write_all(&pong);
            None
        }
        0x1 => String::from_utf8(payload).ok(),
        _ => None,
    }
}

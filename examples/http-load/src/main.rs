//! HTTP/1.1 keep-alive stress harness.
//!
//! Two roles in one binary:
//!
//! ```sh
//! # terminal 1 — the server.
//! http-load server [addr=127.0.0.1:9400]
//!
//! # terminal 2 — the client fleet.
//! http-load client [addr=127.0.0.1:9400] [concurrency=64] [requests=100000]
//! ```
//!
//! Each worker keeps one persistent connection and sends requests back-to-back,
//! so the test measures the real amortized cost of keep-alive request handling
//! plus the thread-pool saturation point.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("");
    let num = |i: usize, default: usize| -> usize {
        args.get(i).and_then(|s| s.parse().ok()).unwrap_or(default)
    };
    match role {
        "server" => server(args.get(2).map(String::as_str).unwrap_or("127.0.0.1:9400")),
        "client" => {
            client(
                args.get(2).map(String::as_str).unwrap_or("127.0.0.1:9400"),
                num(3, 64),
                num(4, 100_000),
            );
            Ok(())
        }
        _ => {
            eprintln!("usage: http-load server [addr]");
            eprintln!("       http-load client [addr] [concurrency] [requests]");
            std::process::exit(2);
        }
    }
}

fn server(addr: &str) -> std::io::Result<()> {
    App::new("http-load")
        .get("/", "Health check.", |_| "sutegi up")
        .run(addr)
}

fn client(addr: &str, concurrency: usize, requests: usize) {
    assert!(
        concurrency > 0 && requests > 0,
        "concurrency and requests must be > 0"
    );

    let per_worker = requests / concurrency;
    let remainder = requests % concurrency;
    let total = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let latencies: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::with_capacity(requests)));

    let t0 = Instant::now();
    let mut handles = Vec::with_capacity(concurrency);
    for i in 0..concurrency {
        let count = per_worker + if i < remainder { 1 } else { 0 };
        let total = Arc::clone(&total);
        let errors = Arc::clone(&errors);
        let latencies = Arc::clone(&latencies);
        let addr = addr.to_string();
        handles.push(thread::spawn(move || {
            worker(&addr, count, total, errors, latencies);
        }));
    }

    for h in handles {
        let _ = h.join();
    }
    let elapsed = t0.elapsed();

    let total = total.load(Ordering::Relaxed);
    let errors = errors.load(Ordering::Relaxed);
    let mut all = latencies.lock().unwrap();
    all.sort_unstable();
    let n = all.len();
    let pct = |p: f64| all[((n as f64 * p) as usize).min(n.saturating_sub(1))];

    println!(
        "requests={} errors={} concurrency={} elapsed={:.3}s throughput={:.0} req/s",
        total,
        errors,
        concurrency,
        elapsed.as_secs_f64(),
        total as f64 / elapsed.as_secs_f64(),
    );
    if n > 0 {
        println!(
            "latency µs: p50={} p90={} p99={} p999={} max={}",
            pct(0.50),
            pct(0.90),
            pct(0.99),
            pct(0.999),
            all[n - 1]
        );
    }
}

fn worker(
    addr: &str,
    count: usize,
    total: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    latencies: Arc<Mutex<Vec<u64>>>,
) {
    let mut conn = Connection::open(addr);
    let mut local: Vec<u64> = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let t0 = Instant::now();
        let res = conn.request("GET / HTTP/1.1\r\nHost: load\r\nConnection: keep-alive\r\n\r\n");
        let elapsed = t0.elapsed().as_micros() as u64;
        match res {
            Ok(true) => {}
            Ok(false) => {
                // Server closed; reopen and retry once.
                conn = Connection::open(addr);
                if conn
                    .request("GET / HTTP/1.1\r\nHost: load\r\nConnection: keep-alive\r\n\r\n")
                    .is_err()
                {
                    errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
            Err(_) => {
                errors.fetch_add(1, Ordering::Relaxed);
                conn = Connection::open(addr);
                continue;
            }
        }
        local.push(elapsed);
        total.fetch_add(1, Ordering::Relaxed);
        if local.len() >= 1024 {
            latencies.lock().unwrap().extend_from_slice(&local);
            local.clear();
        }
    }
    if !local.is_empty() {
        latencies.lock().unwrap().extend_from_slice(&local);
    }
}

struct Connection {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Connection {
    fn open(addr: &str) -> Self {
        let stream = TcpStream::connect(addr).expect("connect failed");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().expect("clone failed"));
        Self { stream, reader }
    }

    /// Send a request and read the response headers + body. Returns `Ok(true)`
    /// if the connection can stay open, `Ok(false)` if the server signalled
    /// close, and `Err` on a hard I/O failure.
    fn request(&mut self, req: &str) -> std::io::Result<bool> {
        self.stream.write_all(req.as_bytes())?;
        self.stream.flush()?;

        let mut status = String::new();
        self.reader.read_line(&mut status)?;
        if status.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "empty status line",
            ));
        }

        let mut content_len = None;
        let mut connection_close = false;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line)?;
            if line == "\r\n" || line.is_empty() {
                break;
            }
            let lower = line.to_lowercase();
            if lower.starts_with("content-length:") {
                content_len = lower
                    .split(':')
                    .nth(1)
                    .and_then(|s| s.trim().parse::<usize>().ok());
            }
            if lower.starts_with("connection:") && lower.contains("close") {
                connection_close = true;
            }
        }

        // Drain body.
        if let Some(len) = content_len {
            let mut buf = vec![0u8; len];
            self.reader.read_exact(&mut buf)?;
        } else {
            // No content-length; for this harness we treat it as zero body.
        }

        Ok(!connection_close)
    }
}

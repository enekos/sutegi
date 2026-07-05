//! End-to-end reactor tests over real sockets: a raw TCP client speaking
//! masked RFC 6455 frames against an adopted server connection.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sutegi_http::{Method, Request};
use sutegi_ws::{text_frame, Conn, Handlers, Msg, WsConfig, WsRuntime};

// ---- client-side wire helpers ----------------------------------------------

fn client_frame(opcode: u8, payload: &[u8], fin: bool) -> Vec<u8> {
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let mut out = vec![if fin { 0x80 } else { 0x00 } | opcode];
    match payload.len() {
        n if n <= 125 => out.push(0x80 | n as u8),
        n if n <= 65535 => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
    out
}

/// Read one server frame (unmasked) off the socket → (opcode, payload).
fn read_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).expect("frame header");
    let opcode = hdr[0] & 0x0F;
    assert_eq!(hdr[1] & 0x80, 0, "server frames must be unmasked");
    let len = match hdr[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            stream.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            stream.read_exact(&mut b).unwrap();
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    (opcode, payload)
}

fn upgrade_request() -> Request {
    Request {
        method: Method::Get,
        path: "/ws".into(),
        query: String::new(),
        version: "HTTP/1.1".into(),
        headers: vec![("upgrade".into(), "websocket".into())],
        body: Vec::new(),
        peer: None,
    }
}

/// Bind, adopt one accepted connection into a fresh runtime, return the
/// client socket.
fn connect_pair(handlers: Handlers, cfg: WsConfig) -> (Arc<WsRuntime>, TcpStream) {
    let rt = WsRuntime::start(cfg).expect("runtime");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    rt.adopt(server, Vec::new(), Arc::new(handlers), upgrade_request())
        .expect("adopt");
    (rt, client)
}

fn wait_for<F: Fn() -> bool>(what: &str, cond: F) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn test_config() -> WsConfig {
    WsConfig {
        shards: 1,
        raise_nofile: false,
        ..WsConfig::default()
    }
}

// ---- tests ------------------------------------------------------------------

#[test]
fn echo_roundtrip_and_open_close_callbacks() {
    let opened = Arc::new(AtomicUsize::new(0));
    let closed_code = Arc::new(AtomicU16::new(0));
    let handlers = Handlers {
        on_open: Some(Arc::new({
            let opened = Arc::clone(&opened);
            move |_conn: &Conn, req: &Request| {
                assert_eq!(req.path, "/ws");
                opened.fetch_add(1, Ordering::SeqCst);
            }
        })),
        on_message: Some(Arc::new(|conn: &Conn, msg: Msg| {
            if let Msg::Text(t) = msg {
                conn.send_text(&format!("echo:{t}"));
            }
        })),
        on_close: Some(Arc::new({
            let closed = Arc::clone(&closed_code);
            move |_conn: &Conn, code: u16| closed.store(code, Ordering::SeqCst)
        })),
    };
    let (rt, mut client) = connect_pair(handlers, test_config());
    wait_for("open", || opened.load(Ordering::SeqCst) == 1);
    assert_eq!(rt.connections(), 1);

    client
        .write_all(&client_frame(0x1, b"kaixo", true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x1);
    assert_eq!(payload, b"echo:kaixo");

    // Clean close handshake: we send close, server echoes it, socket dies.
    client
        .write_all(&client_frame(0x8, &1000u16.to_be_bytes(), true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x8);
    assert_eq!(&payload[..2], &1000u16.to_be_bytes());

    wait_for("close callback", || {
        closed_code.load(Ordering::SeqCst) == 1000
    });
    wait_for("conn count zero", || rt.connections() == 0);
}

#[test]
fn fragmented_message_reassembles() {
    let handlers = Handlers {
        on_message: Some(Arc::new(|conn: &Conn, msg: Msg| {
            if let Msg::Text(t) = msg {
                conn.send_text(&t);
            }
        })),
        ..Handlers::default()
    };
    let (_rt, mut client) = connect_pair(handlers, test_config());

    client
        .write_all(&client_frame(0x1, b"one ", false))
        .unwrap();
    client
        .write_all(&client_frame(0x0, b"two ", false))
        .unwrap();
    client
        .write_all(&client_frame(0x0, b"three", true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x1);
    assert_eq!(payload, b"one two three");
}

#[test]
fn ping_gets_pong_with_payload() {
    let (_rt, mut client) = connect_pair(Handlers::default(), test_config());
    client
        .write_all(&client_frame(0x9, b"marco", true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0xA);
    assert_eq!(payload, b"marco");
}

#[test]
fn protocol_violation_closes_with_1002() {
    let (_rt, mut client) = connect_pair(Handlers::default(), test_config());
    // Continuation frame with no message in flight.
    client
        .write_all(&client_frame(0x0, b"orphan", true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x8);
    assert_eq!(&payload[..2], &1002u16.to_be_bytes());
}

#[test]
fn invalid_utf8_text_closes_with_1007() {
    let (_rt, mut client) = connect_pair(Handlers::default(), test_config());
    client
        .write_all(&client_frame(0x1, &[0xFF, 0xFE, 0xFD], true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x8);
    assert_eq!(&payload[..2], &1007u16.to_be_bytes());
}

#[test]
fn oversized_frame_closes_with_1009() {
    let cfg = WsConfig {
        max_frame: 1024,
        ..test_config()
    };
    let (_rt, mut client) = connect_pair(Handlers::default(), cfg);
    client
        .write_all(&client_frame(0x2, &vec![0u8; 4096], true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x8);
    assert_eq!(&payload[..2], &1009u16.to_be_bytes());
}

#[test]
fn pipelined_leftover_bytes_are_processed() {
    // Frames that arrived glued to the HTTP upgrade request reach the
    // handler without waiting for new socket activity.
    let handlers = Handlers {
        on_message: Some(Arc::new(|conn: &Conn, msg: Msg| {
            if let Msg::Text(t) = msg {
                conn.send_text(&format!("got:{t}"));
            }
        })),
        ..Handlers::default()
    };
    let rt = WsRuntime::start(test_config()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    rt.adopt(
        server,
        client_frame(0x1, b"early", true),
        Arc::new(handlers),
        upgrade_request(),
    )
    .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x1);
    assert_eq!(payload, b"got:early");
}

#[test]
fn broadcast_shares_one_encoded_frame() {
    let roster: Arc<Mutex<Vec<Conn>>> = Arc::new(Mutex::new(Vec::new()));
    let handlers = Handlers {
        on_open: Some(Arc::new({
            let roster = Arc::clone(&roster);
            move |conn: &Conn, _req: &Request| roster.lock().unwrap().push(conn.clone())
        })),
        ..Handlers::default()
    };
    let rt = WsRuntime::start(WsConfig {
        shards: 2,
        raise_nofile: false,
        ..WsConfig::default()
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handlers = Arc::new(handlers);

    let mut clients = Vec::new();
    for _ in 0..10 {
        let client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        rt.adopt(server, Vec::new(), Arc::clone(&handlers), upgrade_request())
            .unwrap();
        clients.push(client);
    }
    wait_for("all opened", || roster.lock().unwrap().len() == 10);

    let frame = text_frame("fan-out");
    for conn in roster.lock().unwrap().iter() {
        conn.send_shared(&frame);
    }
    for client in &mut clients {
        let (op, payload) = read_frame(client);
        assert_eq!(op, 0x1);
        assert_eq!(payload, b"fan-out");
    }
}

#[test]
fn server_initiated_close_completes_handshake() {
    let roster: Arc<Mutex<Vec<Conn>>> = Arc::new(Mutex::new(Vec::new()));
    let handlers = Handlers {
        on_open: Some(Arc::new({
            let roster = Arc::clone(&roster);
            move |conn: &Conn, _req: &Request| roster.lock().unwrap().push(conn.clone())
        })),
        ..Handlers::default()
    };
    let (rt, mut client) = connect_pair(handlers, test_config());
    wait_for("open", || !roster.lock().unwrap().is_empty());

    roster.lock().unwrap()[0].close(1001, "going away");
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x8);
    assert_eq!(&payload[..2], &1001u16.to_be_bytes());
    assert_eq!(&payload[2..], b"going away");
    // Acknowledge; the server should then drop the socket entirely.
    client
        .write_all(&client_frame(0x8, &1001u16.to_be_bytes(), true))
        .unwrap();
    wait_for("conn gone", || rt.connections() == 0);

    // A stale handle is harmless after the slot is gone.
    roster.lock().unwrap()[0].send_text("into the void");
}

#[test]
fn slow_consumer_is_dropped_at_buffer_cap() {
    let roster: Arc<Mutex<Vec<Conn>>> = Arc::new(Mutex::new(Vec::new()));
    let handlers = Handlers {
        on_open: Some(Arc::new({
            let roster = Arc::clone(&roster);
            move |conn: &Conn, _req: &Request| roster.lock().unwrap().push(conn.clone())
        })),
        ..Handlers::default()
    };
    let cfg = WsConfig {
        max_buffered: 64 * 1024,
        ..test_config()
    };
    let (rt, client) = connect_pair(handlers, cfg);
    wait_for("open", || !roster.lock().unwrap().is_empty());

    // Never read from `client`; pump until the kernel buffers fill and the
    // server-side queue passes the cap.
    let conn = roster.lock().unwrap()[0].clone();
    let chunk = "x".repeat(16 * 1024);
    for _ in 0..200 {
        conn.send_text(&chunk);
        if rt.connections() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_for("slow consumer dropped", || rt.connections() == 0);
    drop(client);
}

#[test]
fn handler_panic_does_not_kill_the_shard() {
    let handlers = Handlers {
        on_message: Some(Arc::new(|conn: &Conn, msg: Msg| {
            if let Msg::Text(t) = msg {
                if t == "boom" {
                    panic!("handler exploded");
                }
                conn.send_text(&format!("ok:{t}"));
            }
        })),
        ..Handlers::default()
    };
    let (_rt, mut client) = connect_pair(handlers, test_config());
    client.write_all(&client_frame(0x1, b"boom", true)).unwrap();
    // The shard survives and keeps serving the same connection.
    client
        .write_all(&client_frame(0x1, b"still here", true))
        .unwrap();
    let (op, payload) = read_frame(&mut client);
    assert_eq!(op, 0x1);
    assert_eq!(payload, b"ok:still here");
}

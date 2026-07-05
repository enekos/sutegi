//! End-to-end WebSocket coverage through the assembled app: a real RFC 6455
//! handshake against `App::run`, then frames over the upgraded socket. This
//! exercises the whole path the unit tests can't: route dispatch → handshake
//! validation → `Body::Upgrade` detach → reactor adoption → echo.
#![cfg(feature = "ws")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use sutegi::prelude::*;

fn boot(addr: &'static str, build: impl FnOnce() -> App + Send + 'static) {
    thread::spawn(move || {
        let _ = build().run(addr);
    });
    for _ in 0..300 {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("server {addr} did not become ready");
}

fn chat_app() -> App {
    App::new("ws-e2e").ws(
        "/ws",
        "Echo socket.",
        Ws::new().on_message(|conn: &Conn, msg: Msg| {
            if let Msg::Text(t) = msg {
                conn.send_text(&format!("echo:{t}"));
            }
        }),
    )
}

fn handshake(addr: &str, path: &str, extra: &str) -> (TcpStream, Vec<String>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n"
    )
    .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let trimmed = line.trim_end().to_string();
        if trimmed.is_empty() {
            break;
        }
        lines.push(trimmed);
    }
    // No body follows a 101; the BufReader holds no leftover on this path.
    (stream, lines)
}

fn client_text(payload: &[u8]) -> Vec<u8> {
    let mask = [7u8, 7, 7, 7];
    let mut out = vec![0x81];
    assert!(payload.len() <= 125);
    out.push(0x80 | payload.len() as u8);
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
    out
}

fn read_text(stream: &mut TcpStream) -> Vec<u8> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).unwrap();
    assert_eq!(hdr[0], 0x81);
    let mut payload = vec![0u8; (hdr[1] & 0x7F) as usize];
    stream.read_exact(&mut payload).unwrap();
    payload
}

#[test]
fn full_handshake_and_echo() {
    boot("127.0.0.1:18470", chat_app);
    let (mut stream, lines) = handshake("127.0.0.1:18470", "/ws", "");
    assert!(
        lines[0].starts_with("HTTP/1.1 101 Switching Protocols"),
        "{lines:?}"
    );
    // The RFC 6455 worked example key → worked example accept.
    assert!(
        lines
            .iter()
            .any(|l| l.eq_ignore_ascii_case("sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")),
        "{lines:?}"
    );
    assert!(!lines
        .iter()
        .any(|l| l.to_ascii_lowercase().starts_with("content-")));

    stream.write_all(&client_text(b"hello")).unwrap();
    assert_eq!(read_text(&mut stream), b"echo:hello");
}

#[test]
fn handshake_refusals() {
    boot("127.0.0.1:18471", chat_app);

    // Plain GET without upgrade headers → 400.
    let mut stream = TcpStream::connect("127.0.0.1:18471").unwrap();
    stream
        .write_all(b"GET /ws HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 400"));

    // Wrong version → 426 advertising 13.
    let mut stream = TcpStream::connect("127.0.0.1:18471").unwrap();
    stream.write_all(
        b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\nConnection: close\r\n\r\n",
    )
    .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let s = String::from_utf8_lossy(&buf);
    assert!(s.starts_with("HTTP/1.1 426"), "{s}");
    assert!(s.to_ascii_lowercase().contains("sec-websocket-version: 13"));
}

#[test]
fn broadcast_across_two_clients() {
    // A tiny room: every message fans out to all connections.
    static ROSTER: std::sync::Mutex<Vec<Conn>> = std::sync::Mutex::new(Vec::new());
    boot("127.0.0.1:18472", || {
        App::new("room").ws(
            "/room",
            "Broadcast room.",
            Ws::new()
                .on_open(|conn: &Conn, _req: &Request| {
                    ROSTER.lock().unwrap().push(conn.clone());
                })
                .on_message(|_conn: &Conn, msg: Msg| {
                    if let Msg::Text(t) = msg {
                        let frame = text_frame(&t);
                        for peer in ROSTER.lock().unwrap().iter() {
                            peer.send_shared(&frame);
                        }
                    }
                })
                .on_close(|conn: &Conn, _code| {
                    ROSTER.lock().unwrap().retain(|c| c.id() != conn.id());
                }),
        )
    });

    let (mut a, _) = handshake("127.0.0.1:18472", "/room", "");
    let (mut b, _) = handshake("127.0.0.1:18472", "/room", "");
    // Wait until both are in the roster (adoption is async to the 101).
    for _ in 0..100 {
        if ROSTER.lock().unwrap().len() == 2 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    a.write_all(&client_text(b"to the room")).unwrap();
    assert_eq!(read_text(&mut a), b"to the room");
    assert_eq!(read_text(&mut b), b"to the room");
}

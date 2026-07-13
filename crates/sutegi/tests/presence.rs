//! End-to-end presence: track on join, diffs on join/leave/disconnect,
//! cross-pod state sync over a shared broker, and heartbeat expiry of a
//! silently-dead pod.
#![cfg(feature = "presence")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use sutegi::prelude::*;

// A second copy of the minimal ws client (integration tests cannot share
// modules without a common crate; kept deliberately tiny).
struct WsClient {
    stream: TcpStream,
}

impl WsClient {
    fn connect(addr: &str, path: &str) -> WsClient {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.starts_with("HTTP/1.1 101"), "{status}");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim_end().is_empty() {
                break;
            }
        }
        WsClient { stream }
    }

    fn send(&mut self, text: &str) {
        let payload = text.as_bytes();
        let mask = [7u8, 7, 7, 7];
        let mut out = vec![0x81u8];
        match payload.len() {
            n if n <= 125 => out.push(0x80 | n as u8),
            n => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        self.stream.write_all(&out).unwrap();
    }

    fn recv(&mut self) -> Json {
        let mut hdr = [0u8; 2];
        self.stream.read_exact(&mut hdr).unwrap();
        assert_eq!(hdr[0], 0x81, "expected a FIN text frame");
        let len = match hdr[1] & 0x7F {
            126 => {
                let mut ext = [0u8; 2];
                self.stream.read_exact(&mut ext).unwrap();
                u16::from_be_bytes(ext) as usize
            }
            127 => {
                let mut ext = [0u8; 8];
                self.stream.read_exact(&mut ext).unwrap();
                u64::from_be_bytes(ext) as usize
            }
            n => n as usize,
        };
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).unwrap();
        Json::parse(std::str::from_utf8(&payload).unwrap()).unwrap()
    }

    fn recv_event(&mut self, event: &str) -> Json {
        for _ in 0..50 {
            let frame = self.recv();
            if frame.pointer("/event").and_then(Json::as_str) == Some(event) {
                return frame;
            }
        }
        panic!("never saw event {event:?}");
    }

    fn join(&mut self, topic: &str, nick: &str) -> Json {
        self.send(&format!(
            r#"{{"topic":"{topic}","event":"stg:join","ref":"1","payload":{{"nick":"{nick}"}}}}"#
        ));
        self.recv_event("stg:reply")
    }
}

fn str_at<'j>(json: &'j Json, path: &str) -> Option<&'j str> {
    json.pointer(path).and_then(Json::as_str)
}

/// A room that tracks presence under the nick, meta = {nick}.
fn tracked_room() -> Channel {
    Channel::new("room:*").doc("A room with presence.").on_join(
        |socket: &Socket, payload: &Json| {
            let nick = payload
                .pointer("/nick")
                .and_then(Json::as_str)
                .ok_or_else(|| Json::str("nick required"))?
                .to_string();
            Presence::track(
                socket,
                &nick,
                Json::obj(vec![("nick", Json::str(nick.clone()))]),
            );
            Ok(Json::Null)
        },
    )
}

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

#[test]
fn track_pushes_state_and_diffs_the_room() {
    boot("127.0.0.1:18490", || {
        let hub = Channels::new().channel(tracked_room()).build();
        App::new("presence-e2e").channels("/channels", "Chat.", hub)
    });
    let mut ada = WsClient::connect("127.0.0.1:18490", "/channels");
    ada.join("room:1", "ada");
    // The tracked member gets the full state, itself included.
    let state = ada.recv_event("presence_state");
    assert_eq!(str_at(&state, "/payload/ada/metas/0/nick"), Some("ada"));

    // A second member joins: ada sees the diff.
    let mut bob = WsClient::connect("127.0.0.1:18490", "/channels");
    bob.join("room:1", "bob");
    let diff = ada.recv_event("presence_diff");
    assert_eq!(
        str_at(&diff, "/payload/joins/bob/metas/0/nick"),
        Some("bob")
    );
    // Bob's own state view has both.
    let state = bob.recv_event("presence_state");
    assert!(state.pointer("/payload/ada").is_some());
    assert!(state.pointer("/payload/bob").is_some());

    // Disconnect: the room hears the leave.
    drop(bob);
    let diff = ada.recv_event("presence_diff");
    assert_eq!(
        str_at(&diff, "/payload/leaves/bob/metas/0/nick"),
        Some("bob")
    );
}

#[test]
fn presence_syncs_across_two_pods_sharing_a_broker() {
    let bus = PubSub::new();
    let (bus_a, bus_b) = (bus.clone(), bus);
    boot("127.0.0.1:18491", move || {
        let hub = Channels::new()
            .channel(tracked_room())
            .broker(bus_a)
            .build();
        App::new("pod-a").channels("/channels", "Pod A.", hub)
    });
    boot("127.0.0.1:18492", move || {
        let hub = Channels::new()
            .channel(tracked_room())
            .broker(bus_b)
            .build();
        App::new("pod-b").channels("/channels", "Pod B.", hub)
    });

    let mut ada = WsClient::connect("127.0.0.1:18491", "/channels");
    ada.join("room:x", "ada");
    ada.recv_event("presence_state");

    // Bob joins on the other pod: ada sees the cross-pod diff, and bob's
    // state sync (the "req" round-trip) includes ada from pod A.
    let mut bob = WsClient::connect("127.0.0.1:18492", "/channels");
    bob.join("room:x", "bob");
    let diff = ada.recv_event("presence_diff");
    assert_eq!(
        str_at(&diff, "/payload/joins/bob/metas/0/nick"),
        Some("bob")
    );
    // Ada's presence reaches bob's pod via the state-sync reply; it may
    // arrive as a presence_diff after his initial state push.
    let mut saw_ada = false;
    for _ in 0..10 {
        let frame = bob.recv();
        match str_at(&frame, "/event") {
            Some("presence_state") if frame.pointer("/payload/ada").is_some() => {
                saw_ada = true;
                break;
            }
            Some("presence_diff") if frame.pointer("/payload/joins/ada").is_some() => {
                saw_ada = true;
                break;
            }
            _ => continue,
        }
    }
    assert!(saw_ada, "bob never learned about ada across pods");

    // Ada leaves (disconnect): bob's pod fans the leave diff. Earlier
    // sync-phase diffs may still be queued; scan until the leave shows up
    // (bounded by the socket read timeout per frame).
    drop(ada);
    let mut saw_leave = false;
    for _ in 0..10 {
        let diff = bob.recv_event("presence_diff");
        if str_at(&diff, "/payload/leaves/ada/metas/0/nick") == Some("ada") {
            saw_leave = true;
            break;
        }
    }
    assert!(saw_leave, "bob never saw ada leave");
}

#[test]
fn a_silently_dead_pod_is_expired_by_heartbeat() {
    // Fast heartbeat so expiry (2.5×) lands within the test budget.
    let bus = PubSub::new();
    let bus_app = bus.clone();
    boot("127.0.0.1:18493", move || {
        let hub = Channels::new()
            .channel(tracked_room())
            .broker(bus_app)
            .presence_heartbeat(Duration::from_millis(200))
            .build();
        App::new("expiry").channels("/channels", "Chat.", hub)
    });
    let mut ada = WsClient::connect("127.0.0.1:18493", "/channels");
    ada.join("room:e", "ada");
    ada.recv_event("presence_state");

    // A "pod" that will die without saying goodbye: inject its state claim
    // directly on the broker (exactly what a real pod's heartbeat sends),
    // then go silent.
    bus.publish(
        "stg:pres:room:e",
        r#"{"o":"ghost-pod","a":"state","e":{"ghost":[{"nick":"ghost"}]}}"#,
    );
    let diff = ada.recv_event("presence_diff");
    assert_eq!(
        str_at(&diff, "/payload/joins/ghost/metas/0/nick"),
        Some("ghost")
    );

    // No further heartbeats from ghost-pod: within ~2.5×200 ms + one tick it
    // must be expired and reported as leaves.
    let diff = ada.recv_event("presence_diff"); // blocks ≤ 5 s read timeout
    assert_eq!(
        str_at(&diff, "/payload/leaves/ghost/metas/0/nick"),
        Some("ghost")
    );
}

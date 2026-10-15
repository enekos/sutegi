#![cfg(all(feature = "contract", feature = "channels"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use sutegi::prelude::*;

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

    fn join(&mut self, topic: &str, payload: Json, reference: &str) -> Json {
        self.send(&format!(
            r#"{{"topic":"{topic}","event":"stg:join","ref":"{reference}","payload":{payload}}}"#
        ));
        self.recv_event("stg:reply")
    }
}

#[derive(Contract)]
struct JoinTable {
    #[contract(min_len = 1)]
    viewer: String,
}

#[derive(Contract)]
struct Snapshot {
    rows: u32,
}

#[derive(Contract)]
struct EditCell {
    row: u32,
    value: String,
}

#[derive(Contract)]
struct Ack {
    version: u64,
}

#[derive(Contract, Debug, PartialEq)]
#[contract(tag = "kind")]
enum TableChange {
    Edited { row: u32, value: String },
    Cleared,
}

fn table_channel() -> Channel {
    Channel::new("table:*")
        .doc("Live edits on one table.")
        .typed_join(
            "Join with who is watching.",
            |socket: &Socket, j: JoinTable| -> Result<Snapshot, String> {
                if j.viewer == "mallory" {
                    return Err("not on this table".to_string());
                }
                socket.assign("viewer", Json::str(j.viewer));
                Ok(Snapshot { rows: 3 })
            },
        )
        .typed_on(
            "edit",
            "Edit one cell.",
            |socket: &Socket, e: EditCell| -> Result<Ack, String> {
                if e.row >= 3 {
                    return Err(format!("row {} does not exist", e.row));
                }
                socket.broadcast_typed(
                    "change",
                    &TableChange::Edited {
                        row: e.row,
                        value: e.value,
                    },
                );
                Ok(Ack { version: 2 })
            },
        )
        .typed_emits::<TableChange>("change", "Someone changed the table.")
}

fn boot(addr: &'static str) {
    thread::spawn(move || {
        let hub = Channels::new().channel(table_channel()).build();
        let _ = App::new("typed-channels")
            .channels("/live", "Live socket.", hub)
            .run(addr);
    });
    for _ in 0..300 {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("server {addr} did not become ready");
}

fn at<'j>(json: &'j Json, path: &str) -> Option<&'j Json> {
    json.pointer(path)
}

#[test]
fn typed_join_push_reply_and_emit() {
    boot("127.0.0.1:18560");
    let mut c = WsClient::connect("127.0.0.1:18560", "/live");

    let reply = c.join("table:t1", Json::obj(vec![("viewer", Json::str(""))]), "1");
    assert_eq!(at(&reply, "/payload/status"), Some(&Json::str("error")));
    assert_eq!(
        at(&reply, "/payload/response/error"),
        Some(&Json::str("validation failed"))
    );
    assert!(at(&reply, "/payload/response/errors/viewer").is_some());

    let reply = c.join(
        "table:t1",
        Json::obj(vec![("viewer", Json::str("mallory"))]),
        "2",
    );
    assert_eq!(
        at(&reply, "/payload/response/error"),
        Some(&Json::str("not on this table"))
    );

    let reply = c.join(
        "table:t1",
        Json::obj(vec![("viewer", Json::str("ada"))]),
        "3",
    );
    assert_eq!(at(&reply, "/payload/status"), Some(&Json::str("ok")));
    assert_eq!(at(&reply, "/payload/response/rows"), Some(&Json::Num(3.0)));

    c.send(r#"{"topic":"table:t1","event":"edit","ref":"4","join_ref":"3","payload":{"row":9,"value":"x"}}"#);
    let reply = c.recv_event("stg:reply");
    assert_eq!(
        at(&reply, "/payload/response/error"),
        Some(&Json::str("row 9 does not exist"))
    );

    c.send(
        r#"{"topic":"table:t1","event":"edit","ref":"5","join_ref":"3","payload":{"row":"one"}}"#,
    );
    let reply = c.recv_event("stg:reply");
    assert!(
        at(&reply, "/payload/response/errors/row").is_some(),
        "{reply}"
    );
    assert!(
        at(&reply, "/payload/response/errors/value").is_some(),
        "{reply}"
    );

    c.send(r#"{"topic":"table:t1","event":"edit","ref":"6","join_ref":"3","payload":{"row":1,"value":"hi"}}"#);
    let change = c.recv_event("change");
    assert_eq!(at(&change, "/payload/kind"), Some(&Json::str("edited")));
    assert_eq!(at(&change, "/payload/value"), Some(&Json::str("hi")));
    let reply = c.recv_event("stg:reply");
    assert_eq!(
        at(&reply, "/payload/response/version"),
        Some(&Json::Num(2.0))
    );
}

#[test]
fn typed_channels_reach_the_manifests_and_the_client() {
    let hub = Channels::new()
        .channel(table_channel())
        .channel(Channel::new("lobby").doc("Untyped."))
        .build();
    let app = App::new("typed-channels").channels("/live", "Live socket.", hub);
    let reg = app.contract();
    assert_eq!(reg.channels().len(), 1);
    assert!(reg.def("TableChange").is_some());
    let ts = app.contract_ts();
    for needle in [
        "export const channels = {",
        "table: (socket: ChannelSocket, topic: string, input: JoinTable): TypedChannel<Snapshot, { change: TableChange; }, { edit: [EditCell, Ack]; }> =>",
        "typedChannel(socket.channel(\"table:\" + topic, input)),",
        "  | { kind: \"edited\"; row: number; value: string; }",
    ] {
        assert!(ts.contains(needle), "missing {needle:?} in:\n{ts}");
    }

    let svc = app.service();
    let resp = svc(Request {
        method: Method::Get,
        path: "/__channels".into(),
        query: String::new(),
        version: "HTTP/1.1".into(),
        headers: vec![],
        body: vec![],
        peer: None,
    });
    let sutegi::http::Body::Full(body) = resp.body else {
        panic!()
    };
    let manifest = Json::parse(std::str::from_utf8(&body).unwrap()).unwrap();
    let table = &manifest["channels"][0];
    assert_eq!(
        table["join"]["payload_schema"]["properties"]["viewer"]["minLength"],
        Json::Num(1.0)
    );
    assert!(table["emits"][0]["payload_schema"].get("oneOf").is_some());
}

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};
use sutegi_web::{App, Lifecycle, Method, Response, UploadLimits};

struct Server {
    addr: String,
    lifecycle: Lifecycle,
    thread: Option<thread::JoinHandle<std::io::Result<()>>>,
}
impl Server {
    fn start(app: App) -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap().to_string();
        drop(socket);
        let lifecycle = app.lifecycle();
        let listen = addr.clone();
        let server = thread::spawn(move || app.run(&listen));
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(&addr).is_err() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        Self {
            addr,
            lifecycle,
            thread: Some(server),
        }
    }
    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.lifecycle.stop();
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}
fn app(limits: UploadLimits) -> App {
    App::new("upload-test")
        .max_body(16)
        .upload(
            Method::Post,
            "/recording",
            "Recording upload",
            limits,
            |head| {
                (head.header("authorization") != Some("Bearer test")).then(|| Response::new(403))
            },
            |ctx, body| {
                assert!(
                    ctx.req.body.is_empty(),
                    "streaming route must not buffer the body"
                );
                match std::io::copy(body, &mut std::io::sink()) {
                    Ok(n) => Response::new(201).with_body(n.to_string()),
                    Err(_) => Response::new(500), // transport supplies the appropriate 400/408
                }
            },
        )
        .post("/plain", "Buffered route", |_| Response::new(200))
}
fn reply(stream: &mut TcpStream) -> String {
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn authorization_and_size_checks_run_before_body_reads() {
    let server = Server::start(app(UploadLimits::default()));
    for (head, status) in [
        (
            "POST /recording HTTP/1.1\r\nContent-Length: 209715200\r\n\r\n",
            403,
        ),
        (
            "POST /recording HTTP/1.1\r\nAuthorization: Bearer test\r\nContent-Length: 209715201\r\n\r\n",
            413,
        ),
        (
            "POST /recording HTTP/1.1\r\nAuthorization: Bearer test\r\n\r\n",
            411,
        ),
        (
            "POST /recording HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            501,
        ),
        ("POST /plain HTTP/1.1\r\nContent-Length: 17\r\n\r\n", 413),
    ] {
        let mut stream = server.connect();
        stream.write_all(head.as_bytes()).unwrap();
        assert!(reply(&mut stream).starts_with(&format!("HTTP/1.1 {status}")));
    }
}

#[test]
fn streams_200_mib_without_populating_request_body() {
    let server = Server::start(app(UploadLimits::default()));
    let mut stream = server.connect();
    write!(
        stream,
        "POST /recording HTTP/1.1\r\nAuthorization: Bearer test\r\nContent-Length: {}\r\n\r\n",
        200 * 1024 * 1024
    )
    .unwrap();
    let block = [42u8; 64 * 1024];
    for _ in 0..3200 {
        stream.write_all(&block).unwrap();
    }
    let response = reply(&mut stream);
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    assert!(response.ends_with("209715200"));
}

#[test]
fn interrupted_and_stalled_uploads_cannot_report_success() {
    let server = Server::start(app(UploadLimits {
        timeout: Duration::from_millis(100),
        idle_timeout: Duration::from_millis(30),
        ..Default::default()
    }));
    let mut partial = server.connect();
    partial.write_all(b"POST /recording HTTP/1.1\r\nAuthorization: Bearer test\r\nContent-Length: 10\r\n\r\nshort").unwrap();
    partial.shutdown(Shutdown::Write).unwrap();
    assert!(reply(&mut partial).starts_with("HTTP/1.1 400"));
    let mut stalled = server.connect();
    stalled.write_all(b"POST /recording HTTP/1.1\r\nAuthorization: Bearer test\r\nContent-Length: 10\r\n\r\nx").unwrap();
    assert!(reply(&mut stalled).starts_with("HTTP/1.1 408"));
}

#[test]
fn draining_fails_readiness_and_deadline_interrupts_an_upload() {
    let server = Server::start(app(UploadLimits::default()));
    let mut stalled = server.connect();
    stalled.write_all(b"POST /recording HTTP/1.1\r\nAuthorization: Bearer test\r\nContent-Length: 100\r\n\r\nx").unwrap();
    server.lifecycle.drain(Duration::from_millis(100));
    let mut probe = server.connect();
    probe
        .write_all(b"GET /__ready HTTP/1.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    assert!(reply(&mut probe).starts_with("HTTP/1.1 503"));
    let started = Instant::now();
    let mut bytes = vec![];
    let _ = stalled.read_to_end(&mut bytes);
    assert!(started.elapsed() < Duration::from_secs(2));
}

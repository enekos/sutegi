//! Incremental request bodies. Upload routes opt in before body allocation.
use crate::{Request, Response};
use std::io::{self, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct UploadLimits {
    pub max_bytes: usize,
    pub timeout: Duration,
    pub idle_timeout: Duration,
}
impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 200 * 1024 * 1024,
            timeout: Duration::from_secs(300),
            idle_timeout: Duration::from_secs(15),
        }
    }
}

/// HTTP services can opt selected routes into streaming. Plain closures keep
/// the existing buffered behavior and global body limit.
pub trait Service {
    fn handle(&self, request: Request) -> Response;
    fn upload_limits(&self, _head: &Request) -> Option<UploadLimits> {
        None
    }
    fn handle_upload(&self, _head: Request, _body: &mut RequestBody<'_>) -> Response {
        Response::new(405)
    }
}
impl<F: Fn(Request) -> Response> Service for F {
    fn handle(&self, request: Request) -> Response {
        self(request)
    }
}

/// A length-delimited reader. It never reads bytes belonging to a subsequent
/// request. Errors are sticky: ignoring one cannot turn a partial upload into
/// a successful HTTP response. Use a fixed buffer or `io::copy`, optionally
/// feeding each chunk into `sutegi_crypto::Sha256`.
pub struct RequestBody<'a> {
    reader: &'a mut dyn Read,
    socket: Option<TcpStream>,
    remaining: usize,
    deadline: Instant,
    idle_timeout: Duration,
    failure: Option<u16>,
}
impl<'a> RequestBody<'a> {
    pub(crate) fn new(
        reader: &'a mut dyn Read,
        socket: TcpStream,
        length: usize,
        limits: UploadLimits,
    ) -> Self {
        Self {
            reader,
            socket: Some(socket),
            remaining: length,
            deadline: crate::lifecycle::deadline_after(limits.timeout),
            idle_timeout: limits.idle_timeout,
            failure: None,
        }
    }
    pub fn remaining(&self) -> usize {
        self.remaining
    }
    pub fn failure(&self) -> Option<u16> {
        self.failure
    }
    fn fail(&mut self, status: u16, error: io::Error) -> io::Result<usize> {
        self.failure = Some(status);
        Err(error)
    }
}
impl Read for RequestBody<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(status) = self.failure {
            return Err(io::Error::other(format!(
                "upload already failed ({status})"
            )));
        }
        if buf.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || self.idle_timeout.is_zero() {
            return self.fail(
                408,
                io::Error::new(io::ErrorKind::TimedOut, "upload deadline exceeded"),
            );
        }
        if let Some(socket) = &self.socket {
            socket.set_read_timeout(Some(left.min(self.idle_timeout)))?;
        }
        let size = buf.len().min(self.remaining).min(64 * 1024);
        match self.reader.read(&mut buf[..size]) {
            Ok(0) => self.fail(
                400,
                io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete upload"),
            ),
            Ok(n) => {
                self.remaining -= n;
                if Instant::now() > self.deadline {
                    self.fail(
                        408,
                        io::Error::new(io::ErrorKind::TimedOut, "upload deadline exceeded"),
                    )
                } else {
                    Ok(n)
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(e),
            Err(e) => {
                let status = if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) {
                    408
                } else {
                    400
                };
                self.fail(status, e)
            }
        }
    }
}

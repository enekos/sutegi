//! Readiness-based I/O: a thin, uniform wrapper over `kqueue` (macOS/BSD)
//! and `epoll` (Linux).
//!
//! This is the piece `std` cannot provide and the reason a thread-per-socket
//! server tops out around thousands of connections: with a poller, one
//! thread sleeps on *all* of its sockets at once and wakes only for the few
//! with actual traffic. Level-triggered on both platforms, so a partial read
//! simply fires again — no edge-tracking subtlety.

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

/// A readiness event for the connection registered under `token`.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub token: usize,
    pub readable: bool,
    pub writable: bool,
    /// Peer hung up (best-effort fast path; EOF on read is authoritative).
    pub hup: bool,
}

pub struct Poller {
    fd: RawFd,
}

impl Drop for Poller {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

// ---------------------------------------------------------------------------
// kqueue (macOS, *BSD)
// ---------------------------------------------------------------------------
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
mod imp {
    use super::*;

    impl Poller {
        pub fn new() -> io::Result<Poller> {
            let fd = cvt(unsafe { libc::kqueue() })?;
            Ok(Poller { fd })
        }

        fn change(&self, fd: RawFd, filter: i16, flags: u16, token: usize) -> io::Result<()> {
            let ev = libc::kevent {
                ident: fd as usize,
                filter,
                flags,
                fflags: 0,
                data: 0,
                udata: token as *mut libc::c_void,
            };
            cvt(unsafe {
                libc::kevent(self.fd, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null())
            })?;
            Ok(())
        }

        /// Register `fd`: read interest always, write interest if `write`.
        pub fn add(&self, fd: RawFd, token: usize, write: bool) -> io::Result<()> {
            self.change(fd, libc::EVFILT_READ, libc::EV_ADD, token)?;
            if write {
                self.change(fd, libc::EVFILT_WRITE, libc::EV_ADD, token)?;
            }
            Ok(())
        }

        /// Toggle write interest (read interest is permanent).
        pub fn set_write(&self, fd: RawFd, token: usize, want: bool) -> io::Result<()> {
            let flags = if want { libc::EV_ADD } else { libc::EV_DELETE };
            match self.change(fd, libc::EVFILT_WRITE, flags, token) {
                // Deleting an already-absent filter is a no-op, not an error.
                Err(e) if !want && e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
                r => r,
            }
        }

        /// Deregistration happens implicitly when the fd is closed; kqueue
        /// drops its filters with the last close of the file.
        pub fn del(&self, _fd: RawFd) -> io::Result<()> {
            Ok(())
        }

        pub fn wait(&self, events: &mut Vec<Event>, timeout: Duration) -> io::Result<()> {
            const CAP: usize = 1024;
            let mut kevs: [libc::kevent; CAP] = unsafe { std::mem::zeroed() };
            let ts = libc::timespec {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_nsec: timeout.subsec_nanos() as libc::c_long,
            };
            let n = loop {
                let n = unsafe {
                    libc::kevent(
                        self.fd,
                        std::ptr::null(),
                        0,
                        kevs.as_mut_ptr(),
                        CAP as i32,
                        &ts,
                    )
                };
                if n >= 0 {
                    break n as usize;
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            };
            events.clear();
            for kev in kevs.iter().take(n) {
                events.push(Event {
                    token: kev.udata as usize,
                    readable: kev.filter == libc::EVFILT_READ,
                    writable: kev.filter == libc::EVFILT_WRITE,
                    hup: kev.flags & libc::EV_EOF != 0,
                });
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// epoll (Linux)
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    impl Poller {
        pub fn new() -> io::Result<Poller> {
            let fd = cvt(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) })?;
            Ok(Poller { fd })
        }

        fn ctl(&self, op: libc::c_int, fd: RawFd, token: usize, write: bool) -> io::Result<()> {
            let mut ev = libc::epoll_event {
                events: (libc::EPOLLIN | libc::EPOLLRDHUP) as u32
                    | if write { libc::EPOLLOUT as u32 } else { 0 },
                u64: token as u64,
            };
            cvt(unsafe { libc::epoll_ctl(self.fd, op, fd, &mut ev) })?;
            Ok(())
        }

        /// Register `fd`: read interest always, write interest if `write`.
        pub fn add(&self, fd: RawFd, token: usize, write: bool) -> io::Result<()> {
            self.ctl(libc::EPOLL_CTL_ADD, fd, token, write)
        }

        /// Toggle write interest (read interest is permanent).
        pub fn set_write(&self, fd: RawFd, token: usize, want: bool) -> io::Result<()> {
            self.ctl(libc::EPOLL_CTL_MOD, fd, token, want)
        }

        /// Deregistration happens implicitly when the fd is closed (epoll
        /// removes an fd with the last close of the file description).
        pub fn del(&self, _fd: RawFd) -> io::Result<()> {
            Ok(())
        }

        pub fn wait(&self, events: &mut Vec<Event>, timeout: Duration) -> io::Result<()> {
            const CAP: usize = 1024;
            let mut eevs: [libc::epoll_event; CAP] = unsafe { std::mem::zeroed() };
            let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
            let n = loop {
                let n = unsafe { libc::epoll_wait(self.fd, eevs.as_mut_ptr(), CAP as i32, ms) };
                if n >= 0 {
                    break n as usize;
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            };
            events.clear();
            for eev in eevs.iter().take(n) {
                let bits = eev.events;
                events.push(Event {
                    token: eev.u64 as usize,
                    readable: bits & (libc::EPOLLIN | libc::EPOLLRDHUP | libc::EPOLLERR) as u32
                        != 0,
                    writable: bits & (libc::EPOLLOUT | libc::EPOLLERR) as u32 != 0,
                    hup: bits & (libc::EPOLLRDHUP | libc::EPOLLHUP) as u32 != 0,
                });
            }
            Ok(())
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
compile_error!("sutegi-ws needs kqueue or epoll; this platform has neither");

/// A self-pipe used to wake a poller from other threads: the read end is
/// registered under a reserved token, writers put one byte in.
pub struct WakePipe {
    pub read_fd: RawFd,
    pub write_fd: RawFd,
}

impl WakePipe {
    pub fn new() -> io::Result<WakePipe> {
        let mut fds = [0 as RawFd; 2];
        cvt(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
        for fd in fds {
            unsafe {
                cvt(libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK))?;
                cvt(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC))?;
            }
        }
        Ok(WakePipe {
            read_fd: fds[0],
            write_fd: fds[1],
        })
    }

    /// Wake the owning poller. Callable from any thread; if the pipe is full
    /// a wake-up is already pending, which is all we need.
    pub fn wake(&self) {
        unsafe { libc::write(self.write_fd, [1u8].as_ptr() as *const libc::c_void, 1) };
    }

    /// Drain pending wake bytes (called by the poller thread).
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            let n = unsafe { libc::read(self.read_fd, buf.as_mut_ptr() as *mut libc::c_void, 64) };
            if n <= 0 {
                break;
            }
        }
    }
}

impl Drop for WakePipe {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

/// Best-effort raise of the open-file limit toward the hard cap — a million
/// sockets is a million fds, and default soft limits (256 on macOS, 1024 on
/// many Linuxes) are the first wall a big fleet hits. Returns the resulting
/// soft limit.
pub fn raise_nofile_limit() -> u64 {
    unsafe {
        let mut rl: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) != 0 {
            return 0;
        }
        // macOS refuses rlim_cur = RLIM_INFINITY; its true per-process
        // ceiling is kern.maxfilesperproc, so clamp to it there.
        #[cfg(target_os = "macos")]
        let target = {
            let mut maxfiles: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>();
            let name = std::ffi::CString::new("kern.maxfilesperproc").unwrap();
            if libc::sysctlbyname(
                name.as_ptr(),
                &mut maxfiles as *mut _ as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            ) == 0
                && maxfiles > 0
            {
                rl.rlim_max.min(maxfiles as libc::rlim_t)
            } else {
                rl.rlim_max
            }
        };
        #[cfg(not(target_os = "macos"))]
        let target = rl.rlim_max;
        let attempt = libc::rlimit {
            rlim_cur: target,
            rlim_max: rl.rlim_max,
        };
        if libc::setrlimit(libc::RLIMIT_NOFILE, &attempt) == 0 {
            return target as u64;
        }
        rl.rlim_cur as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;

    #[test]
    fn poller_sees_readable_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();

        let poller = Poller::new().unwrap();
        poller.add(server.as_raw_fd(), 7, false).unwrap();

        let mut events = Vec::new();
        // Nothing to read yet.
        poller.wait(&mut events, Duration::from_millis(10)).unwrap();
        assert!(events.iter().all(|e| !e.readable || e.token != 7));

        client.write_all(b"ping").unwrap();
        poller.wait(&mut events, Duration::from_secs(2)).unwrap();
        assert!(
            events.iter().any(|e| e.token == 7 && e.readable),
            "expected readable event, got {events:?}"
        );
    }

    #[test]
    fn wake_pipe_wakes_poller() {
        let poller = Poller::new().unwrap();
        let pipe = WakePipe::new().unwrap();
        poller.add(pipe.read_fd, usize::MAX, false).unwrap();

        pipe.wake();
        let mut events = Vec::new();
        poller.wait(&mut events, Duration::from_secs(2)).unwrap();
        assert!(events.iter().any(|e| e.token == usize::MAX && e.readable));
        pipe.drain();

        // Drained: no further event.
        poller.wait(&mut events, Duration::from_millis(10)).unwrap();
        assert!(events.iter().all(|e| e.token != usize::MAX));
    }

    #[test]
    fn write_interest_toggles() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();

        let poller = Poller::new().unwrap();
        poller.add(server.as_raw_fd(), 3, false).unwrap();
        let mut events = Vec::new();

        // An idle healthy socket is instantly writable once interest is on.
        poller.set_write(server.as_raw_fd(), 3, true).unwrap();
        poller.wait(&mut events, Duration::from_secs(2)).unwrap();
        assert!(events.iter().any(|e| e.token == 3 && e.writable));

        poller.set_write(server.as_raw_fd(), 3, false).unwrap();
        poller.wait(&mut events, Duration::from_millis(10)).unwrap();
        assert!(events.iter().all(|e| !(e.token == 3 && e.writable)));
    }

    #[test]
    fn raise_nofile_reports_something() {
        assert!(raise_nofile_limit() >= 256);
    }
}

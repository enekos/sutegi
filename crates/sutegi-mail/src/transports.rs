//! The non-network transports: `Log` (dev default), `Memory` (tests), and
//! `Sendmail` (pipe to the local MTA binary — pairs with a VPS Postfix).

use crate::message::Email;
use crate::Transport;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Writes each message to stderr (or a file) instead of sending — Laravel's
/// `log` mailer. The dev default: everything works end-to-end, nothing
/// escapes the machine.
#[derive(Clone, Debug, Default)]
pub struct Log {
    path: Option<String>,
}

impl Log {
    /// Log to stderr.
    pub fn new() -> Log {
        Log { path: None }
    }

    /// Append to a file instead (e.g. `storage/mail.log`).
    pub fn to_file(path: &str) -> Log {
        Log {
            path: Some(path.to_string()),
        }
    }
}

impl Transport for Log {
    fn send(&self, email: &Email, rendered: &str, message_id: &str) -> Result<String, String> {
        let banner = format!(
            "--- sutegi mail ({} recipient(s), id {message_id}) ---\n{rendered}\n",
            email.recipients().len()
        );
        match &self.path {
            Some(path) => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(banner.as_bytes()))
                .map_err(|e| format!("mail log {path}: {e}"))?,
            None => eprint!("{banner}"),
        }
        Ok(message_id.to_string())
    }
}

/// Collects sent messages in memory for assertions — Laravel's `array` mailer.
/// Clone freely; all clones share the same inbox.
#[derive(Clone, Default)]
pub struct Memory {
    inbox: Arc<Mutex<Vec<(Email, String)>>>,
}

impl Memory {
    pub fn new() -> Memory {
        Memory::default()
    }

    /// Every message sent so far, as `(email, rendered)` pairs.
    pub fn sent(&self) -> Vec<(Email, String)> {
        self.inbox.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn count(&self) -> usize {
        self.inbox.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inbox.lock() {
            g.clear();
        }
    }
}

impl Transport for Memory {
    fn send(&self, email: &Email, rendered: &str, message_id: &str) -> Result<String, String> {
        self.inbox
            .lock()
            .map_err(|_| "mail memory inbox poisoned".to_string())?
            .push((email.clone(), rendered.to_string()));
        Ok(message_id.to_string())
    }
}

/// Pipes the rendered message to a local `sendmail`-compatible binary
/// (`sendmail -t -i` reads recipients from the headers). The classic
/// bare-metal shape: your VPS's Postfix does the actual delivery and TLS.
#[derive(Clone, Debug)]
pub struct Sendmail {
    program: String,
}

impl Default for Sendmail {
    fn default() -> Self {
        Sendmail::new()
    }
}

impl Sendmail {
    pub fn new() -> Sendmail {
        Sendmail {
            program: "/usr/sbin/sendmail".to_string(),
        }
    }

    pub fn at(path: &str) -> Sendmail {
        Sendmail {
            program: path.to_string(),
        }
    }
}

impl Transport for Sendmail {
    fn send(&self, email: &Email, rendered: &str, message_id: &str) -> Result<String, String> {
        use std::process::{Command, Stdio};
        // `-t` reads To/Cc from headers; Bcc recipients are passed explicitly
        // since they are never rendered.
        let mut cmd = Command::new(&self.program);
        cmd.arg("-t").arg("-i");
        for bcc in &email.bcc {
            cmd.arg(&bcc.email);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", self.program))?;
        child
            .stdin
            .take()
            .ok_or("sendmail stdin unavailable")?
            .write_all(rendered.as_bytes())
            .map_err(|e| format!("sendmail write: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("sendmail wait: {e}"))?;
        if out.status.success() {
            Ok(message_id.to_string())
        } else {
            Err(format!(
                "sendmail exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email() -> Email {
        Email::new()
            .from("a@b.co")
            .to("t@b.co")
            .subject("s")
            .text("body")
    }

    #[test]
    fn memory_collects_and_clears() {
        let mem = Memory::new();
        let shared = mem.clone();
        mem.send(&email(), "rendered-1", "id-1").unwrap();
        shared.send(&email(), "rendered-2", "id-2").unwrap();
        assert_eq!(mem.count(), 2);
        assert_eq!(mem.sent()[1].1, "rendered-2");
        mem.clear();
        assert_eq!(shared.count(), 0);
    }

    #[test]
    fn log_to_file_appends() {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sutegi-mail-log-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let log = Log::to_file(path.to_str().unwrap());
        log.send(&email(), "RENDERED", "id-9").unwrap();
        log.send(&email(), "AGAIN", "id-10").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("RENDERED") && contents.contains("AGAIN"));
        assert!(contents.contains("id-9"));
        let _ = std::fs::remove_file(&path);
    }
}

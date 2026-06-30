//! PostgreSQL frontend/backend protocol v3 over a blocking socket.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;

use sutegi_json::Json;

use crate::crypto;
use crate::{Config, PgValue};

/// Protocol version 3.0 (`0x00030000`).
const PROTOCOL_VERSION: i32 = 196608;

/// One live connection to a backend.
pub struct Client {
    write: TcpStream,
    read: std::io::BufReader<TcpStream>,
    /// Set once the connection is known to be broken so the pool drops it.
    broken: bool,
}

impl Client {
    /// Open a connection and run the startup + authentication handshake.
    pub fn connect(cfg: &Config) -> Result<Client, String> {
        let stream = match cfg.timeout {
            Some(t) => {
                let addr = format!("{}:{}", cfg.host, cfg.port);
                let addrs: Vec<_> = std::net::ToSocketAddrs::to_socket_addrs(&addr)
                    .map_err(|e| format!("resolve {addr}: {e}"))?
                    .collect();
                let first = addrs
                    .first()
                    .ok_or_else(|| format!("no addresses for {addr}"))?;
                TcpStream::connect_timeout(first, t).map_err(|e| format!("connect: {e}"))?
            }
            None => TcpStream::connect((cfg.host.as_str(), cfg.port))
                .map_err(|e| format!("connect: {e}"))?,
        };
        stream.set_nodelay(true).ok();
        if let Some(t) = cfg.timeout {
            stream.set_read_timeout(Some(t)).ok();
            stream.set_write_timeout(Some(t)).ok();
        }
        let read = std::io::BufReader::new(
            stream
                .try_clone()
                .map_err(|e| format!("clone socket: {e}"))?,
        );
        let mut client = Client {
            write: stream,
            read,
            broken: false,
        };
        client.startup(cfg)?;
        Ok(client)
    }

    /// Send the startup message and drive authentication to `ReadyForQuery`.
    fn startup(&mut self, cfg: &Config) -> Result<(), String> {
        // Startup packet: length + version + key/value pairs + final NUL.
        let mut body = Vec::new();
        body.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        for (k, v) in [
            ("user", cfg.user.as_str()),
            ("database", cfg.dbname.as_str()),
        ] {
            body.extend_from_slice(k.as_bytes());
            body.push(0);
            body.extend_from_slice(v.as_bytes());
            body.push(0);
        }
        body.push(0);
        let mut packet = ((body.len() + 4) as i32).to_be_bytes().to_vec();
        packet.extend_from_slice(&body);
        self.send_raw(&packet)?;

        self.authenticate(cfg)?;

        // Drain ParameterStatus / BackendKeyData until ReadyForQuery.
        loop {
            let (tag, body) = self.recv()?;
            match tag {
                b'Z' => return Ok(()),
                b'E' => return Err(parse_error(&body)),
                _ => continue, // S (ParameterStatus), K (BackendKeyData), N (Notice)
            }
        }
    }

    fn authenticate(&mut self, cfg: &Config) -> Result<(), String> {
        let mut scram: Option<Scram> = None;
        loop {
            let (tag, body) = self.recv()?;
            match tag {
                b'E' => return Err(parse_error(&body)),
                b'R' => {
                    let code = be_i32(&body, 0);
                    match code {
                        0 => return Ok(()),             // AuthenticationOk
                        3 => self.auth_cleartext(cfg)?, // AuthenticationCleartextPassword
                        5 => {
                            // AuthenticationMD5Password: 4-byte salt follows.
                            let salt = &body[4..8];
                            self.auth_md5(cfg, salt)?;
                        }
                        10 => {
                            // AuthenticationSASL: NUL-separated mechanism list.
                            let mechs = parse_cstr_list(&body[4..]);
                            if !mechs.iter().any(|m| m == "SCRAM-SHA-256") {
                                return Err(format!(
                                    "server offered no supported SASL mechanism (got {mechs:?})"
                                ));
                            }
                            scram = Some(self.scram_first(cfg)?);
                        }
                        11 => {
                            // AuthenticationSASLContinue: server-first message.
                            let s = scram.as_mut().ok_or("SASL continue before SASL start")?;
                            let server_first = std::str::from_utf8(&body[4..])
                                .map_err(|_| "non-utf8 SASL data")?;
                            let client_final = s.handle_server_first(cfg, server_first)?;
                            // SASLResponse: just the message bytes.
                            self.send_msg(b'p', client_final.as_bytes())?;
                        }
                        12 => {
                            // AuthenticationSASLFinal: verify the server signature.
                            let s = scram.as_ref().ok_or("SASL final before SASL start")?;
                            let server_final = std::str::from_utf8(&body[4..])
                                .map_err(|_| "non-utf8 SASL data")?;
                            s.verify_server_final(server_final)?;
                        }
                        other => return Err(format!("unsupported auth request code {other}")),
                    }
                }
                other => {
                    return Err(format!(
                        "unexpected message {:?} during authentication",
                        other as char
                    ))
                }
            }
        }
    }

    fn auth_cleartext(&mut self, cfg: &Config) -> Result<(), String> {
        let pw = cfg.password.as_deref().unwrap_or("");
        let mut msg = pw.as_bytes().to_vec();
        msg.push(0);
        self.send_msg(b'p', &msg)
    }

    fn auth_md5(&mut self, cfg: &Config, salt: &[u8]) -> Result<(), String> {
        let pw = cfg.password.as_deref().unwrap_or("");
        // "md5" + md5( md5(password + user) + salt )
        let inner = crypto::hex(&crypto::md5(
            [pw.as_bytes(), cfg.user.as_bytes()].concat().as_slice(),
        ));
        let outer = crypto::hex(&crypto::md5([inner.as_bytes(), salt].concat().as_slice()));
        let mut msg = format!("md5{outer}").into_bytes();
        msg.push(0);
        self.send_msg(b'p', &msg)
    }

    fn scram_first(&mut self, _cfg: &Config) -> Result<Scram, String> {
        let scram = Scram::new()?;
        let client_first = format!("n,,{}", scram.client_first_bare);
        // SASLInitialResponse: mechanism name, then i32 length + initial response.
        let mut msg = Vec::new();
        msg.extend_from_slice(b"SCRAM-SHA-256");
        msg.push(0);
        msg.extend_from_slice(&(client_first.len() as i32).to_be_bytes());
        msg.extend_from_slice(client_first.as_bytes());
        self.send_msg(b'p', &msg)?;
        Ok(scram)
    }

    /// Run a parameterized statement, returning rows as JSON objects.
    pub fn query(&mut self, sql: &str, params: &[PgValue]) -> Result<Vec<Json>, String> {
        let (rows, _) = self.extended(sql, params)?;
        Ok(rows)
    }

    /// Run a parameterized statement, returning the number of rows affected.
    pub fn execute(&mut self, sql: &str, params: &[PgValue]) -> Result<u64, String> {
        let (_, affected) = self.extended(sql, params)?;
        Ok(affected)
    }

    /// Run one or more statements via the simple query protocol (no params).
    /// Used for `BEGIN`/`COMMIT`/`ROLLBACK` and multi-statement migrations.
    pub fn batch(&mut self, sql: &str) -> Result<(), String> {
        let mut msg = sql.as_bytes().to_vec();
        msg.push(0);
        self.send_msg(b'Q', &msg)?;
        let mut err = None;
        loop {
            let (tag, body) = self.recv()?;
            match tag {
                b'Z' => break,
                b'E' => err = Some(parse_error(&body)),
                _ => continue,
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Cheap liveness probe used by the pool before handing out a connection.
    pub fn ping(&mut self) -> bool {
        !self.broken && self.batch("SELECT 1").is_ok()
    }

    /// The extended-query exchange: Parse → Bind → Describe → Execute → Sync,
    /// then collect rows and the affected-row count.
    fn extended(&mut self, sql: &str, params: &[PgValue]) -> Result<(Vec<Json>, u64), String> {
        let mut out = Vec::new();

        // Parse: unnamed statement, no declared parameter types (server infers).
        out.extend(frame(b'P', |b| {
            b.push(0); // statement name ""
            b.extend_from_slice(sql.as_bytes());
            b.push(0);
            b.extend_from_slice(&0i16.to_be_bytes()); // 0 parameter type OIDs
        }));

        // Bind: unnamed portal/statement, all params text format, results text.
        out.extend(frame(b'B', |b| {
            b.push(0); // portal ""
            b.push(0); // statement ""
            b.extend_from_slice(&1i16.to_be_bytes()); // one format code...
            b.extend_from_slice(&0i16.to_be_bytes()); // ...= text, applies to all
            b.extend_from_slice(&(params.len() as i16).to_be_bytes());
            for p in params {
                match encode_param(p) {
                    Some(bytes) => {
                        b.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                        b.extend_from_slice(&bytes);
                    }
                    None => b.extend_from_slice(&(-1i32).to_be_bytes()), // NULL
                }
            }
            b.extend_from_slice(&1i16.to_be_bytes()); // one result format code...
            b.extend_from_slice(&0i16.to_be_bytes()); // ...= text
        }));

        // Describe the portal (yields RowDescription), execute all rows, sync.
        out.extend(frame(b'D', |b| {
            b.push(b'P');
            b.push(0);
        }));
        out.extend(frame(b'E', |b| {
            b.push(0); // portal ""
            b.extend_from_slice(&0i32.to_be_bytes()); // unlimited rows
        }));
        out.extend(frame(b'S', |_| {}));
        self.send_raw(&out)?;

        // Collect the response stream up to ReadyForQuery.
        let mut fields: Vec<(String, i32)> = Vec::new();
        let mut rows = Vec::new();
        let mut affected = 0u64;
        let mut err = None;
        loop {
            let (tag, body) = self.recv()?;
            match tag {
                b'T' => fields = parse_row_description(&body),
                b'D' => rows.push(parse_data_row(&body, &fields)),
                b'C' => affected = command_tag_count(&body),
                b'E' => err = Some(parse_error(&body)),
                b'Z' => break,
                _ => continue, // 1 ParseComplete, 2 BindComplete, t, n, N, S
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok((rows, affected)),
        }
    }

    // --- raw I/O -----------------------------------------------------------

    fn send_raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.write.write_all(bytes).map_err(|e| {
            self.broken = true;
            format!("write: {e}")
        })?;
        self.write.flush().map_err(|e| {
            self.broken = true;
            format!("flush: {e}")
        })
    }

    /// Write a tagged message: 1 type byte + i32 length (self-inclusive) + body.
    fn send_msg(&mut self, tag: u8, body: &[u8]) -> Result<(), String> {
        let mut packet = Vec::with_capacity(body.len() + 5);
        packet.push(tag);
        packet.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        packet.extend_from_slice(body);
        self.send_raw(&packet)
    }

    /// Read one tagged message: returns `(type, body)`.
    fn recv(&mut self) -> Result<(u8, Vec<u8>), String> {
        let mut header = [0u8; 5];
        self.read.read_exact(&mut header).map_err(|e| {
            self.broken = true;
            format!("read header: {e}")
        })?;
        let len = be_i32(&header[1..], 0) as usize;
        if len < 4 {
            self.broken = true;
            return Err(format!("invalid message length {len}"));
        }
        let mut body = vec![0u8; len - 4];
        self.read.read_exact(&mut body).map_err(|e| {
            self.broken = true;
            format!("read body: {e}")
        })?;
        Ok((header[0], body))
    }
}

/// Build a tagged frame whose length prefix is filled in after the body.
fn frame(tag: u8, build: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut body = Vec::new();
    build(&mut body);
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(tag);
    out.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Encode a parameter in text format. `None` means SQL `NULL`.
fn encode_param(p: &PgValue) -> Option<Vec<u8>> {
    match p {
        PgValue::Null => None,
        PgValue::Int(i) => Some(i.to_string().into_bytes()),
        PgValue::Real(r) => Some(r.to_string().into_bytes()),
        PgValue::Text(s) => Some(s.clone().into_bytes()),
        PgValue::Bool(b) => Some(if *b { b"t".to_vec() } else { b"f".to_vec() }),
    }
}

fn be_i32(buf: &[u8], at: usize) -> i32 {
    i32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn be_i16(buf: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([buf[at], buf[at + 1]])
}

/// Parse a NUL-separated, NUL-terminated list of strings.
fn parse_cstr_list(buf: &[u8]) -> Vec<String> {
    buf.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// RowDescription → `(column name, type OID)` per field.
fn parse_row_description(body: &[u8]) -> Vec<(String, i32)> {
    let count = be_i16(body, 0) as usize;
    let mut pos = 2;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let end = body[pos..].iter().position(|&b| b == 0).unwrap_or(0);
        let name = String::from_utf8_lossy(&body[pos..pos + end]).into_owned();
        pos += end + 1;
        // table_oid(4) col_attr(2) type_oid(4) type_size(2) type_mod(4) format(2)
        let type_oid = be_i32(body, pos + 6);
        pos += 18;
        fields.push((name, type_oid));
    }
    fields
}

/// DataRow → a JSON object keyed by column name, typed by the field OID.
fn parse_data_row(body: &[u8], fields: &[(String, i32)]) -> Json {
    let count = be_i16(body, 0) as usize;
    let mut pos = 2;
    let mut obj = BTreeMap::new();
    for i in 0..count {
        let len = be_i32(body, pos);
        pos += 4;
        let (name, oid) = fields
            .get(i)
            .cloned()
            .unwrap_or_else(|| (format!("col{i}"), 0));
        if len < 0 {
            obj.insert(name, Json::Null);
            continue;
        }
        let raw = &body[pos..pos + len as usize];
        pos += len as usize;
        obj.insert(name, decode_value(oid, raw));
    }
    Json::Obj(obj)
}

/// Decode a text-format column value into JSON, typed by its PostgreSQL OID.
fn decode_value(oid: i32, raw: &[u8]) -> Json {
    let text = String::from_utf8_lossy(raw);
    match oid {
        16 => Json::Bool(text == "t"), // bool
        20 | 21 | 23 | 26 => text
            .parse::<i64>()
            .map(Json::int)
            .unwrap_or_else(|_| Json::str(text.into_owned())), // int8/int2/int4/oid
        700 | 701 | 1700 => text
            .parse::<f64>()
            .map(Json::Num)
            .unwrap_or_else(|_| Json::str(text.into_owned())), // float4/float8/numeric
        _ => Json::str(text.into_owned()),
    }
}

/// CommandComplete tag (e.g. "INSERT 0 3", "UPDATE 2", "SELECT 5") → row count.
fn command_tag_count(body: &[u8]) -> u64 {
    let tag = String::from_utf8_lossy(body);
    let tag = tag.trim_end_matches('\0');
    tag.split_whitespace()
        .last()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0)
}

/// ErrorResponse → a `code: message` string. Fields are `type byte + cstr`,
/// terminated by a zero type byte.
fn parse_error(body: &[u8]) -> String {
    let mut message = String::new();
    let mut code = String::new();
    let mut pos = 0;
    while pos < body.len() && body[pos] != 0 {
        let ftype = body[pos];
        pos += 1;
        let end = body[pos..].iter().position(|&b| b == 0).unwrap_or(0);
        let value = String::from_utf8_lossy(&body[pos..pos + end]).into_owned();
        pos += end + 1;
        match ftype {
            b'M' => message = value,
            b'C' => code = value,
            _ => {}
        }
    }
    if code.is_empty() {
        format!("postgres error: {message}")
    } else {
        format!("postgres error [{code}]: {message}")
    }
}

// ---------------------------------------------------------------------------
// SCRAM-SHA-256 client state (RFC 5802 / RFC 7677).
// ---------------------------------------------------------------------------

struct Scram {
    client_nonce: String,
    client_first_bare: String,
    server_signature: Option<[u8; 32]>,
}

impl Scram {
    fn new() -> Result<Scram, String> {
        let client_nonce = crypto::base64_encode(&random_bytes(18)?);
        Ok(Scram {
            client_first_bare: format!("n=,r={client_nonce}"),
            client_nonce,
            server_signature: None,
        })
    }

    /// Given the server-first message, compute the client-final message and
    /// stash the expected server signature for later verification.
    fn handle_server_first(&mut self, cfg: &Config, server_first: &str) -> Result<String, String> {
        let mut nonce = None;
        let mut salt_b64 = None;
        let mut iterations = None;
        for attr in server_first.split(',') {
            match attr.split_once('=') {
                Some(("r", v)) => nonce = Some(v.to_string()),
                Some(("s", v)) => salt_b64 = Some(v.to_string()),
                Some(("i", v)) => iterations = v.parse::<u32>().ok(),
                _ => {}
            }
        }
        let nonce = nonce.ok_or("SCRAM server-first missing nonce")?;
        let salt = crypto::base64_decode(&salt_b64.ok_or("SCRAM server-first missing salt")?)?;
        let iterations = iterations.ok_or("SCRAM server-first missing iteration count")?;
        if !nonce.starts_with(&self.client_nonce) {
            return Err("SCRAM server nonce does not extend client nonce".into());
        }

        let password = cfg.password.as_deref().unwrap_or("");
        let salted = crypto::pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
        let client_key = crypto::hmac_sha256(&salted, b"Client Key");
        let stored_key = crypto::sha256(&client_key);

        let client_final_bare = format!("c=biws,r={nonce}");
        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare, server_first, client_final_bare
        );

        let client_signature = crypto::hmac_sha256(&stored_key, auth_message.as_bytes());
        let mut proof = client_key;
        for (p, s) in proof.iter_mut().zip(client_signature.iter()) {
            *p ^= *s;
        }

        let server_key = crypto::hmac_sha256(&salted, b"Server Key");
        self.server_signature = Some(crypto::hmac_sha256(&server_key, auth_message.as_bytes()));

        Ok(format!(
            "{},p={}",
            client_final_bare,
            crypto::base64_encode(&proof)
        ))
    }

    fn verify_server_final(&self, server_final: &str) -> Result<(), String> {
        let expected = self
            .server_signature
            .ok_or("SCRAM final before server-first")?;
        for attr in server_final.split(',') {
            if let Some(("v", v)) = attr.split_once('=') {
                let got = crypto::base64_decode(v)?;
                if got == expected {
                    return Ok(());
                }
                return Err("SCRAM server signature mismatch (auth failed)".into());
            }
        }
        Err("SCRAM server-final missing verifier".into())
    }
}

/// Read `n` unpredictable bytes from the OS CSPRNG (`/dev/urandom`), falling
/// back to a time/address-seeded mix if that is unavailable.
fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    use std::fs::File;
    if let Ok(mut f) = File::open("/dev/urandom") {
        let mut buf = vec![0u8; n];
        if f.read_exact(&mut buf).is_ok() {
            return Ok(buf);
        }
    }
    // Fallback: hash time + stack/heap addresses. Good enough for a nonce.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut out = Vec::with_capacity(n);
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let local = 0u8;
    let addr = &local as *const u8 as u64;
    while out.len() < n {
        let mut h = DefaultHasher::new();
        seed.hash(&mut h);
        addr.hash(&mut h);
        out.len().hash(&mut h);
        let v = h.finish();
        out.extend_from_slice(&v.to_le_bytes());
        seed = seed.wrapping_add(v).wrapping_add(0x9e3779b97f4a7c15);
    }
    out.truncate(n);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_tag_counts() {
        assert_eq!(command_tag_count(b"INSERT 0 3\0"), 3);
        assert_eq!(command_tag_count(b"UPDATE 2\0"), 2);
        assert_eq!(command_tag_count(b"DELETE 0\0"), 0);
        assert_eq!(command_tag_count(b"SELECT 5\0"), 5);
        assert_eq!(command_tag_count(b"CREATE TABLE\0"), 0);
    }

    #[test]
    fn decodes_typed_values() {
        assert_eq!(decode_value(16, b"t"), Json::Bool(true));
        assert_eq!(decode_value(16, b"f"), Json::Bool(false));
        assert_eq!(decode_value(23, b"42"), Json::int(42));
        assert_eq!(decode_value(701, b"2.5"), Json::Num(2.5));
        assert_eq!(decode_value(25, b"hello"), Json::str("hello"));
    }

    #[test]
    fn parses_error_fields() {
        // S=ERROR, C=28P01, M=password authentication failed, then terminator.
        let body = b"SERROR\0C28P01\0Mpassword authentication failed\0\0";
        let msg = parse_error(body);
        assert!(msg.contains("28P01"));
        assert!(msg.contains("password authentication failed"));
    }

    #[test]
    fn scram_client_first_shape() {
        let s = Scram::new().unwrap();
        assert!(s.client_first_bare.starts_with("n=,r="));
        assert!(s.client_nonce.len() >= 20);
    }

    #[test]
    fn random_bytes_are_sized_and_varied() {
        let a = random_bytes(18).unwrap();
        let b = random_bytes(18).unwrap();
        assert_eq!(a.len(), 18);
        assert_ne!(a, b); // astronomically unlikely to collide
    }
}

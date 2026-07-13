//! PostgreSQL frontend/backend protocol v3 over a blocking socket.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;

use sutegi_json::Json;

use crate::crypto;
use crate::{Config, PgValue};

/// Protocol version 3.0 (`0x00030000`).
const PROTOCOL_VERSION: i32 = 196608;

/// Cap on distinct statements cached per connection. Once reached, further
/// novel SQL runs through the unnamed statement rather than growing without
/// bound (the server keeps every named statement alive for the session).
const STMT_CACHE_MAX: usize = 256;

/// One live connection to a backend.
pub struct Client {
    write: TcpStream,
    read: std::io::BufReader<TcpStream>,
    /// Set once the connection is known to be broken so the pool drops it.
    broken: bool,
    /// SQL text → server-side prepared-statement name, so a repeated query
    /// skips the `Parse` step. Empty/disabled means always use the unnamed
    /// statement. Reset implicitly when the connection (and this `Client`) is
    /// dropped, which is also when the server forgets the statements.
    stmt_cache: BTreeMap<String, String>,
    /// Monotonic id for minting unique prepared-statement names.
    stmt_seq: u32,
    /// Whether to cache prepared statements at all (from [`Config`]).
    cache_enabled: bool,
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
            stmt_cache: BTreeMap::new(),
            stmt_seq: 0,
            cache_enabled: cfg.statement_cache,
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

    /// The extended-query exchange, with per-connection statement caching.
    ///
    /// A novel SQL string is `Parse`d into a named statement and remembered;
    /// on repeat only `Bind`/`Execute` are sent. A cached plan can be
    /// invalidated by DDL run on the same connection (PostgreSQL raises
    /// "cached plan must not change result type"); we detect that, evict the
    /// entry, and retry once with a fresh `Parse`.
    fn extended(&mut self, sql: &str, params: &[PgValue]) -> Result<(Vec<Json>, u64), String> {
        let (name, need_parse) = self.resolve_statement(sql);
        match self.extended_once(sql, params, &name, need_parse) {
            Ok(res) => {
                // Only commit a freshly-parsed *named* statement to the cache
                // after it round-trips cleanly (a Parse error must not leave a
                // phantom entry).
                if self.cache_enabled && need_parse && !name.is_empty() {
                    self.stmt_cache.insert(sql.to_string(), name);
                }
                Ok(res)
            }
            Err(e) => {
                if !need_parse && !self.broken && e.contains("cached plan") {
                    // Reused a statement the server invalidated: drop it and
                    // retry, which mints and parses a fresh one.
                    self.stmt_cache.remove(sql);
                    return self.extended(sql, params);
                }
                Err(e)
            }
        }
    }

    /// Resolve the prepared-statement name to Bind against, and whether this
    /// call must `Parse` first. Returns an empty name for the unnamed
    /// statement (caching off, or the per-connection cache is full).
    fn resolve_statement(&mut self, sql: &str) -> (String, bool) {
        pick_statement(
            &self.stmt_cache,
            &mut self.stmt_seq,
            self.cache_enabled,
            sql,
        )
    }

    /// One Parse(?)→Bind→Describe→Execute→Sync round-trip against `stmt_name`
    /// (empty = unnamed), collecting rows and the affected-row count.
    fn extended_once(
        &mut self,
        sql: &str,
        params: &[PgValue],
        stmt_name: &str,
        parse: bool,
    ) -> Result<(Vec<Json>, u64), String> {
        let mut out = Vec::new();
        let name_bytes = stmt_name.as_bytes();

        // Parse: named (or unnamed) statement, no declared parameter types
        // (the server infers them). Skipped when the statement is cached.
        if parse {
            out.extend(frame(b'P', |b| {
                b.extend_from_slice(name_bytes);
                b.push(0);
                b.extend_from_slice(sql.as_bytes());
                b.push(0);
                b.extend_from_slice(&0i16.to_be_bytes()); // 0 parameter type OIDs
            }));
        }

        // Bind: unnamed portal against `stmt_name`, all params text, results text.
        out.extend(frame(b'B', |b| {
            b.push(0); // portal ""
            b.extend_from_slice(name_bytes);
            b.push(0);
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

    /// The read-side socket, for the LISTEN/NOTIFY [`crate::Listener`] to tune
    /// (clearing the read timeout, cloning a shutdown handle).
    pub(crate) fn read_stream(&self) -> &TcpStream {
        self.read.get_ref()
    }

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
    pub(crate) fn send_msg(&mut self, tag: u8, body: &[u8]) -> Result<(), String> {
        let mut packet = Vec::with_capacity(body.len() + 5);
        packet.push(tag);
        packet.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        packet.extend_from_slice(body);
        self.send_raw(&packet)
    }

    /// Read one tagged message: returns `(type, body)`.
    pub(crate) fn recv(&mut self) -> Result<(u8, Vec<u8>), String> {
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

/// Decide which prepared statement to Bind against, and whether a `Parse` is
/// needed first. Pure so the caching policy can be tested without a socket.
/// Returns `("", true)` for the unnamed statement (caching off or cache full),
/// `(name, false)` for a cache hit, or a freshly-minted `(name, true)` to parse.
fn pick_statement(
    cache: &BTreeMap<String, String>,
    seq: &mut u32,
    enabled: bool,
    sql: &str,
) -> (String, bool) {
    if !enabled {
        return (String::new(), true);
    }
    if let Some(name) = cache.get(sql) {
        return (name.clone(), false);
    }
    if cache.len() >= STMT_CACHE_MAX {
        return (String::new(), true);
    }
    *seq += 1;
    (format!("sutegi_s{}", seq), true)
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
        // JSON and vectors travel as text; the server casts them to the target
        // column type (json/jsonb, or pgvector's `vector`).
        PgValue::Json(s) => Some(s.clone().into_bytes()),
        PgValue::Vector(s) => Some(s.clone().into_bytes()),
    }
}

// The `be_*` readers saturate to 0 on a short buffer rather than panicking:
// with no TLS on the wire (yet), a MITM or a buggy server could send a
// truncated message, and an index panic there is a client-side DoS. The
// message parsers below also bounds-check every slice for the same reason.
pub(crate) fn be_i32(buf: &[u8], at: usize) -> i32 {
    match buf.get(at..at + 4) {
        Some(b) => i32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

fn be_i16(buf: &[u8], at: usize) -> i16 {
    match buf.get(at..at + 2) {
        Some(b) => i16::from_be_bytes([b[0], b[1]]),
        None => 0,
    }
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
    // Cap the pre-allocation to what the buffer could actually hold (each field
    // is at least 19 bytes) so a bogus count can't request a huge Vec.
    let mut fields = Vec::with_capacity(count.min(body.len() / 19 + 1));
    for _ in 0..count {
        if pos >= body.len() {
            break;
        }
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
        // A length that runs past the message end is malformed — stop rather
        // than slice out of bounds.
        let end = match pos.checked_add(len as usize) {
            Some(e) if e <= body.len() => e,
            _ => {
                obj.insert(name, Json::Null);
                break;
            }
        };
        let raw = &body[pos..end];
        pos = end;
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
        // json (114) / jsonb (3802): decode straight into structured JSON, so a
        // JSON column arrives as a real object/array, not a string.
        114 | 3802 => Json::parse(&text).unwrap_or_else(|_| Json::str(text.into_owned())),
        // Everything else (text, timestamps, and pgvector's `vector`, whose OID
        // is extension-assigned) comes back as a string; typed extractors parse
        // it further where needed.
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
pub(crate) fn parse_error(body: &[u8]) -> String {
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
        // The iteration count comes from the server, and PBKDF2 loops that many
        // times. With no TLS on the wire yet, a malicious or MITM'd server could
        // send a huge count to pin the client's CPU (a DoS). Bound it: real
        // servers use a few thousand; 1,000,000 is a generous ceiling.
        if !(1..=1_000_000).contains(&iterations) {
            return Err(format!("SCRAM iteration count out of range: {iterations}"));
        }
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
    fn statement_cache_policy() {
        let mut seq = 0u32;

        // Disabled: always the unnamed statement, always parse, seq untouched.
        let cache = BTreeMap::new();
        assert_eq!(
            pick_statement(&cache, &mut seq, false, "SELECT 1"),
            (String::new(), true)
        );
        assert_eq!(seq, 0);

        // Enabled miss: mint a fresh name and parse it.
        let mut cache = BTreeMap::new();
        let (name, parse) = pick_statement(&cache, &mut seq, true, "SELECT 1");
        assert_eq!((name.as_str(), parse), ("sutegi_s1", true));
        // Caller commits it on success; a hit then skips Parse.
        cache.insert("SELECT 1".to_string(), name);
        assert_eq!(
            pick_statement(&cache, &mut seq, true, "SELECT 1"),
            ("sutegi_s1".to_string(), false)
        );
        // A different SQL mints the next name.
        assert_eq!(
            pick_statement(&cache, &mut seq, true, "SELECT 2"),
            ("sutegi_s2".to_string(), true)
        );
    }

    #[test]
    fn statement_cache_falls_back_to_unnamed_when_full() {
        let mut seq = 0u32;
        let mut cache = BTreeMap::new();
        for i in 0..STMT_CACHE_MAX {
            cache.insert(format!("q{i}"), format!("sutegi_s{i}"));
        }
        // Cache full + novel SQL: unnamed statement, no seq bump.
        assert_eq!(
            pick_statement(&cache, &mut seq, true, "novel"),
            (String::new(), true)
        );
        assert_eq!(seq, 0);
        // A still-cached SQL is unaffected.
        cache.insert("hot".to_string(), "sutegi_hot".to_string());
        assert_eq!(
            pick_statement(&cache, &mut seq, true, "hot"),
            ("sutegi_hot".to_string(), false)
        );
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

    // -- adversarial: malformed wire messages must not panic the client -------
    //
    // These parsers consume server-controlled bytes, and with no TLS on the
    // connection yet a MITM could inject truncated/overlong frames. A parser
    // panic aborts the worker, so every message parser must degrade gracefully.

    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn message_parsers_never_panic_on_garbage() {
        let mut seed = 0x5047_5f46_555a_5a00u64; // "PG_FUZZ"
        let fields = vec![
            ("a".to_string(), 23),
            ("b".to_string(), 16),
            ("c".to_string(), 25),
        ];
        for _ in 0..50_000 {
            let len = (splitmix(&mut seed) as usize) % 64;
            let body: Vec<u8> = (0..len).map(|_| splitmix(&mut seed) as u8).collect();
            // Each of these previously indexed unchecked into `body`.
            let _ = parse_row_description(&body);
            let _ = parse_data_row(&body, &fields);
            let _ = parse_data_row(&body, &[]); // no field metadata at all
            let _ = parse_error(&body);
            let _ = parse_cstr_list(&body);
            let _ = command_tag_count(&body);
        }
    }

    #[test]
    fn scram_handshake_survives_hostile_server_messages() {
        // The server drives these strings; with no TLS a MITM can too. Parsing
        // must never panic, and the iteration count must be bounded.
        let cfg = Config::from_url("postgres://u:p@localhost/db").unwrap();
        let mut seed = 0x5343_5241_4d00_0000u64; // "SCRAM"
        for _ in 0..20_000 {
            let len = (splitmix(&mut seed) as usize) % 80;
            let alphabet = b"rsi=,0123456789abcdefghijklmnopABCDEF+/= ";
            let msg: String = (0..len)
                .map(|_| alphabet[(splitmix(&mut seed) as usize) % alphabet.len()] as char)
                .collect();
            let mut scram = Scram::new().unwrap();
            let _ = scram.handle_server_first(&cfg, &msg); // Ok/Err, never panic/hang
            let _ = scram.verify_server_final(&msg);
        }
        // A hostile, well-formed-looking huge iteration count is rejected, not run.
        let mut scram = Scram::new().unwrap();
        let nonce = &scram.client_nonce.clone();
        let evil = format!("r={nonce}extra,s=YWJjZA==,i=4000000000");
        let err = scram.handle_server_first(&cfg, &evil).unwrap_err();
        assert!(err.contains("iteration count"), "got: {err}");
    }

    #[test]
    fn data_row_survives_truncated_and_overlong_lengths() {
        let fields = vec![("x".to_string(), 25)];
        // Claims one column of length 1000 but the body is far shorter.
        let mut body = vec![0, 1]; // column count = 1
        body.extend_from_slice(&1000i32.to_be_bytes());
        body.extend_from_slice(b"short");
        let row = parse_data_row(&body, &fields);
        // The overlong column degrades to null instead of panicking.
        assert_eq!(row.get("x"), Some(&Json::Null));
    }
}

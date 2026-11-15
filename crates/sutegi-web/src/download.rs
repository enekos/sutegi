//! Seekable response bodies for recordings and other large objects.
use std::io::{Read, Seek, SeekFrom};
use sutegi_http::{Method, Request, Response};

/// Serve one already-authorized open object with bounded memory. Supports
/// GET and HEAD, one byte range (closed, open-ended, or suffix), and 416 for
/// unsatisfiable ranges. Invalid/multiple ranges are ignored with a full 200.
/// Range is ignored on HEAD; If-Range falls back to 200 because this helper
/// does not own an ETag/Last-Modified validator. Authenticate before opening.
/// Use immutable keys when recording content types must match their bytes.
pub fn download(
    request: &Request,
    mut reader: impl Read + Seek + Send + 'static,
    content_type: &str,
) -> Response {
    if !matches!(request.method, Method::Get | Method::Head) {
        return Response::new(405).with_header("allow", "GET, HEAD");
    }
    let length = match reader.seek(SeekFrom::End(0)) {
        Ok(length) => length,
        Err(_) => return Response::new(500),
    };
    let range = if request.method == Method::Get && request.header("if-range").is_none() {
        request.header("range").map(|v| parse_range(v, length))
    } else {
        None
    };
    let (status, start, count) = match range {
        Some(Range::Bytes(start, count)) => (206, start, count),
        Some(Range::Unsatisfiable) => {
            return Response::new(416)
                .with_header("accept-ranges", "bytes")
                .with_header("content-range", &format!("bytes */{length}"));
        }
        _ => (200, 0, length),
    };
    if reader.seek(SeekFrom::Start(start)).is_err() {
        return Response::new(500);
    }
    let mut response = Response::new(status)
        .with_header("content-type", content_type)
        .with_header("accept-ranges", "bytes");
    if status == 206 {
        response = response.with_header(
            "content-range",
            &format!("bytes {start}-{}/{length}", start + count - 1),
        );
    }
    response.with_reader(reader, count)
}

enum Range {
    Ignore,
    Unsatisfiable,
    Bytes(u64, u64),
}
fn decimal(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        None
    } else {
        value.parse().ok()
    }
}
fn parse_range(value: &str, length: u64) -> Range {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Range::Ignore;
    };
    let Some((first, last)) = spec.split_once('-') else {
        return Range::Ignore;
    };
    if first.is_empty() {
        let Some(suffix) = decimal(last) else {
            return Range::Ignore;
        };
        if suffix == 0 || length == 0 {
            return Range::Unsatisfiable;
        }
        let count = suffix.min(length);
        return Range::Bytes(length - count, count);
    }
    let Some(start) = decimal(first) else {
        return Range::Ignore;
    };
    let end = if last.is_empty() {
        length.saturating_sub(1)
    } else {
        let Some(end) = decimal(last) else {
            return Range::Ignore;
        };
        if end < start {
            return Range::Ignore;
        }
        end.min(length.saturating_sub(1))
    };
    if start >= length {
        Range::Unsatisfiable
    } else {
        Range::Bytes(start, end - start + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn response(range: &str) -> Response {
        let mut request = Request {
            method: Method::Get,
            path: "/recording".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: vec![],
            peer: None,
        };
        request.headers.push(("range".into(), range.into()));
        download(&request, Cursor::new(b"0123456789".to_vec()), "video/webm")
    }
    #[test]
    fn byte_ranges_match_wire_lengths() {
        for (range, status, body) in [
            ("bytes=2-4", 206, "234"),
            ("bytes=8-", 206, "89"),
            ("bytes=-3", 206, "789"),
            ("bytes=0-999", 206, "0123456789"),
            ("bytes=10-", 416, ""),
            ("bytes=-0", 416, ""),
            ("bytes=4-2", 200, "0123456789"),
            ("bytes=0-1,4-5", 200, "0123456789"),
            ("bytes=18446744073709551616-", 200, "0123456789"),
        ] {
            let response = response(range);
            assert_eq!(response.status, status, "{range}");
            let mut wire = Vec::new();
            sutegi_http::write_response(&mut wire, response, false).unwrap();
            let wire = String::from_utf8(wire).unwrap();
            assert!(wire.contains(&format!("content-length: {}\r\n", body.len())));
            assert_eq!(wire.split_once("\r\n\r\n").unwrap().1, body);
        }
    }
}

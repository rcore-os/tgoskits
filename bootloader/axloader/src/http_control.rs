//! Platform-independent HTTP control-plane framing and validation.

extern crate alloc;

use alloc::{string::String, vec::Vec};

pub const MAX_HEADER_BYTES: usize = 4 * 1024;
pub const MAX_SERIAL_BINDING_BYTES: usize = 1024;
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// The parsed request line and headers. The body is read by the transport
/// adapter after this bounded framing step completes.
#[derive(Debug, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub content_length: usize,
}

/// Parse one HTTP/1.x header block and return the first byte of its body.
///
/// The parser deliberately accepts neither duplicate `Content-Length` nor
/// `Transfer-Encoding`; the two frontends therefore share the same framing
/// contract instead of making different choices about an ambiguous request.
pub fn parse_head(bytes: &[u8]) -> Result<(RequestHead, usize), &'static str> {
    let header_end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or("request header incomplete")?;
    if header_end > MAX_HEADER_BYTES {
        return Err("request header too large");
    }
    let header = core::str::from_utf8(&bytes[..header_end]).map_err(|_| "request is not UTF-8")?;
    let mut lines = header.split("\r\n");
    let first = lines.next().ok_or("missing request line")?;
    let mut words = first.split_ascii_whitespace();
    let method = words.next().unwrap_or_default();
    let path = words.next().unwrap_or_default();
    let version = words.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") || words.next().is_some() {
        return Err("invalid HTTP request");
    }

    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("invalid HTTP header")?;
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err("duplicate content length");
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "invalid content length")?,
            );
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err("transfer encoding is unsupported");
        }
        headers.push((key.into(), value.into()));
    }
    Ok((
        RequestHead {
            method: method.into(),
            path: path.into(),
            headers,
            content_length: content_length.unwrap_or(0),
        },
        header_end,
    ))
}

/// Return the maximum body accepted by one route.
pub fn body_limit(method: &str, path: &str, initramfs_limit: usize) -> usize {
    match (method, path) {
        ("POST", "/api/v1/serial/continue") | ("POST", "/api/v1/ota/confirm") => {
            MAX_SERIAL_BINDING_BYTES
        }
        ("POST", "/api/v1/boot/jobs") => MAX_HEADER_BYTES,
        ("PUT", "/api/v1/ota/image") => MAX_IMAGE_BYTES,
        _ => MAX_IMAGE_BYTES.max(initramfs_limit),
    }
}

/// Validate the declared length and bytes already read from a stream.
pub fn validate_body_length(
    method: &str,
    path: &str,
    declared: usize,
    already_read: usize,
    initramfs_limit: usize,
) -> Result<(), &'static str> {
    if declared > body_limit(method, path, initramfs_limit) {
        return Err(match (method, path) {
            ("POST", "/api/v1/serial/continue")
            | ("POST", "/api/v1/ota/confirm")
            | ("POST", "/api/v1/boot/jobs") => "request body too large",
            _ => "body too large",
        });
    }
    if already_read > declared {
        return Err("body exceeds content length");
    }
    Ok(())
}

pub fn error_status(error: &str) -> &'static str {
    match error {
        "unknown_boot_job" => "404 Not Found",
        "body too large" => "413 Payload Too Large",
        "request body too large" => "400 Bad Request",
        "request header too large" => "431 Request Header Fields Too Large",
        _ => "400 Bad Request",
    }
}

pub fn parse_update_id(value: &str) -> Option<[u8; 36]> {
    let bytes: [u8; 36] = value.as_bytes().try_into().ok()?;
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        .then_some(bytes)
}

pub fn valid_image_version(value: &str) -> bool {
    value.len() <= 96 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INITRAMFS_LIMIT: usize = 256 * 1024 * 1024;

    #[test]
    fn request_head_rejects_ambiguous_framing() {
        let duplicate =
            b"POST /api/v1/ota/confirm HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\na";
        assert_eq!(parse_head(duplicate), Err("duplicate content length"));
        let transfer = b"POST /api/v1/ota/confirm HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            parse_head(transfer),
            Err("transfer encoding is unsupported")
        );
    }

    #[test]
    fn request_head_rejects_duplicate_control_headers_case_insensitively() {
        for header in [
            "x-boot-epoch",
            "x-serial-binding",
            "x-image-sha256",
            "x-image-version",
            "x-update-id",
            "x-update-source",
        ] {
            let request = format!(
                "GET /api/v1/status HTTP/1.1\r\n{header}: first\r\n{}: second\r\n\r\n",
                header.to_ascii_uppercase()
            );
            assert_eq!(
                parse_head(request.as_bytes()).map(|_| ()),
                Err("duplicate request header")
            );
        }

        let repeatable =
            b"GET /api/v1/status HTTP/1.1\r\nx-trace-id: first\r\nX-TRACE-ID: second\r\n\r\n";
        let (head, _) = parse_head(repeatable).unwrap();
        assert_eq!(head.headers.len(), 2);
        assert_eq!(head.headers[0].0, "x-trace-id");
        assert_eq!(head.headers[0].1, "first");
    }

    #[test]
    fn route_limits_and_pre_read_bytes_share_the_same_contract() {
        assert_eq!(
            body_limit("POST", "/api/v1/serial/continue", INITRAMFS_LIMIT),
            MAX_SERIAL_BINDING_BYTES
        );
        assert_eq!(
            validate_body_length(
                "POST",
                "/api/v1/serial/continue",
                MAX_SERIAL_BINDING_BYTES + 1,
                0,
                INITRAMFS_LIMIT,
            ),
            Err("request body too large")
        );
        assert_eq!(
            validate_body_length("POST", "/api/v1/boot/jobs", 1, 2, INITRAMFS_LIMIT),
            Err("body exceeds content length")
        );
        assert_eq!(
            error_status("request header too large"),
            "431 Request Header Fields Too Large"
        );
    }

    #[test]
    fn update_ids_and_versions_keep_ota_metadata_bounded() {
        assert!(parse_update_id("01234567-89ab-cdef-0123-456789abcdef").is_some());
        assert!(parse_update_id("01234567-89ab-cdef-0123-456789abcdeg").is_none());
        assert!(valid_image_version("2026.10.13-loongarch"));
        assert!(!valid_image_version("line\nfeed"));
    }
}

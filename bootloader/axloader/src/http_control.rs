//! Platform-independent HTTP control-plane framing and validation.

extern crate alloc;

use alloc::{string::String, vec::Vec};

use httpboot_protocol::OtaSource;
use thiserror::Error;

use crate::{boot_job::FileKind, integrity::decode_sha256};

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

/// The control-plane operation selected by a request method and path.
///
/// Dynamic path components borrow from the request path so classification
/// does not allocate or move ownership into the transport adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route<'a> {
    DeviceStatus,
    OtaStatus,
    BootJobStatus { id: &'a str },
    SerialContinue,
    SerialBindingRevoke { id: &'a str },
    OtaConfirm,
    OtaImage,
    BootJobCreate,
    BootJobCancel { id: &'a str },
    BootJobUpload { id: &'a str, kind: FileKind },
    BootJobStart { id: &'a str },
    Unknown,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AdmissionError {
    #[error("stale boot epoch")]
    StaleBootEpoch,
    #[error("OTA trial requires confirmation")]
    OtaTrialRequiresConfirmation,
}

impl AdmissionError {
    pub fn status(self) -> &'static str {
        match self {
            Self::StaleBootEpoch | Self::OtaTrialRequiresConfirmation => "409 Conflict",
        }
    }

    pub fn body(self) -> &'static [u8] {
        match self {
            Self::StaleBootEpoch => br#"{"error":"stale_boot_epoch"}"#,
            Self::OtaTrialRequiresConfirmation => br#"{"error":"ota_trial_requires_confirmation"}"#,
        }
    }
}

/// Classify a request into the control-plane operation its backend must run.
pub fn route<'a>(method: &str, path: &'a str) -> Route<'a> {
    const BOOT_JOBS: &str = "/api/v1/boot/jobs/";

    match (method, path) {
        ("GET", "/api/v1/status") => Route::DeviceStatus,
        ("GET", "/api/v1/ota/status") => Route::OtaStatus,
        ("GET", _) => path
            .strip_prefix(BOOT_JOBS)
            .and_then(job_status_id)
            .map_or(Route::Unknown, |id| Route::BootJobStatus { id }),
        ("POST", "/api/v1/serial/continue") => Route::SerialContinue,
        ("POST", "/api/v1/boot/jobs") => Route::BootJobCreate,
        ("POST", "/api/v1/ota/confirm") => Route::OtaConfirm,
        ("POST", _) => path
            .strip_prefix(BOOT_JOBS)
            .and_then(|tail| tail.split_once('/'))
            .filter(|(_, operation)| *operation == "start")
            .map_or(Route::Unknown, |(id, _)| Route::BootJobStart { id }),
        ("DELETE", _) => {
            if let Some(id) = path.strip_prefix("/api/v1/serial/bindings/") {
                return Route::SerialBindingRevoke { id };
            }
            if let Some(tail) = path.strip_prefix(BOOT_JOBS) {
                match tail.split_once('/') {
                    Some((id, "")) => return Route::BootJobCancel { id },
                    None => return Route::BootJobCancel { id: tail },
                    _ => {}
                }
            }
            Route::Unknown
        }
        ("PUT", "/api/v1/ota/image") => Route::OtaImage,
        ("PUT", _) => path
            .strip_prefix(BOOT_JOBS)
            .and_then(|tail| tail.split_once('/'))
            .and_then(|(id, operation)| match operation {
                "kernel" => Some((id, FileKind::Kernel)),
                "initramfs" => Some((id, FileKind::Initramfs)),
                _ => None,
            })
            .map_or(Route::Unknown, |(id, kind)| Route::BootJobUpload {
                id,
                kind,
            }),
        _ => Route::Unknown,
    }
}

fn job_status_id(tail: &str) -> Option<&str> {
    (!tail.is_empty() && !tail.contains('/')).then_some(tail)
}

impl Route<'_> {
    /// Apply the epoch and OTA-trial rules shared by both HTTP backends.
    pub fn check_admission(
        &self,
        request_epoch: Option<&str>,
        current_epoch: &str,
        ota_trial: bool,
    ) -> Result<(), AdmissionError> {
        if matches!(
            self,
            Route::DeviceStatus | Route::OtaStatus | Route::BootJobStatus { .. }
        ) {
            return Ok(());
        }
        if request_epoch != Some(current_epoch) {
            return Err(AdmissionError::StaleBootEpoch);
        }
        if ota_trial
            && !matches!(
                self,
                Route::SerialContinue
                    | Route::SerialBindingRevoke { .. }
                    | Route::OtaConfirm
                    | Route::OtaImage
            )
        {
            return Err(AdmissionError::OtaTrialRequiresConfirmation);
        }
        Ok(())
    }
}

impl RequestHead {
    /// Return the first value of a request header, ignoring ASCII case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn ota_source(&self) -> Result<OtaSource, &'static str> {
        match self.header("x-update-source") {
            Some("server") => Ok(OtaSource::Server),
            Some(_) => Err("invalid OTA update source"),
            None => Ok(OtaSource::Direct),
        }
    }

    pub fn ota_image_version(&self) -> Result<Option<&str>, &'static str> {
        let version = self.header("x-image-version");
        if version.is_some_and(|value| !valid_image_version(value)) {
            return Err("invalid OTA image version");
        }
        Ok(version)
    }

    pub fn ota_image_sha256(&self) -> Option<[u8; 32]> {
        self.header("x-image-sha256").and_then(decode_sha256)
    }

    pub fn ota_update_id(&self) -> Option<[u8; 36]> {
        self.header("x-update-id").and_then(parse_update_id)
    }
}

/// Parse one HTTP/1.x header block and return the first byte of its body.
///
/// The parser deliberately accepts neither duplicate single-value control
/// headers nor `Transfer-Encoding`; the two frontends therefore share the same
/// framing contract instead of making different choices about an ambiguous
/// request.
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
    let mut single_value_headers = [false; SINGLE_VALUE_HEADERS.len()];
    for line in lines.take_while(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("invalid HTTP header")?;
        let key = key.trim();
        let value = value.trim();
        if let Some(slot) = single_value_header_slot(key) {
            if single_value_headers[slot] {
                return Err(
                    if SINGLE_VALUE_HEADERS[slot].eq_ignore_ascii_case("content-length") {
                        "duplicate content length"
                    } else {
                        "duplicate request header"
                    },
                );
            }
            single_value_headers[slot] = true;
        }
        if key.eq_ignore_ascii_case("content-length") {
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

const SINGLE_VALUE_HEADERS: [&str; 7] = [
    "content-length",
    "x-boot-epoch",
    "x-serial-binding",
    "x-image-sha256",
    "x-image-version",
    "x-update-id",
    "x-update-source",
];

fn single_value_header_slot(key: &str) -> Option<usize> {
    SINGLE_VALUE_HEADERS
        .iter()
        .position(|header| key.eq_ignore_ascii_case(header))
}

/// Return the maximum body accepted by one route.
pub fn body_limit(method: &str, path: &str, initramfs_limit: usize) -> usize {
    match route(method, path) {
        Route::SerialContinue | Route::OtaConfirm => MAX_SERIAL_BINDING_BYTES,
        Route::BootJobCreate => MAX_HEADER_BYTES,
        Route::OtaImage => MAX_IMAGE_BYTES,
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
        return Err(match route(method, path) {
            Route::SerialContinue | Route::OtaConfirm | Route::BootJobCreate => {
                "request body too large"
            }
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

        let server_ota = b"GET /api/v1/status HTTP/1.1\r\nX-Update-ID: \
                           01234567-89ab-cdef-0123-456789abcdef\r\nX-Update-Source: server\r\n\r\n";
        let (head, _) = parse_head(server_ota).unwrap();
        assert_eq!(head.ota_source(), Ok(OtaSource::Server));
        assert!(head.ota_update_id().is_some());

        let repeatable =
            b"GET /api/v1/status HTTP/1.1\r\nx-trace-id: first\r\nX-TRACE-ID: second\r\n\r\n";
        let (head, _) = parse_head(repeatable).unwrap();
        assert_eq!(head.header("X-trace-id"), Some("first"));
    }

    #[test]
    fn route_classifies_backend_operations_and_unknown_requests() {
        assert_eq!(route("GET", "/api/v1/status"), Route::DeviceStatus);
        assert_eq!(route("GET", "/api/v1/ota/status"), Route::OtaStatus);
        assert_eq!(
            route("GET", "/api/v1/boot/jobs/job-1"),
            Route::BootJobStatus { id: "job-1" }
        );
        assert_eq!(route("GET", "/api/v1/boot/jobs/"), Route::Unknown);
        assert_eq!(route("GET", "/api/v1/boot/jobs/job/extra"), Route::Unknown);
        assert_eq!(
            route("DELETE", "/api/v1/serial/bindings/binding-1"),
            Route::SerialBindingRevoke { id: "binding-1" }
        );
        assert_eq!(
            route("DELETE", "/api/v1/boot/jobs/job-1"),
            Route::BootJobCancel { id: "job-1" }
        );
        assert_eq!(
            route("PUT", "/api/v1/boot/jobs/job-1/kernel"),
            Route::BootJobUpload {
                id: "job-1",
                kind: FileKind::Kernel
            }
        );
        assert_eq!(
            route("PUT", "/api/v1/boot/jobs/job-1/initramfs"),
            Route::BootJobUpload {
                id: "job-1",
                kind: FileKind::Initramfs
            }
        );
        assert_eq!(
            route("POST", "/api/v1/boot/jobs/job-1/start"),
            Route::BootJobStart { id: "job-1" }
        );
        assert_eq!(
            route("POST", "/api/v1/boot/jobs/job-1/unknown"),
            Route::Unknown
        );
        assert_eq!(route("GET", "/api/v1/unknown"), Route::Unknown);
        assert_eq!(route("POST", "/api/v1/status"), Route::Unknown);
    }

    #[test]
    fn route_admission_shares_epoch_and_trial_rules() {
        for selected_route in [
            Route::DeviceStatus,
            Route::OtaStatus,
            Route::BootJobStatus { id: "job-1" },
        ] {
            assert_eq!(
                selected_route.check_admission(Some("stale"), "current", true),
                Ok(())
            );
        }

        for selected_route in [
            Route::SerialContinue,
            Route::SerialBindingRevoke { id: "binding-1" },
            Route::OtaConfirm,
            Route::OtaImage,
        ] {
            assert_eq!(
                selected_route.check_admission(None, "current", false),
                Err(AdmissionError::StaleBootEpoch)
            );
            assert_eq!(
                selected_route.check_admission(Some("current"), "current", true),
                Ok(())
            );
        }

        for selected_route in [Route::BootJobCreate, Route::Unknown] {
            assert_eq!(
                selected_route.check_admission(None, "current", false),
                Err(AdmissionError::StaleBootEpoch)
            );
            assert_eq!(
                selected_route.check_admission(Some("current"), "current", true),
                Err(AdmissionError::OtaTrialRequiresConfirmation)
            );
        }
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

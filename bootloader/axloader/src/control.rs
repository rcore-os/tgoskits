extern crate alloc;

use alloc::{format, string::String};
use core::str;

use crate::{boards, discovery, http, identity};

const CONTROL_BODY_LIMIT: usize = 8192;
const HELLO_RETRY_LIMIT: usize = 8;
const HELLO_RETRY_STALL: core::time::Duration = core::time::Duration::from_millis(500);
const BOOT_OFFER_POLL_LIMIT: usize = 30;
const BOOT_OFFER_POLL_STALL: core::time::Duration = core::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    NoServerUrl,
    Identity(identity::IdentityError),
    Http(http::DownloadError),
    NonUtf8,
    MissingField(&'static str),
    InvalidNumber(&'static str),
    ServerError,
    PollLimit,
}

#[derive(Debug, Clone)]
pub struct BootOffer {
    pub boot_id: String,
    pub kernel_url: String,
    pub kernel_size: u64,
    pub image_format: String,
    pub arch: String,
    pub entry_symbol: Option<String>,
}

pub fn fetch_boot_offer() -> Result<BootOffer, ControlError> {
    let mac = identity::mac_address_string().map_err(ControlError::Identity)?;
    crate::logln!("loader_mac: {mac}");
    let server_url = server_url(&mac)?;
    crate::logln!("server_url: {server_url}");

    let hello_url = format!("{server_url}/api/v1/httpboot/loaders/hello");
    let hello_body = format!(
        "{{\"protocol_version\":1,\"nonce\":\"{}\",\"arch\":\"{}\",\"board\":\"{}\",\"mac\":\"{}\"\
         ,\"firmware_vendor\":\"UEFI\",\"loader_version\":\"axloader\",\"capabilities\":{{\"\
         image_formats\":[\"elf64\"],\"range_get\":true,\"sha256\":false}}}}",
        boards::active::BOARD_NAME,
        boards::active::ARCH_NAME,
        boards::active::BOARD_NAME,
        mac
    );
    let hello_response = post_hello_with_retry(&hello_url, &hello_body)?;
    let hello_text = str::from_utf8(&hello_response).map_err(|_| ControlError::NonUtf8)?;
    let poll_url =
        json_string_field(hello_text, "poll_url").ok_or(ControlError::MissingField("poll_url"))?;
    crate::logln!("loader_poll_url: {poll_url}");

    for poll_round in 1..=BOOT_OFFER_POLL_LIMIT {
        crate::logln!("boot_offer_poll: {poll_round}/{BOOT_OFFER_POLL_LIMIT}");
        let body = http::get_json_body(poll_url, CONTROL_BODY_LIMIT).map_err(ControlError::Http)?;
        let text = str::from_utf8(&body).map_err(|_| ControlError::NonUtf8)?;
        match json_string_field(text, "state") {
            Some("ready") => return parse_ready_offer(text),
            Some("waiting") => {
                if let Some(message) = json_string_field(text, "message") {
                    crate::logln!("boot_offer_waiting: {message}");
                }
                uefi::boot::stall(BOOT_OFFER_POLL_STALL);
            }
            Some("error") => return Err(ControlError::ServerError),
            _ => return Err(ControlError::MissingField("state")),
        }
    }

    Err(ControlError::PollLimit)
}

fn post_hello_with_retry(url: &str, body: &str) -> Result<alloc::vec::Vec<u8>, ControlError> {
    let mut last_error = None;
    for attempt in 1..=HELLO_RETRY_LIMIT {
        match http::post_json_body(url, body, CONTROL_BODY_LIMIT) {
            Ok(response) => return Ok(response),
            Err(err) => {
                last_error = Some(err);
                if attempt < HELLO_RETRY_LIMIT {
                    uefi::boot::stall(HELLO_RETRY_STALL);
                }
            }
        }
    }
    Err(ControlError::Http(
        last_error.unwrap_or(http::DownloadError::RequestFailed),
    ))
}

fn parse_ready_offer(input: &str) -> Result<BootOffer, ControlError> {
    let arch = json_string_field(input, "arch").ok_or(ControlError::MissingField("arch"))?;
    if arch != boards::active::ARCH_NAME {
        return Err(ControlError::ServerError);
    }
    let image_format = json_string_field(input, "image_format")
        .ok_or(ControlError::MissingField("image_format"))?;
    if image_format != "elf64" {
        return Err(ControlError::ServerError);
    }

    Ok(BootOffer {
        boot_id: json_string_field(input, "boot_id")
            .ok_or(ControlError::MissingField("boot_id"))?
            .into(),
        kernel_url: json_string_field(input, "kernel_url")
            .ok_or(ControlError::MissingField("kernel_url"))?
            .into(),
        kernel_size: json_u64_field(input, "kernel_size")
            .ok_or(ControlError::InvalidNumber("kernel_size"))?,
        image_format: image_format.into(),
        arch: arch.into(),
        entry_symbol: json_nullable_string_field(input, "entry_symbol").map(String::from),
    })
}

fn trim_trailing_slash(input: &str) -> &str {
    input.strip_suffix('/').unwrap_or(input)
}

fn server_url(mac: &str) -> Result<String, ControlError> {
    match discovery::discover_server(mac) {
        Ok(url) => return Ok(url),
        Err(err) => crate::logln!("discovery_error: {err:?}"),
    }

    let server_url = option_env!("BOOTLOADER_HTTP_SERVER_URL").ok_or(ControlError::NoServerUrl)?;
    Ok(trim_trailing_slash(server_url).into())
}

fn json_string_field<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    parse_json_string(field_value(input, key)?)
}

fn json_nullable_string_field<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let value = field_value(input, key)?;
    if value.starts_with("null") {
        None
    } else {
        parse_json_string(value)
    }
}

fn json_u64_field(input: &str, key: &str) -> Option<u64> {
    let value = field_value(input, key)?;
    let end = value
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(value.len());
    value.get(..end)?.parse().ok()
}

fn field_value<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let key_start = input.find(&pattern)?;
    let after_key = input.get(key_start + pattern.len()..)?;
    let colon = after_key.find(':')?;
    Some(after_key.get(colon + 1..)?.trim_start())
}

fn parse_json_string(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => return None,
            b'"' => return input.get(1..index),
            _ => index += 1,
        }
    }
    None
}

extern crate alloc;

use alloc::{format, string::String, vec::Vec};
use core::str;

use httpboot_protocol::{SERIAL_BOOT_PREFIX, SERIAL_PROTOCOL_VERSION, SERIAL_READY_PREFIX};

use crate::{boards, console};

const SERIAL_BOOT_LINE_LIMIT: usize = 4096;
const SERIAL_BOOT_WAIT_POLLS: usize = 60_000;
const SERIAL_BOOT_POLL_STALL: core::time::Duration = core::time::Duration::from_millis(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    Timeout,
    NonUtf8,
    MissingField(&'static str),
    InvalidNumber(&'static str),
    InvalidProtocolVersion,
    ServerError,
    LineTooLong,
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
    crate::logln!("serial_control_wait: waiting for AXLOADER BOOT");
    announce_ready();
    let line = read_boot_line()?;
    parse_boot_offer(&line)
}

fn announce_ready() {
    crate::logln!(
        concat!(
            "{}{{\"protocol_version\":1,",
            "\"board\":\"{}\",",
            "\"arch\":\"{}\",",
            "\"loader_version\":\"axloader\"}}"
        ),
        SERIAL_READY_PREFIX,
        boards::active::BOARD_NAME,
        boards::active::ARCH_NAME,
    );
}

fn read_boot_line() -> Result<String, ControlError> {
    let mut line = Vec::new();
    for _ in 0..SERIAL_BOOT_WAIT_POLLS {
        while let Some(byte) = console::serial_read_byte() {
            match byte {
                b'\r' => {}
                b'\n' => {
                    if line.is_empty() {
                        continue;
                    }
                    let text = str::from_utf8(&line).map_err(|_| ControlError::NonUtf8)?;
                    if text.trim_start().starts_with(SERIAL_BOOT_PREFIX) {
                        return Ok(text.into());
                    }
                    crate::logln!("serial_control_ignored: {text}");
                    line.clear();
                }
                byte => {
                    if line.len() >= SERIAL_BOOT_LINE_LIMIT {
                        return Err(ControlError::LineTooLong);
                    }
                    line.push(byte);
                }
            }
        }
        uefi::boot::stall(SERIAL_BOOT_POLL_STALL);
    }

    Err(ControlError::Timeout)
}

fn parse_boot_offer(input: &str) -> Result<BootOffer, ControlError> {
    let input = input
        .trim()
        .strip_prefix(SERIAL_BOOT_PREFIX)
        .ok_or(ControlError::MissingField("serial_boot_prefix"))?;
    let protocol_version = json_u64_field(input, "protocol_version")
        .ok_or(ControlError::InvalidNumber("protocol_version"))?;
    if protocol_version != u64::from(SERIAL_PROTOCOL_VERSION) {
        return Err(ControlError::InvalidProtocolVersion);
    }
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

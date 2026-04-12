#![cfg_attr(feature = "ax-std", no_std)]

#[cfg(feature = "ax-std")]
extern crate alloc;
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use alloc::string::String;
#[cfg(feature = "ax-std")]
use core::fmt::Write as _;
#[cfg(feature = "ax-std")]
use std::println;

pub const RESULT_BEGIN_MARKER: &str = "AXTEST_RESULT_BEGIN";
pub const RESULT_END_MARKER: &str = "AXTEST_RESULT_END";

/// Emit one structured result record for the host-side runner.
///
/// The record is framed by fixed begin/end markers so the runner can extract it
/// from mixed console output. `details_json` is embedded as raw JSON and should
/// therefore already be a valid JSON object/array/value.
pub fn emit_case_result(
    case_id: &str,
    status: &str,
    message: Option<&str>,
    details_json: Option<&str>,
) {
    #[cfg(feature = "ax-std")]
    {
        println!("{RESULT_BEGIN_MARKER}");
        let mut payload = String::new();
        payload.push('{');
        push_json_field(&mut payload, "case_id", case_id);
        payload.push(',');
        push_json_field(&mut payload, "status", status);
        if let Some(message) = message {
            payload.push(',');
            push_json_field(&mut payload, "message", message);
        }
        if let Some(details_json) = details_json {
            payload.push_str(",\"details\":");
            payload.push_str(details_json);
        }
        payload.push('}');
        println!("{payload}");
        println!("{RESULT_END_MARKER}");
    }

    #[cfg(not(feature = "ax-std"))]
    {
        let _ = (case_id, status, message, details_json);
    }
}

#[cfg(feature = "ax-std")]
fn push_json_field(payload: &mut String, key: &str, value: &str) {
    push_json_string(payload, key);
    payload.push(':');
    push_json_string(payload, value);
}

#[cfg(feature = "ax-std")]
fn push_json_string(payload: &mut String, value: &str) {
    payload.push('"');
    for ch in value.chars() {
        match ch {
            '"' => payload.push_str("\\\""),
            '\\' => payload.push_str("\\\\"),
            '\n' => payload.push_str("\\n"),
            '\r' => payload.push_str("\\r"),
            '\t' => payload.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(payload, "\\u{:04x}", ch as u32);
            }
            ch => payload.push(ch),
        }
    }
    payload.push('"');
}

/// Convenience wrapper for a passing guest case.
pub fn emit_case_pass(case_id: &str, message: &str, details_json: Option<&str>) {
    emit_case_result(case_id, "pass", Some(message), details_json);
}

/// Convenience wrapper for a failing guest case.
pub fn emit_case_fail(case_id: &str, message: &str, details_json: Option<&str>) {
    emit_case_result(case_id, "fail", Some(message), details_json);
}

/// Convenience wrapper for a skipped guest case.
pub fn emit_case_skip(case_id: &str, message: &str, details_json: Option<&str>) {
    emit_case_result(case_id, "skip", Some(message), details_json);
}

/// Emit an error result and terminate the guest immediately afterwards.
pub fn emit_case_error(case_id: &str, message: &str, details_json: Option<&str>) -> ! {
    emit_case_result(case_id, "error", Some(message), details_json);
    power_off_or_hang();
}

/// Terminate the guest if the runtime can power off cleanly; otherwise spin.
///
/// The non-`ax-std` fallback keeps the CPU busy so the function still has a
/// well-defined diverging behavior in minimal environments.
pub fn power_off_or_hang() -> ! {
    #[cfg(feature = "ax-std")]
    {
        use std::os::arceos::modules::ax_hal;
        ax_hal::power::system_off();
    }

    #[cfg(not(feature = "ax-std"))]
    loop {
        core::hint::spin_loop();
    }
}

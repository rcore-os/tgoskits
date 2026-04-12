#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use std::println;

use axvisor_guestlib::{emit_case_fail, power_off_or_hang};

const CASE_ID: &str = "example.fail_report";

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() -> ! {
    println!("Running {CASE_ID}");
    emit_case_fail(
        CASE_ID,
        "example guest reported fail",
        Some(r#"{"example":"fail","value":-1}"#),
    );
    power_off_or_hang();
}

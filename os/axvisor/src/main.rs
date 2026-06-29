// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # Axvisor Kernel
//!
//! Kernel entry point for the Axvisor hypervisor.
//!
//! This module wires together early boot presentation, hardware virtualization
//! enablement, VM initialization/startup, and the interactive management shell.
//! The implementation is intentionally small so that the boot order is visible
//! from a single file.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[macro_use]
extern crate log;

#[macro_use]
extern crate alloc;

use ax_std as _;
#[cfg(target_os = "none")]
extern crate ax_std as std;

#[cfg(not(axtest))]
mod config;
#[cfg(all(
    not(axtest),
    any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64"
    )
))]
mod fdt;
#[cfg(target_arch = "loongarch64")]
mod guest_platform;
#[cfg(not(axtest))]
mod images;
#[cfg(not(axtest))]
mod manager;
#[cfg(not(axtest))]
mod shell;

#[cfg(not(axtest))]
use std::println;

/// Startup banners printed before the hypervisor begins initialization.
#[cfg(not(axtest))]
const LOGO: [&str; 2] = [
    r#"
       d8888            888     888  d8b
      d88888            888     888  Y8P
     d88P888            888     888
    d88P 888  888  888  Y88b   d88P  888  .d8888b    .d88b.   888d888
   d88P  888  `Y8bd8P'   Y88b d88P   888  88K       d88""88b  888P"
  d88P   888    X88K      Y88o88P    888  "Y8888b.  888  888  888
 d8888888888  .d8""8b.     Y888P     888       X88  Y88..88P  888
d88P     888  888  888      Y8P      888   88888P'   "Y88P"   888
"#,
    r#"
    _         __     ___
   / \   __  _\ \   / (_)___  ___  _ __
  / _ \  \ \/ /\ \ / /| / __|/ _ \| '__|
 / ___ \  >  <  \ V / | \__ \ (_) | |
/_/   \_\/_/\_\  \_/  |_|___/\___/|_|
"#,
];

/// Prints the startup banner to the console.
#[cfg(not(axtest))]
fn print_logo() {
    println!();
    println!("{}", LOGO[0]);
    println!();
    println!("by AxVisor Team");
    println!();
}

/// Axvisor kernel entry point.
///
/// The startup sequence is:
///
/// 1. Print the startup banner.
/// 2. Check and enable hardware virtualization on every CPU.
/// 3. Build and start configured guest VMs.
/// 4. Enter the management shell after the default guests have exited.
#[cfg(not(axtest))]
#[cfg_attr(target_os = "none", unsafe(no_mangle))]
fn main() {
    print_logo();

    info!("Starting virtualization...");
    let manager = manager::AxvmManager::new().expect("failed to initialize AxVM manager");

    manager.init_default_vms();
    manager.start_default_vms();

    info!("[OK] Default guest initialized");

    shell::console_init();
}

/// Axvisor test entry point, activated by `--cfg axtest`.
///
/// Runs all registered `#[axtest]` functions and reports results in KTAP format.
/// Prints `AXTEST_SUITE_OK` on success or panics with `AXTEST_SUITE_FAIL` on failure.
#[cfg(axtest)]
#[cfg_attr(target_os = "none", unsafe(no_mangle))]
fn main() {
    use core::fmt::Arguments;

    fn axtest_print(args: Arguments<'_>) {
        std::print!("{}", args);
    }

    axtest::set_printer(axtest_print);
    axtest::set_coverage_wait_fn(wait_for_coverage_extraction);
    let summary = axtest::init().run_tests();
    if summary.failed == 0 {
        axtest::dump_coverage();
        std::println!("AXTEST_SUITE_OK");
        ax_hal::power::system_off();
    } else {
        panic!("AXTEST_SUITE_FAIL failed={}", summary.failed);
    }
}

fn wait_for_coverage_extraction() {
    // Give the host enough time to read the profraw via the QEMU monitor
    // before we proceed to system_off. CI runs QEMU without KVM, where a
    // ~30 MB memsave takes well under a second; 5 s is a comfortable cap.
    const WAIT_NANOS: u64 = 5_000_000_000;
    let start = ax_hal::time::wall_time_nanos();
    while ax_hal::time::wall_time_nanos().saturating_sub(start) < WAIT_NANOS {
        core::hint::spin_loop();
    }
}

/// Smoke tests to verify the axtest framework works on Axvisor.
#[cfg(axtest)]
mod axtests {
    use axtest::prelude::*;

    #[axtest]
    fn arithmetic_smoke() {
        ax_assert_eq!(2 + 2, 4);
    }

    #[axtest]
    fn explicit_result_smoke() -> axtest::AxTestResult {
        ax_assert!(true);
        axtest::AxTestResult::Ok
    }
}

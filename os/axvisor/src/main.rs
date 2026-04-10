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
//! The main kernel binary for the Axvisor hypervisor.

#![no_std]
#![no_main]
#![feature(used_with_arg)]
#![cfg(target_os = "none")]

#[macro_use]
extern crate log;

#[macro_use]
extern crate alloc;

extern crate ax_std as std;

#[cfg(target_arch = "x86_64")]
extern crate axplat_x86_qemu_q35;

mod hal;
mod logo;
mod shell;
mod task;
mod vmm;

#[unsafe(no_mangle)]
fn main() {
    logo::print_logo();

    info!("Starting virtualization...");
    info!("Hardware support: {:?}", axvm::has_hardware_support());
    hal::enable_virtualization();

    vmm::init();
    vmm::start();

    info!("[OK] Default guest initialized");

    shell::console_init();
}

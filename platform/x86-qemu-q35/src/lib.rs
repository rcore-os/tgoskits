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

#![no_std]
#![cfg(all(target_arch = "x86_64", target_os = "none"))]
#![allow(missing_abi)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate axplat;

use core::ptr::addr_of;

mod apic;
mod boot;
mod console;
mod init;
mod mem;
mod power;
mod time;

#[cfg(feature = "smp")]
mod mp;

pub mod config {
    pub mod plat {
        pub const PHYS_VIRT_OFFSET: usize = 0xffff_8000_0000_0000;
        pub const BOOT_STACK_SIZE: usize = 0x40000;
    }

    pub mod devices {
        pub const TIMER_FREQUENCY: usize = 4_000_000_000; // 100 MHz
    }
}

fn current_cpu_id() -> usize {
    match raw_cpuid::CpuId::new().get_feature_info() {
        Some(finfo) => finfo.initial_local_apic_id() as usize,
        None => 0,
    }
}

unsafe extern fn rust_entry(magic: usize, mbi: usize) {
    if magic == self::boot::MULTIBOOT_BOOTLOADER_MAGIC {
        axplat::call_main(current_cpu_id(), mbi);
    }
}

unsafe extern fn rust_entry_secondary(_magic: usize) {
    #[cfg(feature = "smp")]
    if _magic == self::boot::MULTIBOOT_BOOTLOADER_MAGIC {
        axplat::call_secondary_main(current_cpu_id());
    }
}

pub fn cpu_count() -> usize {
    unsafe extern {
        static SMP: usize;
    }

    addr_of!(SMP) as _
}

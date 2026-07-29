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

//! Runtime library of [ArceOS](https://github.com/arceos-org/arceos).
//!
//! Any application uses ArceOS should link this library. It does some
//! initialization work before entering the application's `main` function.
//!
//! # Cargo Features
//!
//! - `paging`: Enable page table manipulation support.
//! - `irq`: Enable interrupt handling support.
//! - `multitask`: Enable multi-threading support.
//! - `smp`: Enable SMP (symmetric multiprocessing) support.
//! - `fs`: Enable filesystem support.
//! - `net`: Enable networking support.
//! - `display`: Enable graphics support.
//!
//! All the features are optional and disabled by default.

#![feature(extern_item_impls)]
#![cfg_attr(not(test), no_std)]
#![allow(missing_abi)]

#[macro_use]
extern crate ax_log;

extern crate ax_driver as _;

#[cfg(all(target_os = "none", not(feature = "std-compat"), not(test)))]
mod lang_items;
#[cfg(all(
    feature = "stack-protector",
    any(target_os = "none", target_env = "musl"),
    not(test)
))]
mod stack_protector;

#[cfg(feature = "smp")]
mod mp;

mod boot_memory;
mod bootstrap;
mod guard;
#[cfg(feature = "paging")]
mod kernel_mapping;
mod klib;

#[cfg(any(feature = "irq", test))]
mod clock_event;
#[cfg(any(feature = "irq", feature = "multitask", test))]
mod clock_event_runtime;
mod devices;
mod fs;
#[cfg(feature = "irq")]
mod interrupt_bootstrap;
#[cfg(any(feature = "ipi", feature = "wake-ipi", test))]
mod ipi_delivery;
#[cfg(feature = "irq")]
pub mod irq;
mod registers;
#[cfg(feature = "serial")]
pub mod serial;

#[cfg(feature = "multitask")]
pub mod task;

#[cfg(all(feature = "net", feature = "fs"))]
mod unix_ns;

#[cfg(feature = "aic8800-wifi")]
mod wifi_glue;

pub use ax_hal as hal;

pub(crate) mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

/// Maximum logical CPU count represented by runtime-sized CPU masks.
#[cfg(feature = "smp")]
pub const CPU_CAPACITY: usize = build_info::CPU_CAPACITY;

/// A uniprocessor runtime represents only CPU zero.
#[cfg(not(feature = "smp"))]
pub const CPU_CAPACITY: usize = 1;

pub use bootstrap::rust_main;

#[cfg(feature = "smp")]
pub use self::mp::rust_main_secondary;

extern crate alloc;

#[cfg(feature = "fs")]
pub(crate) fn runtime_default_task_stack_size() -> usize {
    build_info::TASK_STACK_SIZE
}

#[eii]
fn ax_app_entry() {
    #[cfg(not(test))]
    unsafe extern "C" {
        /// Legacy application's entry point.
        safe fn main();
    }
    // Default implementation
    #[cfg(not(test))]
    main();
}

struct LogIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_log::LogIf for LogIfImpl {
    fn console_write_str(s: &str) {
        #[cfg(feature = "serial")]
        if serial::route_console_bytes(s.as_bytes()).is_some() {
            return;
        }
        ax_hal::console::write_text_bytes(s.as_bytes());
    }

    fn try_write_log_record(record: &str) -> bool {
        #[cfg(feature = "serial")]
        {
            serial::route_console_bytes(record.as_bytes()).is_some()
        }
        #[cfg(not(feature = "serial"))]
        {
            let _ = record;
            false
        }
    }

    fn current_time() -> core::time::Duration {
        ax_hal::time::monotonic_time()
    }

    fn current_cpu_id() -> Option<usize> {
        #[cfg(feature = "smp")]
        if is_init_ok() {
            Some(ax_hal::percpu::this_cpu_id())
        } else {
            None
        }
        #[cfg(not(feature = "smp"))]
        Some(0)
    }

    fn current_task_id() -> Option<u64> {
        if is_init_ok() {
            #[cfg(feature = "multitask")]
            {
                task::current_thread_id().ok().map(|id| id.as_u64())
            }
            #[cfg(not(feature = "multitask"))]
            None
        } else {
            None
        }
    }
}

use core::sync::atomic::{AtomicUsize, Ordering};

/// Number of CPUs that have completed initialization.
static INITED_CPUS: AtomicUsize = AtomicUsize::new(0);

fn is_init_ok() -> bool {
    INITED_CPUS.load(Ordering::Acquire) == ax_hal::cpu_num()
}

#[cfg(test)]
mod tests {
    #[test]
    fn fs_init_accepts_bootargs_without_fs_feature() {
        crate::fs::init(Some("root=/dev/nvme0n1"));
    }
}

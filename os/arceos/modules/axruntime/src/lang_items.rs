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

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    match axpanic::enter_panic(current_cpu_id()) {
        axpanic::PanicDisposition::Primary => panic_primary(info),
        // Once panic ownership is established, recursive and cross-CPU panic
        // entries must avoid the full print/backtrace path and stop locally.
        axpanic::PanicDisposition::Recursive | axpanic::PanicDisposition::Concurrent => {
            halt_current_cpu()
        }
    }
}

fn panic_primary(info: &PanicInfo) -> ! {
    // Advertise the fatal path before printing so downstream output helpers can
    // switch to a more conservative mode.
    let _oops_guard = axpanic::enter_oops();
    panic_message(info);
    panic_backtrace();
    panic_shutdown()
}

fn panic_message(info: &PanicInfo) {
    ax_println!("{}", info);
}

fn panic_backtrace() {
    if should_print_panic_backtrace() {
        ax_println!("{}", axbacktrace::Backtrace::capture());
    }
}

// This is only the first gate on panic-path backtrace emission. Now it
// enforces a single attempt, and in future it can be extended with stricter
// policies such as platform-specific suppression or additional recursion-aware
// degradation.
fn should_print_panic_backtrace() -> bool {
    axpanic::should_emit_panic_backtrace()
}

fn panic_shutdown() -> ! {
    ax_hal::power::system_off()
}

fn halt_current_cpu() -> ! {
    loop {
        ax_hal::asm::halt();
        core::hint::spin_loop();
    }
}

fn current_cpu_id() -> usize {
    #[cfg(feature = "smp")]
    {
        ax_hal::percpu::this_cpu_id()
    }

    #[cfg(not(feature = "smp"))]
    {
        0
    }
}

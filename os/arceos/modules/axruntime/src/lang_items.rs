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

use core::{
    fmt::{self, Write},
    panic::PanicInfo,
};

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Panic diagnostics dereference the current task's exact stack capability.
    // Keep the CPU and current-thread publication stable for the entire fatal
    // path; shutdown never restores local interrupt delivery.
    ax_hal::asm::disable_irqs();
    match axpanic::enter_panic(current_cpu_id()) {
        axpanic::PanicDisposition::Primary => panic_primary(info),
        // Once panic ownership is established, recursive and cross-CPU panic
        // entries must avoid the full print/backtrace path and terminate the
        // system instead of halting one CPU and risking test timeouts.
        axpanic::PanicDisposition::Recursive => {
            crate::console::write_emergency_text_bytes(b"recursive kernel panic\n");
            panic_shutdown()
        }
        axpanic::PanicDisposition::Concurrent => {
            crate::console::write_emergency_text_bytes(b"concurrent kernel panic\n");
            panic_shutdown()
        }
    }
}

fn panic_primary(info: &PanicInfo) -> ! {
    let _oops_guard = axpanic::enter_oops();
    panic_message(info);
    panic_backtrace();
    panic_shutdown()
}

fn panic_message(info: &PanicInfo) {
    let mut writer = EmergencyWriter;
    let _ = writeln!(writer, "{info}");
}

fn panic_backtrace() {
    if should_print_panic_backtrace() {
        let mut writer = EmergencyWriter;
        let _ = axbacktrace::write_current_raw(&mut writer, "panic");
    }
}

fn should_print_panic_backtrace() -> bool {
    axpanic::should_emit_panic_backtrace()
}

fn panic_shutdown() -> ! {
    let _ = crate::console::flush_emergency_output();
    ax_hal::power::system_off()
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

struct EmergencyWriter;

impl Write for EmergencyWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        crate::console::write_emergency_text_bytes(text.as_bytes());
        Ok(())
    }
}

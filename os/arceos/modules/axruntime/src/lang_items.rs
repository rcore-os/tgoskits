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
use axpanic::PanicDisposition;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let cpu_id = current_cpu_id();
    match axpanic::enter_panic(cpu_id) {
        PanicDisposition::Primary => primary_panic(info),
        PanicDisposition::Recursive | PanicDisposition::Concurrent => secondary_panic(),
    }
}

fn current_cpu_id() -> usize {
    #[cfg(feature = "smp")]
    {
        ax_hal::percpu::this_cpu_id()
    }
    #[cfg(not(feature = "smp"))]
    0
}

fn primary_panic(info: &PanicInfo) -> ! {
    let _oops = axpanic::enter_oops();

    ax_println!("{}", info);
    if axpanic::should_emit_panic_backtrace() {
        let bt = axbacktrace::Backtrace::capture();
        ax_println!("{}", bt.report("panic"));
    }
    ax_hal::power::system_off()
}

fn secondary_panic() -> ! {
    let _oops = axpanic::enter_oops();

    #[cfg(feature = "irq")]
    loop {
        ax_hal::asm::wait_for_irqs();
    }
    #[cfg(not(feature = "irq"))]
    loop {
        core::hint::spin_loop();
    }
}

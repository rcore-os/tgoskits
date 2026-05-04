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

use core::sync::atomic::{AtomicUsize, Ordering};

const PANIC_CPU_INVALID: usize = usize::MAX;

static PANIC_CPU: AtomicUsize = AtomicUsize::new(PANIC_CPU_INVALID);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PanicDisposition {
    Primary,
    Recursive,
    Concurrent,
}

pub(crate) fn classify_current_panic() -> PanicDisposition {
    let current_cpu = current_cpu_id();

    match PANIC_CPU.compare_exchange(
        PANIC_CPU_INVALID,
        current_cpu,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => PanicDisposition::Primary,
        Err(owner_cpu) if owner_cpu == current_cpu => PanicDisposition::Recursive,
        Err(_) => PanicDisposition::Concurrent,
    }
}

pub(crate) fn halt_current_cpu() -> ! {
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

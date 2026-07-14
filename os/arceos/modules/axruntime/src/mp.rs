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

static SECONDARY_CPUID_BY_SLOT: [AtomicUsize; crate::build_info::CPU_CAPACITY - 1] =
    [const { AtomicUsize::new(usize::MAX) }; crate::build_info::CPU_CAPACITY - 1];

static ENTERED_CPUS: AtomicUsize = AtomicUsize::new(1);

fn prepare_secondary_boot_stack(slot: usize, cpu_id: usize) {
    SECONDARY_CPUID_BY_SLOT[slot].store(cpu_id, Ordering::Release);
}

#[allow(clippy::absurd_extreme_comparisons)]
pub fn start_secondary_cpus(primary_cpu_id: usize) {
    let mut slot = 0;
    let cpu_num = ax_hal::cpu_num();
    for i in 0..cpu_num {
        if i != primary_cpu_id && slot < cpu_num - 1 {
            prepare_secondary_boot_stack(slot, i);

            let stack_top = 0;

            debug!("starting CPU {i}...");
            ax_hal::power::cpu_boot(i, stack_top);
            slot += 1;

            while ENTERED_CPUS.load(Ordering::Acquire) <= slot {
                core::hint::spin_loop();
            }
        }
    }
}

/// The main entry point of the ArceOS runtime for secondary cores.
///
/// It is called from the bootstrapping code in the specific platform crate.
#[ax_plat::secondary_main]
pub fn rust_main_secondary(cpu_id: usize) -> ! {
    // Park harts whose logical index is beyond the compile-time CPU count: QEMU
    // may start more harts (`-smp M`) than the kernel was built for
    // (`CPU_CAPACITY == N`). Mirror Linux — run on the first N CPUs and park the
    // excess, rather than panicking in `percpu::init_secondary(cpu_id)` /
    // `AxCpuMask::one_shot(cpu_id)` / `RUN_QUEUES[cpu_id]`, which all assert
    // `index < CPU_CAPACITY`. Must precede `init_secondary`, which would otherwise
    // mis-index the per-CPU area first.
    if cpu_id >= crate::build_info::CPU_CAPACITY {
        loop {
            ax_hal::asm::wait_for_irqs();
        }
    }
    ax_hal::percpu::init_secondary(cpu_id);
    crate::guard::assert_boot_guards_released();
    // After per-CPU init, before scheduler/IPI/IRQ paths can allocate.
    // This is a no-op for allocator backends that do not need per-CPU state.
    ax_alloc::init_percpu_slab(cpu_id);
    #[cfg(all(feature = "tls", feature = "multitask"))]
    crate::task::initialize_early_bootstrap_tls()
        .expect("failed to initialize secondary bootstrap TLS");
    #[cfg(all(feature = "tls", not(feature = "multitask")))]
    super::init_tls();
    ax_hal::init_early_secondary(cpu_id);

    ENTERED_CPUS.fetch_add(1, Ordering::Release);
    info!("Secondary CPU {cpu_id} started.");

    #[cfg(feature = "paging")]
    ax_mm::init_memory_management_secondary();

    ax_hal::init_later_secondary(cpu_id);

    #[cfg(feature = "multitask")]
    {
        crate::task::initialize_secondary(cpu_id)
            .expect("failed to initialize secondary task scheduler");
    }

    #[cfg(feature = "ipi")]
    ax_ipi::init();

    // Bring up local IRQ/IPI delivery before publishing INITED_CPUS so the
    // primary cannot enter user-visible init while remote CPUs still lack SGI
    // handlers or pending per-CPU IRQ enables.
    #[cfg(feature = "irq")]
    super::init_percpu_irq(cpu_id);

    #[cfg(feature = "irq")]
    ax_hal::asm::enable_irqs();

    #[cfg(all(feature = "irq", feature = "ipi"))]
    ax_ipi::mark_current_cpu_ready();

    #[cfg(feature = "multitask")]
    crate::task::publish_current_cpu_online().expect("failed to publish secondary scheduler CPU");

    info!("Secondary CPU {cpu_id:x} init OK.");
    super::INITED_CPUS.fetch_add(1, Ordering::Release);

    while !super::is_init_ok() {
        core::hint::spin_loop();
    }

    #[cfg(feature = "multitask")]
    crate::task::run_idle();
    #[cfg(not(feature = "multitask"))]
    loop {
        ax_hal::asm::wait_for_irqs();
    }
}

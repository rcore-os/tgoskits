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

static ENTERED_CPUS: AtomicUsize = AtomicUsize::new(1);

const fn secondary_cpu_is_usable(cpu_id: usize, runtime_cpu_count: usize) -> bool {
    cpu_id < runtime_cpu_count
}

#[allow(clippy::absurd_extreme_comparisons)]
pub fn start_secondary_cpus(primary_cpu_id: usize) {
    let mut slot = 0;
    let cpu_num = ax_hal::cpu_num();
    assert_eq!(
        ax_hal::mem::cpu_shared_memory_model(),
        ax_hal::mem::CpuSharedMemoryModel::Coherent,
        "SMP requires coherent cacheable memory shared by all CPUs"
    );
    for i in 0..cpu_num {
        if i != primary_cpu_id && slot < cpu_num.saturating_sub(1) {
            debug!("starting CPU {i}...");
            ax_hal::power::cpu_boot(i);
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
    // The platform may enter more harts than the runtime topology selected.
    // ax_hal::cpu_num() is already min(platform_cpu_count, CPU_CAPACITY); use
    // that same limit for secondary admission and the BSP completion count.
    // This must precede per-CPU initialization, which indexes the final area.
    if !secondary_cpu_is_usable(cpu_id, ax_hal::cpu_num()) {
        loop {
            ax_hal::asm::wait_for_irqs();
        }
    }
    ax_hal::percpu::init_secondary(cpu_id);
    crate::guard::assert_boot_preemption_held();
    // After per-CPU init, before scheduler/IPI/IRQ paths can allocate.
    // This is a no-op for allocator backends that do not need per-CPU state.
    ax_alloc::init_percpu_slab(cpu_id);
    ax_hal::init_early_secondary(cpu_id);

    #[cfg(feature = "tls")]
    crate::task::initialize_early_bootstrap_tls()
        .expect("failed to initialize secondary bootstrap TLS");

    ENTERED_CPUS.fetch_add(1, Ordering::Release);
    info!("Secondary CPU {cpu_id} started.");

    #[cfg(feature = "paging")]
    ax_mm::init_memory_management_secondary();
    super::bootstrap::initialize_scheduler_before_platform(
        || crate::task::initialize_secondary(cpu_id),
        || ax_hal::init_later_secondary(cpu_id),
    )
    .expect("failed to initialize secondary task scheduler");

    #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
    ax_ipi::init();

    // Bring up local IRQ/IPI delivery before publishing INITED_CPUS so the
    // primary cannot enter user-visible init while remote CPUs still lack SGI
    // handlers or pending per-CPU IRQ enables.
    super::interrupt_bootstrap::init_cpu(cpu_id);

    // Complete architecture-local IPI readiness before the scheduler exposes
    // this CPU as a target. A scheduler safe point after publication may rearm
    // a physical self-doorbell even while bootstrap preemption is still held.
    #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
    {
        ax_hal::asm::flush_tlb(None);
        ax_ipi::mark_current_cpu_ready();
    }
    let online_cpu = crate::task::publish_current_cpu_online()
        .expect("failed to publish secondary scheduler CPU");
    crate::task::start_current_ktimer_service().expect("failed to create secondary ktimer service");
    super::clock_event_runtime::enable_irqs_after_scheduler_online(online_cpu);
    crate::guard::release_bootstrap_preemption();

    // Publishing a log record is safe as soon as the per-CPU area exists, but
    // waking the owner worker may select a run queue or send an IPI. Publish
    // that separate capability only after this CPU has completed every
    // scheduler, IRQ, and IPI prerequisite compiled into this runtime.
    super::serial::mark_log_wake_ready(cpu_id);

    info!("Secondary CPU {cpu_id:x} init OK.");
    super::INITED_CPUS.fetch_add(1, Ordering::Release);

    while !super::is_init_ok() {
        core::hint::spin_loop();
    }
    crate::task::run_idle();
}

#[cfg(test)]
mod tests {
    use super::secondary_cpu_is_usable;

    #[test]
    fn secondary_admission_uses_the_runtime_cpu_limit() {
        assert!(secondary_cpu_is_usable(0, 2));
        assert!(secondary_cpu_is_usable(1, 2));
        assert!(!secondary_cpu_is_usable(2, 2));
        assert!(!secondary_cpu_is_usable(7, 2));
    }
}

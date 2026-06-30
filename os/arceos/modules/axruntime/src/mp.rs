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

use ax_hal::mem::VirtAddr;
#[cfg(not(feature = "plat-dyn"))]
use ax_hal::mem::virt_to_phys;

#[cfg(not(feature = "plat-dyn"))]
struct SecondaryBootStack {
    pages: ax_alloc::GlobalPage,
}

#[cfg(not(feature = "plat-dyn"))]
impl SecondaryBootStack {
    fn alloc() -> Self {
        use ax_hal::mem::PAGE_SIZE_4K;
        use ax_memory_addr::align_up_4k;
        let stack_size = align_up_4k(crate::runtime_default_task_stack_size());
        let mut pages =
            ax_alloc::GlobalPage::alloc_contiguous(stack_size / PAGE_SIZE_4K, PAGE_SIZE_4K)
                .expect("failed to allocate secondary boot stack");
        pages.zero();
        Self { pages }
    }

    fn bottom(&self) -> VirtAddr {
        self.pages.start_vaddr()
    }

    fn top(&self) -> VirtAddr {
        self.bottom() + self.pages.size()
    }
}

#[cfg(not(feature = "plat-dyn"))]
static SECONDARY_BOOT_STACKS: [ax_lazyinit::LazyInit<SecondaryBootStack>;
    crate::build_info::CPU_CAPACITY - 1] =
    [const { ax_lazyinit::LazyInit::new() }; crate::build_info::CPU_CAPACITY - 1];

static SECONDARY_CPUID_BY_SLOT: [AtomicUsize; crate::build_info::CPU_CAPACITY - 1] =
    [const { AtomicUsize::new(usize::MAX) }; crate::build_info::CPU_CAPACITY - 1];

static ENTERED_CPUS: AtomicUsize = AtomicUsize::new(1);

#[cfg(all(feature = "multitask", not(feature = "plat-dyn")))]
fn secondary_boot_stack_bottom(slot: usize) -> VirtAddr {
    SECONDARY_BOOT_STACKS[slot].bottom()
}

#[cfg(not(feature = "plat-dyn"))]
fn secondary_boot_stack_top(slot: usize) -> VirtAddr {
    SECONDARY_BOOT_STACKS[slot].top()
}

#[cfg(all(feature = "multitask", not(feature = "plat-dyn")))]
fn secondary_slot_from_cpu_id(cpu_id: usize) -> usize {
    SECONDARY_CPUID_BY_SLOT
        .iter()
        .position(|slot_cpu_id| slot_cpu_id.load(Ordering::Acquire) == cpu_id)
        .unwrap_or_else(|| panic!("secondary slot is not initialized for cpu_id {cpu_id}"))
}

#[cfg(feature = "multitask")]
fn secondary_boot_stack_bounds(cpu_id: usize) -> (VirtAddr, usize) {
    #[cfg(feature = "plat-dyn")]
    {
        ax_hal::mem::boot_stack_bounds(cpu_id)
    }
    #[cfg(not(feature = "plat-dyn"))]
    {
        let slot = secondary_slot_from_cpu_id(cpu_id);
        (
            secondary_boot_stack_bottom(slot),
            crate::runtime_default_task_stack_size(),
        )
    }
}

fn prepare_secondary_boot_stack(slot: usize, cpu_id: usize) {
    #[cfg(not(feature = "plat-dyn"))]
    SECONDARY_BOOT_STACKS[slot].init_once(SecondaryBootStack::alloc());

    SECONDARY_CPUID_BY_SLOT[slot].store(cpu_id, Ordering::Release);
}

#[allow(clippy::absurd_extreme_comparisons)]
pub fn start_secondary_cpus(primary_cpu_id: usize) {
    let mut slot = 0;
    let cpu_num = ax_hal::cpu_num();
    for i in 0..cpu_num {
        if i != primary_cpu_id && slot < cpu_num - 1 {
            prepare_secondary_boot_stack(slot, i);

            #[cfg(feature = "plat-dyn")]
            let stack_top = 0;
            #[cfg(not(feature = "plat-dyn"))]
            let stack_top = virt_to_phys(secondary_boot_stack_top(slot)).as_usize();

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
    // After per-CPU init, before scheduler/IPI/IRQ paths can allocate.
    // This is a no-op for allocator backends that do not need per-CPU state.
    ax_alloc::init_percpu_slab(cpu_id);
    ax_hal::init_early_secondary(cpu_id);

    ENTERED_CPUS.fetch_add(1, Ordering::Release);
    info!("Secondary CPU {cpu_id} started.");

    #[cfg(feature = "paging")]
    ax_mm::init_memory_management_secondary();

    ax_hal::init_later_secondary(cpu_id);

    #[cfg(feature = "multitask")]
    {
        let (stack_ptr, stack_size) = secondary_boot_stack_bounds(cpu_id);
        ax_task::init_scheduler_secondary(stack_ptr, stack_size);
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

    info!("Secondary CPU {cpu_id:x} init OK.");
    super::INITED_CPUS.fetch_add(1, Ordering::Release);

    while !super::is_init_ok() {
        core::hint::spin_loop();
    }

    #[cfg(all(feature = "tls", not(feature = "multitask")))]
    super::init_tls();

    #[cfg(feature = "multitask")]
    ax_task::run_idle();
    #[cfg(not(feature = "multitask"))]
    loop {
        ax_hal::asm::wait_for_irqs();
    }
}

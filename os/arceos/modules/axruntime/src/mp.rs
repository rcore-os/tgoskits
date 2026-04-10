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

use ax_config::{TASK_STACK_SIZE, plat::MAX_CPU_NUM};
use ax_hal::mem::{VirtAddr, virt_to_phys};

#[unsafe(link_section = ".bss.stack")]
static mut SECONDARY_BOOT_STACK: [[u8; TASK_STACK_SIZE]; MAX_CPU_NUM - 1] =
    [[0; TASK_STACK_SIZE]; MAX_CPU_NUM - 1];

static ENTERED_CPUS: AtomicUsize = AtomicUsize::new(1);

#[allow(clippy::absurd_extreme_comparisons)]
pub fn start_secondary_cpus(primary_cpu_id: usize) {
    let mut logic_cpu_id = 0;
    let cpu_num = ax_hal::cpu_num();
    for i in 0..cpu_num {
        if i != primary_cpu_id && logic_cpu_id < cpu_num - 1 {
            let stack_top = virt_to_phys(VirtAddr::from(unsafe {
                SECONDARY_BOOT_STACK[logic_cpu_id].as_ptr_range().end as usize
            }));

            debug!("starting CPU {i}...");
            ax_hal::power::cpu_boot(i, stack_top.as_usize());
            logic_cpu_id += 1;

            while ENTERED_CPUS.load(Ordering::Acquire) <= logic_cpu_id {
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
    ax_hal::percpu::init_secondary(cpu_id);
    #[cfg(all(feature = "alloc", feature = "buddy-slab"))]
    ax_alloc::init_precpu_slab(cpu_id);
    ax_hal::init_early_secondary(cpu_id);

    ENTERED_CPUS.fetch_add(1, Ordering::Release);
    info!("Secondary CPU {cpu_id} started.");

    #[cfg(feature = "paging")]
    ax_mm::init_memory_management_secondary();

    ax_hal::init_later_secondary(cpu_id);

    #[cfg(feature = "multitask")]
    ax_task::init_scheduler_secondary();

    #[cfg(feature = "ipi")]
    ax_ipi::init();

    info!("Secondary CPU {cpu_id:x} init OK.");
    super::INITED_CPUS.fetch_add(1, Ordering::Release);

    while !super::is_init_ok() {
        core::hint::spin_loop();
    }

    #[cfg(feature = "irq")]
    ax_hal::asm::enable_irqs();

    #[cfg(feature = "irq")]
    ax_hal::time::set_oneshot_timer(100);

    #[cfg(all(feature = "tls", not(feature = "multitask")))]
    super::init_tls();

    #[cfg(feature = "multitask")]
    ax_task::run_idle();
    #[cfg(not(feature = "multitask"))]
    loop {
        ax_hal::asm::wait_for_irqs();
    }
}

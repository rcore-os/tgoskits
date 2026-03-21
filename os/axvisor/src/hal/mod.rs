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

use std::os::arceos::{self, modules::axhal::percpu::this_cpu_id};

use arceos::modules::axhal;
use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};
use axvm::AxVMPerCpu;
use page_table_multiarch::PagingHandler;

#[cfg_attr(target_arch = "aarch64", path = "arch/aarch64/mod.rs")]
#[cfg_attr(target_arch = "x86_64", path = "arch/x86_64/mod.rs")]
#[cfg_attr(target_arch = "riscv64", path = "arch/riscv64/mod.rs")]
pub mod arch;

use crate::{hal::arch::hardware_check, vmm};

#[allow(unused)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum CacheOp {
    /// Write back to memory
    Clean,
    /// Invalidate cache
    Invalidate,
    /// Clean and invalidate
    CleanAndInvalidate,
}

pub struct AxMmHalImpl;

impl AxMmHal for AxMmHalImpl {
    fn alloc_frame() -> Option<HostPhysAddr> {
        <axhal::paging::PagingHandlerImpl as PagingHandler>::alloc_frame()
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        <axhal::paging::PagingHandlerImpl as PagingHandler>::dealloc_frame(paddr)
    }

    #[inline]
    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        <axhal::paging::PagingHandlerImpl as PagingHandler>::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: axaddrspace::HostVirtAddr) -> axaddrspace::HostPhysAddr {
        std::os::arceos::modules::axhal::mem::virt_to_phys(vaddr)
    }
}

// pub struct AxVCpuHalImpl;

// impl AxVCpuHal for AxVCpuHalImpl {
//     type MmHal = AxMmHalImpl;

//     fn irq_hanlder() {
//         axhal::irq::irq_handler(0);
//     }
// }

#[percpu::def_percpu]
static mut AXVM_PER_CPU: AxVMPerCpu = AxVMPerCpu::new_uninit();

/// Init hardware virtualization support in each core.
pub(crate) fn enable_virtualization() {
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering;

    use std::thread;

    use arceos::api::task::{AxCpuMask, ax_set_current_affinity};

    static CORES: AtomicUsize = AtomicUsize::new(0);

    info!("Enabling hardware virtualization support on all cores...");

    hardware_check();

    let cpu_count = std::os::arceos::modules::axhal::cpu_num();

    for cpu_id in 0..cpu_count {
        thread::spawn(move || {
            info!("Core {cpu_id} is initializing hardware virtualization support...");
            // Initialize cpu affinity here.
            assert!(
                ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
                "Initialize CPU affinity failed!"
            );

            info!("Enabling hardware virtualization support on core {cpu_id}");

            vmm::init_timer_percpu();

            // SAFETY: We are initializing the percpu state for the first time
            #[allow(static_mut_refs)]
            let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
            percpu
                .init(this_cpu_id())
                .expect("Failed to initialize percpu state");
            percpu
                .hardware_enable()
                .expect("Failed to enable virtualization");

            info!("Hardware virtualization support enabled on core {cpu_id}");

            let _ = CORES.fetch_add(1, Ordering::Release);
        });
    }

    info!("Waiting for all cores to enable hardware virtualization...");

    // Wait for all cores to enable virtualization.
    while CORES.load(Ordering::Acquire) != cpu_count {
        // Use `yield_now` instead of `core::hint::spin_loop` to avoid deadlock.
        thread::yield_now();
    }

    info!("All cores have enabled hardware virtualization support.");
}

mod impl_host;
mod impl_memory;
mod impl_time;
mod impl_vmm;

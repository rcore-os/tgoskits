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

use ax_hal;
use ax_page_table_multiarch::PagingHandler;
use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};

#[cfg_attr(target_arch = "aarch64", path = "arch/aarch64/mod.rs")]
#[cfg_attr(target_arch = "loongarch64", path = "arch/loongarch64/mod.rs")]
#[cfg_attr(target_arch = "x86_64", path = "arch/x86_64/mod.rs")]
#[cfg_attr(target_arch = "riscv64", path = "arch/riscv64/mod.rs")]
pub mod arch;

pub struct AxMmHalImpl;

impl AxMmHal for AxMmHalImpl {
    fn alloc_frame() -> Option<HostPhysAddr> {
        <ax_hal::paging::PagingHandlerImpl as PagingHandler>::alloc_frame()
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        <ax_hal::paging::PagingHandlerImpl as PagingHandler>::dealloc_frame(paddr)
    }

    #[inline]
    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        <ax_hal::paging::PagingHandlerImpl as PagingHandler>::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: axaddrspace::HostVirtAddr) -> axaddrspace::HostPhysAddr {
        ax_hal::mem::virt_to_phys(vaddr)
    }
}

// pub struct AxVCpuHalImpl;

// impl AxVCpuHal for AxVCpuHalImpl {
//     type MmHal = AxMmHalImpl;

//     fn irq_hanlder() {
//         ax_hal::trap::irq_handler(0);
//     }
// }

mod impl_console;
#[cfg(feature = "fs")]
mod impl_fs;
mod impl_host;
mod impl_irq;
mod impl_memory;
mod impl_sync;
mod impl_task;
mod impl_time;

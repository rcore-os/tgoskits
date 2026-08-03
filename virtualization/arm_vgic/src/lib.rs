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

//! ARM Virtual Generic Interrupt Controller (VGIC) implementation.
//!
//! This crate provides virtualization support for ARM's Generic Interrupt Controller (GIC),
//! enabling virtual machines to manage interrupts in a virtualized environment.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod devops_impl;
mod error;
pub mod host;

pub use error::{VgicError, VgicResult};

/// Virtual GIC implementation module.
pub mod vgic;
pub use vgic::Vgic;

mod consts;
mod interrupt;
// mod list_register;
mod registers;
mod vgicd;
/// Virtual timer implementation module.
pub mod vtimer;

#[cfg(feature = "vgicv3")]
/// GICv3 specific implementation module.
pub mod v3;

#[cfg(target_arch = "aarch64")]
/// Re-export arch specific APIs for VGIC to avoid doc build errors
mod api_reexp {
    #[allow(unused_imports)]
    pub use crate::host::{
        get_host_gicd_base, get_host_gicr_base, hardware_inject_virtual_interrupt, read_vgicd_iidr,
        read_vgicd_typer,
    };
}

#[allow(dead_code)]
#[cfg(not(target_arch = "aarch64"))]
mod api_reexp {
    use ax_memory_addr::{PhysAddr, pa};

    pub fn read_vgicd_iidr() -> u32 {
        0
    }

    pub fn read_vgicd_typer() -> u32 {
        0
    }

    pub fn get_host_gicd_base() -> PhysAddr {
        pa!(0)
    }

    pub fn get_host_gicr_base() -> PhysAddr {
        pa!(0)
    }

    pub fn hardware_inject_virtual_interrupt(_vector: u8) {}
}

#[cfg(all(test, feature = "vgicv3", not(target_arch = "aarch64")))]
mod test_host {
    use alloc::boxed::Box;
    use core::time::Duration;

    use ax_memory_addr::{PhysAddr, VirtAddr};

    use crate::host::ArmVgicHostIf;

    struct TestArmVgicHostIf;

    #[ax_crate_interface::impl_interface]
    impl ArmVgicHostIf for TestArmVgicHostIf {
        fn alloc_contiguous_frames(_frame_count: usize, _frame_align: usize) -> Option<PhysAddr> {
            None
        }

        fn dealloc_contiguous_frames(_start_paddr: PhysAddr, _frame_count: usize) {}

        fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
            VirtAddr::from_usize(paddr.as_usize())
        }

        fn host_cpu_num() -> usize {
            1
        }

        fn current_vcpu_id() -> usize {
            0
        }

        fn current_time_nanos() -> u64 {
            0
        }

        fn register_timer(
            _deadline: Duration,
            _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) {
        }

        fn read_vgicd_iidr() -> u32 {
            0
        }

        fn read_vgicd_typer() -> u32 {
            0
        }

        fn get_host_gicd_base() -> PhysAddr {
            PhysAddr::from_usize(0)
        }

        fn get_host_gicr_base() -> PhysAddr {
            PhysAddr::from_usize(0)
        }

        fn host_private_interrupt_enable_mask() -> u32 {
            1 << 26
        }

        fn hardware_inject_virtual_interrupt(_vector: u8) {}
    }
}

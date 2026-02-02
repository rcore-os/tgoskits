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

mod devops_impl;

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
    pub use axvisor_api::arch::{
        get_host_gicd_base, get_host_gicr_base, hardware_inject_virtual_interrupt, read_vgicd_iidr,
        read_vgicd_typer,
    };
}

#[allow(dead_code)]
#[cfg(not(target_arch = "aarch64"))]
mod api_reexp {
    use memory_addr::{pa, PhysAddr};

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

    pub fn hardware_inject_virtual_interrupt(_vector: axvisor_api::vmm::InterruptVector) {}
}

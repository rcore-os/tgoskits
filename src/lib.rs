#![cfg_attr(not(test), no_std)]

mod devops_impl;

pub mod vgic;
pub use vgic::Vgic;

mod consts;
mod interrupt;
// mod list_register;
mod registers;
mod vgicd;
pub mod vtimer;

#[cfg(feature = "vgicv3")]
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

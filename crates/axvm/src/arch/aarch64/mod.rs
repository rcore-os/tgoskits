mod ept;
mod pcpu;
mod vcpu;
mod context_frame;

pub use pcpu::AxArchPerCpuState;
use axerrno::AxResult;

/// context frame for aarch64
pub type ContextFrame = super::context_frame::Aarch64ContextFrame;

pub fn has_hardware_support(&mut self) -> AxResult {
    let id_aa64mmfr1_el1: u64;
    unsafe {
        asm!("mrs {}, ID_AA64MMFR1_EL1", out(reg) id_aa64mmfr1_el1);
    }
    let vmid_bits = (id_aa64mmfr1_el1 >> 8) & 0xF;
    Ok(vmid_bits != 0)
}

// // Following are things for the new, unified code structure. Just a stub here.
// use crate::AxVMHal;
// use axerrno::AxResult;
// use crate::mm::{GuestPhysAddr, HostPhysAddr};

// /// The architecture dependent configuration of a `AxArchVCpu`.
// #[derive(Clone, Copy, Debug, Default)]
// pub struct AxArchVCpuConfig {}

// pub struct AxArchVCpu<H: AxVMHal> {
//     _marker: core::marker::PhantomData<H>,
// }

// impl<H: AxVMHal> AxArchVCpu<H> {
//     pub fn new(_config: AxArchVCpuConfig) -> AxResult<Self> {
//         Ok(Self {
//             _marker: core::marker::PhantomData,
//         })
//     }

//     pub fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
//         unimplemented!()
//     }

//     pub fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
//         unimplemented!()
//     }

//     pub fn run(&mut self) -> AxResult<crate::vcpu::AxArchVCpuExitReason> {
//         unimplemented!()
//     }

//     pub fn bind(&mut self) -> AxResult {
//         unimplemented!()
//     }

//     pub fn unbind(&mut self) -> AxResult {
//         unimplemented!()
//     }
// }

// pub struct AxArchPerCpuState<H: AxVMHal> {
//     _marker: core::marker::PhantomData<H>,
// }

// impl<H: AxVMHal> AxArchPerCpuState<H> {
//     pub fn new(_cpu_id: usize) -> Self {
//         Self {
//             _marker: core::marker::PhantomData,
//         }
//     }

//     pub fn is_enabled(&self) -> bool {
//         unimplemented!()
//     }

//     pub fn hardware_enable(&mut self) -> AxResult<()> {
//         unimplemented!()
//     }

//     pub fn hardware_disable(&mut self) -> AxResult<()> {
//         unimplemented!()
//     }
// }
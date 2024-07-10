mod ept;
mod pcpu;
mod vcpu;
mod context_frame;
mod sync;
mod hvc;

use spin::once::Once;
use core::arch::asm;

use axhal::arch::register_exception_handler_aarch64;

pub use pcpu::PerCpu as AxArchPerCpuState;
pub use vcpu::VCpu as AxArchVCpu;
// pub use vcpu::VCpu as AxVMVcpu;
pub use vcpu::AxArchVCpuConfig;

use sync::{hvc_handler, data_abort_handler, HVC_EXCEPTION, DATA_ABORT_EXCEPTION};

use axerrno::AxResult;
pub use self::ept::NestedPageTable as A64PageTable;

/// context frame for aarch64
pub type ContextFrame = context_frame::Aarch64ContextFrame;

pub fn has_hardware_support() -> bool {
    let id_aa64mmfr1_el1: u64;
    unsafe {
        asm!("mrs {}, ID_AA64MMFR1_EL1", out(reg) id_aa64mmfr1_el1);
    }
    let vmid_bits = (id_aa64mmfr1_el1 >> 8) & 0xF;
    vmid_bits != 0
}

static INIT: Once = Once::new();

pub fn register_lower_aarch64_synchronous_handler() -> AxResult {
    INIT.call_once(|| {
        if !register_exception_handler_aarch64(HVC_EXCEPTION, hvc_handler) {
            panic!("Failed to register HVC handler");
        };
        if !register_exception_handler_aarch64(DATA_ABORT_EXCEPTION, data_abort_handler) {
            panic!("Failed to register data abort handler");
        }
    });
    return Ok(())
}

// // Following are things for the new, unified code structure. Just a stub here.
// use crate::AxVMHal;
// use axerrno::AxResult;
// use crate::mm::{GuestPhysAddr, HostPhysAddr};

// The architecture dependent configuration of a `AxArchVCpu`.
// #[derive(Clone, Copy, Debug, Default)]
// pub struct AxArchVCpuConfig {
// //need some initial configuration
// }

// pub struct AxArchVCpu<H: AxVMHal> {
//     pub vcpu: VCpu<H:AxvmHal>,
//     _marker: core::marker::PhantomData<H>,
// }

// impl<H: AxVMHal> AxArchVCpu<H> {
//     //init and
//     pub fn new(_config: AxArchVCpuConfig) -> AxResult<Self> {
//         Ok(Self {
//             _marker: core::marker::PhantomData,
//         })
//     }

//     pub fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
//         self.vcpu.set_elr(entry);
//         Ok(())
//     }

//     pub fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
//         msr!(VTTBR_EL2, ept_root);
//         Ok(())
//     }

//     // what is the function of the value:vttbr_token?
//     pub fn run(&mut self, vttbr_token: usize) -> AxResult<crate::vcpu::AxArchVCpuExitReason> {
//         //
//         self.vcpu.run(vttbr_token);
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
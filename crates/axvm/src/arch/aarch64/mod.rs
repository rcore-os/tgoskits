mod context_frame;
mod ept;
mod hvc;
mod pcpu;
mod sync;
mod vcpu;

use core::arch::asm;
use spin::once::Once;

use axhal::arch::register_exception_handler_aarch64;

pub use pcpu::PerCpu as AxArchPerCpuState;
pub use vcpu::VCpu as AxArchVCpu;
// pub use vcpu::VCpu as AxVMVcpu;
pub use vcpu::AxArchVCpuConfig;

use sync::{data_abort_handler, hvc_handler, DATA_ABORT_EXCEPTION, HVC_EXCEPTION};

pub use self::ept::NestedPageTable as A64PageTable;
use axerrno::AxResult;

/// context frame for aarch64
pub type ContextFrame = context_frame::Aarch64ContextFrame;

pub fn has_hardware_support() -> bool {
    true
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
    return Ok(());
}

mod ept;
mod pcpu;
mod vcpu;
mod context_frame;

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
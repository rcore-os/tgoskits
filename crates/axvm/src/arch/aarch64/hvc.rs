
use aarch64_cpu::{asm, asm::barrier, registers::*};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::vcpu::VmCpuRegisters;

/// HVC SYS type
pub const HVC_SYS: usize = 0;
/// HVC SYS event
pub const HVC_SYS_BOOT: usize = 0;

#[repr(C)]
/// HVC default message
pub struct HvcDefaultMsg {
    /// hvc type id
    pub fid: usize,
    /// hvc event
    pub event: usize,
}

#[inline(never)]
/// Handles the HVC guest event.
///
/// # Arguments
///
/// * `hvc_type` - The type of HVC event.
/// * `event` - The event identifier.
/// * `x0` - The value of register x0.
/// * `x1` - The value of register x1.
/// * `_x2` - The value of register x2 (unused).
/// * `_x3` - The value of register x3 (unused).
/// * `_x4` - The value of register x4 (unused).
/// * `_x5` - The value of register x5 (unused).
/// * `_x6` - The value of register x6 (unused).
///
/// # Returns
///
/// Returns the result of the HVC guest handler.
/// If the HVC type is unknown, an error is returned.
pub fn hvc_guest_handler(
    hvc_type: usize,
    event: usize,
    x0: usize,
    x1: usize,
    _x2: usize,
    _x3: usize,
    _x4: usize,
    _x5: usize,
    _x6: usize,
) -> Result<usize, ()> {
    match hvc_type {
        HVC_SYS => hvc_sys_handler(event, x0, x1),
        _ => {
            info!("hvc_guest_handler: unknown hvc type {} event {}", hvc_type, event);
            Err(())
        }
    }
}

/// Runs the guest by trapping to EL2.
///
/// # Arguments
///
/// * `token` - The vttbr_el2 value.
/// * `regs_addr` - The address of the registers.
///
/// # Returns
///
/// The result of the hvc_call function.
pub fn run_guest_by_trap2el2(token: usize, regs_addr: usize) -> usize {
    // mode is in x7. hvc_type: HVC_SYS; event: HVC_SYS_BOOT
    hvc_call(token, regs_addr, 0, 0, 0, 0, 0, 0)
}

#[inline(never)]
fn hvc_sys_handler(event: usize, _root_paddr: usize, _vm_ctx_addr: usize) -> Result<usize, ()> {
    match event {
        HVC_SYS_BOOT => {
            // init_hv(root_paddr, vm_ctx_addr);
            // panic!("abandon area");
            Ok(0)
        }

        _ => Err(()),
    }
}

#[inline(never)]
fn hvc_call(
    x0: usize, 
    x1: usize, 
    x2: usize, 
    x3: usize, 
    x4: usize,
    x5: usize,
    x6: usize,
    x7: usize,
) -> usize {
    let r0;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "hvc #0",
            inout("x0") x0 => r0,
            inout("x1") x1 => _,
            inout("x2") x2 => _,
            inout("x3") x3 => _,
            inout("x4") x4 => _,
            inout("x5") x5 => _,
            inout("x6") x6 => _,
            inout("x7") x7 => _,
            options(nomem, nostack)
        );
    }
    r0
}

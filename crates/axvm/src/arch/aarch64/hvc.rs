
use aarch64_cpu::{asm, asm::barrier, registers::*};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::arch::vcpu::VmCpuRegisters;

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
/* 
unsafe fn cache_invalidate(cache_level: usize) {
    core::arch::asm!(
        r#"
        msr csselr_el1, {0}
        mrs x4, ccsidr_el1 // read cache size id.
        and x0, x4, #0x7
        add x0, x0, #0x4 // x0 = cache line size.
        ldr x3, =0x7fff
        and x2, x3, x4, lsr #13 // x2 = cache set number – 1.
        ldr x3, =0x3ff
        and x3, x3, x4, lsr #3 // x3 = cache associativity number – 1.
        clz w4, w3 // x4 = way position in the cisw instruction.
        mov x5, #0 // x5 = way counter way_loop.
    // way_loop:
    1:
        mov x6, #0 // x6 = set counter set_loop.
    // set_loop:
    2:
        lsl x7, x5, x4
        orr x7, {0}, x7 // set way.
        lsl x8, x6, x0
        orr x7, x7, x8 // set set.
        dc csw, x7 // clean and invalidate cache line.
        add x6, x6, #1 // increment set counter.
        cmp x6, x2 // last set reached yet?
        ble 2b // if not, iterate set_loop,
        add x5, x5, #1 // else, next way.
        cmp x5, x3 // last way reached yet?
        ble 1b // if not, iterate way_loop
        "#,
        in(reg) cache_level,
        options(nostack)
    );
}

#[inline(never)]
/// hvc handler for initial hv
/// x0: root_paddr, x1: vm regs context addr
fn init_hv(root_paddr: usize, vm_ctx_addr: usize) {
    // cptr_el2: Condtrols trapping to EL2 for accesses to the CPACR, Trace functionality 
    //           an registers associated with floating-point and Advanced SIMD execution.

    // ldr x2, =(0x30c51835)  // do not set sctlr_el2 as this value, some fields have no use.
    unsafe {
        core::arch::asm!("
            mov x3, xzr           // Trap nothing from EL1 to El2.
            msr cptr_el2, x3"
        );
    }
    // dcache_clean_flush(0x70000000, 0xf000000);
    let regs: &VmCpuRegisters = unsafe{core::mem::transmute(vm_ctx_addr)};
    // set vm system related register
    msr!(VTTBR_EL2, root_paddr);
    regs.vm_system_regs.ext_regs_restore();

    unsafe {
        cache_invalidate(0<<1);
        cache_invalidate(1<<1);
        core::arch::asm!("
            ic  iallu
            tlbi	alle2
            tlbi	alle1         // Flush tlb
            dsb	nsh
            isb"
        );
    }   
   
}
*/
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

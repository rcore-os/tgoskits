use super::hvc::{hvc_guest_handler, HVC_SYS, HVC_SYS_BOOT};
use super::vcpu::VmCpuRegisters;
use super::ContextFrame;

use axhal::arch::exception_utils::*;
use axhal::arch::TrapFrame;

const HVC_RETURN_REG: usize = 0;
const SMC_RETURN_REG: usize = 0;

pub const HVC_EXCEPTION: usize = 0x16;
pub const DATA_ABORT_EXCEPTION: usize = 0x24;

pub fn data_abort_handler(ctx: &mut TrapFrame) {
    /*
    let emu_ctx = EmuContext {
        address: exception_fault_addr(),
        width: exception_data_abort_access_width(),
        write: exception_data_abort_access_is_write(),
        sign_ext: exception_data_abort_access_is_sign_ext(),
        reg: exception_data_abort_access_reg(),
        reg_width: exception_data_abort_access_reg_width(),
    };
    */
    let context_frame: &mut ContextFrame =
        unsafe { &mut *(ctx as *mut TrapFrame as *mut ContextFrame) };
    debug!(
        "data fault addr 0x{:x}, esr: 0x{:x}",
        exception_fault_addr(),
        exception_esr()
    );
    let elr = context_frame.exception_pc();

    if !exception_data_abort_handleable() {
        panic!(
            "Data abort not handleable 0x{:x}, esr 0x{:x}",
            exception_fault_addr(),
            exception_esr()
        );
    }

    if !exception_data_abort_is_translate_fault() {
        // No migrate need
        panic!(
            "Data abort is not translate fault 0x{:x}\n ctx: {}",
            exception_fault_addr(),
            context_frame
        );
    }
    /*
    if !emu_handler(&emu_ctx) {
        active_vm().unwrap().show_pagetable(emu_ctx.address);
        info!(
            "write {}, width {}, reg width {}, addr {:x}, iss {:x}, reg idx {}, reg val 0x{:x}, esr 0x{:x}",
            exception_data_abort_access_is_write(),
            emu_ctx.width,
            emu_ctx.reg_width,
            emu_ctx.address,
            exception_iss(),
            emu_ctx.reg,
            ctx.get_gpr(emu_ctx.reg),
            exception_esr()
        );
        panic!(
            "data_abort_handler: Failed to handler emul device request, ipa 0x{:x} elr 0x{:x}",
            emu_ctx.address, elr
        );
    }
    */
    let val = elr + exception_next_instruction_step();
    context_frame.set_exception_pc(val);
}

pub fn hvc_handler(ctx: &mut TrapFrame) {
    let context_frame: &mut ContextFrame =
        unsafe { &mut *(ctx as *mut TrapFrame as *mut ContextFrame) };
    let x0 = context_frame.gpr(0);
    let x1 = context_frame.gpr(1);
    let x2 = context_frame.gpr(2);
    let x3 = context_frame.gpr(3);
    let x4 = context_frame.gpr(4);
    let x5 = context_frame.gpr(5);
    let x6 = context_frame.gpr(6);
    let mode = context_frame.gpr(7);
    debug!("hvc_handler: mode:{}", mode);
    let hvc_type = (mode >> 8) & 0xff;
    let event = mode & 0xff;

    match hvc_guest_handler(hvc_type, event, x0, x1, x2, x3, x4, x5, x6) {
        Ok(val) => {
            context_frame.set_gpr(HVC_RETURN_REG, val);
        }
        Err(_) => {
            warn!(
                "Failed to handle hvc request fid 0x{:x} event 0x{:x}",
                hvc_type, event
            );
            context_frame.set_gpr(HVC_RETURN_REG, usize::MAX);
        }
    }
}

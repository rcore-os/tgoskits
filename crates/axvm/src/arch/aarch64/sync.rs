use axerrno::{AxError, AxResult};
use axvcpu::{AccessWidth, AxArchVCpuExitReason};

use super::exception_utils::*;
use super::hvc::{hvc_guest_handler, HVC_SYS, HVC_SYS_BOOT};
use super::ContextFrame;

const HVC_RETURN_REG: usize = 0;
const SMC_RETURN_REG: usize = 0;

pub const HVC_EXCEPTION: usize = 0x16;
pub const DATA_ABORT_EXCEPTION: usize = 0x24;

pub fn data_abort_handler(context_frame: &mut ContextFrame) -> AxResult<AxArchVCpuExitReason> {
    debug!(
        "data fault addr 0x{:x}, esr: 0x{:x}",
        exception_fault_addr(),
        exception_esr()
    );

    let address = exception_fault_addr();
    let width = exception_data_abort_access_width();
    let is_write = exception_data_abort_access_is_write();
    // let sign_ext = exception_data_abort_access_is_sign_ext();
    let reg = exception_data_abort_access_reg();
    // let reg_width = exception_data_abort_access_reg_width();

    let elr = context_frame.exception_pc();
    let val = elr + exception_next_instruction_step();
    context_frame.set_exception_pc(val);

    let access_width = match AccessWidth::try_from(width) {
        Ok(width) => width,
        Err(_) => return Err(AxError::InvalidInput),
    };

    if is_write {
        return Ok(AxArchVCpuExitReason::MmioWrite {
            addr: address,
            width: access_width,
            data: context_frame.gpr(reg) as u64,
        });
    }
    Ok(AxArchVCpuExitReason::MmioRead {
        addr: address,
        width: access_width,
    })
}

pub fn hvc_handler(context_frame: &mut ContextFrame) -> AxResult<AxArchVCpuExitReason> {
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

    Ok(AxArchVCpuExitReason::Nothing)
}

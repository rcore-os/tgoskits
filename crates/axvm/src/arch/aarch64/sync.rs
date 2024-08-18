use axerrno::{AxError, AxResult};
use axvcpu::{AccessWidth, AxVCpuExitReason};

use super::exception_utils::*;
use super::ContextFrame;

const HVC_RETURN_REG: usize = 0;

pub fn data_abort_handler(context_frame: &mut ContextFrame) -> AxResult<AxVCpuExitReason> {
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
        return Ok(AxVCpuExitReason::MmioWrite {
            addr: address,
            width: access_width,
            data: context_frame.gpr(reg) as u64,
        });
    }
    Ok(AxVCpuExitReason::MmioRead {
        addr: address,
        width: access_width,
    })
}

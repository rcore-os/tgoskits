use core::arch::global_asm;

use riscv_decode::Instruction;

use crate::GuestPhysAddr;
use axerrno::{AxError, AxResult};
global_asm!(include_str!("mem_extable.S"));

extern "C" {
    fn _copy_to_guest(dest_gpa: usize, src: *const u8, len: usize) -> usize;
    fn _copy_from_guest(dest: *mut u8, src_gpa: usize, len: usize) -> usize;
    fn _fetch_guest_instruction(gva: usize, raw_inst: *mut u32) -> isize;
}

/// Represents the activate VM address space. Used to directly access a guest's memory.
pub fn fetch_guest_instruction(pc: GuestPhysAddr) -> AxResult<u32> {
    let mut raw_inst = 0u32;
    // Safety: _fetch_guest_instruction internally detects and handles an invalid guest virtual
    // address in `pc' and will only write up to 4 bytes to `raw_inst`.
    let ret = unsafe { _fetch_guest_instruction(pc, &mut raw_inst) };
    if ret < 0 {
        return Err(AxError::BadAddress);
    }
    // let inst = riscv_decode::decode(raw_inst).map_err(|_| HyperError::DecodeError)?;
    Ok(raw_inst)
}

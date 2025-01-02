//! Detect instruction sets (ISA extensions) by trap-and-return procedure
//!
//! First, it disables all S-level interrupts. Remaining traps in RISC-V core
//! are all exceptions.
//! Then, it filters out illegal instruction from exceptions.
//! ref: <https://github.com/luojia65/zihai/blob/main/zihai/src/detect.rs>

use core::arch::{asm, naked_asm};
use riscv::register::{
    scause::{Exception, Scause, Trap},
    sstatus,
    stvec::{self, Stvec, TrapMode},
};

/// Detect if hypervisor extension exists on current hart environment
///
/// This function tries to read hgatp and returns false if the read operation failed.
pub fn detect_h_extension() -> bool {
    // run detection by trap on csrr instruction.
    let ans = with_detect_trap(0, || unsafe {
        asm!("csrr  {}, 0x680", out(reg) _, options(nomem, nostack)); // 0x680 => hgatp
    });
    // return the answer from output flag. 0 => success, 2 => failed, illegal instruction
    ans != 2
}

// Tries to execute all instructions defined in clojure `f`.
// If resulted in an exception, this function returns its exception id.
//
// This function is useful to detect if an instruction exists on current environment.
#[inline]
fn with_detect_trap(param: usize, f: impl FnOnce()) -> usize {
    // disable interrupts and handle exceptions only
    let (sie, stvec, tp) = unsafe { init_detect_trap(param) };
    // run detection inner
    f();
    // restore trap handler and enable interrupts
    unsafe { restore_detect_trap(sie, stvec, tp) }
}

// rust trap handler for detect exceptions
extern "C" fn rust_detect_trap(trap_frame: &mut TrapFrame) {
    // store returned exception id value into tp register
    // specially: illegal instruction => 2
    trap_frame.tp = trap_frame.scause.bits();
    // if illegal instruction, skip current instruction
    match trap_frame.scause.cause() {
        Trap::Exception(Exception::IllegalInstruction) => {
            let mut insn_bits = riscv_illegal_insn_bits((trap_frame.stval & 0xFFFF) as u16);
            if insn_bits == 0 {
                let insn_half = unsafe { *(trap_frame.sepc as *const u16) };
                insn_bits = riscv_illegal_insn_bits(insn_half);
            }
            // skip current instruction
            trap_frame.sepc = trap_frame.sepc.wrapping_add(insn_bits);
        }
        Trap::Exception(_) => unreachable!(), // FIXME: unexpected instruction errors
        Trap::Interrupt(_) => unreachable!(), // filtered out for sie == false
    }
}

// Gets risc-v instruction bits from illegal instruction stval value, or 0 if unknown
#[inline]
fn riscv_illegal_insn_bits(insn: u16) -> usize {
    if insn == 0 {
        return 0; // stval[0..16] == 0, unknown
    }
    if insn & 0b11 != 0b11 {
        return 2; // 16-bit
    }
    if insn & 0b11100 != 0b11100 {
        return 4; // 32-bit
    }
    // FIXME: add >= 48-bit instructions in the future if we need to detect such instrucions
    // >= 48-bit, unknown from this function by now
    0
}

// Initialize environment for trap detection and filter in exception only
#[inline]
unsafe fn init_detect_trap(param: usize) -> (bool, Stvec, usize) {
    // clear SIE to handle exception only
    let stored_sie = sstatus::read().sie();
    unsafe {
        sstatus::clear_sie();
    }
    // use detect trap handler to handle exceptions
    let stored_stvec = stvec::read();
    let mut trap_addr = on_detect_trap as usize;
    if trap_addr & 0b1 != 0 {
        trap_addr += 0b1;
    }
    let stored_tp: usize;
    unsafe {
        stvec::write(trap_addr, TrapMode::Direct);
        // store tp register. tp will be used to load parameter and store return value
        asm!("mv  {}, tp", "mv  tp, {}", out(reg) stored_tp, in(reg) param, options(nomem, nostack));
    }
    // returns preserved previous hardware states
    (stored_sie, stored_stvec, stored_tp)
}

// Restore previous hardware states before trap detection
#[inline]
unsafe fn restore_detect_trap(sie: bool, stvec: Stvec, tp: usize) -> usize {
    // read the return value from tp register, and restore tp value
    let ans: usize;
    unsafe {
        asm!("mv  {}, tp", "mv  tp, {}", out(reg) ans, in(reg) tp, options(nomem, nostack));
        // restore trap vector settings
        asm!("csrw  stvec, {}", in(reg) stvec.bits(), options(nomem, nostack));
        // enable interrupts
        if sie {
            sstatus::set_sie();
        };
    }
    ans
}

// Trap frame for instruction exception detection
#[repr(C)]
struct TrapFrame {
    ra: usize,
    tp: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
    sstatus: usize,
    sepc: usize,
    scause: Scause,
    stval: usize,
}

// Assembly trap handler for instruction detection.
//
// This trap handler shares the same stack from its prospective caller,
// the caller must ensure it has abundant stack size for a trap handler.
//
// This function should not be used in conventional trap handling,
// as it does not preserve a special trap stack, and it's designed to
// handle exceptions only rather than interrupts.
#[naked]
unsafe extern "C" fn on_detect_trap() -> ! {
    unsafe {
        naked_asm!(
            ".p2align 2",
            "addi   sp, sp, -8*21",
            "sd     ra, 0*8(sp)",
            "sd     tp, 1*8(sp)",
            "sd     a0, 2*8(sp)",
            "sd     a1, 3*8(sp)",
            "sd     a2, 4*8(sp)",
            "sd     a3, 5*8(sp)",
            "sd     a4, 6*8(sp)",
            "sd     a5, 7*8(sp)",
            "sd     a6, 8*8(sp)",
            "sd     a7, 9*8(sp)",
            "sd     t0, 10*8(sp)",
            "sd     t1, 11*8(sp)",
            "sd     t2, 12*8(sp)",
            "sd     t3, 13*8(sp)",
            "sd     t4, 14*8(sp)",
            "sd     t5, 15*8(sp)",
            "sd     t6, 16*8(sp)",
            "csrr   t0, sstatus",
            "sd     t0, 17*8(sp)",
            "csrr   t1, sepc",
            "sd     t1, 18*8(sp)",
            "csrr   t2, scause",
            "sd     t2, 19*8(sp)",
            "csrr   t3, stval",
            "sd     t3, 20*8(sp)",
            "mv     a0, sp",
            "call   {rust_detect_trap}",
            "ld     t0, 17*8(sp)",
            "csrw   sstatus, t0",
            "ld     t1, 18*8(sp)",
            "csrw   sepc, t1",
            "ld     t2, 19*8(sp)",
            "csrw   scause, t2",
            "ld     t3, 20*8(sp)",
            "csrw   stval, t3",
            "ld     ra, 0*8(sp)",
            "ld     tp, 1*8(sp)",
            "ld     a0, 2*8(sp)",
            "ld     a1, 3*8(sp)",
            "ld     a2, 4*8(sp)",
            "ld     a3, 5*8(sp)",
            "ld     a4, 6*8(sp)",
            "ld     a5, 7*8(sp)",
            "ld     a6, 8*8(sp)",
            "ld     a7, 9*8(sp)",
            "ld     t0, 10*8(sp)",
            "ld     t1, 11*8(sp)",
            "ld     t2, 12*8(sp)",
            "ld     t3, 13*8(sp)",
            "ld     t4, 14*8(sp)",
            "ld     t5, 15*8(sp)",
            "ld     t6, 16*8(sp)",
            "addi   sp, sp, 8*21",
            "sret",
            rust_detect_trap = sym rust_detect_trap,
        )
    }
}

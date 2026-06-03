use core::{arch::global_asm, mem::offset_of};

use crate::irq;

const SCAUSE_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);
const SCAUSE_SUPERVISOR_TIMER: usize = 5;

#[repr(C)]
struct TrapFrame {
    ra: usize,
    gp: usize,
    tp: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    s0: usize,
    s1: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    s2: usize,
    s3: usize,
    s4: usize,
    s5: usize,
    s6: usize,
    s7: usize,
    s8: usize,
    s9: usize,
    s10: usize,
    s11: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
    sp: usize,
    sstatus: usize,
    sepc: usize,
    scause: usize,
    stval: usize,
}

pub fn setup() {
    let addr = trap_addr() & !0x3;
    unsafe {
        core::arch::asm!(
            "csrw stvec, {addr}",
            addr = in(reg) addr,
            options(nostack, preserves_flags)
        );
    }
}

pub fn trap_addr() -> usize {
    unsafe extern "C" {
        fn __riscv64_trap_entry();
    }
    __riscv64_trap_entry as *const () as usize
}

#[unsafe(no_mangle)]
extern "C" fn __riscv64_handle_trap(tf: &mut TrapFrame) {
    let scause = tf.scause;
    if (scause & SCAUSE_INTERRUPT_BIT) != 0 {
        let cause = scause & !SCAUSE_INTERRUPT_BIT;
        if cause == SCAUSE_SUPERVISOR_TIMER {
            irq::handle_irq(irq::systimer_irq());
            return;
        }
    }

    panic!(
        "Unhandled RISC-V trap: scause={:#x}, sepc={:#x}, stval={:#x}",
        tf.scause, tf.sepc, tf.stval
    );
}

const TF_RA: usize = offset_of!(TrapFrame, ra);
const TF_GP: usize = offset_of!(TrapFrame, gp);
const TF_TP: usize = offset_of!(TrapFrame, tp);
const TF_T0: usize = offset_of!(TrapFrame, t0);
const TF_T1: usize = offset_of!(TrapFrame, t1);
const TF_T2: usize = offset_of!(TrapFrame, t2);
const TF_S0: usize = offset_of!(TrapFrame, s0);
const TF_S1: usize = offset_of!(TrapFrame, s1);
const TF_A0: usize = offset_of!(TrapFrame, a0);
const TF_A1: usize = offset_of!(TrapFrame, a1);
const TF_A2: usize = offset_of!(TrapFrame, a2);
const TF_A3: usize = offset_of!(TrapFrame, a3);
const TF_A4: usize = offset_of!(TrapFrame, a4);
const TF_A5: usize = offset_of!(TrapFrame, a5);
const TF_A6: usize = offset_of!(TrapFrame, a6);
const TF_A7: usize = offset_of!(TrapFrame, a7);
const TF_S2: usize = offset_of!(TrapFrame, s2);
const TF_S3: usize = offset_of!(TrapFrame, s3);
const TF_S4: usize = offset_of!(TrapFrame, s4);
const TF_S5: usize = offset_of!(TrapFrame, s5);
const TF_S6: usize = offset_of!(TrapFrame, s6);
const TF_S7: usize = offset_of!(TrapFrame, s7);
const TF_S8: usize = offset_of!(TrapFrame, s8);
const TF_S9: usize = offset_of!(TrapFrame, s9);
const TF_S10: usize = offset_of!(TrapFrame, s10);
const TF_S11: usize = offset_of!(TrapFrame, s11);
const TF_T3: usize = offset_of!(TrapFrame, t3);
const TF_T4: usize = offset_of!(TrapFrame, t4);
const TF_T5: usize = offset_of!(TrapFrame, t5);
const TF_T6: usize = offset_of!(TrapFrame, t6);
const TF_SP: usize = offset_of!(TrapFrame, sp);
const TF_SSTATUS: usize = offset_of!(TrapFrame, sstatus);
const TF_SEPC: usize = offset_of!(TrapFrame, sepc);
const TF_SCAUSE: usize = offset_of!(TrapFrame, scause);
const TF_STVAL: usize = offset_of!(TrapFrame, stval);
const TF_SIZE: usize = core::mem::size_of::<TrapFrame>();
const TF_ALLOC_SIZE: usize = (TF_SIZE + 15) & !15;

global_asm!(
    ".section .text.trap, \"ax\"",
    ".p2align 2",
    ".globl __riscv64_trap_entry",
    "__riscv64_trap_entry:",
    "addi sp, sp, -{tf_alloc_size}",
    "sd ra, {tf_ra}(sp)",
    "sd gp, {tf_gp}(sp)",
    "sd tp, {tf_tp}(sp)",
    "sd t0, {tf_t0}(sp)",
    "sd t1, {tf_t1}(sp)",
    "sd t2, {tf_t2}(sp)",
    "sd s0, {tf_s0}(sp)",
    "sd s1, {tf_s1}(sp)",
    "sd a0, {tf_a0}(sp)",
    "sd a1, {tf_a1}(sp)",
    "sd a2, {tf_a2}(sp)",
    "sd a3, {tf_a3}(sp)",
    "sd a4, {tf_a4}(sp)",
    "sd a5, {tf_a5}(sp)",
    "sd a6, {tf_a6}(sp)",
    "sd a7, {tf_a7}(sp)",
    "sd s2, {tf_s2}(sp)",
    "sd s3, {tf_s3}(sp)",
    "sd s4, {tf_s4}(sp)",
    "sd s5, {tf_s5}(sp)",
    "sd s6, {tf_s6}(sp)",
    "sd s7, {tf_s7}(sp)",
    "sd s8, {tf_s8}(sp)",
    "sd s9, {tf_s9}(sp)",
    "sd s10, {tf_s10}(sp)",
    "sd s11, {tf_s11}(sp)",
    "sd t3, {tf_t3}(sp)",
    "sd t4, {tf_t4}(sp)",
    "sd t5, {tf_t5}(sp)",
    "sd t6, {tf_t6}(sp)",
    "addi t0, sp, {tf_alloc_size}",
    "sd t0, {tf_sp}(sp)",
    "csrr t0, sstatus",
    "sd t0, {tf_sstatus}(sp)",
    "csrr t0, sepc",
    "sd t0, {tf_sepc}(sp)",
    "csrr t0, scause",
    "sd t0, {tf_scause}(sp)",
    "csrr t0, stval",
    "sd t0, {tf_stval}(sp)",
    "mv a0, sp",
    "call {trap_handler}",
    "ld t0, {tf_sepc}(sp)",
    "csrw sepc, t0",
    "ld t0, {tf_sstatus}(sp)",
    "csrw sstatus, t0",
    "ld ra, {tf_ra}(sp)",
    "ld gp, {tf_gp}(sp)",
    "ld tp, {tf_tp}(sp)",
    "ld t0, {tf_t0}(sp)",
    "ld t1, {tf_t1}(sp)",
    "ld t2, {tf_t2}(sp)",
    "ld s0, {tf_s0}(sp)",
    "ld s1, {tf_s1}(sp)",
    "ld a0, {tf_a0}(sp)",
    "ld a1, {tf_a1}(sp)",
    "ld a2, {tf_a2}(sp)",
    "ld a3, {tf_a3}(sp)",
    "ld a4, {tf_a4}(sp)",
    "ld a5, {tf_a5}(sp)",
    "ld a6, {tf_a6}(sp)",
    "ld a7, {tf_a7}(sp)",
    "ld s2, {tf_s2}(sp)",
    "ld s3, {tf_s3}(sp)",
    "ld s4, {tf_s4}(sp)",
    "ld s5, {tf_s5}(sp)",
    "ld s6, {tf_s6}(sp)",
    "ld s7, {tf_s7}(sp)",
    "ld s8, {tf_s8}(sp)",
    "ld s9, {tf_s9}(sp)",
    "ld s10, {tf_s10}(sp)",
    "ld s11, {tf_s11}(sp)",
    "ld t3, {tf_t3}(sp)",
    "ld t4, {tf_t4}(sp)",
    "ld t5, {tf_t5}(sp)",
    "ld t6, {tf_t6}(sp)",
    "addi sp, sp, {tf_alloc_size}",
    "sret",
    trap_handler = sym __riscv64_handle_trap,
    tf_ra = const TF_RA,
    tf_gp = const TF_GP,
    tf_tp = const TF_TP,
    tf_t0 = const TF_T0,
    tf_t1 = const TF_T1,
    tf_t2 = const TF_T2,
    tf_s0 = const TF_S0,
    tf_s1 = const TF_S1,
    tf_a0 = const TF_A0,
    tf_a1 = const TF_A1,
    tf_a2 = const TF_A2,
    tf_a3 = const TF_A3,
    tf_a4 = const TF_A4,
    tf_a5 = const TF_A5,
    tf_a6 = const TF_A6,
    tf_a7 = const TF_A7,
    tf_s2 = const TF_S2,
    tf_s3 = const TF_S3,
    tf_s4 = const TF_S4,
    tf_s5 = const TF_S5,
    tf_s6 = const TF_S6,
    tf_s7 = const TF_S7,
    tf_s8 = const TF_S8,
    tf_s9 = const TF_S9,
    tf_s10 = const TF_S10,
    tf_s11 = const TF_S11,
    tf_t3 = const TF_T3,
    tf_t4 = const TF_T4,
    tf_t5 = const TF_T5,
    tf_t6 = const TF_T6,
    tf_sp = const TF_SP,
    tf_sstatus = const TF_SSTATUS,
    tf_sepc = const TF_SEPC,
    tf_scause = const TF_SCAUSE,
    tf_stval = const TF_STVAL,
    tf_alloc_size = const TF_ALLOC_SIZE,
);

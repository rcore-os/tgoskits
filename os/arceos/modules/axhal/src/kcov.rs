//! Architecture-specific KCOV trampolines.
//!
//! These are the `__sanitizer_cov_trace_pc` entry points that the compiler
//! injects into every instrumented basic block. They save/restore the
//! recursion guard and call the implementation in the kernel crate.

#![cfg(feature = "starry-kcov")]

use core::arch::naked_asm;

/// Recursion guard: nonzero while inside `kcov_trace_pc_impl`.
/// Set/cleared by the trampoline asm before/after calling the implementation.
static mut IN_KCOV_TRACE: u8 = 0;

// Provided by the kernel crate (`starry_kernel::kcov`).
unsafe extern "C" {
    fn kcov_trace_pc_impl(pc: u64);
}

// ---------------------------------------------------------------------------
// x86_64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
/// # Safety
///
/// Called by the compiler as a coverage instrumentation hook. The standard
/// x86_64 System V C ABI applies: the return address is on the stack.
/// Must only be called by instrumented code when KCOV is enabled.
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    naked_asm!(
        "cmp byte ptr [rip + {guard}], 0",
        "jne 1f",
        "mov byte ptr [rip + {guard}], 1",
        "mov rdi, [rsp]",
        "call {impl}",
        "mov byte ptr [rip + {guard}], 0",
        "1:",
        "ret",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

// ---------------------------------------------------------------------------
// aarch64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
/// # Safety
///
/// Called by the compiler as a coverage instrumentation hook. The standard
/// AAPCS64 ABI applies: `x30` holds the return address. Must only be called
/// by instrumented code when KCOV is enabled.
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    naked_asm!(
        "adrp x16, {guard}",
        "ldrb w17, [x16, #:lo12:{guard}]",
        "cbnz w17, 1f",
        "mov w17, #1",
        "strb w17, [x16, #:lo12:{guard}]",
        "str x30, [sp, #-16]!",
        "mov x0, x30",
        "bl {impl}",
        "ldr x30, [sp], #16",
        "adrp x16, {guard}",
        "strb wzr, [x16, #:lo12:{guard}]",
        "1:",
        "ret",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

// ---------------------------------------------------------------------------
// riscv64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
/// # Safety
///
/// Called by the compiler as a coverage instrumentation hook. The standard
/// RISC-V calling convention applies: `ra` holds the return address. Must
/// only be called by instrumented code when KCOV is enabled.
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    naked_asm!(
        "la t0, {guard}",
        "lb t1, 0(t0)",
        "bnez t1, 1f",
        "li t1, 1",
        "sb t1, 0(t0)",
        "addi sp, sp, -16",
        "sd ra, 0(sp)",
        "mv a0, ra",
        "call {impl}",
        "ld ra, 0(sp)",
        "addi sp, sp, 16",
        "la t0, {guard}",
        "sb zero, 0(t0)",
        "1:",
        "ret",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

// ---------------------------------------------------------------------------
// loongarch64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "loongarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
/// # Safety
///
/// Called by the compiler as a coverage instrumentation hook. The standard
/// LoongArch calling convention applies: `ra` holds the return address. Must
/// only be called by instrumented code when KCOV is enabled.
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    naked_asm!(
        "la.local $t0, {guard}",
        "ld.b $t1, $t0, 0",
        "bnez $t1, 1f",
        "ori $t1, $zero, 1",
        "st.b $t1, $t0, 0",
        "addi.d $sp, $sp, -16",
        "st.d $ra, $sp, 0",
        "ori $a0, $ra, 0",
        "bl {impl}",
        "ld.d $ra, $sp, 0",
        "addi.d $sp, $sp, 16",
        "la.local $t0, {guard}",
        "st.b $zero, $t0, 0",
        "1:",
        "jirl $zero, $ra, 0",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

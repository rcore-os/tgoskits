//! Link-time binding of CPU-owned entry state to this runtime's area layout.

use core::mem::{align_of, size_of};

use ax_cpu::registers::CpuEntryState;

use crate::{CPU_AREA_ARCH_STATE_OFFSET, CPU_AREA_ARCH_STATE_SIZE};

const _: () = {
    assert!(size_of::<CpuEntryState>() <= CPU_AREA_ARCH_STATE_SIZE);
    assert!(CPU_AREA_ARCH_STATE_OFFSET.is_multiple_of(align_of::<CpuEntryState>()));
    assert!(CPU_AREA_ARCH_STATE_OFFSET + size_of::<CpuEntryState>() <= 0x800);
};

// Absolute hidden symbols bind offsets, not addresses. They require no runtime
// relocation or initialization and cannot be interposed by another image.
core::arch::global_asm!(
    ".global __AX_CPU_AREA_ARCH_STATE_OFFSET",
    ".hidden __AX_CPU_AREA_ARCH_STATE_OFFSET",
    ".set __AX_CPU_AREA_ARCH_STATE_OFFSET, {offset}",
    offset = const CPU_AREA_ARCH_STATE_OFFSET,
);

#[cfg(target_arch = "riscv64")]
const _: () = {
    use ax_cpu::registers::TaskEntryState;

    use crate::{
        EXECUTION_CONTEXT_ARCH_STATE_OFFSET, EXECUTION_CONTEXT_ARCH_STATE_SIZE,
        EXECUTION_CONTEXT_CPU_BASE_OFFSET,
    };

    assert!(size_of::<TaskEntryState>() <= EXECUTION_CONTEXT_ARCH_STATE_SIZE);
    assert!(EXECUTION_CONTEXT_ARCH_STATE_OFFSET.is_multiple_of(align_of::<TaskEntryState>()));
    // RISC-V entry uses signed twelve-bit load/store immediates. Keeping the
    // entire reserve below 2048 prevents %lo from silently wrapping negative.
    assert!(EXECUTION_CONTEXT_ARCH_STATE_OFFSET + size_of::<TaskEntryState>() <= 0x800);
    assert!(EXECUTION_CONTEXT_CPU_BASE_OFFSET + size_of::<usize>() <= 0x800);
};

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    ".global __AX_CPU_TASK_ARCH_STATE_OFFSET",
    ".hidden __AX_CPU_TASK_ARCH_STATE_OFFSET",
    ".set __AX_CPU_TASK_ARCH_STATE_OFFSET, {state}",
    ".global __AX_CPU_TASK_CPU_BASE_OFFSET",
    ".hidden __AX_CPU_TASK_CPU_BASE_OFFSET",
    ".set __AX_CPU_TASK_CPU_BASE_OFFSET, {cpu}",
    state = const crate::EXECUTION_CONTEXT_ARCH_STATE_OFFSET,
    cpu = const crate::EXECUTION_CONTEXT_CPU_BASE_OFFSET,
);

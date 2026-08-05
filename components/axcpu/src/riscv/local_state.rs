//! RISC-V trap state embedded in the architecture-neutral CPU-local reserves.

use core::mem::{offset_of, size_of};

#[cfg(not(feature = "tls"))]
use cpu_local::CURRENT_THREAD_ARCH_STATE_OFFSET;
use cpu_local::{
    CPU_AREA_ARCH_STATE_OFFSET, CPU_AREA_ARCH_STATE_SIZE, CURRENT_THREAD_ARCH_STATE_SIZE,
};

/// CPU-owned state used by user/kernel trap stack handoff.
#[repr(C)]
struct CpuTrapState {
    kernel_stack_pointer: usize,
    user_trap_frame: usize,
    entry_scratch0: usize,
    entry_scratch1: usize,
}

/// Task-owned scratch needed while recovering the CPU area from `tp`.
#[repr(C)]
struct ThreadTrapState {
    scratch0: usize,
    scratch1: usize,
}

pub(super) const CPU_KERNEL_STACK_POINTER_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuTrapState, kernel_stack_pointer);
pub(super) const CPU_USER_TRAP_FRAME_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuTrapState, user_trap_frame);
#[cfg(feature = "tls")]
pub(super) const CPU_ENTRY_SCRATCH0_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuTrapState, entry_scratch0);
#[cfg(feature = "tls")]
pub(super) const CPU_ENTRY_SCRATCH1_OFFSET: usize =
    CPU_AREA_ARCH_STATE_OFFSET + offset_of!(CpuTrapState, entry_scratch1);
#[cfg(not(feature = "tls"))]
pub(super) const THREAD_SCRATCH0_OFFSET: usize =
    CURRENT_THREAD_ARCH_STATE_OFFSET + offset_of!(ThreadTrapState, scratch0);
#[cfg(not(feature = "tls"))]
pub(super) const THREAD_SCRATCH1_OFFSET: usize =
    CURRENT_THREAD_ARCH_STATE_OFFSET + offset_of!(ThreadTrapState, scratch1);

const _: () = {
    assert!(size_of::<CpuTrapState>() <= CPU_AREA_ARCH_STATE_SIZE);
    assert!(size_of::<ThreadTrapState>() <= CURRENT_THREAD_ARCH_STATE_SIZE);
};

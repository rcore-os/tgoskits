//! RISC-V trap state embedded in the architecture-neutral CPU-local reserves.

use core::mem::offset_of;

/// CPU-owned state used by user/kernel trap stack handoff.
#[repr(C)]
#[derive(Default)]
pub struct CpuEntryState {
    kernel_stack_pointer: usize,
    user_trap_frame: usize,
    entry_scratch0: usize,
    entry_scratch1: usize,
}

/// Task-owned scratch needed while recovering the CPU area from `tp`.
#[repr(C)]
#[derive(Default)]
pub struct TaskEntryState {
    scratch0: usize,
    scratch1: usize,
}

pub(super) const CPU_KERNEL_STACK_POINTER_OFFSET: usize =
    offset_of!(CpuEntryState, kernel_stack_pointer);
pub(super) const CPU_USER_TRAP_FRAME_OFFSET: usize = offset_of!(CpuEntryState, user_trap_frame);
#[cfg(kernel_tls)]
pub(super) const CPU_ENTRY_SCRATCH0_OFFSET: usize = offset_of!(CpuEntryState, entry_scratch0);
#[cfg(kernel_tls)]
pub(super) const CPU_ENTRY_SCRATCH1_OFFSET: usize = offset_of!(CpuEntryState, entry_scratch1);
#[cfg(not(kernel_tls))]
pub(super) const THREAD_SCRATCH0_OFFSET: usize = offset_of!(TaskEntryState, scratch0);
#[cfg(not(kernel_tls))]
pub(super) const THREAD_SCRATCH1_OFFSET: usize = offset_of!(TaskEntryState, scratch1);

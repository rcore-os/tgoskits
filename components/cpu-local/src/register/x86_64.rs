use ax_cpu::registers;

use super::*;
use crate::{
    CPU_AREA_CURRENT_CONTEXT_OFFSET, CPU_AREA_PREEMPTION_STATE_OFFSET, CPU_AREA_SELF_BASE_OFFSET,
    CpuIndex, CpuLocalError, preempt::PreemptionState,
};

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    linux_current: CurrentContextSource::RuntimeAnchor,
    unikernel_tls: CurrentContextSource::RuntimeAnchor,
};

pub(super) struct Backend;

impl ArchitectureRegisterBackend for Backend {
    #[inline(always)]
    fn current_cpu_index() -> Result<CpuIndex, CpuLocalError> {
        // SAFETY: the installed GS base points at the immutable CPU-area
        // header for the current CPU and the caller's preemption/IRQ pin keeps
        // that area selected until this scalar is consumed.
        let index = unsafe { registers::read_gs_u32::<{ crate::CPU_AREA_CPU_INDEX_OFFSET }>() };
        CpuIndex::from_u32(index).ok_or(CpuLocalError::AreaIdentityMismatch)
    }

    #[inline(always)]
    fn current_preemption_snapshot() -> Result<PreemptionSnapshot, CpuLocalError> {
        // SAFETY: x86 owns the selected preemption word in the installed CPU
        // runtime anchor. The fixed GS offset is the architecture-native
        // override of the execution-context default implementation.
        let state = unsafe { registers::read_gs_u32::<CPU_AREA_PREEMPTION_STATE_OFFSET>() };
        Ok(PreemptionSnapshot::from_raw(state))
    }
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, _boot_context: usize) {
    // SAFETY: the caller owns installation of this permanent CPU area.
    unsafe { registers::write_gs_base(area_base) };
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    // SAFETY: installation retains the CPU area's initialized self pointer.
    Ok(unsafe { registers::read_gs_usize::<CPU_AREA_SELF_BASE_OFFSET>() })
}

pub(super) unsafe fn read_current_context(_area_base: usize) -> usize {
    // SAFETY: the installed GS area retains its current-context publication.
    unsafe { registers::read_gs_usize::<CPU_AREA_CURRENT_CONTEXT_OFFSET>() }
}

#[inline(always)]
pub(super) unsafe fn enter_preemption() {
    // SAFETY: the installed CPU owns the live preemption word exclusively.
    unsafe { registers::increment_gs_u32::<CPU_AREA_PREEMPTION_STATE_OFFSET>() };
}

#[inline(always)]
pub(super) unsafe fn read_preemption_state() -> u32 {
    // SAFETY: the live token retains the selected CPU's preemption owner.
    unsafe { registers::read_gs_u32::<CPU_AREA_PREEMPTION_STATE_OFFSET>() }
}

#[inline(always)]
pub(super) unsafe fn compare_exchange_current_preemption_state(current: u32, next: u32) -> bool {
    // SAFETY: positive depth excludes remote writers; local IRQs can update
    // pending state only before or after the one CMPXCHG instruction.
    let observed = unsafe {
        registers::compare_exchange_gs_u32::<CPU_AREA_PREEMPTION_STATE_OFFSET>(current, next)
    };
    observed == current
}

#[inline(always)]
pub(super) unsafe fn decrement_current_preemption_state() {
    // SAFETY: the retained nested depth owns the selected CPU word. The
    // subtraction preserves the pending high bit across local IRQ delivery.
    unsafe { registers::decrement_gs_u32::<CPU_AREA_PREEMPTION_STATE_OFFSET>() };
}

/// Returns the current CPU's preemption word after a caller has raised its
/// depth through [`enter_preemption`].
///
/// # Safety
///
/// The caller must have completed the matching increment before invoking this
/// function and must keep the returned reference within that preemption
/// exclusion. The installed GS area and its preemption word remain mapped for
/// the runtime lifetime.
#[inline(always)]
pub(super) unsafe fn current_preemption_state() -> &'static PreemptionState {
    // SAFETY: the preceding GS increment pins the selected area until exit.
    // A load follows the GS base; LEA would ignore the segment base.
    let area_base = unsafe { registers::read_gs_usize::<CPU_AREA_SELF_BASE_OFFSET>() };
    let state = area_base
        .checked_add(CPU_AREA_PREEMPTION_STATE_OFFSET)
        .unwrap_or_else(|| crate::register::fatal_register_invariant());
    // SAFETY: the installed CPU area is retained for the runtime lifetime and
    // the checked preemption depth pins this access to that area.
    unsafe { &*core::ptr::with_exposed_provenance::<PreemptionState>(state) }
}

/// Compares one transition of the current CPU-owned preemption word.
///
/// # Safety
///
/// `state` must be the owner retained by the caller's positive preemption
/// depth, and no remote CPU may access that owner. A local interrupt may
/// update the word only at an instruction boundary.
#[inline(always)]
#[cfg(test)]
pub(super) unsafe fn compare_exchange_preemption_state(
    state: &PreemptionState,
    current: u32,
    next: u32,
) -> bool {
    let mut observed = current;
    // SAFETY: x86 completes CMPXCHG before recognizing a local interrupt. The
    // absence of a LOCK prefix is valid because the owner contract excludes
    // remote access, matching Linux raw_cpu_try_cmpxchg_4().
    unsafe {
        core::arch::asm!(
            "cmpxchg dword ptr [{state}], {next:e}",
            state = in(reg) state.as_mut_ptr(),
            next = in(reg) next,
            inout("eax") observed,
            options(nostack),
        );
    }
    observed == current
}

#[cfg(kernel_tls)]
pub(super) unsafe fn read_kernel_tls() -> usize {
    registers::read_thread_pointer().as_usize()
}

#[cfg(kernel_tls)]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    // SAFETY: the caller owns the offline or final TLS installation boundary.
    unsafe { registers::write_thread_pointer(ax_cpu::context::KernelTlsBase::new(value)) };
}

use super::*;
use crate::{
    CPU_AREA_CURRENT_CONTEXT_OFFSET, CPU_AREA_PREEMPTION_STATE_OFFSET, CPU_AREA_SELF_BASE_OFFSET,
    preempt::PreemptionState,
};

const IA32_GS_BASE: u32 = 0xc000_0101;
#[cfg(feature = "tls")]
const IA32_FS_BASE: u32 = 0xc000_0100;

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    linux_current: CurrentContextSource::RuntimeAnchor,
    unikernel_tls: CurrentContextSource::RuntimeAnchor,
};

pub(super) struct Backend;

impl ArchitectureRegisterBackend for Backend {
    #[inline(always)]
    fn current_preemption_snapshot() -> Result<PreemptionSnapshot, CpuLocalError> {
        let state: u32;
        // SAFETY: x86 owns the selected preemption word in the installed CPU
        // runtime anchor. The fixed GS offset is the architecture-native
        // override of the execution-context default implementation.
        unsafe {
            core::arch::asm!(
                "mov {state:e}, dword ptr gs:[{offset}]",
                state = out(reg) state,
                offset = const CPU_AREA_PREEMPTION_STATE_OFFSET,
                options(nostack, preserves_flags, readonly),
            );
        }
        Ok(PreemptionSnapshot::from_raw(state))
    }
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, _boot_context: usize) {
    let area_base = area_base as u64;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_GS_BASE,
            in("eax") area_base as u32,
            in("edx") (area_base >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    let area_base: usize;
    unsafe {
        core::arch::asm!(
            "mov {base}, gs:[{offset}]",
            base = out(reg) area_base,
            offset = const CPU_AREA_SELF_BASE_OFFSET,
            options(nostack, preserves_flags),
        );
    }
    Ok(area_base)
}

pub(super) unsafe fn read_current_context(_area_base: usize) -> usize {
    let current_context: usize;
    unsafe {
        core::arch::asm!(
            "mov {current}, gs:[{offset}]",
            current = out(reg) current_context,
            offset = const CPU_AREA_CURRENT_CONTEXT_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    current_context
}

#[inline(always)]
pub(super) unsafe fn enter_preemption() {
    unsafe {
        core::arch::asm!(
            "inc dword ptr gs:[{offset}]",
            offset = const CPU_AREA_PREEMPTION_STATE_OFFSET,
            options(nostack),
        );
    }
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
    let area_base: usize;
    // SAFETY: the preceding GS increment pins this instruction stream to the
    // selected CPU area until the matching preemption exit. `mov` is used
    // instead of `lea`: x86 effective-address calculation does not include a
    // GS base, while this load reads the installed per-CPU self pointer.
    unsafe {
        core::arch::asm!(
            "mov {base}, gs:[{offset}]",
            base = out(reg) area_base,
            offset = const CPU_AREA_SELF_BASE_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
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

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_FS_BASE,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags),
        )
    };
    ((high as usize) << 32) | low as usize
}

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    let value = value as u64;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_FS_BASE,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags),
        )
    };
}

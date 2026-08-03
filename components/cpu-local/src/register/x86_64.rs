use super::*;
use crate::{
    CPU_AREA_CURRENT_THREAD_OFFSET, CPU_AREA_PREEMPT_STATE_OFFSET, CPU_AREA_SELF_BASE_OFFSET,
    PreemptExit, preempt::PREEMPT_NO_RESCHED,
};

const IA32_GS_BASE: u32 = 0xc000_0101;
#[cfg(feature = "tls")]
const IA32_FS_BASE: u32 = 0xc000_0100;

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, _boot_thread: usize) {
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

pub(super) unsafe fn read_current_thread(_area_base: usize) -> usize {
    let current_thread: usize;
    unsafe {
        core::arch::asm!(
            "mov {current}, gs:[{offset}]",
            current = out(reg) current_thread,
            offset = const CPU_AREA_CURRENT_THREAD_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    current_thread
}

// x86_64 stores current directly in the GS runtime anchor. The shared atomic
// publication is therefore the architecture commit; there is no second task
// pointer register to update.
pub(super) unsafe fn write_current_thread(_value: usize) {}

#[inline(always)]
unsafe fn preempt_state() -> u32 {
    let state: u32;
    unsafe {
        core::arch::asm!(
            "mov {state:e}, dword ptr gs:[{offset}]",
            state = out(reg) state,
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    state
}

#[inline(always)]
pub(super) unsafe fn preempt_guard_depth() -> u32 {
    (unsafe { preempt_state() }) & !PREEMPT_NO_RESCHED
}

#[inline(always)]
pub(super) unsafe fn enter_preempt_guard() {
    assert_ne!(
        unsafe { preempt_guard_depth() },
        !PREEMPT_NO_RESCHED,
        "CPU-local preemption guard nesting overflow"
    );
    unsafe {
        core::arch::asm!(
            "inc dword ptr gs:[{offset}]",
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            options(nostack),
        );
    }
}

#[inline(always)]
unsafe fn exit_nested_preempt_guard() {
    unsafe {
        core::arch::asm!(
            "dec dword ptr gs:[{offset}]",
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            options(nostack),
        );
    }
}

#[inline(always)]
unsafe fn try_consume_final_preempt_guard() -> bool {
    let expected = PREEMPT_NO_RESCHED | 1;
    let mut observed = expected;
    let replacement = PREEMPT_NO_RESCHED;
    unsafe {
        core::arch::asm!(
            "cmpxchg dword ptr gs:[{offset}], {replacement:e}",
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            replacement = in(reg) replacement,
            inout("eax") observed,
            options(nostack),
        );
    }
    observed == expected
}

#[inline(always)]
pub(super) unsafe fn prepare_preempt_guard_exit() -> PreemptExit {
    loop {
        let state = unsafe { preempt_state() };
        let depth = state & !PREEMPT_NO_RESCHED;
        assert!(depth > 0, "unbalanced CPU-local preemption guard exit");
        if depth == 1 {
            if state & PREEMPT_NO_RESCHED == 0 {
                return PreemptExit::FinalPending;
            }
            if unsafe { try_consume_final_preempt_guard() } {
                return PreemptExit::FinalConsumed;
            }
            continue;
        }
        unsafe { exit_nested_preempt_guard() };
        return PreemptExit::NestedConsumed;
    }
}

#[inline(always)]
pub(super) unsafe fn consume_final_preempt_guard() -> bool {
    let mut observed = 1u32;
    unsafe {
        core::arch::asm!(
            "cmpxchg dword ptr gs:[{offset}], {replacement:e}",
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            replacement = in(reg) 0u32,
            inout("eax") observed,
            options(nostack),
        );
    }
    observed == 1
}

#[inline(always)]
pub(super) unsafe fn set_preempt_need_resched() {
    let mask = !PREEMPT_NO_RESCHED;
    unsafe {
        core::arch::asm!(
            "and dword ptr gs:[{offset}], {mask:e}",
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            mask = in(reg) mask,
            options(nostack),
        );
    }
}

#[inline(always)]
pub(super) unsafe fn clear_preempt_need_resched() {
    unsafe {
        core::arch::asm!(
            "or dword ptr gs:[{offset}], {mask:e}",
            offset = const CPU_AREA_PREEMPT_STATE_OFFSET,
            mask = in(reg) PREEMPT_NO_RESCHED,
            options(nostack),
        );
    }
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

//! Preserve the native scheduler's preemption ownership across C-shaped locks.
//!
//! Linux lock/unlock entry points do not carry a Rust guard token. One CPU-local
//! scope therefore retains the native token across the outermost DRM lock, with
//! a nesting count for inner locks. While this count is nonzero either that
//! token or an enclosing IRQ/scheduler owner prevents migration. State updates
//! exclude local IRQs, allocate nothing, and never invoke callbacks under a
//! CPU-local mutable borrow.

use ax_task::runtime::{
    cpu::{LocalIrqState, PreemptGuardToken},
    task_runtime,
};
use drm_kapi::locks::LockContextOps;

#[derive(Clone, Copy)]
struct LockScopes {
    depth: u32,
    token: PreemptGuardToken,
}

#[ax_percpu::def_percpu]
static DRM_LOCK_SCOPES: LockScopes = LockScopes {
    depth: 0,
    token: PreemptGuardToken::NONE,
};

pub(super) static OPS: LockContextOps = LockContextOps {
    preempt_disable,
    preempt_enable,
    irq_save_disable,
    irq_restore,
    irq_disable: ax_hal::cpu::interrupt::disable_irqs,
    irq_enable: ax_hal::cpu::interrupt::enable_irqs,
    irqs_disabled: || !ax_hal::cpu::interrupt::irqs_enabled(),
};

/// The caller retains raw local IRQ exclusion throughout this operation.
fn read_scopes() -> LockScopes {
    // SAFETY: callers save and disable local IRQs before resolving the CPU;
    // no pointer or pin escapes the closure and no other CPU accesses this slot.
    unsafe { ax_hal::percpu::with_cpu_pin(|pin| DRM_LOCK_SCOPES.with_current(pin, |state| *state)) }
        .expect("DRM locks require an initialized CPU area")
}

/// The caller retains raw local IRQ exclusion throughout this operation.
fn write_scopes(state: LockScopes) {
    // SAFETY: raw local IRQ exclusion prevents both migration and local
    // re-entry. This slot has no remote readers or writers, and the mutable
    // borrow ends before any scheduler/runtime callback is invoked.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                DRM_LOCK_SCOPES.with_current_mut(exclusive, |current| *current = state);
            });
        })
    }
    .expect("DRM locks require an initialized CPU area");
}

fn preempt_disable() {
    let irq = task_runtime::local_irq_save_and_disable();
    let mut state = read_scopes();
    if state.depth == 0 {
        // A stronger IRQ/scheduler scope returns NONE; its lifetime must
        // enclose every DRM lock acquired inside that scope.
        state.token = task_runtime::preempt_guard_enter();
    }
    state.depth = state
        .depth
        .checked_add(1)
        .expect("DRM lock preemption nesting overflow");
    write_scopes(state);
    // SAFETY: restore exactly the saved state on the same, still-pinned CPU.
    unsafe { task_runtime::local_irq_restore(irq) };
}

fn preempt_enable() {
    let irq = task_runtime::local_irq_save_and_disable();
    let mut state = read_scopes();
    state.depth = state
        .depth
        .checked_sub(1)
        .expect("unbalanced DRM lock preemption release");
    let token = if state.depth == 0 {
        core::mem::replace(&mut state.token, PreemptGuardToken::NONE)
    } else {
        PreemptGuardToken::NONE
    };
    write_scopes(state);
    // SAFETY: the outer preemption token (or stronger caller scope) still
    // pins this CPU. Restore IRQs before the final exit may schedule.
    unsafe { task_runtime::local_irq_restore(irq) };
    if !token.is_none() {
        // SAFETY: this is the unique token acquired for this exact outer
        // scope. All protected writes and local borrows have ended.
        unsafe { task_runtime::preempt_guard_exit(token) };
    }
}

fn irq_save_disable() -> core::ffi::c_ulong {
    task_runtime::local_irq_save_and_disable().into_raw() as core::ffi::c_ulong
}

/// Restore a raw state produced by `irq_save_disable` on the current CPU.
///
/// # Safety
///
/// `flags` must be a raw state value previously returned by a matching call to
/// `irq_save_disable` on the current CPU, called within the corresponding nested
/// IRQ scope without intervening migration or unbalanced restorations.
unsafe fn irq_restore(flags: core::ffi::c_ulong) {
    // SAFETY: the KAPI irqsave lock contract returns this original state
    // once, on the same CPU and in the matching nested IRQ scope.
    unsafe { task_runtime::local_irq_restore(LocalIrqState::from_raw(flags as usize)) };
}

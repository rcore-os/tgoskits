//! OS-independent spin locks for kernel and interrupt contexts.
//!
//! The crate combines `spin`'s raw algorithms with `lock_api`'s safe data
//! guards. Platform operations are supplied by [`LockRuntime`]; the crate does
//! not depend on a concrete scheduler, interrupt controller, or per-CPU
//! implementation.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod context;
mod once;
mod raw;
mod runtime_call;
mod wrapper;

pub use context::*;
pub use once::*;
pub use raw::*;
use trait_ffi::def_extern_trait;
pub use wrapper::*;

/// Describes a lock operation to the runtime's allocation-free lockdep sink.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct LockdepEvent {
    /// Address of the raw lock instance.
    pub lock_address: usize,
    /// Runtime-defined current thread identifier, or zero during early boot.
    pub thread_id: u64,
    /// Nested lock subclass used by lock-order validation.
    pub subclass: u32,
    /// Kind of raw lock operation.
    pub kind: LockKind,
    /// Whether this operation originated from a non-blocking try operation.
    pub is_try: bool,
}

/// Identifies the lock mode used by a [`LockdepEvent`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LockKind {
    /// Exclusive ticket-mutex acquisition.
    Mutex   = 0,
    /// Shared spin-rwlock acquisition.
    RwRead  = 1,
    /// Exclusive spin-rwlock acquisition.
    RwWrite = 2,
}

/// OS capabilities required by context-aware spin locks.
///
/// The generated calls use the Rust ABI. Every argument crossing this boundary
/// is an integer, a `repr` value, or a plain aggregate of those values. The OS
/// implementation must keep IRQ and preemption nesting in CPU-local storage so
/// guards can be dropped in an order different from their acquisition order.
#[def_extern_trait(abi = "rust")]
pub trait LockRuntime {
    /// Enters one nested local-IRQ-disabled section.
    ///
    /// Until the matching [`Self::irq_exit`], the runtime must reject every
    /// scheduling path that could migrate the caller. This is stronger than a
    /// raw hardware IRQ mask and allows [`IrqGuard::cpu_pin`] to expose a
    /// migration proof. A CPU-local provider must separately validate that the
    /// current architecture anchor names one of its installed areas.
    fn irq_enter();

    /// Leaves one nested local-IRQ-disabled section.
    fn irq_exit();

    /// Enters one nested preemption-disabled section.
    ///
    /// The caller must remain on the same CPU until the matching outermost
    /// preemption exit or its typed scheduler-baton transfer.
    fn preempt_enter();

    /// Leaves one nested preemption-disabled section and performs any pending
    /// task-context preemption at the runtime's validated scheduler safe point.
    ///
    /// The decrement, eligibility recheck, and scheduler entry are one runtime
    /// operation. Keeping that sequence below this boundary prevents a stale
    /// "outermost" result from crossing an interrupt or task migration.
    fn preempt_exit();

    /// Leaves the IRQ handler's preemption guard and performs any pending
    /// IRQ-return preemption while hardware interrupts remain disabled.
    ///
    /// # Safety
    ///
    /// Controller EOI and hard-IRQ bookkeeping must be complete. The caller
    /// must return through a trap frame that owns restoration of the interrupted
    /// hardware IRQ state.
    unsafe fn preempt_exit_irq_return();

    /// Returns the current generation-bearing thread identifier.
    fn current_thread_id() -> u64;

    /// Records a successful lock acquisition without allocating or blocking.
    fn lockdep_acquire(event: LockdepEvent);

    /// Records a lock release without allocating or blocking.
    fn lockdep_release(event: LockdepEvent);

    /// Enables or disables the runtime's fixed-buffer lock trace.
    fn lockdep_set_trace_enabled(enabled: bool);

    /// Flushes the runtime's fixed-buffer lock trace to its raw diagnostic sink.
    fn lockdep_dump_trace();
}

/// Enables or disables fixed-buffer lock tracing in the runtime.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    runtime_call::lockdep_set_trace_enabled(enabled);
}

/// Flushes fixed-buffer lock tracing through the runtime diagnostic sink.
pub fn dump_lockdep_trace() {
    runtime_call::lockdep_dump_trace();
}

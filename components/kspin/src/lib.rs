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
mod raw;
mod runtime_call;
mod wrapper;

pub use context::*;
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
    fn irq_enter();

    /// Leaves one nested local-IRQ-disabled section.
    fn irq_exit();

    /// Returns whether local interrupts are currently enabled.
    fn irqs_enabled() -> bool;

    /// Enters one nested preemption-disabled section.
    fn preempt_enter();

    /// Leaves one nested preemption-disabled section.
    ///
    /// Returns `true` only when the outermost section was left and scheduling
    /// is permitted by the scheduler's preemption state.
    fn preempt_exit() -> bool;

    /// Returns whether the current execution context is a hard interrupt.
    fn in_hard_irq() -> bool;

    /// Returns whether the current CPU has a pending reschedule request.
    fn need_resched() -> bool;

    /// Enters the scheduler at a runtime-validated safe point.
    ///
    /// Ordinary guard exit calls this with IRQs enabled. Architecture IRQ
    /// return may call it with IRQs disabled after controller EOI and hard-IRQ
    /// marker teardown; the trap frame then restores the interrupted flags.
    fn schedule();

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

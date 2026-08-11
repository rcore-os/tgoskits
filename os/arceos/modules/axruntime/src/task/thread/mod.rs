use alloc::sync::Arc;
use core::sync::atomic::AtomicU8;

use super::*;

const THREAD_START_PENDING: u8 = 0;
const THREAD_START_ACTIVE: u8 = 1;
const THREAD_START_ABORTED: u8 = 2;

/// Scheduler thread whose resources and registry identity exist but which is
/// not yet runnable.
///
/// OS layers use this move-only transaction boundary to finish private resource
/// construction. They must call [`Self::stage`] before publishing externally
/// visible task identity, then call [`StagedThread::activate`] after committing
/// that identity. Dropping an unpublished value removes the scheduler record
/// and releases context, stack, TLS, extension, and address-space resources in
/// task context.
pub struct PreparedThread {
    system: &'static TaskSystem,
    handle: Option<ThreadHandle>,
    start: Arc<RuntimeThreadStart>,
}

/// Scheduler thread placed on a run queue but not yet activated by its OS.
///
/// This transaction token must be activated or dropped from task context.
/// Completing either path wakes the staged trampoline through a task-context
/// wait queue and is therefore not valid in a hard-interrupt handler.
#[must_use = "staged threads must be activated or explicitly dropped to abort"]
pub struct StagedThread {
    handle: Option<ThreadHandle>,
    start: Arc<RuntimeThreadStart>,
}

type KernelThreadEntry = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug)]
pub(super) struct RuntimeThreadStart {
    state: AtomicU8,
    wait: WaitQueue,
}

pub(super) struct RuntimeThreadData {
    pub(super) entry: ax_sync::SpinLock<Option<KernelThreadEntry>>,
    pub(super) exit_code: AtomicI32,
    pub(super) exit_completed: AtomicBool,
    pub(super) join_wait: WaitQueue,
    pub(super) os_extension: Option<ThreadExtension>,
    pub(super) start: Arc<RuntimeThreadStart>,
    pub(super) _name: String,
}

/// OS extension borrowed through the runtime's outer scheduler extension.
#[derive(Debug)]
pub struct ThreadOsExtensionBorrow<'thread> {
    _runtime: ax_task::ThreadExtensionBorrow<'thread>,
    data: usize,
    ops: &'static ThreadExtensionOps,
}

impl ThreadOsExtensionBorrow<'_> {
    /// Returns the OS-owned opaque value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the OS extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

/// OS extension lease for current-thread lookups without an existing handle.
#[derive(Debug)]
pub struct ThreadOsExtensionLease {
    _runtime: ax_task::ThreadExtensionLease,
    data: usize,
    ops: &'static ThreadExtensionOps,
}

impl ThreadOsExtensionLease {
    /// Returns the OS-owned opaque value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the OS extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

impl RuntimeThreadData {
    pub(super) fn new(
        entry: KernelThreadEntry,
        name: String,
        os_extension: Option<ThreadExtension>,
        start: Arc<RuntimeThreadStart>,
    ) -> Self {
        Self {
            entry: ax_sync::SpinLock::new(Some(entry)),
            exit_code: AtomicI32::new(0),
            exit_completed: AtomicBool::new(false),
            join_wait: WaitQueue::new(),
            os_extension,
            start,
            _name: name,
        }
    }
}

mod extension;
mod lifecycle;
mod publication;

pub(in crate::task) use extension::{RUNTIME_THREAD_EXTENSION_OPS, runtime_thread_extension};
#[cfg(test)]
pub(in crate::task) use extension::{RuntimeExtensionKind, classify_runtime_extension};
pub use extension::{current_os_extension, thread_os_extension};
#[cfg(test)]
pub(in crate::task) use lifecycle::extension_data_after_releasing_lease;
pub use lifecycle::{exit_current, join_thread, wait_thread};
pub(in crate::task) use lifecycle::{
    finish_initial_scheduler_switch, release_transferred_extension, runtime_thread_entry,
};

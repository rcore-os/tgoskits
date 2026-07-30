//! Thread construction data kept independent from an operating system.

use alloc::{sync::Arc, vec, vec::Vec};

use crate::{
    CpuId, SchedulePolicy, SchedulerTickGate, SchedulerTickIrqAccounting, SchedulerTickTaskWork,
    SchedulerTickWork, TaskError, ThreadHandle, ThreadId,
    runtime::{
        AddressSpaceHandle, ExecutionContextHandle, RuntimeStatus, StackHandle, TlsHandle,
        task_runtime,
    },
};

/// Runtime-owned resources whose lifetime follows one thread.
#[repr(C)]
#[derive(Debug, Eq, PartialEq)]
pub struct ThreadResources {
    context: ExecutionContextHandle,
    stack: StackHandle,
    tls: TlsHandle,
    address_space: AddressSpaceHandle,
}

impl ThreadResources {
    /// Empty resources for pure scheduler models.
    pub const NONE: Self = Self {
        context: ExecutionContextHandle::NONE,
        stack: StackHandle::NONE,
        tls: TlsHandle::NONE,
        address_space: AddressSpaceHandle::NONE,
    };

    /// Creates a complete runtime resource bundle from uniquely owned handles.
    ///
    /// # Safety
    ///
    /// Every non-empty handle must be live, belong to the currently installed
    /// [`crate::runtime::TaskRuntime`], and have its unique destruction right
    /// transferred into this bundle. The caller must not construct another
    /// owning bundle from the same scalar handles.
    pub const unsafe fn new(
        context: ExecutionContextHandle,
        stack: StackHandle,
        tls: TlsHandle,
        address_space: AddressSpaceHandle,
    ) -> Self {
        Self {
            context,
            stack,
            tls,
            address_space,
        }
    }

    /// Returns the execution context.
    pub const fn context(&self) -> ExecutionContextHandle {
        self.context
    }
    /// Returns the guarded stack allocation.
    pub const fn stack(&self) -> StackHandle {
        self.stack
    }
    /// Returns the TLS allocation.
    pub const fn tls(&self) -> TlsHandle {
        self.tls
    }
    /// Returns the address-space handle.
    pub const fn address_space(&self) -> AddressSpaceHandle {
        self.address_space
    }

    pub(crate) fn replace_address_space(
        &mut self,
        address_space: AddressSpaceHandle,
    ) -> AddressSpaceHandle {
        core::mem::replace(&mut self.address_space, address_space)
    }

    /// Releases as many resources as possible without losing retry ownership.
    ///
    /// A failed runtime operation leaves its handle and every dependent handle
    /// in this bundle. The task-context reaper can therefore retry the same
    /// transaction without reconstructing an already-consumed identity.
    pub(crate) fn try_release(&mut self) -> Result<(), TaskError> {
        if !self.context.is_none() {
            let status = task_runtime::destroy_context(self.context);
            if status != RuntimeStatus::Success {
                return Err(TaskError::RuntimeFailure(status as u32));
            }
            self.context = ExecutionContextHandle::NONE;
        }
        self.address_space = AddressSpaceHandle::NONE;

        if !self.tls.is_none() {
            let status = task_runtime::deallocate_tls(self.tls);
            if status != RuntimeStatus::Success {
                return Err(TaskError::RuntimeFailure(status as u32));
            }
            self.tls = TlsHandle::NONE;
        }

        if !self.stack.is_none() {
            let status = task_runtime::deallocate_stack(self.stack);
            if status != RuntimeStatus::Success {
                return Err(TaskError::RuntimeFailure(status as u32));
            }
            self.stack = StackHandle::NONE;
        }
        Ok(())
    }
}

impl Drop for ThreadResources {
    fn drop(&mut self) {
        // Ordinary scheduler teardown retains this bundle when a retry is
        // needed. This best-effort fallback is only for construction rollback
        // or shutdown paths where no task-system retry owner remains.
        let _result = self.try_release();
    }
}

/// Why a running thread relinquished its execution context.
///
/// The value crosses the OS extension callback boundary, so its numeric layout
/// is stable and may also be written directly to allocation-free trace records.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitchReason {
    /// A scheduler request selected a more urgent or otherwise eligible thread.
    Preempted = 1,
    /// The thread voluntarily yielded its current service position.
    Yield     = 2,
    /// The thread committed a park or another blocking operation.
    Blocked   = 3,
    /// The thread terminated and will never become runnable again.
    Exited    = 4,
    /// CPU affinity or balancing moved the thread away from this CPU.
    Migrated  = 5,
}

/// CPU affinity expressed against one [`crate::TaskSystem`] topology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuSet {
    allowed: Vec<bool>,
}

impl CpuSet {
    /// Creates a set that permits every CPU in a topology.
    pub fn all(cpu_count: usize) -> Self {
        Self {
            allowed: vec![true; cpu_count],
        }
    }

    /// Creates an empty CPU set for a topology.
    pub fn empty(cpu_count: usize) -> Self {
        Self {
            allowed: vec![false; cpu_count],
        }
    }

    /// Enables one CPU if it is represented by this set.
    pub fn insert(&mut self, cpu: CpuId) -> bool {
        match self.allowed.get_mut(cpu.as_usize()) {
            Some(allowed) => {
                let changed = !*allowed;
                *allowed = true;
                changed
            }
            None => false,
        }
    }

    /// Disables one CPU if it is represented by this set.
    pub fn remove(&mut self, cpu: CpuId) -> bool {
        match self.allowed.get_mut(cpu.as_usize()) {
            Some(allowed) => {
                let changed = *allowed;
                *allowed = false;
                changed
            }
            None => false,
        }
    }

    /// Tests whether a CPU is allowed.
    pub fn contains(&self, cpu: CpuId) -> bool {
        self.allowed.get(cpu.as_usize()).copied().unwrap_or(false)
    }

    /// Returns the number of CPUs represented by the set.
    pub fn topology_len(&self) -> usize {
        self.allowed.len()
    }

    /// Returns whether this set permits every CPU selected by `required`.
    pub fn covers(&self, required: &Self) -> bool {
        self.allowed.len() == required.allowed.len()
            && self
                .allowed
                .iter()
                .zip(&required.allowed)
                .all(|(allowed, is_required)| !is_required || *allowed)
    }

    pub(crate) fn copy_from_set(&mut self, source: &Self) -> Result<(), TaskError> {
        if self.allowed.len() != source.allowed.len() {
            return Err(TaskError::InvalidConfiguration);
        }
        self.allowed.copy_from_slice(&source.allowed);
        Ok(())
    }
}

/// OS-owned callbacks attached to a thread without exposing OS types.
#[repr(C)]
#[derive(Debug)]
pub struct ThreadExtensionOps {
    /// Invoked before the thread becomes the current execution context.
    pub on_switch_in: unsafe extern "Rust" fn(data: usize, thread: ThreadId),
    /// Invoked after the thread stops being the current execution context.
    pub on_switch_out: unsafe extern "Rust" fn(data: usize, thread: ThreadId, reason: SwitchReason),
    /// Invoked in task context after the thread exits.
    pub on_exit: unsafe extern "Rust" fn(data: usize, thread: ThreadId),
    /// Invoked in task context for requested Deadline overrun notification.
    pub on_deadline_overrun: unsafe extern "Rust" fn(data: usize, thread: ThreadId),
    /// Releases the OS-owned extension data in task or reaper context.
    pub drop: unsafe extern "Rust" fn(data: usize),
}

/// Opaque OS-specific data attached to a thread.
#[derive(Debug)]
pub struct ThreadExtension {
    data: usize,
    ops: &'static ThreadExtensionOps,
    scheduler_tick_work: Option<SchedulerTickWork>,
}

impl ThreadExtension {
    /// Creates an extension from opaque data and a static callback table.
    ///
    /// # Safety
    ///
    /// `data` must satisfy every callback contract in `ops`, and the owning OS
    /// must ensure callbacks do not allocate, block, or re-enter the scheduler
    /// when invoked as switch hooks. Task-context callbacks must return to the
    /// dedicated service thread; abandoning that stack leaves their explicit
    /// in-flight lifetime claim closed to prevent use-after-free.
    pub const unsafe fn new(data: usize, ops: &'static ThreadExtensionOps) -> Self {
        Self {
            data,
            ops,
            scheduler_tick_work: None,
        }
    }

    /// Adds task-context work gated by scheduler tick interest.
    ///
    /// The scheduler hard-IRQ path only publishes a typed deferred-work record.
    /// The callback runs later on the dedicated task-work service thread.
    ///
    /// # Safety
    ///
    /// `callback` must interpret `data` according to this extension, remain
    /// valid for its complete lifetime, and return normally to the task-work
    /// service. The callback may use task-context synchronization but must not
    /// retain the borrowed extension data after it returns.
    pub unsafe fn with_scheduler_tick_work(
        mut self,
        gate: Arc<SchedulerTickGate>,
        callback: SchedulerTickTaskWork,
    ) -> Self {
        self.scheduler_tick_work = Some(SchedulerTickWork::new(gate, callback));
        self
    }

    /// Adds bounded hard-IRQ accounting followed by deferred task-context work.
    ///
    /// The accounting callback runs for every observed scheduler tick while
    /// the gate is enabled, even when task-work publications coalesce. The
    /// task callback retains the lifetime and execution contract documented by
    /// [`Self::with_scheduler_tick_work`].
    ///
    /// # Safety
    ///
    /// `irq_accounting` must interpret `data` according to this extension and
    /// perform only bounded, allocation-free, non-blocking operations valid in
    /// hard-IRQ context. `callback` must satisfy the task-work contract of
    /// [`Self::with_scheduler_tick_work`].
    pub unsafe fn with_scheduler_tick_accounting_work(
        mut self,
        gate: Arc<SchedulerTickGate>,
        irq_accounting: SchedulerTickIrqAccounting,
        callback: SchedulerTickTaskWork,
    ) -> Self {
        self.scheduler_tick_work = Some(SchedulerTickWork::with_irq_accounting(
            gate,
            irq_accounting,
            callback,
        ));
        self
    }

    /// Returns the opaque OS-owned value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }

    /// Clones the gate used to select scheduler-tick task work.
    ///
    /// Runtime extension composition uses this to install the same interest
    /// generation on an outer scheduler-owned extension.
    pub fn scheduler_tick_work_gate(&self) -> Option<Arc<SchedulerTickGate>> {
        self.scheduler_tick_work
            .as_ref()
            .map(SchedulerTickWork::gate)
    }

    /// Returns whether this extension registered scheduler-tick IRQ accounting.
    pub fn has_scheduler_tick_irq_accounting(&self) -> bool {
        self.scheduler_tick_work
            .as_ref()
            .is_some_and(SchedulerTickWork::has_irq_accounting)
    }

    /// Forwards scheduler-tick IRQ accounting to this extension.
    ///
    /// Returns `false` when this extension did not register accounting.
    ///
    /// # Safety
    ///
    /// The caller must invoke this only for the current scheduler thread with
    /// local IRQs disabled while retaining this extension for the complete
    /// callback.
    pub unsafe fn forward_scheduler_tick_irq_accounting(&self, thread: ThreadId) -> bool {
        let Some(work) = self.scheduler_tick_work.as_ref() else {
            return false;
        };
        unsafe { work.account_irq(self.data, thread) }
    }

    /// Forwards one scheduler-tick task-work callback to this extension.
    ///
    /// Returns `false` when this extension did not register such work.
    ///
    /// # Safety
    ///
    /// The caller must own an ordinary task-context publication authorized by
    /// the gate returned from [`Self::scheduler_tick_work_gate`], keep this
    /// extension alive for the call, and invoke it at most once for that
    /// publication.
    pub unsafe fn forward_scheduler_tick_work(&self, thread: ThreadId) -> bool {
        let Some(work) = self.scheduler_tick_work.as_ref() else {
            return false;
        };
        unsafe { work.invoke(self.data, thread) };
        true
    }

    pub(crate) const fn as_view(&self) -> ThreadExtensionView {
        ThreadExtensionView {
            data: self.data,
            ops: self.ops,
        }
    }

    pub(crate) fn scheduler_tick_work(&self) -> Option<SchedulerTickWork> {
        self.scheduler_tick_work.clone()
    }
}

impl Drop for ThreadExtension {
    fn drop(&mut self) {
        // SAFETY: construction transfers the unique callback-data destruction
        // right into this non-cloneable owner.
        unsafe { (self.ops.drop)(self.data) };
    }
}

/// Copy-only borrowed identity for an installed OS extension.
#[derive(Clone, Copy, Debug)]
pub struct ThreadExtensionView {
    data: usize,
    ops: &'static ThreadExtensionOps,
}

/// Extension identity borrowed for exactly as long as a strong thread handle.
///
/// This wrapper deliberately does not expose its copyable internal view. The
/// strong handle borrowed by the wrapper prevents the registry reaper from
/// destroying the extension while its opaque data is being inspected.
#[derive(Debug)]
pub struct ThreadExtensionBorrow<'thread> {
    view: ThreadExtensionView,
    _thread: &'thread ThreadHandle,
}

impl<'thread> ThreadExtensionBorrow<'thread> {
    pub(crate) const fn new(view: ThreadExtensionView, thread: &'thread ThreadHandle) -> Self {
        Self {
            view,
            _thread: thread,
        }
    }

    /// Returns the borrowed opaque data value.
    pub const fn data(&self) -> usize {
        self.view.data()
    }

    /// Returns the callback table used as the extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.view.ops()
    }
}

/// Owned extension lease used when the caller has no pre-existing handle.
///
/// Keeping this value alive pins both the thread header and the registry record,
/// so current-thread helpers cannot return data that becomes stale immediately
/// after their temporary lookup handle is dropped.
#[derive(Debug)]
pub struct ThreadExtensionLease {
    view: ThreadExtensionView,
    thread: ThreadHandle,
}

impl ThreadExtensionLease {
    pub(crate) const fn new(view: ThreadExtensionView, thread: ThreadHandle) -> Self {
        Self { view, thread }
    }

    /// Returns the generation-bearing identity pinned by this lease.
    pub fn thread_id(&self) -> ThreadId {
        self.thread.id()
    }

    /// Returns the leased opaque data value.
    pub const fn data(&self) -> usize {
        self.view.data()
    }

    /// Returns the callback table used as the extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.view.ops()
    }

    /// Releases the strong lookup lease while retaining the extension view.
    ///
    /// Fresh thread-entry trampolines need this operation before invoking an
    /// entry point that terminates through a non-unwinding scheduler switch.
    /// Otherwise the suspended stack permanently pins the exited thread.
    ///
    /// # Safety
    ///
    /// The caller must be the running thread identified by [`Self::thread_id`].
    /// Its registry record must remain live until every use of the returned
    /// view completes. The consumed lookup lease and its pinned thread header
    /// must not be accessed again, and the returned view must not escape past
    /// thread exit.
    pub unsafe fn release_for_current_thread_entry(self) -> ThreadExtensionView {
        let view = self.view;
        drop(self);
        view
    }
}

impl ThreadExtensionView {
    /// Returns the borrowed opaque data value.
    pub const fn data(self) -> usize {
        self.data
    }

    /// Returns the callback table used as the extension type identity.
    pub const fn ops(self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

/// Validated inputs used to create a scheduler thread record.
#[derive(Debug)]
pub struct ThreadSpec {
    policy: SchedulePolicy,
    affinity: Option<CpuSet>,
    // Runtime resources must be dropped before the extension that owns their
    // address-space and entry metadata, including on fallback destruction.
    resources: ThreadResources,
    extension: Option<ThreadExtension>,
}

impl ThreadSpec {
    /// Creates a thread specification with full topology affinity.
    pub const fn new(policy: SchedulePolicy) -> Self {
        Self {
            policy,
            affinity: None,
            resources: ThreadResources::NONE,
            extension: None,
        }
    }

    /// Restricts the thread to an explicit CPU set.
    pub fn with_affinity(mut self, affinity: CpuSet) -> Self {
        self.affinity = Some(affinity);
        self
    }

    /// Attaches OS-specific state.
    pub fn with_extension(mut self, extension: ThreadExtension) -> Self {
        self.extension = Some(extension);
        self
    }

    /// Associates a complete runtime resource bundle with the thread.
    ///
    /// # Safety
    ///
    /// `resources` must satisfy [`ThreadResources::new`] and must be consumed by
    /// exactly this specification and its eventual scheduler record.
    pub unsafe fn with_resources(mut self, resources: ThreadResources) -> Self {
        self.resources = resources;
        self
    }

    /// Returns the base scheduling policy.
    pub const fn policy(&self) -> SchedulePolicy {
        self.policy
    }

    /// Returns explicit affinity, if one was supplied.
    pub fn affinity(&self) -> Option<&CpuSet> {
        self.affinity.as_ref()
    }

    pub(crate) fn into_owned_parts(mut self) -> (Option<ThreadExtension>, ThreadResources) {
        let extension = self.extension.take();
        let resources = core::mem::replace(&mut self.resources, ThreadResources::NONE);
        (extension, resources)
    }
}

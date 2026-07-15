//! Thread construction data kept independent from an operating system.

use alloc::{vec, vec::Vec};

use crate::{
    CpuId, SchedulePolicy, TaskError, ThreadHandle, ThreadId,
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

    pub(crate) fn release(mut self) -> Result<(), TaskError> {
        let statuses = self.release_handles();
        statuses
            .into_iter()
            .find(|status| *status != RuntimeStatus::Success)
            .map_or(Ok(()), |status| {
                Err(TaskError::RuntimeFailure(status as u32))
            })
    }

    fn release_handles(&mut self) -> [RuntimeStatus; 3] {
        let context = core::mem::replace(&mut self.context, ExecutionContextHandle::NONE);
        let context_status = if context.is_none() {
            RuntimeStatus::Success
        } else {
            task_runtime::destroy_context(context)
        };
        if context_status != RuntimeStatus::Success {
            // A failed destruction leaves the execution context potentially
            // live. It may still contain raw pointers into both allocations,
            // so losing those allocations would be a use-after-free. Registry
            // removal has already committed and there is no safe retry owner;
            // abandon all scalar handles and leak the resources instead.
            self.tls = TlsHandle::NONE;
            self.stack = StackHandle::NONE;
            self.address_space = AddressSpaceHandle::NONE;
            return [
                context_status,
                RuntimeStatus::Success,
                RuntimeStatus::Success,
            ];
        }
        let tls = core::mem::replace(&mut self.tls, TlsHandle::NONE);
        let stack = core::mem::replace(&mut self.stack, StackHandle::NONE);
        self.address_space = AddressSpaceHandle::NONE;
        [
            context_status,
            if tls.is_none() {
                RuntimeStatus::Success
            } else {
                task_runtime::deallocate_tls(tls)
            },
            if stack.is_none() {
                RuntimeStatus::Success
            } else {
                task_runtime::deallocate_stack(stack)
            },
        ]
    }
}

impl Drop for ThreadResources {
    fn drop(&mut self) {
        let _statuses = self.release_handles();
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
        Self { data, ops }
    }

    /// Returns the opaque OS-owned value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }

    pub(crate) const fn as_view(&self) -> ThreadExtensionView {
        ThreadExtensionView {
            data: self.data,
            ops: self.ops,
        }
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
    extension: Option<ThreadExtension>,
    resources: ThreadResources,
}

impl ThreadSpec {
    /// Creates a thread specification with full topology affinity.
    pub const fn new(policy: SchedulePolicy) -> Self {
        Self {
            policy,
            affinity: None,
            extension: None,
            resources: ThreadResources::NONE,
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

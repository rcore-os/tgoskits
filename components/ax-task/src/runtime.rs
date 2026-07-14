//! Operating-system capability boundary used by the scheduler.
//!
//! The boundary deliberately passes only scalar values, function pointers and
//! transparent opaque handles. Ownership of contexts, stacks and address
//! spaces remains in the runtime that created them.

use trait_ffi::def_extern_trait;

macro_rules! opaque_handle {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[repr(transparent)]
        pub struct $name(usize);

        impl $name {
            /// Sentinel returned before the corresponding runtime object exists.
            pub const NONE: Self = Self(0);

            /// Creates a handle from the runtime-owned opaque value.
            pub const fn from_raw(raw: usize) -> Self {
                Self(raw)
            }

            /// Returns the runtime-owned opaque value.
            pub const fn into_raw(self) -> usize {
                self.0
            }

            /// Returns whether this is the absent-handle sentinel.
            pub const fn is_none(self) -> bool {
                self.0 == 0
            }
        }
    };
}

opaque_handle!(
    /// Opaque pointer-sized handle to the runtime-owned task system.
    TaskSystemHandle
);
opaque_handle!(
    /// Opaque pointer-sized handle to one pinned CPU-local scheduler object.
    CpuLocalHandle
);
opaque_handle!(
    /// Opaque handle to an architecture execution context.
    ExecutionContextHandle
);
opaque_handle!(
    /// Opaque handle to a runtime-owned stack allocation.
    StackHandle
);
opaque_handle!(
    /// Opaque handle to a runtime-owned TLS allocation.
    TlsHandle
);
opaque_handle!(
    /// Opaque handle to a runtime-owned address space.
    AddressSpaceHandle
);
opaque_handle!(
    /// Token returned by the nested IRQ guard service.
    IrqGuardToken
);

/// Logical CPU identifier exchanged with the operating-system runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RuntimeCpuId(u32);

impl RuntimeCpuId {
    /// Creates a logical CPU identifier.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric logical CPU identifier.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Stable runtime operation status used across the trait-ffi boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeStatus {
    /// The operation completed successfully.
    Success         = 0,
    /// The runtime capability has not been initialized.
    NotInitialized  = 1,
    /// A supplied handle is stale or unknown to the runtime.
    InvalidHandle   = 2,
    /// A supplied value violates the runtime contract.
    InvalidArgument = 3,
    /// The runtime cannot allocate the requested resource.
    NoMemory        = 4,
    /// The runtime does not implement this optional capability.
    Unsupported     = 5,
    /// The requested resource is temporarily busy.
    Busy            = 6,
    /// A platform operation failed.
    Platform        = 7,
}

/// Result of an operation that creates one opaque runtime resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RuntimeHandleResult {
    /// Completion status.
    pub status: RuntimeStatus,
    /// New resource handle when `status` is [`RuntimeStatus::Success`].
    pub handle: usize,
}

impl RuntimeHandleResult {
    /// Creates a successful handle result.
    pub const fn success(handle: usize) -> Self {
        Self {
            status: RuntimeStatus::Success,
            handle,
        }
    }

    /// Creates a failed handle result.
    pub const fn failure(status: RuntimeStatus) -> Self {
        Self { status, handle: 0 }
    }
}

/// Stack allocation requirements supplied to the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct StackRequest {
    /// Usable stack bytes, excluding the guard region.
    pub usable_size: usize,
    /// Required stack alignment in bytes.
    pub alignment: usize,
    /// Number of inaccessible guard bytes below the usable range.
    pub guard_size: usize,
}

/// Kernel context entry point.
///
/// Per-thread arguments remain in scheduler-owned thread metadata and are
/// recovered by the entry trampoline through the current-thread facade. This
/// matches the four architecture `TaskContext::init` contracts, which enter a
/// fresh context without a portable argument register contract.
pub type KernelEntry = unsafe extern "C" fn() -> !;

/// Architecture-neutral request for a new kernel execution context.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct KernelContextRequest {
    /// Runtime-owned stack backing the context.
    pub stack: StackHandle,
    /// Initial instruction entry point.
    pub entry: KernelEntry,
    /// Optional TLS allocation.
    pub tls: TlsHandle,
    /// Optional address space installed before first entry.
    pub address_space: AddressSpaceHandle,
}

/// Architecture-neutral request for a context that will enter userspace.
///
/// The initial entry is still a trusted runtime trampoline; unlike a kernel
/// context, the request must name the address space that trampoline will enter.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct UserContextRequest {
    /// Runtime-owned stack backing the trusted entry trampoline.
    pub stack: StackHandle,
    /// Initial trusted instruction entry point.
    pub entry: KernelEntry,
    /// Optional TLS allocation.
    pub tls: TlsHandle,
    /// Mandatory user address space installed before first entry.
    pub address_space: AddressSpaceHandle,
}

/// Allocation requirements for a thread-local storage area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TlsRequest {
    /// TLS template start, or zero when no template is required.
    pub template_start: usize,
    /// Bytes copied from the template.
    pub initialized_size: usize,
    /// Total allocation size including zero-filled bytes.
    pub total_size: usize,
    /// Required allocation alignment.
    pub alignment: usize,
}

/// Allocation-free scheduler switch diagnostic record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SchedSwitchRecord {
    /// Logical CPU performing the switch.
    pub cpu: RuntimeCpuId,
    /// Previous generation-based thread identifier encoded as a scalar.
    pub previous_thread: u64,
    /// Next generation-based thread identifier encoded as a scalar.
    pub next_thread: u64,
    /// Monotonic switch timestamp.
    pub timestamp_ns: u64,
    /// Policy-specific reason code defined by ax-task.
    pub reason: u32,
}

/// OS capabilities needed by the scheduling core.
///
/// Implementations must keep task-system and CPU-local handles valid until
/// shutdown. All IRQ-path methods must be allocation-free and non-blocking.
#[def_extern_trait(mod_path = "runtime", abi = "rust")]
pub trait TaskRuntime {
    /// Returns the runtime-owned task-system handle, or `NONE` before setup.
    fn task_system_handle() -> TaskSystemHandle;

    /// Returns the pinned CPU-local object for the calling CPU.
    fn current_cpu_local_handle() -> CpuLocalHandle;

    /// Returns the pinned CPU-local object for `cpu`, or `NONE` while offline.
    fn cpu_local_handle(cpu: RuntimeCpuId) -> CpuLocalHandle;

    /// Returns the calling CPU's logical identifier.
    fn current_cpu_id() -> RuntimeCpuId;

    /// Returns the number of CPUs published online to the scheduler.
    fn online_cpu_count() -> u32;

    /// Saves raw interrupt state, disables local IRQs and enters nested guards.
    fn irq_guard_enter() -> IrqGuardToken;

    /// Leaves one nested IRQ guard and restores the outer raw state if needed.
    ///
    /// # Safety
    ///
    /// `token` must have been returned by `irq_guard_enter` on this CPU and
    /// must be exited exactly once. Tokens may be exited in non-LIFO order.
    unsafe fn irq_guard_exit(token: IrqGuardToken);

    /// Consumes the scheduler IRQ-guard baton on a freshly entered context.
    ///
    /// A resumed context consumes the baton by dropping the guard suspended on
    /// its own stack. A fresh context has no guard object, so its trampoline
    /// calls this hook exactly once after completing scheduler switch tail.
    fn finish_initial_context_switch();

    /// Prevents the outgoing thread from nesting another scheduler frame.
    ///
    /// Unlike the IRQ-guard baton, this nesting belongs to the execution
    /// context. The runtime must save it with the outgoing context and restore
    /// the incoming context's own value in `switch_context`.
    fn scheduler_frame_guard_enter();

    /// Leaves the context-local scheduler-frame guard after a switch returns.
    ///
    /// This hook must only update nesting. It must not schedule recursively;
    /// the caller still has context-switch tail work and an IRQ guard to unwind.
    fn scheduler_frame_guard_exit();

    /// Returns whether execution is currently inside a hard interrupt.
    fn in_hard_irq() -> bool;

    /// Returns monotonic time in nanoseconds.
    fn monotonic_ns() -> u64;

    /// Returns the smallest programmable timer interval in nanoseconds.
    fn timer_resolution_ns() -> u64;

    /// Programs the local one-shot timer for an absolute monotonic deadline.
    fn program_oneshot_timer(deadline_ns: u64) -> RuntimeStatus;

    /// Sends a coalescible scheduler IPI directly to `cpu`.
    fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus;

    /// Waits for a local interrupt after the scheduler's idle handshake.
    fn wait_for_interrupt();

    /// Allocates a guarded stack satisfying `request`.
    fn allocate_stack(request: StackRequest) -> RuntimeHandleResult;

    /// Releases a stack after the reaper proves no context can reference it.
    fn deallocate_stack(stack: StackHandle) -> RuntimeStatus;

    /// Allocates a TLS area satisfying `request`.
    fn allocate_tls(request: TlsRequest) -> RuntimeHandleResult;

    /// Releases a TLS area after its execution context has been destroyed.
    fn deallocate_tls(tls: TlsHandle) -> RuntimeStatus;

    /// Creates a kernel execution context.
    fn create_kernel_context(request: KernelContextRequest) -> RuntimeHandleResult;

    /// Creates a user-capable execution context with a mandatory address space.
    fn create_user_context(request: UserContextRequest) -> RuntimeHandleResult;

    /// Destroys an execution context that cannot be scheduled again.
    fn destroy_context(context: ExecutionContextHandle) -> RuntimeStatus;

    /// Switches from `previous` to `next` with local interrupts disabled.
    ///
    /// # Safety
    ///
    /// Both handles must identify live contexts owned by the runtime. The
    /// caller must have committed scheduler state and released runqueue locks.
    unsafe fn switch_context(previous: ExecutionContextHandle, next: ExecutionContextHandle);

    /// Installs the next context's address space before its switch-in hook.
    ///
    /// [`AddressSpaceHandle::NONE`] is an active request to restore the
    /// runtime's kernel-only address-space state; it must not inherit the
    /// previous user context's translation root.
    fn install_address_space(address_space: AddressSpaceHandle) -> RuntimeStatus;

    /// Flushes the current address space's local translation cache.
    fn flush_tlb_local(start: usize, size: usize);

    /// Emits an allocation-free context-switch trace record.
    fn trace_sched_switch(record: SchedSwitchRecord);

    /// Reports an unrecoverable scheduler invariant and terminates execution.
    fn fatal_invariant(code: u32, argument: usize) -> !;
}

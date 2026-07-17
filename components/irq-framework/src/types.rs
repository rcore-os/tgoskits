use alloc::boxed::Box;
use core::{cell::UnsafeCell, mem::MaybeUninit, sync::atomic::{AtomicU8, Ordering}};

/// An IRQ controller domain id.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IrqDomainId(pub u16);

/// Hardware interrupt line number within an IRQ domain.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HwIrq(pub u32);

/// A framework IRQ id, scoped by controller domain.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IrqId {
    /// IRQ controller domain.
    pub domain: IrqDomainId,
    /// Hardware interrupt line within the domain.
    pub hwirq: HwIrq,
}

impl IrqId {
    /// Creates an IRQ id from a domain and hardware line.
    pub const fn new(domain: IrqDomainId, hwirq: HwIrq) -> Self {
        Self { domain, hwirq }
    }
}

/// CPU trap vector observed at the architecture trap boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TrapVector(pub usize);

/// A firmware or controller interrupt source that can be resolved to [`IrqId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqSource {
    /// ACPI Global System Interrupt.
    AcpiGsi(u32),
    /// ACPI Global System Interrupt with explicit routing metadata.
    AcpiGsiRoute(AcpiGsiRoute),
    /// Explicit controller-domain line.
    ControllerLine {
        /// IRQ controller domain.
        domain: IrqDomainId,
        /// Hardware interrupt line within the domain.
        hwirq: HwIrq,
    },
}

/// ACPI IRQ trigger configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpiIrqTrigger {
    /// Edge-triggered interrupt.
    Edge,
    /// Level-triggered interrupt.
    Level,
}

/// ACPI IRQ polarity configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpiIrqPolarity {
    /// Active-high interrupt.
    ActiveHigh,
    /// Active-low interrupt.
    ActiveLow,
}

/// ACPI GSI controller kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpiGsiController {
    /// I/O APIC controller.
    IoApic,
    /// LoongArch PCH-PIC controller.
    PchPic,
}

/// Fully described firmware-owned ACPI GSI routing metadata.
///
/// CPU vector allocation belongs to the target interrupt controller and is
/// deliberately not represented here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcpiGsiRoute {
    /// Global System Interrupt number.
    pub gsi: u32,
    /// Controller kind.
    pub controller: AcpiGsiController,
    /// ACPI controller id.
    pub controller_id: u16,
    /// Controller MMIO base address.
    pub controller_address: u64,
    /// Controller-local input line.
    pub controller_input: u8,
    /// Trigger configuration.
    pub trigger: AcpiIrqTrigger,
    /// Polarity configuration.
    pub polarity: AcpiIrqPolarity,
}

/// A logical CPU id.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CpuId(pub usize);

/// Logical CPU selection for one inter-processor interrupt transaction.
///
/// Hardware identifiers are deliberately absent from this type. The platform
/// resolves [`CpuId`] through immutable boot metadata immediately before the
/// architecture send primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuIpiTarget {
    /// Deliver to the CPU executing the send operation.
    Current {
        /// Logical identity of the current CPU, used for contract validation.
        cpu: CpuId,
    },
    /// Deliver to one logical CPU.
    Other {
        /// Logical identity of the destination CPU.
        cpu: CpuId,
    },
    /// Deliver to every configured CPU except the caller.
    ///
    /// Platforms without a hardware broadcast command may deliver this as a
    /// bounded best-effort sequence. Repeating the transaction after
    /// [`IpiSendStatus::Retry`] can therefore redeliver to an earlier CPU;
    /// callers must coalesce the work carried by a broadcast doorbell.
    AllExceptCurrent {
        /// Logical identity of the current CPU.
        current: CpuId,
        /// Number of logical CPUs covered by this transaction.
        cpu_count: usize,
    },
}

/// Result of a bounded, allocation-free IPI send attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IpiSendStatus {
    /// The architecture accepted the interrupt request.
    Success = 0,
    /// The transport was temporarily unable to accept the request.
    Retry   = 1,
    /// The IRQ or CPU target does not satisfy the platform contract.
    Invalid = 2,
}

/// A compact CPU mask for low-level IRQ affinity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuMask {
    bits: u128,
}

impl CpuMask {
    /// Creates an empty CPU mask.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Creates a CPU mask containing a single CPU.
    pub fn from_cpu(cpu: CpuId) -> Self {
        let mut mask = Self::empty();
        mask.insert(cpu);
        mask
    }

    /// Creates a CPU mask containing CPUs in the range `0..cpu_count`.
    pub fn first_n(cpu_count: usize) -> Self {
        let mut mask = Self::empty();
        let end = cpu_count.min(u128::BITS as usize);
        for cpu in 0..end {
            mask.insert(CpuId(cpu));
        }
        mask
    }

    /// Adds a CPU to this mask.
    pub fn insert(&mut self, cpu: CpuId) {
        if cpu.0 < u128::BITS as usize {
            self.bits |= 1u128 << cpu.0;
        }
    }

    /// Removes a CPU from this mask.
    pub fn remove(&mut self, cpu: CpuId) {
        if cpu.0 < u128::BITS as usize {
            self.bits &= !(1u128 << cpu.0);
        }
    }

    /// Returns whether the CPU is present in this mask.
    pub const fn contains(self, cpu: CpuId) -> bool {
        cpu.0 < u128::BITS as usize && (self.bits & (1u128 << cpu.0)) != 0
    }

    /// Returns whether no CPU is present in this mask.
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Iterates over the CPUs in this mask.
    pub fn iter(self) -> CpuMaskIter {
        CpuMaskIter { bits: self.bits }
    }
}

/// Iterator over [`CpuMask`].
pub struct CpuMaskIter {
    bits: u128,
}

impl Iterator for CpuMaskIter {
    type Item = CpuId;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bits == 0 {
            return None;
        }
        let cpu = self.bits.trailing_zeros() as usize;
        self.bits &= !(1u128 << cpu);
        Some(CpuId(cpu))
    }
}

/// IRQ registration scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqScope {
    /// The action is visible on every CPU.
    Global,
    /// The action is CPU-local and only visible to matching CPUs.
    PerCpu {
        /// Target CPUs.
        cpus: CpuMask,
    },
}

/// Hardware routing preference for an IRQ line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqAffinity {
    /// The platform may route the line to any CPU.
    Any,
    /// Route the line to one fixed logical CPU.
    Fixed(CpuId),
}

/// Execution contract for an IRQ action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqExecution {
    /// The handler may run concurrently if the controller delivers it that way.
    Concurrent,
    /// The framework prevents nested/concurrent calls to this action.
    NonReentrant,
}

/// Whether an IRQ line is exclusive or shared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareMode {
    /// No other action can share the IRQ.
    Exclusive,
    /// Multiple actions can share the IRQ.
    Shared,
}

/// Whether an IRQ action should be enabled after registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoEnable {
    /// Register the action but leave it disabled.
    No,
    /// Enable the action after registration.
    Yes,
}

/// Return value from a raw IRQ handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqReturn {
    /// This action did not handle the IRQ.
    Unhandled,
    /// This action handled the IRQ.
    Handled,
    /// This action handled the IRQ and asks the OS adapter to wake deferred work.
    Wake,
    /// This action needs one ordinary task-side acknowledgement continuation.
    ///
    /// The framework masks the shared backing line before invoking `wake` and
    /// keeps it masked until the exact generation-bearing token is consumed by
    /// [`crate::Registry::finish_continuation`]. The action remains enabled;
    /// this is separate from the fail-closed [`Self::QuenchAndWake`] path.
    ///
    /// The endpoint may return this only when its device remains level-asserted
    /// or its interrupt controller retains and replays arrivals observed while
    /// masked (for example an MSI-X PBA). Edge sources without such a latch
    /// must fail closed instead; the framework never invents a lost edge.
    Defer(&'static IrqContinuationWake),
    /// This action hit a fail-closed condition and must stop receiving IRQs.
    ///
    /// For a global action, the registry disables the returning action and
    /// masks the complete backing line before dispatch ends. For a per-CPU
    /// action, it masks only the CPU that observed the failure; the same action
    /// remains eligible on unaffected CPUs. The OS adapter must wake recovery,
    /// mask the device source, and explicitly release this action's quench
    /// ownership before the affected line can reopen.
    QuenchAndWake,
}

/// Aggregated dispatch result.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrqOutcome {
    /// At least one action handled the IRQ.
    pub handled: bool,
    /// At least one action requested a wakeup.
    pub wake: bool,
    /// Number of handlers called by this dispatch.
    pub called: usize,
}

/// IRQ status snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqStatus {
    /// Whether this action's framework gate is enabled.
    ///
    /// A per-CPU action can remain enabled here while one CPU is independently
    /// quenched and masked.
    pub action_enabled: bool,
    /// Whether this action owns an emergency backing-line quench.
    pub quench_owned: bool,
    /// Whether an ordinary acknowledgement continuation owns the line.
    pub continuation_pending: bool,
    /// Whether the platform line is enabled.
    pub line_enabled: bool,
    /// Whether the platform reports the IRQ pending.
    pub pending: bool,
    /// Whether the platform reports the IRQ in service.
    pub in_service: bool,
    /// Number of in-flight dispatches for this descriptor.
    pub in_flight: usize,
    /// Whether this action is currently running.
    pub action_running: bool,
}

/// Preallocated notification invoked when one disabled IRQ action is drained.
///
/// The callback can run in hard-IRQ context as the final invocation of the
/// selected action returns. It must not allocate, block, free callback-owned
/// storage, or invoke arbitrary user code. Queueing a fixed work item is the
/// intended use. The framework holds no registry metadata lock while invoking
/// it and keeps the descriptor pinned against detach/free until it returns.
pub struct IrqDrainWake {
    data: usize,
    wake: unsafe fn(usize),
}

/// Preallocated token handoff invoked after a deferred IRQ action is masked.
pub struct IrqContinuationWake {
    data: usize,
    wake: unsafe fn(usize, IrqContinuationToken),
}

impl core::fmt::Debug for IrqContinuationWake {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqContinuationWake")
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

impl PartialEq for IrqContinuationWake {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data && core::ptr::fn_addr_eq(self.wake, other.wake)
    }
}

impl Eq for IrqContinuationWake {}

impl IrqContinuationWake {
    /// Creates a shutdown-lifetime deferred IRQ notification target.
    ///
    /// # Safety
    ///
    /// `data` and the callback must remain valid for shutdown lifetime. The
    /// callback runs in hard IRQ context after the backing line is masked. It
    /// must publish the token into preallocated storage without allocation,
    /// blocking, freeing, arbitrary callbacks, or IRQ-registry reentry.
    pub const unsafe fn new(
        data: usize,
        wake: unsafe fn(usize, IrqContinuationToken),
    ) -> Self {
        Self { data, wake }
    }

    pub(crate) fn notify(&self, token: IrqContinuationToken) {
        unsafe {
            // SAFETY: construction binds this callback to shutdown-lifetime
            // data and the registry invokes it only after recording the exact
            // action generation and masking its backing line.
            (self.wake)(self.data, token);
        }
    }
}

impl IrqDrainWake {
    /// Creates a stable action-drain notification target.
    ///
    /// # Safety
    ///
    /// The callback and `data` must remain valid for shutdown lifetime. The
    /// callback must be safe to invoke concurrently from hard-IRQ context with
    /// exactly this `data` value. It must not allocate, block, free
    /// callback-owned storage, invoke arbitrary user code, or reenter the IRQ
    /// registry that delivered the notification.
    pub const unsafe fn new(data: usize, wake: unsafe fn(usize)) -> Self {
        Self { data, wake }
    }

    pub(crate) fn notify(&self) {
        unsafe {
            // SAFETY: construction assigns the callback its data contract.
            // `disable_async` requires this object to remain static, and the
            // callback's documented context rules are upheld by its provider.
            (self.wake)(self.data);
        }
    }
}

/// Generation-bearing proof that one selected IRQ action was disabled.
///
/// Descriptor peers on a shared line are deliberately outside this token. A
/// completed token proves that the selected handler is drained and its wake
/// callback has returned; it does not prove that descriptor peers are drained
/// or that the descriptor is detachable. The wake target itself has
/// independent shutdown lifetime and is not owned by this copyable token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqDrainToken {
    pub(crate) handle: IrqHandle,
    pub(crate) epoch: u64,
}

/// Linear completion capability for one deferred IRQ action generation.
///
/// This type is deliberately neither `Copy` nor `Clone`. Losing it leaves the
/// action masked; it can never accidentally complete a later generation.
#[derive(Debug, Eq, PartialEq)]
#[must_use = "a deferred IRQ continuation token keeps its backing line masked"]
pub struct IrqContinuationToken {
    pub(crate) handle: IrqHandle,
    pub(crate) epoch: u64,
}

impl IrqContinuationToken {
    /// Returns the registered action identity without exposing its generation.
    pub const fn action(&self) -> IrqHandle {
        self.handle
    }
}

const CONTINUATION_SLOT_EMPTY: u8 = 0;
const CONTINUATION_SLOT_WRITING: u8 = 1;
const CONTINUATION_SLOT_READY: u8 = 2;
const CONTINUATION_SLOT_READING: u8 = 3;

/// Fixed single-token handoff from hard IRQ to one continuation worker.
///
/// The framework masks the action's backing line before the sole producer
/// publishes. The sole consumer must either finish the token or restore it
/// before another generation can be delivered.
pub struct IrqContinuationSlot {
    state: AtomicU8,
    token: UnsafeCell<MaybeUninit<IrqContinuationToken>>,
}

// Access is serialized by the explicit four-state SPSC protocol. A live
// token's IRQ action keeps the producer line masked until the consumer either
// restores or finishes that token.
unsafe impl Sync for IrqContinuationSlot {}

impl IrqContinuationSlot {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(CONTINUATION_SLOT_EMPTY),
            token: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Publishes the framework-minted token without allocation or blocking.
    pub fn publish(
        &self,
        token: IrqContinuationToken,
    ) -> Result<(), IrqContinuationToken> {
        if self
            .state
            .compare_exchange(
                CONTINUATION_SLOT_EMPTY,
                CONTINUATION_SLOT_WRITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return Err(token);
        }
        unsafe {
            // SAFETY: WRITING grants this sole producer exclusive slot access.
            (*self.token.get()).write(token);
        }
        self.state.store(CONTINUATION_SLOT_READY, Ordering::Release);
        Ok(())
    }

    /// Takes the currently published token for bounded task-side service.
    pub fn take(&self) -> Option<IrqContinuationToken> {
        if self
            .state
            .compare_exchange(
                CONTINUATION_SLOT_READY,
                CONTINUATION_SLOT_READING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return None;
        }
        let token = unsafe {
            // SAFETY: READY publication initialized the slot and READING gives
            // this sole consumer exclusive ownership of that value.
            (*self.token.get()).assume_init_read()
        };
        self.state.store(CONTINUATION_SLOT_EMPTY, Ordering::Release);
        Some(token)
    }

    /// Restores a contended continuation without minting a new generation.
    pub fn restore(&self, token: IrqContinuationToken) -> Result<(), IrqContinuationToken> {
        self.publish(token)
    }

    pub fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == CONTINUATION_SLOT_READY
    }
}

impl Default for IrqContinuationSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// IRQ framework errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqError {
    /// Invalid IRQ number.
    InvalidIrq,
    /// Invalid CPU id.
    InvalidCpu,
    /// The target CPU is offline.
    CpuOffline,
    /// A synchronous IRQ operation timed out.
    Timeout,
    /// IRQ line/action sharing rules reject the operation.
    Busy,
    /// Allocation failed.
    NoMemory,
    /// The requested descriptor or action does not exist.
    NotFound,
    /// This operation is not legal from IRQ context.
    InIrqContext,
    /// The platform adapter does not support this operation.
    Unsupported,
    /// The platform controller reported an error.
    Controller,
}

/// Context passed to IRQ handlers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqContext {
    /// IRQ number being dispatched.
    pub irq: IrqId,
    /// CPU handling the IRQ.
    pub cpu: CpuId,
}

/// Owned hard-IRQ endpoint invoked by [`crate::Registry::dispatch`].
///
/// Registration transfers endpoint ownership to the framework. Dispatch calls
/// only these registered endpoints and never invents an empty/default handler.
/// The callback runs in hard-IRQ context and must be bounded: it must not
/// allocate, free, block, or call arbitrary user code. Reading/acknowledging
/// the device source and publishing a snapshot into preallocated state is the
/// intended use.
pub type BoxedIrqHandler = Box<dyn FnMut(IrqContext) -> IrqReturn + Send + 'static>;

/// Owned hard-IRQ endpoint for callbacks that may run concurrently.
///
/// This has the same bounded, allocation-free and non-blocking callback
/// contract as [`BoxedIrqHandler`].
pub type ConcurrentBoxedIrqHandler = Box<dyn Fn(IrqContext) -> IrqReturn + Send + Sync + 'static>;

pub(crate) enum IrqHandler {
    NonReentrant(BoxedIrqHandler),
    Concurrent(ConcurrentBoxedIrqHandler),
}

/// External capabilities supplied by the OS/platform adapter.
///
/// # Safety
///
/// Implementations must preserve the lifetime and CPU-ownership contracts of
/// [`IrqOps::run_on_cpu_sync`]. In particular, a returned call must never leave
/// a deferred invocation that can retain or use its raw argument. When an
/// implementation is [`Sync`], every operation must additionally be safe to
/// invoke concurrently from different CPUs.
pub unsafe trait IrqOps {
    /// Saved local IRQ state.
    type LocalIrqState;

    /// Returns a transient snapshot of the current CPU for status reporting.
    ///
    /// The returned ID does not pin execution and must not be used to choose
    /// whether a CPU-owned operation runs locally or remotely. Such operations
    /// must use [`IrqOps::run_on_cpu_sync`].
    fn current_cpu(&self) -> CpuId;

    /// Returns whether the CPU is online.
    fn cpu_online(&self, cpu: CpuId) -> bool;

    /// Returns whether the current execution context is an IRQ context.
    fn in_irq_context(&self) -> bool;

    /// Saves and disables local IRQs for metadata lock acquisition.
    fn local_irq_save(&self) -> Self::LocalIrqState;

    /// Restores local IRQ state saved by [`IrqOps::local_irq_save`].
    fn local_irq_restore(&self, state: Self::LocalIrqState);

    /// Runs a CPU-owned thunk synchronously on the target CPU.
    ///
    /// Implementations must make the local-versus-remote decision while the
    /// caller is pinned. A local thunk must run under that same pin. A remote
    /// request from IRQ context must return [`IrqError::InIrqContext`] rather
    /// than attempting a cross-CPU rendezvous. On success, `f(arg)` must have
    /// completed exactly once on `cpu`. On failure it may have completed or not,
    /// but the implementation must not invoke `f` after this method returns.
    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError>;

    /// Routes a global IRQ line to the requested CPU affinity.
    fn set_affinity(&self, _irq: IrqId, _affinity: IrqAffinity) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }

    /// Enables or disables an IRQ line.
    ///
    /// Disabling the current global or per-CPU line is part of the emergency
    /// [`IrqReturn::QuenchAndWake`] path. That operation must therefore be
    /// bounded, allocation-free, non-blocking, and callable from hard-IRQ
    /// context. A remote per-CPU update is reached only through
    /// [`IrqOps::run_on_cpu_sync`].
    fn set_enabled(&self, irq: IrqId, cpu: Option<CpuId>, enabled: bool) -> Result<(), IrqError>;

    /// Returns whether the IRQ line is enabled.
    fn is_enabled(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Returns whether the IRQ line is pending.
    fn is_pending(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Returns whether the IRQ line is in service.
    fn is_in_service(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Relaxes a spin wait.
    fn relax(&self);
}

/// Request parameters for an IRQ action.
pub struct IrqRequest {
    pub(crate) handler: Option<IrqHandler>,
    pub(crate) scope: IrqScope,
    pub(crate) affinity: IrqAffinity,
    pub(crate) execution: IrqExecution,
    pub(crate) share_mode: ShareMode,
    pub(crate) auto_enable: AutoEnable,
}

impl IrqRequest {
    /// Creates a new exclusive, global IRQ request that starts disabled.
    ///
    /// Registration only publishes ownership and routing metadata. Callers
    /// must finish binding device-side state and then call
    /// [`crate::Registry::enable`] explicitly.
    pub fn new(handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static) -> Self {
        Self {
            handler: Some(IrqHandler::NonReentrant(Box::new(handler))),
            scope: IrqScope::Global,
            affinity: IrqAffinity::Any,
            execution: IrqExecution::NonReentrant,
            share_mode: ShareMode::Exclusive,
            auto_enable: AutoEnable::No,
        }
    }

    /// Creates a new exclusive, global concurrent IRQ request that starts
    /// disabled.
    ///
    /// Callers must explicitly enable the returned action after all
    /// device-side state visible to the handler has been published.
    pub fn new_concurrent(
        handler: impl Fn(IrqContext) -> IrqReturn + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Some(IrqHandler::Concurrent(Box::new(handler))),
            scope: IrqScope::Global,
            affinity: IrqAffinity::Any,
            execution: IrqExecution::Concurrent,
            share_mode: ShareMode::Exclusive,
            auto_enable: AutoEnable::No,
        }
    }

    pub(crate) fn supports_concurrent(&self) -> bool {
        matches!(self.handler.as_ref(), Some(IrqHandler::Concurrent(_)))
    }

    /// Sets the IRQ scope.
    pub fn scope(mut self, scope: IrqScope) -> Self {
        self.scope = scope;
        self
    }

    /// Sets the IRQ affinity.
    pub fn affinity(mut self, affinity: IrqAffinity) -> Self {
        self.affinity = affinity;
        self
    }

    /// Sets the action execution contract.
    pub fn execution(mut self, execution: IrqExecution) -> Self {
        self.execution = execution;
        self
    }

    /// Sets the sharing mode.
    pub fn share_mode(mut self, share_mode: ShareMode) -> Self {
        self.share_mode = share_mode;
        self
    }

    /// Sets whether the action should be enabled after request.
    pub fn auto_enable(mut self, auto_enable: AutoEnable) -> Self {
        self.auto_enable = auto_enable;
        self
    }

    /// Returns whether the action should be enabled after request.
    pub const fn auto_enable_mode(&self) -> AutoEnable {
        self.auto_enable
    }
}

/// Token returned from request and used for later lifecycle operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqHandle {
    pub(crate) irq: IrqId,
    pub(crate) id: u64,
}

impl IrqHandle {
    /// Returns the IRQ number associated with this handle.
    pub const fn irq(self) -> IrqId {
        self.irq
    }

    /// Returns the framework-local action id.
    pub const fn id(self) -> u64 {
        self.id
    }
}

use alloc::boxed::Box;

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

/// Generation-checked handle to one prepared IRQ-chip line endpoint.
///
/// The platform owns the backing endpoint in shutdown-lifetime storage. The
/// framework stores only this value-only key, so no pointer provenance or
/// controller driver object crosses the runtime boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IrqLineBinding {
    slot: u32,
    reserved: u32,
    generation: u64,
}

impl IrqLineBinding {
    /// Creates a validated binding key. Generation zero is permanently
    /// reserved for uninitialized storage.
    pub const fn new(slot: u32, generation: u64) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self {
                slot,
                reserved: 0,
                generation,
            })
        }
    }

    /// Returns the platform arena slot.
    pub const fn slot(self) -> u32 {
        self.slot
    }

    /// Returns the exact slot generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Live control available for a prepared IRQ line.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqLineControl {
    /// The prepared irq-chip endpoint can physically mask and unmask the line.
    Maskable       = 0,
    /// The architecture vector is always live; only the framework action gate
    /// can suppress callback delivery.
    ActionGateOnly = 1,
}

/// Result of the sole fallible IRQ-chip preparation transaction.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedIrqLine {
    binding: IrqLineBinding,
    control: IrqLineControl,
    reserved: [u8; 7],
}

/// Linear proof that a prepared IRQ-chip line generation was released.
///
/// The proof is created only after the platform release transaction succeeds
/// and the framework commits the descriptor back to its unbound state. It is
/// intentionally neither cloneable nor copyable: upper layers can retain it as
/// evidence that the former host action no longer owns a controller endpoint.
#[must_use = "released IRQ line ownership should be transferred or explicitly discarded"]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasedIrqLineProof {
    irq: IrqId,
    released_binding: IrqLineBinding,
}

impl ReleasedIrqLineProof {
    pub(crate) const fn new(irq: IrqId, released_binding: IrqLineBinding) -> Self {
        Self {
            irq,
            released_binding,
        }
    }

    /// Returns the framework IRQ whose old platform endpoint was released.
    pub const fn irq(&self) -> IrqId {
        self.irq
    }

    /// Returns the exact retired platform binding generation.
    pub const fn released_binding(&self) -> IrqLineBinding {
        self.released_binding
    }
}

impl PreparedIrqLine {
    /// Creates a prepared line capability.
    pub const fn new(binding: IrqLineBinding, control: IrqLineControl) -> Self {
        Self {
            binding,
            control,
            reserved: [0; 7],
        }
    }

    /// Returns the generation-checked platform binding.
    pub const fn binding(self) -> IrqLineBinding {
        self.binding
    }

    /// Returns the line's live control mode.
    pub const fn control(self) -> IrqLineControl {
        self.control
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
    /// This action handled the IRQ after explicitly publishing its target wake.
    ///
    /// The framework does not choose or invoke a worker for the action. This
    /// result only records that IRQ return must observe scheduler work already
    /// published by the endpoint's OS glue, for example through a stable
    /// thread wake handle.
    Wake,
    /// This action handled the IRQ, disabled its device-local source, and must
    /// stop receiving callbacks until task-side recovery explicitly enables it.
    ///
    /// The framework disables only this action. It deliberately keeps the
    /// backing line available to unrelated actions sharing that line. Before
    /// returning this value, the endpoint must have precisely masked or
    /// otherwise deasserted its own device source. The only exception is an
    /// exclusive action whose disable transition necessarily closes the sole
    /// backing line. Failing either proof can cause an interrupt storm because
    /// a shared controller line stays open.
    /// For a per-CPU action, the framework suppresses only the observing CPU's
    /// action instance and closes that CPU's unowned line. Healthy CPU
    /// instances remain enabled; task-side recovery may call [`Registry::enable`](crate::Registry::enable)
    /// to clear all local suppression after repairing the source.
    DisableActionAndWake,
    /// This action hit a condition that cannot be isolated at the device source.
    ///
    /// The framework records quench ownership and masks the entire affected
    /// controller line before dispatch ends. For a global action this closes the
    /// shared backing line; for a per-CPU action it closes only the observing
    /// CPU's line instance. Recovery must mask or reset the device source and
    /// explicitly release quench ownership. The action itself remains enabled.
    MaskLineAndWake,
}

/// Aggregated dispatch result.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrqOutcome {
    /// At least one action handled the IRQ.
    pub handled: bool,
    /// At least one action reported a wake already published by its OS glue.
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
    /// Whether the prepared line's framework-applied state is enabled.
    pub line_enabled: bool,
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
/// invoke concurrently from different CPUs. A binding returned by
/// [`IrqOps::prepare_line`] must identify shutdown-lifetime storage whose live
/// operation satisfies the bounded, allocation-free, non-blocking and
/// infallible contract of [`IrqOps::set_line_enabled`]. Generation, scope and
/// CPU-owner mismatches are fatal implementation invariants; they must never be
/// accepted as operations on another hardware line.
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
    /// completed exactly once on `cpu`. On failure `f` must not have begun and
    /// must never be invoked later. This strict cancellation boundary lets the
    /// framework keep controller state transactional without retaining `arg`.
    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError>;

    /// Resolves, validates, routes, and leases one IRQ-chip line endpoint.
    ///
    /// This task-context-only preparation is the sole fallible controller
    /// ownership transition. It completes lookup, routing, capability
    /// validation and endpoint publication. A global line must be physically
    /// masked before success. A per-CPU binding is validated here and then
    /// masked on each online target CPU as a must-succeed commit phase before
    /// the descriptor publishes an action. Failure must leave the device source
    /// fail-closed and must not publish a binding that can later alias a retry.
    fn prepare_line(
        &self,
        irq: IrqId,
        scope: IrqScope,
        affinity: IrqAffinity,
    ) -> Result<PreparedIrqLine, IrqError>;

    /// Enables or disables an already prepared IRQ-chip line.
    ///
    /// The primitive must be bounded, allocation-free, and non-blocking while
    /// local IRQs are disabled. It is infallible after preparation: invalid
    /// generations, unavailable controller ownership, or hardware access
    /// failure are fatal platform invariants and must be contained below this
    /// capability. A remote per-CPU update is reached only through
    /// [`IrqOps::run_on_cpu_sync`]. `cpu` is `None` for a global binding and
    /// `Some(target)` for a per-CPU binding; the latter must execute on exactly
    /// `target`.
    fn set_line_enabled(&self, binding: IrqLineBinding, cpu: Option<CpuId>, enabled: bool);

    /// Releases a globally masked prepared IRQ-chip line generation.
    ///
    /// The framework calls this task-context operation only after reserving the
    /// descriptor against registration and proving that its sole action is
    /// disabled and drained, the line is masked, and no framework controller
    /// claim is active. Implementations must independently synchronize any
    /// controller claim that can race after that reservation. Success retires
    /// exactly `binding`; failure must leave that same binding usable for a
    /// retry or action re-enable.
    ///
    /// The default rejects release for platforms that retain irqchip endpoints
    /// until shutdown.
    fn release_line(&self, _binding: IrqLineBinding) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }

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

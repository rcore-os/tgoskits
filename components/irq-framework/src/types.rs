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

/// Fully described ACPI GSI routing metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcpiGsiRoute {
    /// Global System Interrupt number.
    pub gsi: u32,
    /// CPU trap vector programmed by the platform controller, if known.
    pub vector: usize,
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
    /// Whether this action is enabled in the framework.
    pub action_enabled: bool,
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

/// Boxed IRQ handler ABI.
pub type BoxedIrqHandler = Box<dyn FnMut(IrqContext) -> IrqReturn + Send + 'static>;

/// Boxed IRQ handler ABI for callbacks that may run concurrently.
pub type ConcurrentBoxedIrqHandler = Box<dyn Fn(IrqContext) -> IrqReturn + Send + Sync + 'static>;

pub(crate) enum IrqHandler {
    NonReentrant(BoxedIrqHandler),
    Concurrent(ConcurrentBoxedIrqHandler),
}

/// External capabilities supplied by the OS/platform adapter.
pub trait IrqOps {
    /// Saved local IRQ state.
    type LocalIrqState;

    /// Returns the current CPU.
    fn current_cpu(&self) -> CpuId;

    /// Returns whether the CPU is online.
    fn cpu_online(&self, cpu: CpuId) -> bool;

    /// Returns whether the current execution context is an IRQ context.
    fn in_irq_context(&self) -> bool;

    /// Saves and disables local IRQs for metadata lock acquisition.
    fn local_irq_save(&self) -> Self::LocalIrqState;

    /// Restores local IRQ state saved by [`IrqOps::local_irq_save`].
    fn local_irq_restore(&self, state: Self::LocalIrqState);

    /// Runs a thunk synchronously on the target CPU.
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
    /// Creates a new exclusive, global, auto-enabled IRQ request.
    pub fn new(handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static) -> Self {
        Self {
            handler: Some(IrqHandler::NonReentrant(Box::new(handler))),
            scope: IrqScope::Global,
            affinity: IrqAffinity::Any,
            execution: IrqExecution::NonReentrant,
            share_mode: ShareMode::Exclusive,
            auto_enable: AutoEnable::Yes,
        }
    }

    /// Creates a new exclusive, global, auto-enabled concurrent IRQ request.
    pub fn new_concurrent(
        handler: impl Fn(IrqContext) -> IrqReturn + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Some(IrqHandler::Concurrent(Box::new(handler))),
            scope: IrqScope::Global,
            affinity: IrqAffinity::Any,
            execution: IrqExecution::Concurrent,
            share_mode: ShareMode::Exclusive,
            auto_enable: AutoEnable::Yes,
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

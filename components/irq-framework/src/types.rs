use core::ptr::NonNull;

/// A platform IRQ number.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IrqNumber(pub usize);

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
    pub irq: IrqNumber,
    /// CPU handling the IRQ.
    pub cpu: CpuId,
}

/// Raw IRQ handler ABI.
pub type RawIrqHandler = unsafe fn(ctx: IrqContext, data: NonNull<()>) -> IrqReturn;

/// External capabilities supplied by the OS/platform adapter.
pub trait IrqOps {
    /// Saved local IRQ state.
    type LocalIrqState: Copy;

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
    fn set_affinity(&self, _irq: IrqNumber, _affinity: IrqAffinity) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }

    /// Enables or disables an IRQ line.
    fn set_enabled(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError>;

    /// Returns whether the IRQ line is enabled.
    fn is_enabled(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Returns whether the IRQ line is pending.
    fn is_pending(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Returns whether the IRQ line is in service.
    fn is_in_service(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Relaxes a spin wait.
    fn relax(&self);
}

/// Request parameters for an IRQ action.
#[derive(Clone, Copy, Debug)]
pub struct IrqRequest {
    pub(crate) handler: RawIrqHandler,
    pub(crate) data: NonNull<()>,
    pub(crate) scope: IrqScope,
    pub(crate) affinity: IrqAffinity,
    pub(crate) execution: IrqExecution,
    pub(crate) share_mode: ShareMode,
    pub(crate) auto_enable: AutoEnable,
}

impl IrqRequest {
    /// Creates a new exclusive, global, auto-enabled IRQ request.
    pub const fn new(handler: RawIrqHandler, data: NonNull<()>) -> Self {
        Self {
            handler,
            data,
            scope: IrqScope::Global,
            affinity: IrqAffinity::Any,
            execution: IrqExecution::Concurrent,
            share_mode: ShareMode::Exclusive,
            auto_enable: AutoEnable::Yes,
        }
    }

    /// Sets the IRQ scope.
    pub const fn scope(mut self, scope: IrqScope) -> Self {
        self.scope = scope;
        self
    }

    /// Sets the IRQ affinity.
    pub const fn affinity(mut self, affinity: IrqAffinity) -> Self {
        self.affinity = affinity;
        self
    }

    /// Sets the action execution contract.
    pub const fn execution(mut self, execution: IrqExecution) -> Self {
        self.execution = execution;
        self
    }

    /// Sets the sharing mode.
    pub const fn share_mode(mut self, share_mode: ShareMode) -> Self {
        self.share_mode = share_mode;
        self
    }

    /// Sets whether the action should be enabled after request.
    pub const fn auto_enable(mut self, auto_enable: AutoEnable) -> Self {
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
    pub(crate) irq: IrqNumber,
    pub(crate) id: u64,
}

impl IrqHandle {
    /// Returns the IRQ number associated with this handle.
    pub const fn irq(self) -> IrqNumber {
        self.irq
    }

    /// Returns the framework-local action id.
    pub const fn id(self) -> u64 {
        self.id
    }
}

use crate::BindingInfo;

/// Direction of a platform IRQ binding transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqBindingOperation {
    /// Publish the binding for interrupt delivery.
    Enable,
    /// Mask and withdraw the binding from interrupt delivery.
    Disable,
}

/// Hardware or provider stage that rejected an IRQ binding transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqBindingStage {
    /// The retained vector allocation is unavailable.
    Allocation,
    /// The interrupt provider cannot be resolved.
    ProviderLookup,
    /// The interrupt provider cannot be exclusively accessed.
    ProviderLock,
    /// One provider-owned vector transition failed.
    ProviderVector,
    /// One device table entry transition failed.
    TableEntry,
}

/// Stable reason reported by an IRQ binding stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqBindingFailure {
    /// The lease no longer owns an allocation.
    MissingAllocation,
    /// The registered provider identity no longer resolves.
    ProviderNotFound,
    /// The provider exists but no longer exposes the expected interface.
    ProviderUnavailable,
    /// Another owner currently controls the provider.
    ProviderBusy,
    /// A vector index is outside the device table.
    InvalidVector,
    /// The interrupt controller returned a typed IRQ error.
    Irq(irq_framework::IrqError),
}

/// One failed operation on a logical IRQ source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqBindingFault {
    stage: IrqBindingStage,
    source_id: Option<usize>,
    failure: IrqBindingFailure,
}

impl IrqBindingFault {
    /// Creates one typed transition fault.
    pub const fn new(
        stage: IrqBindingStage,
        source_id: Option<usize>,
        failure: IrqBindingFailure,
    ) -> Self {
        Self {
            stage,
            source_id,
            failure,
        }
    }

    /// Returns the hardware/provider stage that failed.
    pub const fn stage(self) -> IrqBindingStage {
        self.stage
    }

    /// Returns the affected logical source, when the failure is source-local.
    pub const fn source_id(self) -> Option<usize> {
        self.source_id
    }

    /// Returns the stable failure category.
    pub const fn failure(self) -> IrqBindingFailure {
        self.failure
    }
}

/// A failed IRQ binding transition and, when applicable, its rollback failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{operation:?} IRQ binding failed at {fault:?}; rollback failure: {rollback_fault:?}")]
pub struct IrqBindingError {
    operation: IrqBindingOperation,
    fault: IrqBindingFault,
    rollback_fault: Option<IrqBindingFault>,
}

impl IrqBindingError {
    /// Creates an error without a rollback failure.
    pub const fn new(operation: IrqBindingOperation, fault: IrqBindingFault) -> Self {
        Self {
            operation,
            fault,
            rollback_fault: None,
        }
    }

    #[cfg(feature = "pci")]
    pub(crate) const fn with_rollback_fault(
        mut self,
        rollback_fault: Option<IrqBindingFault>,
    ) -> Self {
        self.rollback_fault = rollback_fault;
        self
    }

    /// Returns the requested transition direction.
    pub const fn operation(self) -> IrqBindingOperation {
        self.operation
    }

    /// Returns the first transition fault.
    pub const fn fault(self) -> IrqBindingFault {
        self.fault
    }

    /// Returns the first rollback fault, if cleanup was incomplete.
    pub const fn rollback_fault(self) -> Option<IrqBindingFault> {
        self.rollback_fault
    }
}

/// Ownership lease for the platform side of a registered IRQ binding.
///
/// Binding transitions run in activation/teardown worker context and may
/// acquire provider locks. Hard IRQ handlers use their already-published IRQ
/// endpoint and never call this trait.
pub trait IrqBindingLease: Send + 'static {
    /// Returns immutable firmware/bus identity for the retained binding.
    fn binding_info(&self) -> BindingInfo;

    /// Enables the parent/controller side of the binding.
    ///
    /// # Errors
    ///
    /// Returns the first hardware transition failure. Implementations must
    /// roll back every earlier successful transition before returning.
    fn enable_binding_irq(&self) -> Result<(), IrqBindingError>;

    /// Disables the parent/controller side of the binding.
    ///
    /// # Errors
    ///
    /// Returns the first failure after attempting every independent disable
    /// operation.
    fn disable_binding_irq(&self) -> Result<(), IrqBindingError>;
}

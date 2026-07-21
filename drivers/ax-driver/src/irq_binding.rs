use alloc::sync::Arc;
#[cfg(feature = "nvme")]
use alloc::vec::Vec;
use core::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{BindingInfo, BindingIrq, BindingIrqBinding};

#[cfg(feature = "nvme")]
const MAX_EXACT_IRQ_SOURCES: usize = u64::BITS as usize;

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

/// Failure to create or transfer one exact platform IRQ-source capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExactIrqSourceBindingError {
    /// This platform lease exposes only an aggregate binding transition.
    #[error("the IRQ lease does not expose exact source capabilities")]
    Unsupported,
    /// The source cannot be represented by the fixed ownership bitmap.
    #[error("IRQ source {source_id} is outside the supported 0..64 range")]
    SourceIdOutOfRange { source_id: usize },
    /// Two platform bindings attempted to own the same portable source.
    #[error("IRQ source {source_id} appears more than once in the platform binding")]
    DuplicateSource { source_id: usize },
    /// The requested source is not part of this platform lease.
    #[error("IRQ source {source_id} is not owned by the platform binding")]
    UnknownSource { source_id: usize },
    /// The unique source capability has already left its parent lease.
    #[error("IRQ source {source_id} capability was already transferred")]
    AlreadyTaken { source_id: usize },
}

/// Move-only proof that one portable IRQ source names one platform vector.
///
/// The token deliberately does not enable or disable hardware. Its parent
/// lease retains the allocation and endpoint gate, while this token must stay
/// with the exact IRQ action that consumes the source. Dropping a live token
/// deliberately preserves fail-closed accounting and never permits the parent
/// to release or mint another owner for the same source.
#[must_use = "keep the exact source token with its registered IRQ action"]
pub struct ExactIrqSourceBinding {
    binding: BindingIrqBinding,
    source_bit: u64,
    tracker: Arc<ExactIrqSourceTracker>,
}

impl ExactIrqSourceBinding {
    /// Returns the portable source identity owned by this token.
    pub const fn source_id(&self) -> usize {
        self.binding.source_id
    }

    /// Returns the immutable platform route associated with the source.
    pub const fn irq(&self) -> &BindingIrq {
        &self.binding.irq
    }

    /// Returns the complete immutable source-to-platform binding fact.
    pub const fn binding(&self) -> &BindingIrqBinding {
        &self.binding
    }

    /// Releases fail-closed accounting after the owning IRQ action is gone.
    ///
    /// Ordinary [`Drop`] deliberately leaves the source live forever. This
    /// prevents an implicitly quarantined action from making the parent PCI
    /// lease appear releasable.
    ///
    /// # Safety
    ///
    /// The exact IRQ action that retained this token must have been disabled,
    /// synchronized, removed from its descriptor, and successfully closed.
    /// The callback object must have been destroyed; no active or detached
    /// callback may still name the parent vector.
    pub unsafe fn retire_after_action_close(self) {
        self.tracker
            .live_source_bits
            .fetch_and(!self.source_bit, Ordering::AcqRel);
    }
}

impl fmt::Debug for ExactIrqSourceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactIrqSourceBinding")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "nvme")]
pub(crate) struct ExactIrqSourceSet {
    bindings: Vec<BindingIrqBinding>,
    source_bits: u64,
    tracker: Arc<ExactIrqSourceTracker>,
}

#[cfg(feature = "nvme")]
impl ExactIrqSourceSet {
    pub(crate) fn new(bindings: &[BindingIrqBinding]) -> Result<Self, ExactIrqSourceBindingError> {
        let mut source_bits = 0_u64;
        let mut exact_bindings = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let source_id = binding.source_id;
            if source_id >= MAX_EXACT_IRQ_SOURCES {
                return Err(ExactIrqSourceBindingError::SourceIdOutOfRange { source_id });
            }
            let source_bit = 1_u64 << source_id;
            if source_bits & source_bit != 0 {
                return Err(ExactIrqSourceBindingError::DuplicateSource { source_id });
            }
            source_bits |= source_bit;
            exact_bindings.push(binding.clone());
        }
        Ok(Self {
            bindings: exact_bindings,
            source_bits,
            tracker: Arc::new(ExactIrqSourceTracker::default()),
        })
    }

    pub(crate) fn take(
        &self,
        source_id: usize,
    ) -> Result<ExactIrqSourceBinding, ExactIrqSourceBindingError> {
        let binding = self
            .bindings
            .iter()
            .find(|binding| binding.source_id == source_id)
            .ok_or(ExactIrqSourceBindingError::UnknownSource { source_id })?;
        let source_bit = 1_u64 << source_id;
        if self
            .tracker
            .issued_source_bits
            .fetch_or(source_bit, Ordering::AcqRel)
            & source_bit
            != 0
        {
            return Err(ExactIrqSourceBindingError::AlreadyTaken { source_id });
        }
        self.tracker
            .live_source_bits
            .fetch_or(source_bit, Ordering::Release);
        Ok(ExactIrqSourceBinding {
            binding: binding.clone(),
            source_bit,
            tracker: Arc::clone(&self.tracker),
        })
    }

    pub(crate) fn live_source_bits(&self) -> u64 {
        self.tracker.live_source_bits.load(Ordering::Acquire)
    }

    pub(crate) const fn source_bits(&self) -> u64 {
        self.source_bits
    }
}

#[derive(Default)]
struct ExactIrqSourceTracker {
    #[cfg(feature = "nvme")]
    issued_source_bits: AtomicU64,
    live_source_bits: AtomicU64,
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

    #[cfg(feature = "nvme")]
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

    /// Transfers the unique capability for one portable IRQ source.
    ///
    /// Multi-source controllers use this token to tie every registered action
    /// to the parent allocation that produced its platform IRQ. The default is
    /// explicit unsupported rather than reconstructing ownership from
    /// [`BindingInfo`].
    fn take_exact_irq_source(
        &self,
        _source_id: usize,
    ) -> Result<ExactIrqSourceBinding, ExactIrqSourceBindingError> {
        Err(ExactIrqSourceBindingError::Unsupported)
    }

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

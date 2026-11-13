//! Opaque completion ownership for guest-forwarded physical RISC-V IRQs.

use ax_sync::RawSpinLock;

use super::IrqError;

/// A detached external interrupt completion, consumable on any host CPU.
///
/// The private representation retains the originating controller's completion
/// state. Explicit completion consumes the token; dropping it completes rollback.
/// Completion neither allocates nor acquires an ordinary device or routing lock.
#[derive(Debug)]
#[must_use = "a detached external IRQ must eventually be completed"]
pub struct RiscvExternalIrqClaim {
    inner: Option<crate::arch::ExternalIrqClaim>,
}

impl RiscvExternalIrqClaim {
    pub(crate) const fn new(inner: crate::arch::ExternalIrqClaim) -> Self {
        Self { inner: Some(inner) }
    }

    /// Physical controller-local source identity.
    pub const fn source(&self) -> u32 {
        self.inner
            .as_ref()
            .expect("owned external IRQ claim")
            .source()
    }

    /// Consumes the retained completion; retired APLIC sources stay masked.
    pub fn complete(mut self) -> Result<(), IrqError> {
        // Disarm rollback before entering the hardware path, including errors.
        let claim = self.inner.take().expect("owned external IRQ claim");
        if crate::arch::complete_external_claim(claim) {
            Ok(())
        } else {
            Err(IrqError::Controller)
        }
    }
}

impl Drop for RiscvExternalIrqClaim {
    fn drop(&mut self) {
        if let Some(claim) = self.inner.take()
            && !crate::arch::complete_external_claim(claim)
        {
            warn!("failed to complete dropped RISC-V external IRQ claim");
        }
    }
}

/// Preallocated ownership slot for one detached external IRQ completion.
///
/// Both operations only move tokens under an IRQ-save raw lock. They perform
/// no allocation, hardware access or callbacks while holding that lock. Callers
/// take a token before completing it, so the slot lock never nests with a
/// controller lock. A rejected store returns the original completion owner.
#[derive(Debug, Default)]
pub struct RiscvExternalIrqClaimSlot {
    claim: RawSpinLock<Option<RiscvExternalIrqClaim>>,
}

impl RiscvExternalIrqClaimSlot {
    /// Creates an empty slot without allocating.
    pub const fn new() -> Self {
        Self {
            claim: RawSpinLock::new(None),
        }
    }

    /// Transfers ownership into an empty slot, returning it unchanged if occupied.
    pub fn try_store(&self, claim: RiscvExternalIrqClaim) -> Result<(), RiscvExternalIrqClaim> {
        let mut slot = self.claim.lock_irqsave();
        if slot.is_some() {
            return Err(claim);
        }
        *slot = Some(claim);
        Ok(())
    }

    /// Removes the sole completion owner; a second take returns `None`.
    pub fn take(&self) -> Option<RiscvExternalIrqClaim> {
        self.claim.lock_irqsave().take()
    }
}

/// Configures and enables a guest-owned APLIC source from physical firmware.
/// The source stays at this target until retired, and cannot be reused before
/// platform reset. This operation does not provide device DMA isolation.
pub fn activate_guest_aplic_source(
    source: u32,
    target_cpu: usize,
    trigger: rdif_intc::Trigger,
) -> Result<(), IrqError> {
    let source = core::num::NonZeroU32::new(source).ok_or(IrqError::InvalidIrq)?;
    crate::arch::activate_guest_aplic_source(source, target_cpu, trigger)
}

/// Masks and permanently retires a guest-owned APLIC source until reset.
pub fn retire_guest_aplic_source(source: u32) -> Result<(), IrqError> {
    let source = core::num::NonZeroU32::new(source).ok_or(IrqError::InvalidIrq)?;
    crate::arch::retire_guest_aplic_source(source)
}

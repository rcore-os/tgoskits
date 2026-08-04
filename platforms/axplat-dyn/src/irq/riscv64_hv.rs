//! Split host PLIC completion for RISC-V passthrough devices.

use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use ax_plat::irq::{HwIrq, IrqError, IrqId, RiscvHvIrqIf};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use super::IrqIfImpl;
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use super::RISCV_PLIC_SOURCE_COUNT;

const NO_CLAIM_CONTEXT: usize = 0;

struct ClaimContextSlot {
    encoded_context: AtomicUsize,
}

impl ClaimContextSlot {
    const fn new() -> Self {
        Self {
            encoded_context: AtomicUsize::new(NO_CLAIM_CONTEXT),
        }
    }

    fn reserve(&self, context: usize) -> bool {
        let Some(encoded) = context.checked_add(1) else {
            return false;
        };
        self.encoded_context
            .compare_exchange(
                NO_CLAIM_CONTEXT,
                encoded,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn take(&self) -> Option<usize> {
        let encoded = self
            .encoded_context
            .swap(NO_CLAIM_CONTEXT, Ordering::AcqRel);
        (encoded != NO_CLAIM_CONTEXT).then(|| encoded - 1)
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static HOST_PLIC_CLAIMS: [ClaimContextSlot; RISCV_PLIC_SOURCE_COUNT] =
    [const { ClaimContextSlot::new() }; RISCV_PLIC_SOURCE_COUNT];

/// Completes a detached claim after host dispatch unless guest publication
/// disarms the guard.
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub(super) struct GuestPlicClaim {
    completion: GuestPlicCompletion,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
enum GuestPlicCompletion {
    Direct { context: usize, source: u32 },
    Stored { source: u32 },
    Deferred,
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl GuestPlicClaim {
    pub(super) fn detach(active: &mut somehal::irq::ActiveIrq, irq: IrqId) -> Option<Self> {
        let claim = active.defer_riscv_plic_completion()?;
        let (context, source) = claim.into_parts();
        debug_assert_eq!(irq.hwirq.0, source);
        let completion = claim_slot(source)
            .filter(|slot| slot.reserve(context))
            .map_or(GuestPlicCompletion::Direct { context, source }, |_| {
                GuestPlicCompletion::Stored { source }
            });
        Some(Self { completion })
    }

    pub(super) fn publish_to_guest(&mut self) -> bool {
        let GuestPlicCompletion::Stored { source } = self.completion else {
            return false;
        };
        if !ax_plat::irq::riscv64_hv::publish_physical_plic_claim(source) {
            return false;
        }
        self.completion = GuestPlicCompletion::Deferred;
        true
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
impl Drop for GuestPlicClaim {
    fn drop(&mut self) {
        match self.completion {
            GuestPlicCompletion::Direct { context, source } => {
                complete_claim(context, source);
            }
            GuestPlicCompletion::Stored { source } => {
                complete_guest_plic_source(source);
            }
            GuestPlicCompletion::Deferred => {}
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn claim_slot(source: u32) -> Option<&'static ClaimContextSlot> {
    let source = source as usize;
    (source != 0)
        .then(|| HOST_PLIC_CLAIMS.get(source))
        .flatten()
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn complete_claim(context: usize, source: u32) {
    if let Err(error) = somehal::irq::complete_deferred_riscv_plic_claim(context, source) {
        warn!(
            "failed to complete RISC-V host PLIC source {source} in context {context}: {error:?}"
        );
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn complete_guest_plic_source(source: u32) -> bool {
    let Some(context) = claim_slot(source).and_then(ClaimContextSlot::take) else {
        return false;
    };
    complete_claim(context, source);
    true
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn resolve_plic_irq(source: u32) -> Result<IrqId, IrqError> {
    if source == 0 || source as usize >= RISCV_PLIC_SOURCE_COUNT {
        return Err(IrqError::InvalidIrq);
    }
    let domain = somehal::irq::domain_by_kind_fast(somehal::irq::IrqDomainKind::RiscvPlic)
        .ok_or(IrqError::Unsupported)?;
    Ok(IrqId::new(domain, HwIrq(source)))
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
#[impl_plat_interface]
impl RiscvHvIrqIf for IrqIfImpl {
    fn activate_guest_plic_source(source: u32, target_cpu: usize) -> Result<(), IrqError> {
        let irq = resolve_plic_irq(source)?;
        somehal::irq::irq_set_affinity(
            irq,
            somehal::irq::IrqAffinity::Fixed { cpu_id: target_cpu },
        )?;
        if let Err(error) = somehal::irq::irq_set_enable(irq, true) {
            let _ = somehal::irq::irq_set_affinity(irq, somehal::irq::IrqAffinity::Any);
            return Err(error);
        }
        Ok(())
    }

    fn deactivate_guest_plic_source(source: u32) -> Result<(), IrqError> {
        let irq = resolve_plic_irq(source)?;
        somehal::irq::irq_set_enable(irq, false)?;
        somehal::irq::irq_set_affinity(irq, somehal::irq::IrqAffinity::Any)
    }

    fn complete_guest_plic_source(source: u32) -> bool {
        complete_guest_plic_source(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_context_is_consumed_exactly_once_and_can_be_reused() {
        let slot = ClaimContextSlot::new();

        assert!(slot.reserve(7));
        assert!(!slot.reserve(9));
        assert_eq!(slot.take(), Some(7));
        assert_eq!(slot.take(), None);
        assert!(slot.reserve(9));
        assert_eq!(slot.take(), Some(9));
    }

    #[test]
    fn claim_context_rejects_unencodable_context() {
        let slot = ClaimContextSlot::new();

        assert!(!slot.reserve(usize::MAX));
        assert_eq!(slot.take(), None);
    }
}

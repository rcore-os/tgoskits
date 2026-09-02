//! Host GIC completion handoff for AArch64 passthrough devices.

#[cfg(all(target_arch = "aarch64", feature = "hv"))]
use ax_plat::irq::IrqId;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Aarch64IrqRoute<T> {
    GuestOwned,
    Host(T),
}

enum GuestIrqAttempt<G> {
    NotOwned,
    Accepted,
    Rejected(G),
}

fn route_guest_owned_irq<T, G>(
    try_guest: impl FnOnce() -> GuestIrqAttempt<G>,
    dispatch_host: impl FnOnce() -> T,
) -> Aarch64IrqRoute<T> {
    match try_guest() {
        GuestIrqAttempt::Accepted => Aarch64IrqRoute::GuestOwned,
        GuestIrqAttempt::NotOwned => Aarch64IrqRoute::Host(dispatch_host()),
        GuestIrqAttempt::Rejected(completion) => {
            let outcome = dispatch_host();
            drop(completion);
            Aarch64IrqRoute::Host(outcome)
        }
    }
}

#[cfg(all(target_arch = "aarch64", feature = "hv"))]
pub(super) fn route_assigned_spi_or_host<T>(
    active: &mut somehal::irq::ActiveIrq,
    irq: IrqId,
    dispatch_host: impl FnOnce() -> T,
) -> Aarch64IrqRoute<T> {
    route_guest_owned_irq(|| try_assigned_spi(active, irq), dispatch_host)
}

#[cfg(all(target_arch = "aarch64", feature = "hv"))]
fn try_assigned_spi(
    active: &mut somehal::irq::ActiveIrq,
    irq: IrqId,
) -> GuestIrqAttempt<GuestGicClaim> {
    if !somehal::irq::domain_is_kind(irq.domain, somehal::irq::IrqDomainKind::AArch64Gic) {
        return GuestIrqAttempt::NotOwned;
    }
    let intid = irq.hwirq.0;
    if !ax_plat::irq::aarch64_hv::has_assigned_physical_spi(intid) {
        return GuestIrqAttempt::NotOwned;
    }
    let Some(mut claim) = GuestGicClaim::detach(active, irq) else {
        return GuestIrqAttempt::NotOwned;
    };
    if claim.publish_to_guest() {
        GuestIrqAttempt::Accepted
    } else {
        GuestIrqAttempt::Rejected(claim)
    }
}

/// Completes a detached claim after host dispatch unless guest publication
/// disarms the guard.
#[cfg(all(target_arch = "aarch64", feature = "hv"))]
struct GuestGicClaim {
    claim: Option<somehal::irq::Aarch64GicSpiClaim>,
}

#[cfg(all(target_arch = "aarch64", feature = "hv"))]
impl GuestGicClaim {
    fn detach(active: &mut somehal::irq::ActiveIrq, irq: IrqId) -> Option<Self> {
        let claim = active.defer_aarch64_gic_spi_completion()?;
        debug_assert_eq!(claim.intid(), irq.hwirq.0);
        Some(Self { claim: Some(claim) })
    }

    fn publish_to_guest(&mut self) -> bool {
        let Some(claim) = self.claim.take() else {
            return false;
        };
        let intid = claim.intid();
        if !ax_plat::irq::aarch64_hv::publish_physical_gic_claim(intid) {
            self.claim = Some(claim);
            return false;
        }
        let transferred = claim.transfer_to_sink();
        debug_assert_eq!(transferred, intid);
        true
    }
}

#[cfg(all(target_arch = "aarch64", feature = "hv"))]
impl Drop for GuestGicClaim {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take()
            && let Err(error) = claim.complete()
        {
            warn!("failed to complete deferred AArch64 GIC SPI: {error:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::{Aarch64IrqRoute, GuestIrqAttempt, route_guest_owned_irq};

    #[test]
    fn assigned_spi_is_offered_to_guest_before_host_dispatch() {
        let guest_attempted = Cell::new(false);
        let host_dispatched = Cell::new(false);

        let route = route_guest_owned_irq(
            || {
                guest_attempted.set(true);
                GuestIrqAttempt::<()>::Accepted
            },
            || {
                host_dispatched.set(true);
                79
            },
        );

        assert_eq!(route, Aarch64IrqRoute::GuestOwned);
        assert!(guest_attempted.get());
        assert!(!host_dispatched.get());
    }

    #[test]
    fn unassigned_spi_uses_host_dispatch() {
        let route = route_guest_owned_irq(|| GuestIrqAttempt::<()>::NotOwned, || 79);

        assert_eq!(route, Aarch64IrqRoute::Host(79));
    }

    #[test]
    fn rejected_guest_claim_is_completed_after_host_dispatch() {
        struct CompletionProbe<'a>(&'a Cell<u8>);

        impl Drop for CompletionProbe<'_> {
            fn drop(&mut self) {
                assert_eq!(self.0.get(), 2);
                self.0.set(3);
            }
        }

        let state = Cell::new(0);
        let route = route_guest_owned_irq(
            || {
                state.set(1);
                GuestIrqAttempt::Rejected(CompletionProbe(&state))
            },
            || {
                assert_eq!(state.get(), 1);
                state.set(2);
                79
            },
        );

        assert_eq!(route, Aarch64IrqRoute::Host(79));
        assert_eq!(state.get(), 3);
    }
}

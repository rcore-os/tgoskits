//! Retained guest passthrough IRQ-route ownership.
//!
//! Route ownership is independent from host filesystem support. A guest can
//! own an interrupt action, a direct-injection route, or an MMIO/port endpoint
//! even when the monitor has no block device or mounted filesystem. This
//! module therefore remains available in every AxVM build; the optional host
//! storage transaction consumes its revocation proof but does not own it.

use alloc::{boxed::Box, format, string::String, vec::Vec};
use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use crate::{
    AxVMRef,
    arch::{ArchOps, CurrentArch},
    config::VMInterruptMode,
};

/// Observable phase of a retained guest IRQ-route transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestIrqRouteLeaseState {
    /// No architecture route has been activated yet.
    Prepared,
    /// Every successfully visited guest is retained for explicit revocation.
    Active,
    /// Every retained route was revoked and its owner task was joined.
    Revoked,
}

/// Failure while activating or revoking retained guest IRQ routes.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GuestIrqRouteError {
    /// The requested operation does not match the linear lease phase.
    #[error("guest IRQ route lease is in state {state:?}, expected {expected:?}")]
    InvalidState {
        /// Observed lease state.
        state: GuestIrqRouteLeaseState,
        /// State required by the operation.
        expected: GuestIrqRouteLeaseState,
    },
    /// One architecture route could not be activated.
    #[error("could not activate guest IRQ routes: {detail}")]
    Activation {
        /// Stable diagnostic suitable for the monitor boundary.
        detail: String,
    },
    /// One stopped guest could not release every route through its owner.
    #[error("guest IRQ route revocation failed closed: {detail}")]
    Revocation {
        /// Stable diagnostic suitable for the monitor boundary.
        detail: String,
    },
}

/// Retained owner of architecture IRQ routes activated for passthrough guests.
///
/// The lease deliberately exists even when no host block controller matched a
/// guest mapping. Dropping it cannot prove that callbacks, direct-injection
/// endpoints, or their fixed owner tasks have stopped; callers must invoke
/// [`revoke_guest_irq_route_lease`] and retain failures fail-closed.
#[must_use = "active guest IRQ routes require explicit owner-thread revocation"]
pub struct GuestIrqRouteLease {
    inner: Option<Box<GuestIrqRouteLeaseInner>>,
}

struct GuestIrqRouteLeaseInner {
    guests: Vec<AxVMRef>,
    state: GuestIrqRouteLeaseState,
    quarantine_next: AtomicPtr<GuestIrqRouteLeaseInner>,
}

static GUEST_IRQ_ROUTE_QUARANTINE: AtomicPtr<GuestIrqRouteLeaseInner> =
    AtomicPtr::new(ptr::null_mut());
static GUEST_IRQ_ROUTE_QUARANTINE_COUNT: AtomicUsize = AtomicUsize::new(0);

impl GuestIrqRouteLease {
    /// Creates an empty route transaction before architecture activation.
    pub fn new() -> Self {
        Self {
            inner: Some(Box::new(GuestIrqRouteLeaseInner {
                guests: Vec::new(),
                state: GuestIrqRouteLeaseState::Prepared,
                quarantine_next: AtomicPtr::new(ptr::null_mut()),
            })),
        }
    }

    /// Returns the current linear lifecycle phase.
    pub fn state(&self) -> GuestIrqRouteLeaseState {
        self.inner().state
    }

    fn inner(&self) -> &GuestIrqRouteLeaseInner {
        self.inner
            .as_deref()
            .expect("a live route lease always retains its inner owner")
    }

    fn inner_mut(&mut self) -> &mut GuestIrqRouteLeaseInner {
        self.inner
            .as_deref_mut()
            .expect("a live route lease always retains its inner owner")
    }
}

impl Default for GuestIrqRouteLease {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GuestIrqRouteLease {
    fn drop(&mut self) {
        if self
            .inner
            .as_deref()
            .is_some_and(|inner| inner.state == GuestIrqRouteLeaseState::Active)
        {
            let inner = self
                .inner
                .take()
                .expect("an active route lease retains its inner owner");
            quarantine_active_route_lease(inner);
        }
    }
}

fn quarantine_active_route_lease(inner: Box<GuestIrqRouteLeaseInner>) {
    let quarantined = Box::into_raw(inner);
    let mut head = GUEST_IRQ_ROUTE_QUARANTINE.load(Ordering::Acquire);
    loop {
        // SAFETY: `quarantined` came from `Box::into_raw`, is not reachable by
        // another thread before the release CAS below, and remains allocated
        // for shutdown after publication. Updating its intrusive next pointer
        // therefore has exclusive access to that field.
        unsafe {
            (*quarantined)
                .quarantine_next
                .store(head, Ordering::Relaxed)
        };
        match GUEST_IRQ_ROUTE_QUARANTINE.compare_exchange_weak(
            head,
            quarantined,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(observed) => head = observed,
        }
    }
    let count = GUEST_IRQ_ROUTE_QUARANTINE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    error!(
        "active guest IRQ route lease entered shutdown quarantine; retained lease count={count}"
    );
}

/// Proof that every guest retained by one route lease released its IRQ path.
///
/// The exact VM objects stay alive so a later registry generation cannot
/// satisfy an earlier host-device handoff accidentally.
#[must_use = "host resource return must retain the exact route-revocation proof"]
pub struct GuestIrqRoutesRevoked {
    guests: Box<[AxVMRef]>,
}

impl GuestIrqRoutesRevoked {
    /// Returns whether this proof retained the exact VM object generation.
    pub fn covers_guest(&self, guest: &AxVMRef) -> bool {
        self.guests
            .iter()
            .any(|revoked| core::ptr::eq(revoked.as_ref(), guest.as_ref()))
    }
}

/// Activates architecture IRQ routes after the monitor has transferred every
/// selected host device to its guest owner.
///
/// This function does not perform a storage handoff: the application layer is
/// responsible for ordering any optional block-controller transaction before
/// this call. Retaining that policy outside AxVM keeps non-storage passthrough
/// and filesystem-free monitors on the same IRQ ownership protocol.
pub fn activate_guest_irq_routes(
    route_lease: &mut GuestIrqRouteLease,
) -> Result<(), GuestIrqRouteError> {
    if route_lease.state() != GuestIrqRouteLeaseState::Prepared {
        return Err(GuestIrqRouteError::InvalidState {
            state: route_lease.state(),
            expected: GuestIrqRouteLeaseState::Prepared,
        });
    }
    // Publish Active first. If an architecture hook fails after a previous
    // route crossed its irreversible boundary, the retained prefix can still
    // be revoked through the same lease instead of being dropped implicitly.
    route_lease.inner_mut().state = GuestIrqRouteLeaseState::Active;

    for vm in crate::get_vm_list() {
        if !vm
            .uses_passthrough_resources()
            .map_err(|error| activation_error(format!("VM[{}]: {error}", vm.id())))?
        {
            continue;
        }
        let interrupt_mode = vm
            .passthrough_interrupt_mode()
            .map_err(|error| activation_error(format!("VM[{}] mode: {error}", vm.id())))?;
        // Retain the exact VM before crossing its architecture hook. All
        // read-only validation above completed, while a failing hook may have
        // already published a route generation that requires revocation.
        route_lease.inner_mut().guests.push(vm.clone());
        if interrupt_mode == VMInterruptMode::Passthrough {
            CurrentArch::activate_guest_irq_routes(&vm)
                .map_err(|error| activation_error(format!("VM[{}]: {error}", vm.id())))?;
        }
    }
    Ok(())
}

/// Requests same-owner route close for every retained stopped guest, waits for
/// typed completion, and only then joins the owner tasks.
///
/// A partial failure leaves the lease Active with the exact VM objects intact.
/// Retrying is safe because architecture close and the post-close task join are
/// generation-aware and idempotent.
pub fn revoke_guest_irq_route_lease(
    route_lease: &mut GuestIrqRouteLease,
) -> Result<GuestIrqRoutesRevoked, GuestIrqRouteError> {
    if route_lease.state() != GuestIrqRouteLeaseState::Active {
        return Err(GuestIrqRouteError::InvalidState {
            state: route_lease.state(),
            expected: GuestIrqRouteLeaseState::Active,
        });
    }

    for vm in route_lease.inner().guests.iter().rev() {
        vm.quiesce_for_passthrough_revocation()
            .map_err(|error| revocation_error(format!("VM[{}]: {error}", vm.id())))?;
        let interrupt_mode = vm
            .passthrough_interrupt_mode()
            .map_err(|error| revocation_error(format!("VM[{}]: {error}", vm.id())))?;
        if interrupt_mode == VMInterruptMode::Passthrough {
            CurrentArch::revoke_guest_irq_routes(vm)
                .map_err(|error| revocation_error(format!("VM[{}]: {error}", vm.id())))?;
        }
        vm.join_after_passthrough_irq_revocation()
            .map_err(|error| revocation_error(format!("VM[{}]: {error}", vm.id())))?;
    }
    route_lease.inner_mut().state = GuestIrqRouteLeaseState::Revoked;
    Ok(GuestIrqRoutesRevoked {
        guests: core::mem::take(&mut route_lease.inner_mut().guests).into_boxed_slice(),
    })
}

fn activation_error(detail: impl Into<String>) -> GuestIrqRouteError {
    GuestIrqRouteError::Activation {
        detail: detail.into(),
    }
}

fn revocation_error(detail: impl Into<String>) -> GuestIrqRouteError {
    GuestIrqRouteError::Revocation {
        detail: detail.into(),
    }
}

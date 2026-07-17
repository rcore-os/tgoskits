//! Mask, drain, and revoke x86 IOAPIC passthrough ownership.

use core::sync::atomic::Ordering;

use super::{activation::revoke_ioapic_forwarding_routes, state::*};
use crate::arch::x86_64::host_irq as irq;

pub fn disable_ioapic_irq_forwarding_for_vm(vm_id: usize) {
    if let Err(error) = revoke_ioapic_irq_forwarding_state(vm_id) {
        warn!("failed to drain x86 IOAPIC forwarding for VM[{vm_id}]: {error:?}");
    }
}

/// Revokes every guest forwarding path and waits for callbacks that observed
/// the previous VM identity.
#[cfg(any(feature = "fs", feature = "host-fs"))]
pub fn revoke_ioapic_irq_forwarding_for_vm(vm_id: usize) -> crate::AxVmResult {
    revoke_ioapic_irq_forwarding_state(vm_id)
}

fn revoke_ioapic_irq_forwarding_state(vm_id: usize) -> crate::AxVmResult {
    let _transaction = IoApicRouteTransaction::try_acquire().ok_or_else(|| {
        crate::AxVmError::invalid_state(
            "revoke x86 IOAPIC forwarding routes",
            "another route activation or revocation transaction is active",
        )
    })?;
    let owner = IOAPIC_IRQ_FORWARD_VM_ID.load(Ordering::Acquire);
    if owner != usize::MAX && owner != vm_id {
        return Err(crate::AxVmError::resource_conflict(
            "revoke x86 IOAPIC forwarding owner",
            format_args!("VM[{owner}] owns the forwarding fabric, not VM[{vm_id}]"),
        ));
    }
    if ioapic_forwarding_activation_in_progress() {
        return Err(crate::AxVmError::invalid_state(
            "revoke x86 IOAPIC forwarding routes",
            "a route activation is still in progress",
        ));
    }

    let activated = mask_active_ioapic_forwarding_routes().map_err(revocation_irq_error)?;
    disable_ioapic_forwarding_actions().map_err(revocation_irq_error)?;
    IOAPIC_IRQ_FORWARD_VM_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARD_VCPU_ID.store(usize::MAX, Ordering::Release);
    IOAPIC_IRQ_FORWARDING_ENABLED.store(false, Ordering::Release);
    drain_disabled_ioapic_forwarding().map_err(revocation_irq_error)?;
    let endpoint_result =
        revoke_ioapic_forwarding_routes(activated | quarantined_ioapic_route_mask());
    let action_result = release_ioapic_forwarding_actions();
    merge_revocation_results(endpoint_result, action_result)
}

fn revocation_irq_error(error: irq::IrqError) -> crate::AxVmError {
    crate::AxVmError::interrupt(
        "drain x86 passthrough IRQ forwarding",
        format_args!("{error:?}"),
    )
}

fn quarantined_ioapic_route_mask() -> usize {
    ioapic_irq_hook_gsis().fold(0, |mask, gsi| {
        if matches!(
            *IOAPIC_FORWARDING_ROUTES[gsi].lock(),
            IoApicForwardingRouteState::Quarantined(_)
        ) {
            mask | gsi_bit(gsi)
        } else {
            mask
        }
    })
}

fn mask_active_ioapic_forwarding_routes() -> Result<usize, irq::IrqError> {
    let activated = IOAPIC_IRQ_ACTIVATED.load(Ordering::Acquire);
    for gsi in ioapic_irq_hook_gsis() {
        let bit = gsi_bit(gsi);
        if activated & bit == 0 || IOAPIC_IRQ_MASKED.load(Ordering::Acquire) & bit != 0 {
            continue;
        }
        set_forwarded_host_gsi_enabled(gsi, false)?;
        IOAPIC_IRQ_MASKED.fetch_or(bit, Ordering::AcqRel);
    }
    Ok(activated)
}

fn disable_ioapic_forwarding_actions() -> Result<(), irq::IrqError> {
    for slot in &IOAPIC_IRQ_HANDLES {
        if let Some(handle) = *slot.lock() {
            match irq::disable_irq(handle) {
                Ok(()) => {}
                Err(irq::IrqError::NotFound) => clear_forwarding_handle(slot, handle),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn drain_disabled_ioapic_forwarding() -> Result<(), irq::IrqError> {
    for slot in &IOAPIC_IRQ_HANDLES {
        if let Some(handle) = *slot.lock() {
            match irq::synchronize_irq(handle) {
                Ok(()) => {}
                Err(irq::IrqError::NotFound) => clear_forwarding_handle(slot, handle),
                Err(error) => return Err(error),
            }
        }
    }
    IOAPIC_IRQ_PENDING.store(0, Ordering::Release);
    IOAPIC_IRQ_PENDING_LEVEL.store(0, Ordering::Release);
    Ok(())
}

fn release_ioapic_forwarding_actions() -> crate::AxVmResult {
    let mut first_error = None;
    for (gsi, slot) in IOAPIC_IRQ_HANDLES.iter().enumerate() {
        let Some(handle) = *slot.lock() else {
            continue;
        };
        match irq::free_irq(handle) {
            Ok(()) | Err(irq::IrqError::NotFound) => clear_forwarding_handle(slot, handle),
            Err(error) => {
                first_error.get_or_insert_with(|| {
                    forwarding_irq_error("release x86 IOAPIC forwarding IRQ action", gsi, error)
                });
            }
        }
    }

    if IOAPIC_IRQ_HANDLES
        .iter()
        .all(|handle| handle.lock().is_none())
    {
        IOAPIC_IRQ_HOOK_REGISTERED.store(false, Ordering::Release);
    }
    first_error.map_or(Ok(()), Err)
}

fn clear_forwarding_handle(slot: &IoApicForwardingHandleSlot, handle: irq::IrqHandle) {
    let mut current = slot.lock();
    if *current == Some(handle) {
        *current = None;
    }
}

fn merge_revocation_results(
    endpoint_result: crate::AxVmResult,
    action_result: crate::AxVmResult,
) -> crate::AxVmResult {
    match (endpoint_result, action_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(endpoint), Err(action)) => Err(crate::AxVmError::interrupt(
            "revoke x86 IOAPIC forwarding ownership",
            format_args!("device endpoint revoke failed: {endpoint}; IRQ release failed: {action}"),
        )),
    }
}

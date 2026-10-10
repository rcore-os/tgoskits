use alloc::sync::Arc;

use axdevice_base::DeviceId;

use super::{
    EndpointIrqTransitionPermit, EndpointRouteToken, PciRootBinding,
    lifecycle::{PendingIrqWithdrawal, transfer_pending_irq_withdrawals},
};
use crate::{DeviceManagerError, DeviceManagerResult, ServiceCardinality, ServiceKey};

impl Drop for PciRootBinding {
    fn drop(&mut self) {
        // Drop is only a best-effort fallback for a root that was never
        // explicitly stopped. The real teardown is `stop_lifecycle`, which
        // reports errors instead of masking them; here the binding is being
        // destroyed, so a failure can only be logged.
        if let Err(error) = self.stop_lifecycle() {
            warn!("PCI root teardown lifecycle handoff could not complete: {error}");
        }
        // Destruction cannot retain a retry queue in this binding. Transfer
        // closed endpoint owners before their last local references disappear.
        transfer_pending_irq_withdrawals(&self.pending_irq_withdrawals);
    }
}

impl PciRootBinding {
    pub(super) fn drain_pending_binding_withdrawals(&self) -> DeviceManagerResult {
        let pending = self.take_pending_binding_withdrawals();
        let mut first_error = None;
        for device in pending {
            if let Err(error) = self.withdraw_endpoint(device)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn withdraw_endpoint(&self, device: DeviceId) -> DeviceManagerResult {
        let Some((function, admission)) = self.router.invalidate_device(device) else {
            return Ok(());
        };
        let withdrawal = PendingIrqWithdrawal {
            device,
            function,
            admission,
        };
        let result = withdrawal.admission.wait_for_irq_permits().and_then(|()| {
            let mut permit = EndpointIrqTransitionPermit { _private: () };
            withdrawal
                .function
                .withdraw_irq(&mut permit)
                .map_err(DeviceManagerError::Device)
        });
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.queue_irq_withdrawal(withdrawal);
                Err(error)
            }
        }
    }
}

/// Typed service key published only by a PCI host bundle.
///
/// Bindings stay enumerable through `DeviceRuntime::services()` for
/// diagnostics and host-side verification; endpoint models never receive a
/// `DeviceRuntime`, so route resolution remains dependency-scoped.
pub struct PciRootBindingKey;

impl ServiceKey for PciRootBindingKey {
    type Service = PciRootBinding;
    const NAME: &'static str = "pci-root-binding";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Multiple;
}

pub(crate) struct PciBindingLease {
    pub(super) binding: Arc<PciRootBinding>,
    pub(super) token: EndpointRouteToken,
}

impl Drop for PciBindingLease {
    fn drop(&mut self) {
        let device = self.token.device_id();
        // Revoke the root route unconditionally before contending for the
        // lifecycle owner.  This is idempotent and matches the stable binding
        // generation across admission-epoch replacement during reset.
        self.binding.root.unbind_route_for_binding(&self.token);
        let Some(operation) = self.binding.try_begin_withdrawal_or_defer(device) else {
            // A busy lifecycle queues the withdrawal; a terminal lifecycle is
            // cleaned by root teardown. Neither case may spin in Drop.
            return;
        };
        // Withdraw the root route first. The admission close is the second
        // linearization point; callbacks that already acquired a lease keep
        // their endpoint Arc, while new validation and IRQ permits fail.
        if let Err(error) = self.binding.withdraw_endpoint(device) {
            warn!("PCI endpoint teardown queued a pending IRQ withdrawal: {error}");
        }
        if let Err(error) = self.binding.drain_pending_binding_withdrawals() {
            warn!("PCI endpoint teardown could not drain deferred bindings: {error}");
        }
        if let Err(error) = operation.finish_restore() {
            warn!("PCI endpoint teardown lifecycle handoff could not complete: {error}");
        }
    }
}

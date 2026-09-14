//! Runtime-authenticated PCI endpoint binding and BAR dispatch.

#[cfg(test)]
use alloc::sync::Arc;
use alloc::vec::Vec;

use ax_sync::SpinLock;
use axdevice_base::DeviceError;
#[cfg(test)]
use axdevice_base::{Device, DeviceContext, DeviceId, DeviceResult, IrqLine};

#[cfg(test)]
use super::{PciBdf, PciRootState};
use crate::DeviceManagerResult;
#[cfg(test)]
use crate::{AccessWidth, DeviceManagerError, DeviceNodeId};

const DEFAULT_DRAIN_ATTEMPTS: usize = 1_000_000;

mod cleanup;
mod dispatch;
mod endpoint;
mod lifecycle;
mod routing;

pub(crate) use cleanup::PciBindingLease;
pub use cleanup::PciRootBindingKey;
pub use endpoint::{
    EndpointIrqTransitionPermit, PciBarAccess, PciCommandRevision, PciCommandState,
    PciConfigReadEffect, PciConfigWriteEffect, PciEndpointContext, PciFunction,
};
pub use lifecycle::PciRootBinding;
use lifecycle::PendingIrqWithdrawal;
#[cfg(test)]
use lifecycle::{BindingLifecycleState, transfer_pending_irq_withdrawals};
pub use routing::EndpointRouteToken;
#[cfg(test)]
use routing::{EndpointAdmission, EndpointBindingGeneration, EndpointRouter, RoutedAdmissionEpoch};

// A root binding may be dropped while an endpoint backend is temporarily
// unable to withdraw its IRQ. Keep that owner in a process-lifetime,
// fail-closed queue so dropping the root cannot drop an asserted line.
static ORPHANED_IRQ_WITHDRAWALS: SpinLock<Vec<PendingIrqWithdrawal>> = SpinLock::new(Vec::new());

pub(super) fn pci_config_error(error: super::PciError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access PCI configuration",
        detail: alloc::format!("{error}"),
    }
}

#[cfg(test)]
mod tests;

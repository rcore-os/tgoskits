//! PCI MSI routing and transactional MSI-X lease ownership.

#[cfg(feature = "nvme")]
mod activation;
#[cfg(feature = "nvme")]
mod lease;
#[cfg(feature = "nvme")]
mod quarantine;
mod routing;
#[cfg(feature = "nvme")]
mod transaction;

#[cfg(feature = "nvme")]
pub(crate) use lease::PciMsixActivationFailure;
#[cfg(feature = "nvme")]
pub use lease::{PciIrqLease, PciMsixAllocation};
pub use routing::{PciMsiTarget, msi_target_for_endpoint};

#[cfg(all(test, feature = "nvme"))]
mod tests;

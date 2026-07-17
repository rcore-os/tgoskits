//! PCI MSI routing and transactional MSI-X lease ownership.

mod lease;
mod routing;
mod transaction;

pub use lease::{PciIrqLease, PciMsixAllocation};
pub use routing::{PciMsiTarget, msi_target_for_endpoint};

#[cfg(test)]
mod tests;

//! Platform discovery and registration for migrated block controllers.
//!
//! The first migration exposes only NVMe and RK3588 DWCMSHC eMMC. Other
//! low-level driver crates remain in the workspace but have no `ax-driver`
//! feature or registration entry until they implement the interrupt-driven
//! controller contract.

mod binding;
mod irq_bound;

#[cfg(feature = "nvme")]
pub mod nvme;
#[cfg(feature = "rockchip-sdhci")]
mod rockchip;

pub use binding::*;
pub use irq_bound::IrqBoundBlock;

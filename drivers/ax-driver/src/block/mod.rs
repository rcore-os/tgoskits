//! Platform discovery and registration for migrated block controllers.
//!
//! Every exposed controller implements the owned-DMA, interrupt-driven
//! `BlockController`/`HardwareQueue` contract. Low-level driver crates that
//! have not migrated remain unreachable from `ax-driver`.

mod binding;
mod irq_bound;

#[cfg(feature = "cv181x-sdhci")]
mod cvsd;
#[cfg(feature = "nvme")]
pub mod nvme;
#[cfg(feature = "rockchip-sdhci")]
mod rockchip;

pub use binding::*;
pub use irq_bound::IrqBoundBlock;

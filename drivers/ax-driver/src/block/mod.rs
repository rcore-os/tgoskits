//! Platform discovery and registration for migrated block controllers.
//!
//! Every exposed controller implements the owned-DMA, interrupt-driven
//! `BlockController`/`HardwareQueue` contract. Low-level driver crates that
//! have not migrated remain unreachable from `ax-driver`.

#[cfg(any(feature = "ahci", feature = "ahci-fdt"))]
mod ahci;
mod binding;
mod irq_bound;

#[cfg(feature = "cv181x-sdhci")]
mod cvsd;
#[cfg(feature = "k230-sdhci")]
mod k230_sdhci;
#[cfg(feature = "nvme")]
pub mod nvme;
#[cfg(feature = "phytium-mci")]
mod phytium_mci;
#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
mod rockchip;
#[cfg(feature = "starfive-jh7110-dwmmc")]
mod starfive_mmc;

pub use binding::*;
pub use irq_bound::IrqBoundBlock;

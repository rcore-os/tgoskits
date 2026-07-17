mod binding;
mod irq_bound;
#[cfg(any(
    test,
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "starfive-jh7110-dwmmc",
))]
mod staged;

#[cfg(any(feature = "ahci", feature = "ls2k1000-ahci"))]
pub mod ahci;
#[cfg(feature = "bcm2835-sdhci")]
pub mod bcm2835;
#[cfg(feature = "cvsd")]
pub mod cvsd;
#[cfg(feature = "k230-sdhci")]
pub mod k230_sdhci;
#[cfg(feature = "nvme")]
pub mod nvme;
#[cfg(feature = "phytium-mci")]
pub mod phytium_mci;
#[cfg(feature = "ramdisk")]
pub mod ramdisk;
#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
mod rockchip;
#[cfg(feature = "starfive-jh7110-dwmmc")]
pub mod starfive_mmc;

pub use binding::*;
pub use irq_bound::{IrqBoundBlock, IrqBoundControllerBundle};

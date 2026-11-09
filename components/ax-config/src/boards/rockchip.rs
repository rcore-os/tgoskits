use super::{Board, TARGET_AARCH64};

pub static ORANGEPI_5_PLUS: Board = Board {
    id: "orangepi-5-plus",
    target: TARGET_AARCH64,
};
pub static ROCK_4D: Board = Board {
    id: "rock-4d",
    target: TARGET_AARCH64,
};
pub static ROC_RK3568_PC: Board = Board {
    id: "roc-rk3568-pc",
    target: TARGET_AARCH64,
};
pub static RK3588_CPUFREQ: &[&str] = &["ax-driver/rk3588-cpufreq"];
pub static SDHCI_DWMMC: &[&str] = &["ax-driver/rockchip-sdhci", "ax-driver/rockchip-dwmmc"];

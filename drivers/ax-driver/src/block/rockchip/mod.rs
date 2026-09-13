use fdt_edit::Node;
use sdmmc_protocol::sdio::init::CardInitPreference;

mod clock;
#[cfg(feature = "rockchip-dwmmc")]
mod sd;
#[cfg(feature = "rockchip-sdhci")]
mod sdhci_rk3568;
#[cfg(feature = "rockchip-sdhci")]
mod sdhci_rk3588;

fn supports_block_card_protocol(node: &Node) -> bool {
    node.get_property("no-sd").is_none() || node.get_property("no-mmc").is_none()
}

/// Mirrors Linux MMC core's SD-then-MMC attach order unless firmware limits
/// which protocol the controller may use during initialization.
fn card_init_preference(node: &Node) -> CardInitPreference {
    if node.get_property("no-mmc").is_some() {
        CardInitPreference::SdOnly
    } else if node.get_property("no-sd").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}

fn media_name(preference: CardInitPreference) -> &'static str {
    match preference {
        CardInitPreference::SdOnly => "SD",
        CardInitPreference::MmcFirst => "MMC",
        CardInitPreference::SdFirst => "SD-or-MMC",
        _ => "unknown",
    }
}

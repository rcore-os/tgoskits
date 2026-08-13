//! AIC8800 Wi-Fi chip probe.
//!
//! Platform-specific bring-up (MMIO mapping, SDHCI controller enumeration, IRQ
//! wiring) is the OS glue's responsibility. The OS hands an already-initialized
//! SDIO host to [`probe`], which performs the chip-side bring-up (firmware load,
//! FDRV init) and returns an [`AicWifiNetDev`].
//!
//! That single device object is both the data plane (it implements
//! [`rd_net::Interface`]) and the control plane (it implements
//! [`rd_net::WifiControl`]: STA connect, SoftAP start, RX wake, link policy).
//! The OS registers it through the generic net device path and drives Wi-Fi
//! purely through those traits, without referencing any AIC8800 internal type.

use alloc::sync::Arc;

use sdio_host::SdioHost;

use crate::{common::ChipVariant, fdrv::AicWifiNetDev};

/// Probe an AIC8800 chip over an already-initialized SDIO host.
///
/// The caller (OS glue) is responsible for the platform bring-up that precedes
/// this: mapping MMIO, initializing the SDHCI controller, and registering the
/// controller IRQ. `sdio` must be an enumerated, ready-to-use host.
///
/// This detects the chip variant, loads firmware, and starts FDRV (RX/TX/AP
/// tasks), returning an [`AicWifiNetDev`] ready to register with the network
/// stack. Call [`crate::set_runtime`] before this. Returns an error string if
/// the chip is not a recognized AIC8800 or bring-up fails.
pub fn probe<H: SdioHost + 'static>(mut sdio: H) -> Result<AicWifiNetDev, alloc::string::String> {
    // ---- 芯片识别 ----
    let (vid, did) = sdio.vendor_device_id();
    let chip = ChipVariant::from_vid_did(vid, did);
    log::info!(
        "[aic8800] chip={:?} vid=0x{:04x} did=0x{:04x}",
        chip,
        vid,
        did
    );
    if chip == ChipVariant::Unknown {
        return Err(alloc::format!(
            "unknown Wi-Fi chip: vid=0x{:04x} did=0x{:04x}",
            vid,
            did
        ));
    }

    // ---- 固件加载 ----
    crate::fw::firmware_init(&mut sdio, chip).map_err(|e| {
        log::error!("[aic8800] firmware init failed: {:?}", e);
        alloc::format!("firmware init failed: {:?}", e)
    })?;
    log::info!("[aic8800] firmware loaded");

    // ---- FDRV 初始化 (SdioTransport + RX/TX/AP 任务) ----
    let bus = crate::fdrv::init(sdio, chip).map_err(|e| {
        log::error!("[aic8800] FDRV init failed: {}", e);
        alloc::format!("FDRV init failed: {}", e)
    })?;

    // ---- CARD_INT 回调：SDHCI 控制器收到卡中断时唤醒 RX 处理 ----
    sdhci_cv1800::irq::register_card_irq_callback(crate::fdrv::sdio1_irq_handler);

    // The data plane device also carries the control plane (it implements
    // `WifiControl`). MAC is read live from the bus once the firmware reports
    // it (after lmac config / AP start).
    let mac = bus.conn.sta_mac.lock().unwrap_or([0; 6]);
    Ok(AicWifiNetDev::new(Arc::clone(&bus), chip, mac))
}

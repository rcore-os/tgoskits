//! AIC8800 Wi-Fi chip probe.
//!
//! Platform-specific MMIO and controller initialization are the OS glue's
//! responsibility. This probe extracts the controller's move-only IRQ source
//! and returns it as part of the network device.
//!
//! The OS registers the result through the generic network parts path. Queue,
//! IRQ and wireless control ownership are split exactly once by the runtime.

use alloc::sync::Arc;

use sdio_host::SdioHost;

use crate::{common::ChipVariant, fdrv::AicWifiNetDev};

fn validate_queue_irq_variant(chip: ChipVariant) -> Result<(), &'static str> {
    match chip {
        // These variants share the vendor-supported function-1 FIFO path. V2
        // byte mode and V3 software-interrupt clearing are implemented by the
        // queue owner, while the SDHCI controller closes the rearm window with
        // a CARD_INT status readback.
        ChipVariant::Aic8801 | ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2 => Ok(()),
        // Local vendor sources disagree with the legacy Rust path about DC/DW
        // command/FIFO function ownership. Fail closed until each variant has
        // board evidence; a periodic kicker is intentionally not available.
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW => {
            Err("AIC8800DC/DW CARD_INT and FIFO ownership are not validated")
        }
        ChipVariant::Unknown => Err("unknown AIC8800 variant"),
    }
}

/// Probe an AIC8800 chip over an already-initialized SDIO host.
///
/// The caller (OS glue) is responsible for the platform bring-up that precedes
/// this: mapping MMIO and initializing the SDHCI controller. `sdio` must be an
/// enumerated, ready-to-use host with an unclaimed IRQ source.
///
/// This detects the chip variant, loads firmware, and prepares FDRV, returning
/// an [`AicWifiNetDev`] ready to register with the network
/// stack. Call [`crate::set_runtime`] before this. Returns an error string if
/// the chip is not a recognized AIC8800 or bring-up fails.
pub fn probe<H: SdioHost + 'static>(mut sdio: H) -> Result<AicWifiNetDev, alloc::string::String> {
    let irq_source = sdio
        .take_irq_source()
        .ok_or_else(|| alloc::string::String::from("SDIO host has no unclaimed CARD_INT source"))?;

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
    validate_queue_irq_variant(chip).map_err(alloc::string::String::from)?;

    // ---- 固件加载 ----
    crate::fw::firmware_init(&mut sdio, chip).map_err(|e| {
        log::error!("[aic8800] firmware init failed: {:?}", e);
        alloc::format!("firmware init failed: {:?}", e)
    })?;
    log::info!("[aic8800] firmware loaded");

    // ---- FDRV 初始化 (owner-CPU queue state, no background tasks) ----
    let bus = crate::fdrv::init(sdio, chip).map_err(|e| {
        log::error!("[aic8800] FDRV init failed: {}", e);
        alloc::format!("FDRV init failed: {}", e)
    })?;

    // The consumable device splits data, IRQ, general control and Wi-Fi
    // control ownership in `NetDevice::into_parts`. MAC is read live from the
    // bus once firmware reports it (after LMAC configuration / AP start).
    let mac = bus.conn.sta_mac.lock().unwrap_or([0; 6]);
    Ok(AicWifiNetDev::new(Arc::clone(&bus), chip, mac, irq_source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unvalidated_dc_and_dw_variants_fail_closed() {
        assert!(validate_queue_irq_variant(ChipVariant::Aic8800DC).is_err());
        assert!(validate_queue_irq_variant(ChipVariant::Aic8800DW).is_err());
    }

    #[test]
    fn function_one_fifo_variants_use_the_queue_irq_runtime() {
        assert!(validate_queue_irq_variant(ChipVariant::Aic8801).is_ok());
        assert!(validate_queue_irq_variant(ChipVariant::Aic8800D80).is_ok());
        assert!(validate_queue_irq_variant(ChipVariant::Aic8800D80X2).is_ok());
    }
}

//! AIC8800 Wi-Fi chip probe.
//!
//! Platform-specific MMIO and controller initialization are the OS glue's
//! responsibility. This probe extracts the controller's move-only IRQ source
//! and returns it as part of the network device.
//!
//! The OS registers the result through the generic network parts path. Queue,
//! IRQ and wireless control ownership are split exactly once by the runtime.

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
/// This detects the chip variant and returns a device whose firmware/FDRV
/// startup remains move-only until the network runtime executes it on the
/// selected queue owner CPU. Call [`crate::set_runtime`] before building the
/// network runtime. Returns an error if the chip or IRQ contract is invalid.
pub fn probe<H: SdioHost + 'static>(
    mut sdio: H,
) -> Result<AicWifiNetDev<H>, alloc::string::String> {
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

    Ok(AicWifiNetDev::new(sdio, chip, [0; 6], irq_source))
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use sdio_host::{SdioIrqSource, SdioIrqStatus, error::SdioError};

    use super::*;

    struct ProbeIrqSource;

    impl SdioIrqSource for ProbeIrqSource {
        fn handle_irq(&mut self) -> SdioIrqStatus {
            SdioIrqStatus::Spurious
        }
    }

    struct ProbeOnlyHost {
        irq_source_available: bool,
    }

    impl ProbeOnlyHost {
        fn unexpected_firmware_io<T>() -> T {
            panic!("device probe must not execute firmware or FDRV I/O")
        }
    }

    impl SdioHost for ProbeOnlyHost {
        fn init(&mut self) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn mmio_base(&self) -> usize {
            0
        }

        fn read_byte(&self, _func: u8, _addr: u32) -> Result<u8, SdioError> {
            Self::unexpected_firmware_io()
        }

        fn write_byte(&self, _func: u8, _addr: u32, _val: u8) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn write_byte_read(&self, _func: u8, _addr: u32, _val: u8) -> Result<u8, SdioError> {
            Self::unexpected_firmware_io()
        }

        fn read_fifo(&self, _func: u8, _addr: u32, _buf: &mut [u8]) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn read_fifo_inc(&self, _func: u8, _addr: u32, _buf: &mut [u8]) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn write_fifo(&self, _func: u8, _addr: u32, _buf: &[u8]) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn write_fifo_inc(&self, _func: u8, _addr: u32, _buf: &[u8]) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn set_block_size(&self, _func: u8, _size: u16) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn set_clock(&self, _hz: u32) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn enable_func(&self, _func: u8) -> Result<(), SdioError> {
            Self::unexpected_firmware_io()
        }

        fn vendor_device_id(&self) -> (u16, u16) {
            (crate::common::VID_AIC8801, crate::common::DID_AIC8801)
        }

        fn enable_irq(&self) {
            Self::unexpected_firmware_io()
        }

        fn disable_irq(&self) {
            Self::unexpected_firmware_io()
        }

        fn card_irq_ctrl(&self) -> Option<alloc::sync::Arc<dyn sdio_host::SdioCardIrq>> {
            Self::unexpected_firmware_io()
        }

        fn take_irq_source(&mut self) -> Option<Box<dyn SdioIrqSource>> {
            core::mem::take(&mut self.irq_source_available)
                .then(|| Box::new(ProbeIrqSource) as Box<dyn SdioIrqSource>)
        }
    }

    #[test]
    fn probe_only_identifies_the_device_and_extracts_its_irq_source() {
        let host = ProbeOnlyHost {
            irq_source_available: true,
        };

        let _device = probe(host).expect("probe must defer firmware work to the owner CPU");
    }

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

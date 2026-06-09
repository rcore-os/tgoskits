//! SG2002 AIC8800 Wi-Fi SoftAP bring-up.
//!
//! Drives the full SDIO + firmware + SoftAP sequence, then wraps the AIC8800
//! `rd_net::Interface` device into an `RdNetDriver` and registers it as
//! `wlan0` (static IP + built-in DHCP server) with the network stack.
//!
//! Runs under the dynamic platform (`plat_dyn`), so MMIO regions are mapped
//! at runtime via `axklib::mmio::ioremap_raw` (the static `ax_config::devices`
//! constants are placeholders under `plat_dyn`). The SG2002 register bases are
//! fixed silicon addresses.

use alloc::{boxed::Box, sync::Arc};
use core::ptr::NonNull;

use aic8800::{
    common::ChipVariant,
    fdrv::{WifiClient, init as fdrv_init, sdio1_irq_handler},
    fw,
};
use axnet::{EthernetDriver, RdNetDriver, register_wifi_ap_device};
use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1HwConfig, sdio1_hw_init},
};
use sdio_host::SdioHost;

// SG2002 fixed register bases (physical).
const SYSCON_PADDR: usize = 0x0300_0000;
const CRG_PADDR: usize = 0x0300_2000;
const RTCSYS_CTRL_PADDR: usize = 0x0502_5000;
const RTCSYS_IO_PADDR: usize = 0x0502_7000;
const SDIO1_PADDR: usize = 0x0432_0000;
const WIFI_IRQ: usize = 38;
const MMIO_PAGE: usize = 0x1000;

/// Maps one MMIO page at `paddr`, returning its kernel virtual address.
/// `ioremap` is idempotent (linear map, overwrites on overlap), so mapping a
/// region another driver already mapped (e.g. SYSCON) is safe.
fn map_mmio(paddr: usize, name: &str) -> usize {
    match axklib::mmio::ioremap_raw(paddr.into(), MMIO_PAGE) {
        Ok(mmio) => mmio.as_ptr() as usize,
        Err(e) => panic!("[wifi] failed to map {name} @ {paddr:#x}: {e:?}"),
    }
}

/// Raw IRQ handler trampoline: forwards the SDIO1 controller interrupt to the
/// SDHCI handler.
unsafe fn sdio1_raw_irq_handler(
    _ctx: axklib::irq::IrqContext,
    _data: NonNull<()>,
) -> axklib::irq::IrqReturn {
    sdhci_cv1800::irq::sdhci_irq_handler(0);
    axklib::irq::IrqReturn::Handled
}

pub fn probe_wifi() {
    // Map the MMIO regions the SDIO1 HW bring-up needs, then build the config
    // with phys_virt_offset = 0 (we pass already-mapped virtual addresses).
    let cfg = Sdio1HwConfig::new(
        map_mmio(CRG_PADDR, "CRG"),
        map_mmio(SYSCON_PADDR, "SYSCON"),
        map_mmio(RTCSYS_CTRL_PADDR, "RTCSYS_CTRL"),
        map_mmio(RTCSYS_IO_PADDR, "RTCSYS_IO"),
        map_mmio(SDIO1_PADDR, "SDIO1"),
        0,
    );

    info!("[wifi] SDIO1 HW init starting...");
    sdio1_hw_init(&cfg);
    info!("[wifi] SDIO1 HW init done");

    // Register SDIO1 PLIC IRQ (shared, via the dynamic-platform irq framework).
    if let Err(e) =
        axklib::irq::request_shared(WIFI_IRQ, sdio1_raw_irq_handler, NonNull::dangling())
    {
        error!("[wifi] Failed to register SDIO1 IRQ {}: {:?}", WIFI_IRQ, e);
        return;
    }

    // SDHCI init
    let mut sdio = CviSdhci::new(cfg.sdio1_base_va);
    if let Err(e) = sdio.init() {
        error!("[wifi] SDIO1 init failed: {:?}", e);
        return;
    }

    let (vid, did) = sdio.vendor_device_id();
    let chip = ChipVariant::from_vid_did(vid, did);
    info!(
        "[wifi] AIC8800 detected: vendor=0x{:04x}, device=0x{:04x}, chip={:?}",
        vid, did, chip
    );

    if chip == ChipVariant::Unknown {
        error!("[wifi] Unknown WiFi chip");
        return;
    }

    // Prepare SDHCI for first data transfer (clear stale DAT state)
    sdio.prepare_first_data_xfer();

    // Firmware download
    info!("[wifi] Downloading firmware...");
    if let Err(e) = fw::firmware_init(&mut sdio, chip) {
        error!("[wifi] Firmware init failed: {:?}", e);
        return;
    }
    info!("[wifi] Firmware download complete");

    // FDRV init
    info!("[wifi] FDRV init starting...");
    let bus = match fdrv_init(sdio, chip) {
        Ok(bus) => bus,
        Err(e) => {
            error!("[wifi] FDRV init failed: {}", e);
            return;
        }
    };
    info!("[wifi] FDRV init complete");

    // Register CARD_INT callback
    sdhci_cv1800::irq::register_card_irq_callback(sdio1_irq_handler);

    // ================================================================
    // Start a softAP (open network).
    //
    // Runs the full vendor SDIO sequence: base LMAC config, add an AP-type
    // interface, then beacon download (APM_SET_BEACON_IE_REQ) followed by
    // APM_START_REQ with real beacon metadata. A valid APM_START_CFM with
    // status==0 means the AP is up and broadcasting.
    // ================================================================
    let mut client = WifiClient::new(Arc::clone(&bus));
    let channel = 6u8;
    let ssid = b"PicoClaw-Car";
    match client.start_ap_open(chip, ssid, channel, 6000) {
        Ok(cfm) => {
            info!("==========================================================");
            info!("[wifi] AP started! APM_START_CFM={:02x?}", cfm);
            info!(
                "[wifi] SSID=PicoClaw-Car channel={} (open network)",
                channel
            );
            info!("==========================================================");

            // 注册 wlan0 到网络栈:静态 IP 192.168.50.1/24 + 内建 DHCP server
            // 给连入的手机分配 192.168.50.2。手机随后可 ping 192.168.50.1。
            client.store_net_device();
            match aic8800::fdrv::take_wifi_net_device() {
                Some(aic_dev) => {
                    // 把 AIC8800 的 `rd_net::Interface` 包成 `Net` 再包成
                    // 上游网络栈消费的 `EthernetDriver`(RdNetDriver),与
                    // ax-driver 的 register_net 路径一致。WiFi 走 SDIO 带外
                    // RX,因此 irq_num = None。
                    let net = rd_net::Net::new(aic_dev, axklib::dma::op());
                    match RdNetDriver::new("wlan0", net, None) {
                        Ok(driver) => {
                            let driver: Box<dyn EthernetDriver> = Box::new(driver);
                            register_wifi_ap_device(
                                driver,
                                [192, 168, 50, 1], // server (wlan0) IP
                                [192, 168, 50, 2], // client (phone) IP
                                24,
                            );
                            // AIC8800 RX 走自己的线程独占 SDIO CARD_INT,不经
                            // 以太网 IRQ 框架。注册回调:收到数据帧时唤醒
                            // wlan0-poll 任务驱动网络栈 poll,否则空闲时进来的
                            // ARP/ICMP/数据包无人处理。
                            aic8800::fdrv::register_rx_data_callback(axnet::notify_wifi_rx);
                            info!("[wifi] wlan0 registered: 192.168.50.1/24, DHCP -> 192.168.50.2");
                        }
                        Err(e) => error!("[wifi] failed to build wlan0 driver: {:?}", e),
                    }
                }
                None => error!("[wifi] no net device to register"),
            }
        }
        Err(e) => {
            error!("==========================================================");
            error!("[wifi] AP START FAILED: {:?}", e);
            error!("[wifi] STA mode still works.");
            error!("==========================================================");
        }
    }
}

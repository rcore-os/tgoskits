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

use alloc::boxed::Box;
use core::ptr::NonNull;

use axnet::{EthernetDriver, RdNetDriver, register_wifi_ap_device};
use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1HwConfig, sdio1_hw_init},
};
use sdio_host::SdioHost;
use wifi_host::WifiDriver;

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
    // Install the ArceOS runtime glue into the OS-independent driver cores
    // (timing / delay / yield / task spawn). Must happen before any driver
    // operation that sleeps or polls.
    sdhci_cv1800::glue_arceos::install_delay();
    aic8800::glue_arceos::install_runtime();

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
    info!(
        "[wifi] SDIO device: vendor=0x{:04x}, device=0x{:04x}",
        vid, did
    );

    // Prepare SDHCI for first data transfer (clear stale DAT state)
    sdio.prepare_first_data_xfer();

    // Hand the initialized SDIO host to the chip driver. From here on the
    // kernel only talks to the generic `WifiDriver` trait, not any AIC8800
    // internal type. `probe` detects the chip, loads firmware, and starts the
    // FDRV tasks.
    let mut wifi: Box<dyn WifiDriver> = match aic8800::probe(sdio) {
        Ok(driver) => driver,
        Err(e) => {
            error!("[wifi] chip probe failed: {:?}", e);
            return;
        }
    };
    info!("[wifi] chip probe complete");

    // ================================================================
    // Start a softAP (open network).
    //
    // Runs the full vendor SDIO sequence: base LMAC config, add an AP-type
    // interface, then beacon download (APM_SET_BEACON_IE_REQ) followed by
    // APM_START_REQ with real beacon metadata.
    // ================================================================
    let channel = 6u8;
    let ssid = b"PicoClaw-Car";
    if let Err(e) = wifi.start_ap_open(ssid, channel) {
        error!("==========================================================");
        error!("[wifi] AP START FAILED: {:?}", e);
        error!("==========================================================");
        return;
    }
    info!("==========================================================");
    info!(
        "[wifi] AP started! SSID=PicoClaw-Car channel={} (open)",
        channel
    );
    info!("==========================================================");

    // 注册 wlan0 到网络栈:静态 IP 192.168.50.1/24 + 内建 DHCP server
    // 给连入的手机分配 192.168.50.2。手机随后可 ping 192.168.50.1。
    //
    // WiFi 走 SDIO 带外 RX,因此 irq_num = None。RX 回调:收到数据帧时唤醒
    // wlan0-poll 任务驱动网络栈 poll,否则空闲时进来的 ARP/ICMP/数据包无人处理。
    let Some(net) = wifi.take_net(axklib::dma::op()) else {
        error!("[wifi] no net device to register");
        return;
    };
    match RdNetDriver::new("wlan0", net, None) {
        Ok(driver) => {
            let driver: Box<dyn EthernetDriver> = Box::new(driver);
            register_wifi_ap_device(
                driver,
                [192, 168, 50, 1], // server (wlan0) IP
                [192, 168, 50, 2], // client (phone) IP
                24,
            );
            wifi.set_rx_data_callback(axnet::notify_wifi_rx);
            info!("[wifi] wlan0 registered: 192.168.50.1/24, DHCP -> 192.168.50.2");
        }
        Err(e) => error!("[wifi] failed to build wlan0 driver: {:?}", e),
    }
}

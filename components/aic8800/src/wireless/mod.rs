//! AIC8800 Wi-Fi driver: `wifi_host::WifiDriver` implementation.
//!
//! Platform-specific bring-up (MMIO mapping, SDHCI controller enumeration, IRQ
//! wiring) is the OS glue's responsibility. The OS hands an already-initialized
//! SDIO host to [`probe`], which performs the chip-side bring-up (firmware load,
//! FDRV init) and returns a `Box<dyn WifiDriver>`. The OS then drives Wi-Fi
//! purely through the trait, without referencing any AIC8800 internal type.

use alloc::{boxed::Box, sync::Arc};

use dma_api::DmaOp;
use rd_net::Net;
use sdio_host::SdioHost;
use wifi_host::{WifiDriver, WifiError as HostError};

use crate::{
    common::ChipVariant,
    fdrv::{WifiBus, WifiClient, WifiConfig},
};

fn map_err(e: crate::fdrv::WifiError) -> HostError {
    match e {
        crate::fdrv::WifiError::NotInitialized => HostError::NotInitialized,
        crate::fdrv::WifiError::NetworkNotFound => HostError::NetworkNotFound,
        crate::fdrv::WifiError::ConnectionTimeout => HostError::ConnectionTimeout,
        crate::fdrv::WifiError::AuthenticationFailed => HostError::AuthenticationFailed,
        crate::fdrv::WifiError::InvalidPassword => HostError::AuthenticationFailed,
        crate::fdrv::WifiError::AlreadyConnected => HostError::AlreadyConnected,
        crate::fdrv::WifiError::OperationFailed(msg) => HostError::OperationFailed(msg),
        _ => HostError::OperationFailed(alloc::format!("{:?}", e)),
    }
}

/// AIC8800 Wi-Fi driver instance.
pub struct Aic8800Wifi {
    bus: Arc<WifiBus>,
    client: WifiClient,
    chip: ChipVariant,
    net_taken: bool,
}

/// Probe an AIC8800 chip over an already-initialized SDIO host.
///
/// The caller (OS glue) is responsible for the platform bring-up that precedes
/// this: mapping MMIO, initializing the SDHCI controller, and registering the
/// controller IRQ. `sdio` must be an enumerated, ready-to-use host.
///
/// This detects the chip variant, loads firmware, and starts FDRV (RX/TX/AP
/// tasks), returning a `Box<dyn WifiDriver>`. Call [`crate::set_runtime`]
/// before this. Returns an error if the chip is not a recognized AIC8800.
pub fn probe<H: SdioHost + 'static>(mut sdio: H) -> Result<Box<dyn WifiDriver>, HostError> {
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
        return Err(HostError::OperationFailed(alloc::format!(
            "unknown Wi-Fi chip: vid=0x{:04x} did=0x{:04x}",
            vid,
            did
        )));
    }

    // ---- 固件加载 ----
    crate::fw::firmware_init(&mut sdio, chip).map_err(|e| {
        log::error!("[aic8800] firmware init failed: {:?}", e);
        HostError::NotInitialized
    })?;
    log::info!("[aic8800] firmware loaded");

    // ---- FDRV 初始化 (SdioTransport + RX/TX/AP 任务) ----
    let bus = crate::fdrv::init(sdio, chip).map_err(|e| {
        log::error!("[aic8800] FDRV init failed: {}", e);
        HostError::NotInitialized
    })?;

    // ---- CARD_INT 回调：SDHCI 控制器收到卡中断时唤醒 RX 处理 ----
    sdhci_cv1800::irq::register_card_irq_callback(crate::fdrv::sdio1_irq_handler);

    let client = WifiClient::new(Arc::clone(&bus));
    Ok(Box::new(Aic8800Wifi {
        bus,
        client,
        chip,
        net_taken: false,
    }))
}

impl WifiDriver for Aic8800Wifi {
    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), HostError> {
        // STA 模式连接前需先完成 LMAC 配置（SoftAP 的 start_ap_open 自带配置）。
        self.client
            .lmac_configure(self.chip, 6000)
            .map_err(map_err)?;
        self.client.store_net_device();

        let config = if password.is_empty() {
            WifiConfig::open(ssid)
        } else {
            WifiConfig::wpa2_psk(ssid, password)
        };

        let mut last_err = None;
        for attempt in 0..2 {
            if attempt > 0 {
                log::info!("[aic8800] retrying connect (attempt {})...", attempt + 1);
                crate::runtime::runtime().sleep_ms(3000);
            }
            match self.client.connect(&config, 15000) {
                Ok(()) => {
                    log::info!("[aic8800] connected to '{}'", ssid);
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("[aic8800] connect attempt {} failed: {:?}", attempt + 1, e);
                    last_err = Some(map_err(e));
                }
            }
        }
        Err(last_err.unwrap_or(HostError::OperationFailed("connect failed".into())))
    }

    fn disconnect(&mut self) -> Result<(), HostError> {
        self.client.disconnect().map_err(map_err)?;
        log::info!("[aic8800] disconnected");
        Ok(())
    }

    fn start_ap_open(&mut self, ssid: &[u8], channel: u8) -> Result<(), HostError> {
        let cfm = self
            .client
            .start_ap_open(self.chip, ssid, channel, 6000)
            .map_err(map_err)?;
        log::info!("[aic8800] AP started, APM_START_CFM={:02x?}", cfm);
        // 暂存网络设备，供后续 take_net 取出注册。
        self.client.store_net_device();
        Ok(())
    }

    fn mac_address(&self) -> [u8; 6] {
        self.bus.conn.sta_mac.lock().unwrap_or([0; 6])
    }

    fn take_net(&mut self, dma_op: &'static dyn DmaOp) -> Option<Net> {
        if self.net_taken {
            return None;
        }
        let dev = crate::fdrv::take_wifi_net_device()?;
        self.net_taken = true;
        Some(Net::new(dev, dma_op))
    }

    fn set_rx_data_callback(&mut self, cb: fn()) {
        crate::fdrv::register_rx_data_callback(cb);
    }
}

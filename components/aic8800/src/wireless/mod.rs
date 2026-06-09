//! AIC8800 WiFi 驱动
//!
//! 实现 `wifi_host::WifiDriver` trait，提供 init / connect / disconnect 三阶段接口。

use alloc::sync::Arc;
use core::time::Duration;

use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1HwConfig, sdio1_hw_init},
};
use sdio_host::SdioHost;
use wifi_host::{WifiDriver, WifiError as HostError};

#[cfg(feature = "speed-test")]
pub mod speed_test;

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

/// AIC8800D80 WiFi 驱动
///
/// 实现 `wifi_host::WifiDriver` trait。
pub struct Aic8800Wifi {
    bus: Arc<WifiBus>,
    client: WifiClient,
    #[allow(dead_code)]
    chip: ChipVariant,
}

impl Aic8800Wifi {
    /// 取出暂存的 WiFi 网络设备（用于注册到上游网络栈）
    ///
    /// 只能调用一次。调用后网络设备由集成层通过 `register_net` 持有。
    pub fn take_net_device(&self) -> Option<crate::fdrv::net::device::AicWifiNetDev> {
        crate::fdrv::take_wifi_net_device()
    }
}

impl WifiDriver for Aic8800Wifi {
    fn init() -> Result<Self, HostError> {
        use ax_plat_riscv64_sg2002::config::{devices::*, plat::PHYS_VIRT_OFFSET};

        // ---- Step 1: SoC 硬件初始化 ----
        let hw_cfg = Sdio1HwConfig::new(
            CRG_PADDR,
            SYSCTRL_PADDR,
            RTCSYS_CTRL_PADDR,
            RTCSYS_IO_PADDR,
            SDIO1_PADDR,
            PHYS_VIRT_OFFSET,
        );
        sdio1_hw_init(&hw_cfg);

        ax_plat_riscv64_sg2002::irq::register_sdio1_irq(sdhci_cv1800::irq::sdhci_irq_handler);

        // ---- Step 2: SDIO 卡枚举 ----
        let mut sdio = CviSdhci::new(hw_cfg.sdio1_base_va);
        sdio.init().map_err(|e| {
            log::error!("[aic8800] SDIO init failed: {:?}", e);
            HostError::NotInitialized
        })?;

        // ---- Step 3: 芯片识别 ----
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
                "Unknown WiFi chip: vid=0x{:04x}, did=0x{:04x}",
                vid,
                did
            )));
        }

        // ---- Step 4: 固件加载 ----
        crate::fw::firmware_init(&mut sdio, chip).map_err(|e| {
            log::error!("[aic8800] Firmware init failed: {:?}", e);
            HostError::NotInitialized
        })?;
        log::info!("[aic8800] Firmware loaded");

        // ---- Step 5: FDRV 初始化 (SdioTransport + IRQ + RX/TX 线程) ----
        let bus = crate::fdrv::init(sdio, chip).map_err(|e| {
            log::error!("[aic8800] FDRV init failed: {}", e);
            HostError::NotInitialized
        })?;

        // ---- Step 6: CARD_INT 回调 ----
        sdhci_cv1800::irq::register_card_irq_callback(crate::fdrv::sdio1_irq_handler);

        // ---- Step 7: LMAC 配置 ----
        let mut client = WifiClient::new(Arc::clone(&bus));
        let mac = client.lmac_configure(chip, 6000).map_err(|e| {
            log::error!("[aic8800] LMAC configure failed: {:?}", e);
            map_err(e)
        })?;
        log::info!("[aic8800] WiFi initialized, MAC={:02x?}", mac);

        // ---- Step 8: 创建并暂存网络设备 ----
        client.store_net_device();

        Ok(Self { bus, client, chip })
    }

    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), HostError> {
        let config = if password.is_empty() {
            WifiConfig::open(ssid)
        } else {
            WifiConfig::wpa2_psk(ssid, password)
        };

        let mut last_err = None;
        for attempt in 0..2 {
            if attempt > 0 {
                log::info!("[aic8800] Retrying connect (attempt {})...", attempt + 1);
                ax_task::sleep(core::time::Duration::from_secs(3));
            }

            match self.client.connect(&config, 15000) {
                Ok(()) => {
                    log::info!("[aic8800] Connected to '{}'", ssid);
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("[aic8800] Connect attempt {} failed: {:?}", attempt + 1, e);
                    last_err = Some(map_err(e));
                }
            }
        }

        Err(last_err.unwrap_or(HostError::OperationFailed("connect failed".into())))
    }

    fn disconnect(&mut self) -> Result<(), HostError> {
        self.client.disconnect().map_err(|e| {
            log::warn!("[aic8800] Disconnect error: {:?}", e);
            map_err(e)
        })?;
        log::info!("[aic8800] Disconnected");
        Ok(())
    }
}

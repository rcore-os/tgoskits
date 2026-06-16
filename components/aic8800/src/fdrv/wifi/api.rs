//! 高级 WiFi API
//!
//! 为外部程序提供简单易用的 WiFi 连接接口
//!
//! # 完整使用示例
//!
//! ```no_run
//! use aic8800_fdrv::{WifiClient, WifiConfig};
//!
//! // 1. 初始化 SDHCI 控制器 + 固件 (由调用方完成)
//! let bus = aic8800_fdrv::init(sdio)?;
//!
//! // 2. 创建客户端 + LMAC 配置
//! let client = WifiClient::new(bus);
//! let mac = client.lmac_configure(6000)?;
//!
//! // 3. 扫描 → 连接
//! let config = WifiConfig::wpa2_psk("SSID", "password");
//! client.connect(&config, 15000)?;
//! ```

use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::fmt;

use crate::{
    common::ChipVariant,
    fdrv::{
        consts::AP_MODE_FILTER_DEFAULT,
        core::bus::WifiBus,
        crypto::wpa2::*,
        protocol::{
            lmac_msg::*, send_apm_stop_req, send_eapol_data_frame, send_get_mac_addr_req,
            send_me_chan_config_req, send_me_config_req, send_me_set_ps_mode_req,
            send_mm_add_if_req, send_mm_add_if_req_typed, send_mm_remove_if_req,
            send_mm_set_filter_req, send_mm_start_req, send_reset_req, send_rf_calib_req,
            send_set_control_port_req, start_open_ap, wait_for_eapol,
        },
        wifi::manager::{self, build_wpa2_rsn_ie_from_ap, disconnect},
    },
};

/// WiFi 连接配置
#[derive(Clone, Debug)]
pub struct WifiConfig {
    /// SSID (网络名称)
    pub ssid: Vec<u8>,
    /// 密码 (对于 WPA2-PSK)
    pub password: Option<Vec<u8>>,
    /// BSSID (可选，用于指定连接到特定 AP)
    pub bssid: Option<[u8; 6]>,
    /// 认证类型
    pub auth_type: WifiAuthType,
}

/// WiFi 认证类型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiAuthType {
    /// 开放网络 (无密码)
    Open,
    /// WPA2-PSK (预共享密钥)
    Wpa2Psk,
    /// WPA3-PSK (未来支持)
    Wpa3Psk,
}

impl WifiConfig {
    /// 创建开放网络配置
    pub fn open(ssid: &str) -> Self {
        Self {
            ssid: ssid.as_bytes().to_vec(),
            password: None,
            bssid: None,
            auth_type: WifiAuthType::Open,
        }
    }

    /// 创建 WPA2-PSK 网络配置
    pub fn wpa2_psk(ssid: &str, password: &str) -> Self {
        Self {
            ssid: ssid.as_bytes().to_vec(),
            password: Some(password.as_bytes().to_vec()),
            bssid: None,
            auth_type: WifiAuthType::Wpa2Psk,
        }
    }

    /// 设置 BSSID (可选)
    pub fn with_bssid(mut self, bssid: [u8; 6]) -> Self {
        self.bssid = Some(bssid);
        self
    }
}

/// WiFi 连接错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiError {
    /// 驱动未初始化
    NotInitialized,
    /// 扫描失败
    ScanFailed,
    /// 未找到指定网络
    NetworkNotFound,
    /// 认证失败
    AuthenticationFailed,
    /// 连接超时
    ConnectionTimeout,
    /// 密码错误
    InvalidPassword,
    /// 网络不可用
    NetworkUnavailable,
    /// 已连接
    AlreadyConnected,
    /// 未连接
    NotConnected,
    /// 操作失败
    OperationFailed(String),
}

impl fmt::Display for WifiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WifiError::NotInitialized => write!(f, "Driver not initialized"),
            WifiError::ScanFailed => write!(f, "Scan failed"),
            WifiError::NetworkNotFound => write!(f, "Network not found"),
            WifiError::AuthenticationFailed => write!(f, "Authentication failed"),
            WifiError::ConnectionTimeout => write!(f, "Connection timeout"),
            WifiError::InvalidPassword => write!(f, "Invalid password"),
            WifiError::NetworkUnavailable => write!(f, "Network unavailable"),
            WifiError::AlreadyConnected => write!(f, "Already connected"),
            WifiError::NotConnected => write!(f, "Not connected"),
            WifiError::OperationFailed(msg) => write!(f, "Operation failed: {}", msg),
        }
    }
}

impl core::error::Error for WifiError {}

impl From<CmdError> for WifiError {
    fn from(e: CmdError) -> Self {
        match e {
            CmdError::Timeout => WifiError::ConnectionTimeout,
            CmdError::FirmwareError => WifiError::AuthenticationFailed,
            _ => WifiError::OperationFailed(format!("{:?}", e)),
        }
    }
}

/// 扫描结果
#[derive(Clone, Debug)]
pub struct WifiNetwork {
    /// SSID
    pub ssid: Vec<u8>,
    /// SSID 长度
    pub ssid_len: u8,
    /// BSSID
    pub bssid: [u8; 6],
    /// 信号强度 (dBm)
    pub rssi: i8,
    /// 信道频率 (MHz)
    pub channel_freq: u16,
    /// 加密类型
    pub encryption: WifiEncryption,
    /// 是否为 WPA2/WPA3 网络
    pub has_rsn: bool,
    /// RSN IE (用于 WPA2 连接)
    pub rsn_ie: Vec<u8>,
}

/// WiFi 加密类型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiEncryption {
    /// 无加密
    None,
    /// WEP
    Wep,
    /// WPA
    Wpa,
    /// WPA2
    Wpa2,
    /// WPA3
    Wpa3,
    /// 未知加密
    Unknown,
}

/// 连接状态
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// 未连接
    Disconnected,
    /// 正在连接
    Connecting,
    /// 已连接
    Connected,
    /// 连接失败
    Failed,
}

/// WiFi 客户端
///
/// 封装完整的 WiFi 生命周期：LMAC 配置 → 扫描 → 连接 → 数据传输 → 断连
pub struct WifiClient {
    bus: Arc<WifiBus>,
    vif_idx: u8,
    sta_mac: Option<[u8; 6]>,
}

impl WifiClient {
    /// 创建新的 WiFi 客户端
    pub fn new(bus: Arc<WifiBus>) -> Self {
        Self {
            bus,
            vif_idx: 0,
            sta_mac: None,
        }
    }

    /// 设置 VIF 索引
    pub fn with_vif_idx(mut self, vif_idx: u8) -> Self {
        self.vif_idx = vif_idx;
        self
    }

    // ================================================================
    // Phase 1: LMAC 配置
    // ================================================================

    /// 释放当前固件侧 VIF，把驱动状态复位到「无接口」基线。
    ///
    /// 模式切换（STA↔AP）前的必备步骤：固件里一次只持有一个 VIF，
    /// `bus.conn.vif_idx` 是其唯一真值（初值 `0xFF` 表示无 VIF）。若不先
    /// 释放旧 VIF 就发起新一轮 `MM_ADD_IF`，旧 VIF 在固件中永不回收、
    /// `vif_idx` 被覆盖，造成 VIF 泄漏与寻址错乱。
    ///
    /// 行为：
    ///   1. 若处于 STA 已连接态，先尽力 `disconnect`（忽略错误，保证对端收到 deauth）；
    ///   2. 若存在有效 VIF（`vif_idx != 0xFF`），发送 `MM_REMOVE_IF_REQ` 释放；
    ///   3. 复位连接状态、`vif_idx`、`sta_idx` 与缓存的 MAC。
    ///
    /// 无有效 VIF 时为安全空操作（首次进入某模式即此情形）。
    pub fn teardown_vif(&mut self, timeout_ms: u64) -> Result<(), WifiError> {
        let vif_idx = self
            .bus
            .conn
            .vif_idx
            .load(core::sync::atomic::Ordering::Acquire);

        if vif_idx == 0xFF {
            // 无 VIF：首次进入,直接返回。
            return Ok(());
        }

        // STA 已连接时先尽力断开,让对端收到 deauth（失败不阻断拆除）。
        if self.get_status() == ConnectionStatus::Connected
            && let Err(e) = disconnect(&self.bus, vif_idx, 3)
        {
            log::warn!(
                "[WifiClient] teardown: best-effort disconnect failed: {:?}",
                e
            );
        }

        send_mm_remove_if_req(&self.bus, vif_idx, timeout_ms).map_err(WifiError::from)?;
        log::info!("[WifiClient] teardown: removed vif_idx={}", vif_idx);

        // 复位驱动侧状态到「无接口」基线。
        self.bus
            .conn
            .set_status(crate::fdrv::core::STATUS_DISCONNECTED);
        self.bus
            .conn
            .vif_idx
            .store(0xFF, core::sync::atomic::Ordering::Release);
        self.bus
            .conn
            .sta_idx
            .store(0xFF, core::sync::atomic::Ordering::Release);
        *self.bus.conn.ap_mac.lock() = None;
        self.vif_idx = 0xFF;

        Ok(())
    }

    /// 执行完整 LMAC 配置（初始化后必须调用一次）
    ///
    /// 厂商 D80 序列 (rwnx_main.c):
    ///   stack_start → rf_calib → get_mac → reset → me_config → me_chan_config → add_if
    ///
    /// 注意：stack_start 已在 polling init (init.rs) 中发送，此处不再重复。
    ///
    /// 我们精简后的序列:
    ///   1. MM_SET_RF_CALIB_REQ — RF 校准
    ///   2. MM_GET_MAC_ADDR_REQ — 获取 MAC 地址
    ///   3. MM_RESET_REQ — 复位固件
    ///   4. ME_CONFIG_REQ — MAC 层 HT 能力
    ///   5. ME_CHAN_CONFIG_REQ — 2.4GHz 信道列表
    ///   6. MM_ADD_IF_REQ — 添加 STA 接口
    ///   7. MM_START_REQ — 启动 MAC
    ///   8. MM_SET_FILTER_REQ — 设置接收过滤器
    pub fn lmac_configure(
        &mut self,
        chip: ChipVariant,
        timeout_ms: u64,
    ) -> Result<[u8; 6], WifiError> {
        // 模式切换/重入：先释放可能存在的旧 VIF（AP 或上一轮 STA）。
        self.teardown_vif(timeout_ms)?;

        send_rf_calib_req(&self.bus, chip, timeout_ms).map_err(WifiError::from)?;

        let mac = send_get_mac_addr_req(&self.bus, timeout_ms).map_err(WifiError::from)?;

        send_reset_req(&self.bus, timeout_ms).map_err(WifiError::from)?;

        send_me_config_req(&self.bus, chip, timeout_ms).map_err(WifiError::from)?;
        send_me_chan_config_req(&self.bus, timeout_ms).map_err(WifiError::from)?;

        let vif_idx = send_mm_add_if_req(&self.bus, &mac, timeout_ms).map_err(WifiError::from)?;
        self.vif_idx = vif_idx;

        send_mm_start_req(&self.bus, timeout_ms).map_err(WifiError::from)?;

        send_mm_set_filter_req(&self.bus, 0x1502_868C, timeout_ms).map_err(WifiError::from)?;

        self.sta_mac = Some(mac);

        self.bus
            .conn
            .vif_idx
            .store(vif_idx, core::sync::atomic::Ordering::Release);

        log::debug!(
            "[WifiClient] LMAC configured: mac={:02x?}, vif_idx={}",
            mac,
            vif_idx
        );
        Ok(mac)
    }

    // ================================================================
    // Phase 1 (AP): 启动开放网络 softAP
    // ================================================================

    /// 启动一个开放网络 softAP。
    ///
    /// 完整执行 vendor 在 SDIO 下的起 AP 序列：
    ///   1. LMAC 基础配置（RF calib / get_mac / reset / me_config / me_chan_config）
    ///   2. 以 MM_AP 类型添加接口（而非 STA）
    ///   3. MM_START + 设置过滤器（含 ACCEPT_PROBE_REQ）
    ///   4. 构造 beacon → APM_SET_BEACON_IE_REQ → APM_START_REQ（带真实 bcn 元信息）
    ///
    /// 返回 `Ok(cfm)`（APM_START_CFM 的 param，首字节 status==0 为成功）。
    /// `Err(Timeout)` 多为固件镜像未含 APM task，或参数被固件拒绝。
    pub fn start_ap_open(
        &mut self,
        chip: ChipVariant,
        ssid: &[u8],
        channel: u8,
        timeout_ms: u64,
    ) -> Result<Vec<u8>, WifiError> {
        log::info!("[WifiClient] === start_ap_open START ===");

        // 模式切换/重入：先释放可能存在的旧 VIF（上一轮 STA 或 AP）。
        self.teardown_vif(timeout_ms)?;

        // ---- 基础 LMAC 配置 ----
        send_rf_calib_req(&self.bus, chip, timeout_ms).map_err(WifiError::from)?;
        let mac = send_get_mac_addr_req(&self.bus, timeout_ms).map_err(WifiError::from)?;
        send_reset_req(&self.bus, timeout_ms).map_err(WifiError::from)?;
        send_me_config_req(&self.bus, chip, timeout_ms).map_err(WifiError::from)?;
        send_me_chan_config_req(&self.bus, timeout_ms).map_err(WifiError::from)?;

        // ---- 以 AP 类型添加接口 ----
        let vif_idx =
            send_mm_add_if_req_typed(&self.bus, &mac, MM_AP, timeout_ms).map_err(|e| {
                log::error!("[WifiClient] MM_ADD_IF(AP) failed: {:?}", e);
                WifiError::from(e)
            })?;
        self.vif_idx = vif_idx;
        log::info!(
            "[WifiClient] AP interface added: vif_idx={}, mac={:02x?}",
            vif_idx,
            mac
        );

        send_mm_start_req(&self.bus, timeout_ms).map_err(WifiError::from)?;

        // AP 模式过滤器：NOT_CHANGEABLE(含 MY_UNICAST/OTHER_MGMT，Auth 帧依赖)
        // + ProbeReq/AllBeacon/OtherBSSID。之前误用错误位偏移导致 Auth 收不到。
        send_mm_set_filter_req(&self.bus, AP_MODE_FILTER_DEFAULT, timeout_ms)
            .map_err(WifiError::from)?;

        self.sta_mac = Some(mac);
        self.bus
            .conn
            .vif_idx
            .store(vif_idx, core::sync::atomic::Ordering::Release);
        // 存 AP 自身 MAC，供 rx 线程构造 Auth/Assoc Response 的 SA/BSSID
        self.bus.conn.sta_mac.lock().replace(mac);

        // ---- 完整起 AP 序列：beacon 下发 + APM_START ----
        log::info!("[WifiClient] Starting AP (beacon + APM_START)...");
        let cfm =
            start_open_ap(&self.bus, vif_idx, &mac, ssid, channel, timeout_ms).map_err(|e| {
                log::error!("[WifiClient] start_open_ap failed: {:?}", e);
                WifiError::from(e)
            })?;

        let status = cfm.first().copied().unwrap_or(0xFF);
        log::info!(
            "[WifiClient] === APM_START_CFM received! status={}, cfm={:02x?} ===",
            status,
            &cfm[..cfm.len().min(8)]
        );
        if status == 0 {
            log::info!("[WifiClient] *** AP STARTED — SSID broadcasting ***");
        } else {
            log::warn!(
                "[WifiClient] APM_START_CFM status != 0 ({}), AP may not be up",
                status
            );
        }

        Ok(cfm)
    }

    /// 停止 AP（探测后清理）
    pub fn stop_ap(&self, timeout_ms: u64) -> Result<(), WifiError> {
        send_apm_stop_req(&self.bus, self.vif_idx, timeout_ms).map_err(WifiError::from)?;
        log::info!("[WifiClient] AP stopped (vif_idx={})", self.vif_idx);
        Ok(())
    }

    // ================================================================
    // Phase 2: 扫描
    // ================================================================

    /// 扫描 WiFi 网络
    pub fn scan(
        &self,
        ssid: Option<&[u8]>,
        timeout_ms: u64,
    ) -> Result<Vec<WifiNetwork>, WifiError> {
        log::debug!("[WifiClient] Starting scan...");

        let results = manager::scan(&self.bus, self.vif_idx, ssid, timeout_ms).map_err(|e| {
            log::error!("[WifiClient] Scan failed: {:?}", e);
            WifiError::ScanFailed
        })?;

        let networks: Vec<WifiNetwork> = results
            .into_iter()
            .map(|r| WifiNetwork {
                ssid: r.ssid.to_vec(),
                ssid_len: r.ssid_len,
                bssid: r.bssid,
                rssi: r.rssi,
                channel_freq: r.center_freq,
                encryption: if r.rsn_ie.is_empty() {
                    WifiEncryption::None
                } else {
                    WifiEncryption::Wpa2
                },
                has_rsn: !r.rsn_ie.is_empty(),
                rsn_ie: r.rsn_ie,
            })
            .collect();

        log::debug!(
            "[WifiClient] Scan complete: {} networks found",
            networks.len()
        );
        Ok(networks)
    }

    /// 查找指定 SSID 的网络
    pub fn find_network(&self, ssid: &[u8], timeout_ms: u64) -> Result<WifiNetwork, WifiError> {
        let networks = self.scan(Some(ssid), timeout_ms)?;
        networks
            .into_iter()
            .find(|n| n.ssid[..n.ssid_len as usize] == *ssid)
            .ok_or(WifiError::NetworkNotFound)
    }

    // ================================================================
    // Phase 3: 连接
    // ================================================================

    /// 连接到 WiFi 网络（自动扫描 → 连接 → WPA2 握手）
    pub fn connect(&self, config: &WifiConfig, timeout_ms: u64) -> Result<(), WifiError> {
        log::info!(
            "[WifiClient] Connecting to SSID: {:?}, auth: {:?}",
            core::str::from_utf8(&config.ssid).unwrap_or("<invalid>"),
            config.auth_type
        );

        if self.get_status() == ConnectionStatus::Connected {
            return Err(WifiError::AlreadyConnected);
        }

        // 扫描目标网络
        let network = self.find_network(&config.ssid, 10000)?;

        self.connect_to(&network, config, timeout_ms)
    }

    /// 连接到已知网络（跳过扫描）
    ///
    /// 适用于已通过 scan() 获取 AP 信息的场景
    pub fn connect_to(
        &self,
        network: &WifiNetwork,
        config: &WifiConfig,
        timeout_ms: u64,
    ) -> Result<(), WifiError> {
        // 标记为正在连接
        self.bus
            .conn
            .set_status(crate::fdrv::core::STATUS_CONNECTING);

        let result = self.connect_to_inner(network, config, timeout_ms);
        match &result {
            Ok(()) => {
                self.bus
                    .conn
                    .set_status(crate::fdrv::core::STATUS_CONNECTED);
                self.bus.conn.ap_mac.lock().replace(network.bssid);
            }
            Err(_) => {
                self.bus.conn.set_status(crate::fdrv::core::STATUS_FAILED);
            }
        }
        result
    }

    fn connect_to_inner(
        &self,
        network: &WifiNetwork,
        config: &WifiConfig,
        timeout_ms: u64,
    ) -> Result<(), WifiError> {
        // 构建 RSN IE
        let wpa2_ie = if config.auth_type == WifiAuthType::Wpa2Psk {
            if !network.has_rsn {
                log::warn!(
                    "[WifiClient] Target network doesn't have RSN IE, using default WPA2 IE"
                );
            }
            build_wpa2_rsn_ie_from_ap(&network.rsn_ie)
        } else {
            Vec::new()
        };

        // 发送连接请求
        let connect_result = manager::connect(
            &self.bus,
            self.vif_idx,
            &config.ssid,
            &network.bssid,
            network.channel_freq,
            &wpa2_ie,
            timeout_ms,
        )
        .map_err(WifiError::from)?;

        log::debug!(
            "[WifiClient] Connection established: ap_idx={}",
            connect_result.ap_idx
        );

        // 保存 sta_idx 供 TX 数据帧使用
        self.bus
            .conn
            .sta_idx
            .store(connect_result.ap_idx, core::sync::atomic::Ordering::Release);

        // WPA2 四次握手
        if config.auth_type == WifiAuthType::Wpa2Psk
            && let Some(ref password) = config.password
        {
            self.perform_wpa2_handshake(
                connect_result.ap_idx,
                &network.bssid,
                &config.ssid,
                &wpa2_ie,
                password,
                timeout_ms,
            )?;
        }

        // 禁用 Power Save 模式，防止固件空闲后自动进入省电导致 TX/RX 异常
        if let Err(e) = send_me_set_ps_mode_req(&self.bus, MM_PS_MODE_OFF, 3000) {
            log::warn!("[WifiClient] Failed to disable PS mode: {:?}", e);
        }

        Ok(())
    }

    /// 执行 WPA2 四次握手
    fn perform_wpa2_handshake(
        &self,
        sta_idx: u8,
        bssid: &[u8; 6],
        ssid: &[u8],
        rsn_ie: &[u8],
        password: &[u8],
        timeout_ms: u64,
    ) -> Result<(), WifiError> {
        log::debug!("[WifiClient] Starting WPA2 handshake...");

        let own_mac = self.sta_mac.ok_or_else(|| {
            WifiError::OperationFailed("MAC address not set, call lmac_configure() first".into())
        })?;

        let mut handshake = Wpa2Handshake::new(password, ssid, bssid, &own_mac, rsn_ie);

        loop {
            // 等待 EAPOL 帧
            let eapol =
                wait_for_eapol(&self.bus, timeout_ms).map_err(|_| WifiError::ConnectionTimeout)?;

            match handshake.process_eapol(&eapol) {
                Ok(HandshakeAction::SendM2(m2)) => {
                    send_eapol_data_frame(&self.bus, bssid, &own_mac, &m2, self.vif_idx, sta_idx)
                        .map_err(|e| WifiError::OperationFailed(format!("Send M2 failed: {:?}", e)))?;
                }
                Ok(HandshakeAction::Completed(result)) => {
                    // 安装 PTK
                    manager::install_pairwise_key(
                        &self.bus,
                        self.vif_idx,
                        sta_idx,
                        MAC_CIPHER_CCMP,
                        &result.tk,
                        0,
                    )
                    .map_err(|e| {
                        WifiError::OperationFailed(format!("Install PTK failed: {:?}", e))
                    })?;

                    // 安装 GTK
                    manager::install_group_key(
                        &self.bus,
                        self.vif_idx,
                        0xFF,
                        MAC_CIPHER_CCMP,
                        &result.gtk,
                        result.gtk_key_idx,
                    )
                    .map_err(|e| {
                        WifiError::OperationFailed(format!("Install GTK failed: {:?}", e))
                    })?;

                    // 发送 M4
                    send_eapol_data_frame(
                        &self.bus,
                        bssid,
                        &own_mac,
                        &result.m4_frame,
                        self.vif_idx,
                        sta_idx,
                    )
                    .map_err(|e| WifiError::OperationFailed(format!("Send M4 failed: {:?}", e)))?;

                    // 打开控制端口
                    send_set_control_port_req(&self.bus, sta_idx, true, 5000).map_err(|e| {
                        WifiError::OperationFailed(format!("Open control port failed: {:?}", e))
                    })?;

                    log::info!("[WifiClient] WPA2 handshake completed");
                    return Ok(());
                }
                Err(WpaError::MicMismatch) => return Err(WifiError::InvalidPassword),
                Err(e) => {
                    return Err(WifiError::OperationFailed(format!(
                        "Handshake error: {:?}",
                        e
                    )));
                }
            }
        }
    }

    // ================================================================
    // Phase 4: 状态 & 控制
    // ================================================================

    /// 获取连接状态
    pub fn get_status(&self) -> ConnectionStatus {
        match self.bus.conn.get_status() {
            crate::fdrv::core::STATUS_DISCONNECTED => ConnectionStatus::Disconnected,
            crate::fdrv::core::STATUS_CONNECTING => ConnectionStatus::Connecting,
            crate::fdrv::core::STATUS_CONNECTED => ConnectionStatus::Connected,
            crate::fdrv::core::STATUS_FAILED => ConnectionStatus::Failed,
            _ => ConnectionStatus::Disconnected,
        }
    }

    /// 获取 MAC 地址（需要先调用 lmac_configure）
    pub fn get_mac_address(&self) -> Option<[u8; 6]> {
        self.sta_mac
    }

    /// 等待连接完成
    pub fn wait_for_connection(&self, timeout_ms: u64) -> Result<(), WifiError> {
        let start = crate::runtime::runtime().now_nanos();
        let timeout_ns = timeout_ms * 1_000_000;

        loop {
            if self.get_status() == ConnectionStatus::Connected {
                return Ok(());
            }
            let elapsed = crate::runtime::runtime().now_nanos() - start;
            if elapsed > timeout_ns {
                return Err(WifiError::ConnectionTimeout);
            }
            crate::runtime::runtime().yield_now();
        }
    }

    /// 断开连接
    pub fn disconnect(&self) -> Result<(), WifiError> {
        log::debug!("[WifiClient] Disconnecting...");

        if self.get_status() == ConnectionStatus::Disconnected {
            return Err(WifiError::NotConnected);
        }

        disconnect(&self.bus, self.vif_idx, 3)
            .map_err(|e| WifiError::OperationFailed(format!("Disconnect failed: {:?}", e)))?;

        self.bus
            .conn
            .set_status(crate::fdrv::core::STATUS_DISCONNECTED);
        *self.bus.conn.ap_mac.lock() = None;
        log::debug!("[WifiClient] Disconnected");
        Ok(())
    }

    /// 获取当前连接的 SSID
    pub fn get_current_ssid(&self) -> Option<Vec<u8>> {
        if self.get_status() != ConnectionStatus::Connected {
            return None;
        }
        // TODO: 从 bus 状态获取实际连接的 SSID
        None
    }

    /// 获取信号强度
    pub fn get_rssi(&self) -> Option<i8> {
        // TODO: 从 bus 状态获取当前信号强度
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wifi_config_open() {
        let config = WifiConfig::open("TestNetwork");
        assert_eq!(config.ssid, b"TestNetwork".to_vec());
        assert_eq!(config.auth_type, WifiAuthType::Open);
        assert!(config.password.is_none());
    }

    #[test]
    fn test_wifi_config_wpa2() {
        let config = WifiConfig::wpa2_psk("TestNetwork", "password123");
        assert_eq!(config.ssid, b"TestNetwork".to_vec());
        assert_eq!(config.auth_type, WifiAuthType::Wpa2Psk);
        assert_eq!(config.password, Some(b"password123".to_vec()));
    }

    #[test]
    fn test_wifi_config_with_bssid() {
        let bssid = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let config = WifiConfig::open("TestNetwork").with_bssid(bssid);
        assert_eq!(config.bssid, Some(bssid));
    }

    #[test]
    fn test_wifi_error_display() {
        let err = WifiError::NetworkNotFound;
        assert_eq!(format!("{}", err), "Network not found");
    }
}

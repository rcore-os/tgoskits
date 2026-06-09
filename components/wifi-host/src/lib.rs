//! WiFi 驱动公共接口
//!
//! 定义 `WifiDriver` trait，所有 WiFi 芯片驱动实现此 trait。
//! 上层代码（如 ArceOS runtime）通过此 trait 使用 WiFi 功能，
//! 不依赖具体芯片实现。

#![no_std]

extern crate alloc;

use alloc::string::String;

/// WiFi 驱动错误类型
#[derive(Debug)]
pub enum WifiError {
    /// 硬件未初始化或初始化失败
    NotInitialized,
    /// 扫描未找到指定网络
    NetworkNotFound,
    /// 连接超时
    ConnectionTimeout,
    /// 认证失败（密码错误等）
    AuthenticationFailed,
    /// 已经连接，不能重复连接
    AlreadyConnected,
    /// 固件错误
    FirmwareError,
    /// 不支持的操作
    Unsupported,
    /// IO 错误
    IoError,
    /// 操作失败，附带描述
    OperationFailed(String),
}

/// WiFi 驱动 trait
///
/// 生命周期：
/// ```text
/// init() → [connect() ↔ disconnect()] × N
/// ```
///
/// `init()` 执行硬件初始化、固件加载、LMAC 配置等一次性操作。
/// `connect()` / `disconnect()` 可多次调用。
pub trait WifiDriver: Send + Sync {
    /// 初始化 WiFi 硬件
    ///
    /// 包含：SDHCI/SDIO 初始化、固件加载、RX/TX 线程启动、LMAC 配置。
    /// 此方法在整个 OS 生命周期中只应调用一次。
    fn init() -> Result<Self, WifiError>
    where
        Self: Sized;

    /// 连接到 WiFi 网络
    ///
    /// 内部执行扫描、关联、认证（如 WPA2-PSK）。
    /// 可多次调用（先 disconnect 再 connect）。
    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), WifiError>;

    /// 断开 WiFi 连接
    ///
    /// 向 AP 发送 deauth 帧，清理关联状态。
    /// 应在关机/重启前调用，以便 AP 侧及时清理会话。
    fn disconnect(&mut self) -> Result<(), WifiError>;
}

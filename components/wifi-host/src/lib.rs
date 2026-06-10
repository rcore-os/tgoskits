//! WiFi 驱动公共接口
//!
//! 定义 `WifiDriver` trait，所有 WiFi 芯片驱动实现此 trait。
//! 上层代码（如 ArceOS runtime）通过此 trait 使用 WiFi 功能，
//! 不依赖具体芯片实现。

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, string::String};
use core::task::{Context, Poll};

use dma_api::DmaOp;
use rd_net::Net;

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

/// WiFi 驱动 trait（面向上层 OS）
///
/// 由具体芯片驱动实现。上层（如 Starry kernel）只依赖此 trait，不依赖任何
/// 具体芯片 crate 的内部类型。
///
/// 驱动实例由各芯片 crate 的 `probe(...)` 构造函数创建并返回
/// `Box<dyn WifiDriver>`——平台相关的 MMIO 映射 / SDHCI 枚举等由上层 OS glue
/// 完成后，把已初始化的 SDIO host 交给 `probe`。`probe` 内部完成固件加载、
/// FDRV 初始化、LMAC 配置，对应原 `init()` 的芯片侧职责。
///
/// 之后可调用 STA（`connect`/`disconnect`）或 SoftAP（`start_ap_open`）接口，
/// 并通过 [`take_net`](WifiDriver::take_net) 取出网络设备注册到协议栈。
pub trait WifiDriver: Send + Sync {
    /// 连接到 WiFi 网络（STA 模式）。
    ///
    /// 内部执行扫描、关联、认证（如 WPA2-PSK）。可多次调用。
    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), WifiError>;

    /// 断开 WiFi 连接（STA 模式），向 AP 发送 deauth 帧。
    fn disconnect(&mut self) -> Result<(), WifiError>;

    /// 启动一个开放（无加密）SoftAP，在指定信道广播 `ssid`。
    fn start_ap_open(&mut self, ssid: &[u8], channel: u8) -> Result<(), WifiError>;

    /// 返回本机 MAC 地址。
    fn mac_address(&self) -> [u8; 6];

    /// 取出网络设备并包装成 [`rd_net::Net`]，供上层注册到协议栈。
    ///
    /// `dma_op` 由上层 OS 提供（DMA 能力边界）。只应调用一次；再次调用返回
    /// `None`。
    fn take_net(&mut self, dma_op: &'static dyn DmaOp) -> Option<Net>;

    /// 注册“收到数据帧”回调。SDIO Wi-Fi 走带外 RX，不经以太网 IRQ 框架，
    /// 故数据帧入队后需主动通知协议栈 poll。
    fn set_rx_data_callback(&mut self, cb: fn());
}

/// A poll body provided by the driver core: invoked with a task context,
/// returns `Poll::Ready(())` when the operation is complete (or the task should
/// exit) and `Poll::Pending` otherwise. The OS glue drives it via its executor.
pub type PollFn<'a> = dyn FnMut(&mut Context<'_>) -> Poll<()> + 'a;

/// A pollable body that can be sent to another task (for background tasks).
pub type SendPollFn = dyn FnMut(&mut Context<'_>) -> Poll<()> + Send;

/// Returned by [`WifiRuntime::block_until`] when the deadline elapsed before the
/// poll body completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

/// WiFi 驱动核心所需的 OS 运行时能力。
///
/// driver core 自身不依赖任何具体内核（ArceOS 等）的运行时 crate；它通过此
/// trait 取得定时、延时、让步等能力。由 OS glue 层实现并注入。
///
/// `spawn` 用于启动驱动的后台轮询循环（RX/TX/AP）。`body` 会被反复调用：
/// 返回 `true` 表示循环应继续，返回 `false` 表示总线已关闭、线程应退出。
/// glue 负责把它驱动起来（例如包进 `block_on(poll_fn(...))`）并在
/// 被唤醒时重新 poll。
pub trait WifiRuntime: Send + Sync + 'static {
    /// 单调递增时钟，纳秒。用于超时计算与耗时测量。
    fn now_nanos(&self) -> u64;

    /// 阻塞延时指定毫秒（仅用于初始化/固件上电序列）。
    fn sleep_ms(&self, ms: u64);

    /// 让出 CPU 给其他任务（轮询等待硬件就绪时）。
    fn yield_now(&self);

    /// 启动一个命名的后台轮询任务。
    ///
    /// `poll` 是驱动核心提供的轮询体（不含任何 OS executor 细节）：每次被调用
    /// 返回 `Poll::Pending` 表示尚未结束、`Poll::Ready(())` 表示任务应退出。
    /// glue 负责用本内核的 executor（如 `block_on(poll_fn(...))`）驱动它，并在
    /// 关联的 waker 被唤醒时重新 poll。
    fn spawn_poll_task(&self, name: &str, poll: Box<SendPollFn>);

    /// 阻塞当前任务直到 `poll` 返回 `Poll::Ready`，最多等待 `timeout_ms`
    /// 毫秒（`None` 表示不限时）。超时返回 [`TimedOut`]。
    ///
    /// 用于命令/握手等待这类“阻塞直到响应或超时”的同步点。
    fn block_until(&self, timeout_ms: Option<u64>, poll: &mut PollFn<'_>) -> Result<(), TimedOut>;
}

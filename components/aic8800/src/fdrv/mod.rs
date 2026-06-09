extern crate alloc;

// 模块声明
pub mod consts;
pub mod core;
pub mod crypto;
pub mod net;
pub mod protocol;
pub mod thread;
pub mod wifi;

// ===== 核心 API 重新导出 =====

pub use core::{BusState, WifiBus, init, sdio1_irq_handler};

// 网络设备注册
pub use net::device::{AicWifiNetDev, store_wifi_net_device, take_wifi_net_device};
// RX 数据帧回调注册(上层用其驱动网络栈 poll)
pub use thread::rx::register_rx_data_callback;
// WiFi 客户端 + 配置类型
pub use wifi::api::{
    ConnectionStatus, WifiAuthType, WifiClient, WifiConfig, WifiEncryption, WifiError, WifiNetwork,
};

//! Hub 设备
//!
//! 表示一个 Hub 设备（Root Hub 或 External Hub），管理端口状态和设备枚举。

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::time::Duration;
use futures::{FutureExt, future::BoxFuture};

use usb_if::{
    descriptor::{Class, ConfigurationDescriptor, DeviceDescriptor, EndpointType},
    err::USBError,
    host::{
        ControlSetup,
        hub::{HubDescriptor, PortFeature, PortStatus, PortStatusChange, Speed},
    },
    transfer::{Recipient, Request, RequestType},
};

use super::HubOp;
use crate::{
    Device,
    backend::kmod::hub::{HubInfo, PortChangeInfo},
    osal::Kernel,
};

// Hub 枚举常量 (参照 Linux 内核)

/// 防抖动超时 (2秒)
const HUB_DEBOUNCE_TIMEOUT: u64 = 2000;

/// 防抖动检查间隔 (25ms)
const HUB_DEBOUNCE_STEP: u64 = 25;

/// 防抖动稳定时间 (100ms)
const HUB_DEBOUNCE_STABLE: u64 = 100;

/*
 * Hub Device descriptor
 * USB Hub class device protocols
 */
const HUB_PR_FS: u8 = 0; /* Full speed hub */
const HUB_PR_HS_SINGLE_TT: u8 = 1; /* Hi-speed hub with single TT */
const HUB_PR_HS_MULTI_TT: u8 = 2; /* Hi-speed hub with multiple TT */
const HUB_PR_SS: u8 = 3; /* Super speed hub */

/// Hub 设备
///
/// 表示一个 Hub 设备（Root Hub 或 External Hub）。
pub struct HubDevice {
    settings: HubSettings,
    data: Box<Inner>,
    kernel: Kernel,
}

struct Inner {
    /// Hub 状态
    pub state: HubState,

    /// 端口数量
    pub num_ports: u8,

    /// 端口列表
    pub ports: Vec<Port>,

    pub dev: Device,

    pub descriptor: HubDescriptor,

    pub parent_hub_slot_id: u8,

    /// Root Hub 端口 ID（如果这是外部 Hub）
    pub root_port_id: u8,
}

pub struct HubSettings {
    pub config_value: u8,
    pub interface_number: u8,
    pub alt_setting: u8,
}

impl HubOp for HubDevice {
    fn slot_id(&self) -> u8 {
        self.data.dev.slot_id()
    }

    fn init(&mut self, info: HubInfo) -> BoxFuture<'_, Result<HubInfo, USBError>> {
        self.configure(info).boxed()
    }

    fn changed_ports<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<PortChangeInfo>, USBError>> {
        self.changed_ports().boxed()
    }
}

impl HubDevice {
    /// returns (config_value, interface_number) if the device is a hub
    pub fn is_hub(
        desc: &DeviceDescriptor,
        configs: &[ConfigurationDescriptor],
    ) -> Option<HubSettings> {
        if !matches!(desc.class(), Class::Hub(_)) {
            return None;
        }
        let Some(config) = configs.first() else {
            warn!("Hub device has no configurations");
            return None;
        };

        for interface in &config.interfaces {
            for alt in &interface.alt_settings {
                if alt.subclass != 0x00 && alt.protocol != 0x00 {
                    continue;
                }

                if alt.num_endpoints != 1 {
                    continue;
                }

                if alt.endpoints[0].transfer_type != EndpointType::Interrupt
                    || alt.endpoints[0].direction != usb_if::transfer::Direction::In
                {
                    continue;
                }

                return Some(HubSettings {
                    config_value: config.configuration_value,
                    interface_number: interface.interface_number,
                    alt_setting: alt.alternate_setting,
                });
            }
        }

        None
    }

    /// 创建新的 Hub 设备
    pub async fn new(
        dev: Device,
        settings: HubSettings,
        root_port_id: u8,
        parent_hub_slot_id: u8,
        kernel: &Kernel,
    ) -> Result<Self, USBError> {
        Ok(Self {
            settings,
            data: Box::new(Inner {
                state: HubState::Uninitialized,
                num_ports: 0,
                ports: vec![],
                dev,
                descriptor: unsafe { core::mem::zeroed() },
                parent_hub_slot_id,
                root_port_id,
            }),
            kernel: kernel.clone(),
        })
    }

    pub async fn changed_ports(&mut self) -> Result<Vec<PortChangeInfo>, USBError> {
        let mut changed_ports = vec![];

        // 收集所有端口号，避免借用冲突

        for port_idx in 0..self.data.num_ports {
            let port_id = port_idx + 1;
            let (status, change) = self.get_port_status(port_id).await?;

            debug!("Port {} status: {:?}", port_id, status);

            if change.connection_changed {
                info!("Port {} connection changed: {}", port_id, status.connected);
                // 清除连接变化标志
                self.clear_port_feature(port_id, PortFeature::CConnection)
                    .await?;
            }

            if status.connected && self.data.ports[port_idx as usize].state == PortState::Uninit {
                info!(
                    "Port {} connection changed: connected={}, enabled={}",
                    port_id, status.connected, status.enabled
                );

                // 执行端口验证流程（参考 xHCI Root Hub）
                let validation_result = self.handle_port_connection(port_id, &status).await?;

                self.data.ports[port_idx as usize].state = PortState::Probed;

                changed_ports.push(validation_result);
            }

            if change.enabled_changed {
                info!("Port {} enabled changed: {}", port_id, status.enabled);
                self.clear_port_feature(port_id, PortFeature::CEnable)
                    .await?;
                if let Some(port) = self.data.ports.iter_mut().find(|p| p.id == port_id) {
                    port.status = status;
                }
            }

            if change.reset_complete {
                debug!("Port {} reset complete", port_id);
                self.clear_port_feature(port_id, PortFeature::CReset)
                    .await?;
                if let Some(port) = self.data.ports.iter_mut().find(|p| p.id == port_id) {
                    port.status = status;
                }
            }
        }

        Ok(changed_ports)
    }

    pub fn is_superspeed(&self) -> bool {
        self.data.dev.descriptor().protocol == 3
    }

    pub async fn configure(&mut self, info: HubInfo) -> Result<HubInfo, USBError> {
        // 第二阶段：获取 Hub 描述符（带重试）
        debug!("Configuring hub device, depth={}...", info.hub_depth);
        let mut info = info;

        trace!(
            "settings: config_value={}, interface_number={}, alt_setting={}",
            self.settings.config_value, self.settings.interface_number, self.settings.alt_setting
        );

        let descriptor = self.get_hub_descriptor().await?;

        self.data.descriptor = descriptor;

        if self.hub_descriptor().bNbrPorts == 0 {
            return Err(USBError::from("Hub has zero ports"));
        }
        self.data.num_ports = self.hub_descriptor().bNbrPorts;

        // 解析 Hub 特性和配置参数（参考 U-Boot usb_hub_configure）
        let characteristics = self.data.descriptor.hub_characteristics();

        // 解析 TT 思考时间（Bits[6:5]，参考 USB 2.0 规范）
        let ttt_bits = (characteristics >> 5) & 0x03;
        info.tt.think_time_ns = match ttt_bits {
            0 => 0,       // No TT
            1 => 666,     // 8 FS bit times
            2 => 666 * 2, // 16 FS bit times
            3 => 666 * 3, // 24 FS bit times
            _ => unreachable!(),
        };

        // 判断 Multi-TT（参考 U-Boot）
        // protocol = 0: Full Speed Hub (no TT)
        // protocol = 1: High Speed Single TT
        // protocol = 2: High Speed Multi TT
        // protocol = 3: SuperSpeed Hub (no TT)
        let device_protocol = self.data.dev.descriptor().protocol;

        match device_protocol {
            HUB_PR_FS => {
                info.speed = Speed::Full;
            }
            HUB_PR_HS_SINGLE_TT => {
                info.speed = Speed::High;
                debug!("Hub is High Speed with Single TT");
            }
            HUB_PR_HS_MULTI_TT => {
                info.speed = Speed::High;
                debug!("Hub is High Speed with Multiple TTs");
                match self.data.dev.claim_interface(0, 1).await {
                    Ok(_) => {
                        debug!("TT per port");
                        info.tt.multi = true;
                    }
                    Err(e) => {
                        debug!("Using single TT due to claim interface failure: {e}");
                    }
                }
            }
            HUB_PR_SS => {
                info.speed = Speed::SuperSpeed;
            }
            _ => {
                warn!("Unknown hub protocol: {}", device_protocol);
            }
        }

        debug!(
            "Hub parameters: ports={}, protocol={}, multi_tt={}, tt_think_time={}ns",
            self.data.num_ports, device_protocol, info.tt.multi, info.tt.think_time_ns
        );

        let status = self.get_hub_status().await?;

        debug!(
            "local power source: {}",
            if status.local_power_source() {
                "lost (inactive)"
            } else {
                "good"
            }
        );

        debug!(
            "over current condition exists: {}",
            if status.over_current() { "" } else { "no " }
        );

        // 构造 HubParams
        let params = crate::backend::ty::HubParams {
            num_ports: self.data.num_ports,
            multi_tt: info.tt.multi,
            tt_think_time_ns: info.tt.think_time_ns as _,
            parent_hub_slot_id: self.data.parent_hub_slot_id,
            root_hub_port_number: self.data.root_port_id,
        };

        // 更新 xHCI Slot Context（如果后端支持）
        self.data.dev.update_hub(params).await?;

        if info.hub_depth > -1 && self.is_superspeed() {
            assert!(
                info.hub_depth < 5,
                "Hub depth too large: {}",
                info.hub_depth
            );

            // 外部 SuperSpeed Hub 需要设置 Hub 深度
            self.set_hub_depth(info.hub_depth as _).await?;
            debug!("Set hub depth to {}", info.hub_depth);
        }

        // 第三阶段：初始化端口状态（参考 Linux hub_activate）
        // 初始化所有端口为 Disconnected 状态
        self.data.ports = (1..=self.data.num_ports).map(Port::new).collect();

        self.hub_power_on().await?;

        // 标记 Hub 为运行状态
        self.data.state = HubState::Running;
        debug!("Hub initialized with {} ports", self.data.num_ports);
        Ok(info)
    }

    async fn set_hub_depth(&mut self, depth: u8) -> Result<(), USBError> {
        self.data
            .dev
            .ctrl_ep_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Device,
                    request: Request::Other(0x0c),
                    value: depth as _,
                    index: 0,
                },
                &[],
            )
            .await?;
        Ok(())
    }

    fn hub_descriptor(&self) -> &HubDescriptor {
        &self.data.descriptor
    }

    /// 获取 Hub 描述符（参考 Linux 内核实现）
    async fn get_hub_descriptor(&mut self) -> Result<HubDescriptor, USBError> {
        let mut buff = vec![0u8; 4]; // Hub 描述符最小长度
        self.read_hub_descriptor_raw(&mut buff).await?;
        let desc_len = buff[0] as usize;
        trace!("Hub descriptor length from initial read: {}", desc_len);

        let mut full_buff = vec![0u8; desc_len];

        self.read_hub_descriptor_raw(&mut full_buff).await?;

        let desc = unsafe { (full_buff.as_ptr() as *const HubDescriptor).read_unaligned() };
        Ok(desc)
    }

    async fn read_hub_descriptor_raw(&mut self, buff: &mut [u8]) -> Result<(), USBError> {
        const DT_SS_HUB: u16 = 0x0a;
        const DT_HUB: u16 = 0x9;
        const TYPE_CLASS: u16 = 1 << 5;

        let dtype = if self.is_superspeed() {
            DT_SS_HUB
        } else {
            DT_HUB
        } | TYPE_CLASS;

        let n = self
            .data
            .dev
            .ctrl_ep_mut()
            .control_in(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Device,
                    request: Request::GetDescriptor,
                    value: dtype << 8,
                    index: 0,
                },
                buff,
            )
            .await?;
        trace!("Hub raw descriptor read {n} bytes");

        Ok(())
    }

    async fn hub_power_on(&mut self) -> Result<(), USBError> {
        for port_id in 1..=self.data.num_ports {
            self.set_port_feature(port_id, PortFeature::Power).await?;
            debug!("Powered on port {}", port_id);
        }

        self.kernel.delay(Duration::from_millis(100));
        Ok(())
    }

    async fn get_hub_status(&mut self) -> Result<HubStatus, USBError> {
        let mut buffer = vec![0u8; size_of::<HubStatus>()];

        self.data
            .dev
            .ctrl_ep_mut()
            .control_in(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Device,
                    request: Request::GetStatus,
                    value: 0,
                    index: 0,
                },
                &mut buffer,
            )
            .await?;

        let status = u16::from_le_bytes([buffer[0], buffer[1]]);
        let change = u16::from_le_bytes([buffer[2], buffer[3]]);
        trace!("Hub raw status: 0x{:04x}, change: 0x{:04x}", status, change);
        Ok(HubStatus { status, change })
    }

    // ========== 端口状态获取方法 ==========

    /// 获取端口状态 (参照 Linux usb_hub_port_status)
    ///
    /// 返回: (端口状态, 状态变化标志)
    async fn get_port_status(
        &mut self,
        port_id: u8,
    ) -> Result<(PortStatus, PortStatusChange), USBError> {
        let mut buffer = vec![0u8; 4]; // wPortStatus (2字节) + wPortChange (2字节)

        self.data
            .dev
            .ctrl_ep_mut()
            .control_in(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Other, // Port
                    request: Request::GetStatus,
                    value: 0,
                    index: port_id as u16,
                },
                &mut buffer,
            )
            .await?;

        // 解析端口状态和变化
        let status_raw = u16::from_le_bytes([buffer[0], buffer[1]]);
        let change_raw = u16::from_le_bytes([buffer[2], buffer[3]]);

        trace!(
            "Port {} raw status: 0x{:04x}, change: 0x{:04x}",
            port_id, status_raw, change_raw
        );

        Ok((
            self.parse_port_status(status_raw),
            self.parse_port_change(change_raw),
        ))
    }

    /// 解析端口状态原始数据
    fn parse_port_status(&self, raw: u16) -> PortStatus {
        PortStatus {
            connected: (raw & 0x0001) != 0,
            enabled: (raw & 0x0002) != 0,
            suspended: (raw & 0x0004) != 0,
            over_current: (raw & 0x0008) != 0,
            resetting: (raw & 0x0010) != 0,
            powered: (raw & 0x0100) != 0,
            low_speed: (raw & 0x0200) != 0,
            high_speed: (raw & 0x0400) != 0,
            speed: Speed::from_usb2_hub_status(raw),
            change: PortStatusChange {
                connection_changed: false,
                enabled_changed: false,
                reset_complete: false,
                suspend_changed: false,
                over_current_changed: false,
            },
        }
    }

    /// 解析端口状态变化标志
    fn parse_port_change(&self, raw: u16) -> PortStatusChange {
        PortStatusChange {
            connection_changed: (raw & 0x0001) != 0,
            enabled_changed: (raw & 0x0002) != 0,
            suspend_changed: (raw & 0x0004) != 0,
            over_current_changed: (raw & 0x0008) != 0,
            reset_complete: (raw & 0x0010) != 0,
        }
    }

    /// 设置端口特性
    async fn set_port_feature(
        &mut self,
        port_index: u8,
        feature: PortFeature,
    ) -> Result<(), USBError> {
        self.data
            .dev
            .ctrl_ep_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Other,
                    request: Request::SetFeature,
                    value: feature as u16,
                    index: port_index as u16,
                },
                &[],
            )
            .await
            .map_err(USBError::from)?;
        Ok(())
    }

    /// 清除端口特性
    async fn clear_port_feature(
        &mut self,
        port_id: u8,
        feature: PortFeature,
    ) -> Result<(), USBError> {
        self.data
            .dev
            .ctrl_ep_mut()
            .control_out(
                ControlSetup {
                    request_type: RequestType::Class,
                    recipient: Recipient::Other,
                    request: Request::ClearFeature,
                    value: feature as u16,
                    index: port_id as u16,
                },
                &[],
            )
            .await
            .map_err(USBError::from)?;
        Ok(())
    }

    // ========== 防抖动机制 ==========

    /// 防抖动检测 (参照 Linux hub_port_debounce_be_stable)
    ///
    /// 确保端口连接状态稳定，避免抖动导致误判。
    ///
    /// # 参数
    /// - `port_index`: 端口号（1-based）
    /// - `must_be_connected`: 期望的连接状态
    ///
    /// # 返回
    /// 稳定后的端口状态
    async fn debounce_port(
        &mut self,
        port_index: u8,
        must_be_connected: bool,
    ) -> Result<PortStatus, USBError> {
        let mut stable_count = 0u8;
        let required_stable = (HUB_DEBOUNCE_STABLE / HUB_DEBOUNCE_STEP) as u8;
        let max_attempts = (HUB_DEBOUNCE_TIMEOUT / HUB_DEBOUNCE_STEP) as u8;

        info!(
            "Starting debounce on port {} (expected_connected: {})",
            port_index, must_be_connected
        );

        for attempt in 0..max_attempts {
            // 等待检查间隔（25ms）
            self.kernel
                .delay(core::time::Duration::from_millis(HUB_DEBOUNCE_STEP));

            // 获取当前状态
            let (status, _change) = self.get_port_status(port_index).await?;

            // 验证连接状态是否符合期望
            if status.connected == must_be_connected {
                stable_count = stable_count.saturating_add(1);
                debug!(
                    "Port {} debounce stable: {}/{} (attempt {})",
                    port_index, stable_count, required_stable, attempt
                );

                if stable_count >= required_stable {
                    info!(
                        "Port {} debounce stable (connected: {})",
                        port_index, status.connected
                    );
                    return Ok(status);
                }
            } else {
                // 状态不稳定，重置计数
                stable_count = 0;
                debug!(
                    "Port {} debounce unstable, current_connected: {}, expected: {}",
                    port_index, status.connected, must_be_connected
                );
            }
        }

        // 超时
        warn!(
            "Port {} debounce timeout after {} attempts ({}ms)",
            port_index, max_attempts, HUB_DEBOUNCE_TIMEOUT
        );
        Err(USBError::Timeout)
    }

    // ========== 设备枚举核心方法 ==========

    /// 端口复位 (参照 Linux hub_port_reset)
    ///
    /// 复位端口并等待复位完成。
    ///
    /// # 参数
    /// - `port_index`: 端口号（1-based）
    /// - `status`: 当前端口状态
    async fn reset_port(&mut self, port_id: u8, status: &PortStatus) -> Result<(), USBError> {
        info!("Resetting port {}", port_id);

        // 发送复位请求
        self.set_port_feature(port_id, PortFeature::Reset).await?;

        // 确定复位时间（低速设备需要长复位）
        let reset_time = if status.low_speed {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(50)
        };

        // 等待复位完成
        self.kernel.delay(reset_time);

        // 等待复位完成标志（最多等待 100ms）
        for _retry in 0..10 {
            let (_status, change) = self.get_port_status(port_id).await?;

            if change.reset_complete {
                // 清除复位完成标志
                self.clear_port_feature(port_id, PortFeature::CReset)
                    .await?;
                info!("Port {} reset complete", port_id);
                return Ok(());
            }

            self.kernel.delay(Duration::from_millis(10));
        }

        warn!("Port {} reset timeout", port_id);
        Err(USBError::Timeout)
    }

    /// 处理端口连接事件（参考 xHCI Root Hub 状态机）
    ///
    /// 三阶段验证流程：
    /// 1. 防抖动检测 - 确保连接稳定
    /// 2. 端口复位 - 复位设备到默认状态
    /// 3. 等待启用 - 等待端口启用并验证速度
    async fn handle_port_connection(
        &mut self,
        port_id: u8,
        initial_status: &PortStatus,
    ) -> Result<PortChangeInfo, USBError> {
        info!(
            "Handling connection on port {}, speed: {:?}",
            port_id, initial_status.speed
        );

        // 阶段 1: 防抖动检测（确保连接稳定）
        let stable_status = self.debounce_port(port_id, true).await?;
        if !stable_status.connected {
            return Err(USBError::from("Connection unstable"));
        }

        // 阶段 2: 端口复位
        self.reset_port(port_id, &stable_status).await?;

        // 阶段 3: 等待端口启用（最多等待 500ms）
        let enabled_status = self.wait_for_port_enabled(port_id).await?;

        // 阶段 4: 验证并生成 DeviceAddressInfo
        let port_speed = enabled_status.speed;

        info!(
            "Port {} device ready: speed={:?}, enabled={}",
            port_id, port_speed, enabled_status.enabled
        );

        // ✅ 修复：更新 tt_required 字段
        // 根据 xHCI 规范，LS/FS 设备连接在 HS Hub 时需要 TT
        let hub_speed = match self.data.dev.descriptor().protocol {
            // Hub 协议值：0=FS Hub, 1/2=HS Hub, 3=SS Hub
            1 | 2 => Speed::High,   // HS Hub
            3 => Speed::SuperSpeed, // SS Hub
            _ => Speed::Full,       // FS Hub
        };

        let port = &mut self.data.ports[port_id as usize - 1];

        // TT 需求判断：使用 DeviceSpeed::requires_tt 方法
        port.tt_required = port_speed.requires_tt(hub_speed);

        debug!(
            "TT required: port_speed={:?}, hub_speed={:?}, tt_required={}",
            port_speed, hub_speed, port.tt_required
        );

        let tt_port_on_hub = if port.tt_required {
            Some(port_id)
        } else {
            None
        };

        Ok(PortChangeInfo {
            root_port_id: self.root_port_id(),
            port_id,
            port_speed,
            tt_port_on_hub,
        })
    }

    /// 等待端口启用（参照 xHCI Root Hub 的 handle_reseted）
    async fn wait_for_port_enabled(&mut self, port_id: u8) -> Result<PortStatus, USBError> {
        const MAX_WAIT_MS: u64 = 500;
        const CHECK_INTERVAL_MS: u64 = 10;
        let max_attempts = MAX_WAIT_MS / CHECK_INTERVAL_MS;

        for attempt in 0..max_attempts {
            let (status, _change) = self.get_port_status(port_id).await?;

            if status.enabled && status.connected {
                info!("Port {} enabled after {} checks", port_id, attempt + 1);
                return Ok(status);
            }

            if !status.connected {
                return Err(USBError::from("Device disconnected during enable wait"));
            }

            self.kernel.delay(Duration::from_millis(CHECK_INTERVAL_MS));
        }

        warn!("Port {} enable timeout after {}ms", port_id, MAX_WAIT_MS);
        Err(USBError::Timeout)
    }

    /// 获取 Hub 的 root_port_id
    pub fn root_port_id(&self) -> u8 {
        self.data.root_port_id
    }
}

/// Hub 状态
#[derive(Debug)]
pub enum HubState {
    /// 未初始化
    Uninitialized,

    /// 运行中
    Running,
}

/// 端口
pub struct Port {
    /// 端口号（1-based）
    pub id: u8,

    /// 端口状态
    pub status: PortStatus,

    /// 端口状态机
    pub state: PortState,

    /// 是否需要 Transaction Translator
    pub tt_required: bool,
}

impl Port {
    /// 创建新端口
    pub fn new(index: u8) -> Self {
        Self {
            id: index,
            status: PortStatus {
                connected: false,
                enabled: false,
                suspended: false,
                over_current: false,
                resetting: false,
                powered: false,
                low_speed: false,
                high_speed: false,
                speed: usb_if::host::hub::Speed::Full,
                change: usb_if::host::hub::PortStatusChange {
                    connection_changed: false,
                    enabled_changed: false,
                    reset_complete: false,
                    suspend_changed: false,
                    over_current_changed: false,
                },
            },
            state: PortState::Uninit,
            tt_required: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortState {
    #[default]
    Uninit,
    Reseted,
    Probed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct HubStatus {
    status: u16,
    change: u16,
}

impl HubStatus {
    #[allow(dead_code)]
    fn local_power_source(&self) -> bool {
        (self.status & 0x0001) != 0
    }

    #[allow(dead_code)]
    fn over_current(&self) -> bool {
        (self.status & 0x0002) != 0
    }
}

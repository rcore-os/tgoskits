/// 寄存器宽度
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegWidth {
    U8,
    U16,
    U32,
    U64,
}

/// 内存屏障类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBarrierType {
    Read,
    Write,
    Full,
}

/// Hub 类请求
///
/// 参照 USB 2.0 规范表 11-15。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubRequest {
    GetHubDescriptor,
    GetHubStatus,
    SetHubFeature,
    ClearHubFeature,
    GetPortStatus,
    SetPortFeature,
    ClearPortFeature,
    GetHubDescriptor16, // USB 3.0+
}

/// 端口特性选择器
///
/// 参照 USB 2.0 规范表 11-17。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFeature {
    Connection = 0,
    Enable = 1,
    Suspend = 2,
    OverCurrent = 3,
    Reset = 4,
    Power = 8,
    LowSpeed = 9,
    CConnection = 16,  // 清除连接变化
    CEnable = 17,      // 清除使能变化
    CSuspend = 18,     // 清除挂起变化
    COverCurrent = 19, // 清除过流变化
    CReset = 20,       // 清除复位完成
}

const USB_MAXCHILDREN: usize = 8;
const DEVICE_BITMAP_BYTES: usize = (USB_MAXCHILDREN + 1 + 7).div_ceil(8);

#[derive(Clone, Copy)]
#[allow(non_snake_case)]
#[repr(C, packed)]
pub struct HubDescriptor {
    pub bDescLength: u8,
    pub bDescriptorType: u8,
    pub bNbrPorts: u8,
    wHubCharacteristics: u16,
    pub bPwrOn2PwrGood: u8,
    pub bHubContrCurrent: u8,
    pub u: HubDescriptorVariant,
}

impl HubDescriptor {
    pub fn hub_characteristics(&self) -> u16 {
        u16::from_le(self.wHubCharacteristics)
    }

    /// 从字节数组创建 HubDescriptor 引用
    ///
    /// 如果数据长度不足或描述符类型不匹配，返回 None
    pub fn from_bytes(data: &[u8]) -> Option<&Self> {
        if data.is_empty() {
            return None;
        }

        let length = data[0] as usize;
        if data.len() < length {
            return None;
        }

        // 检查描述符类型（0x29 = Hub 描述符）
        if data.len() < 2 || data[1] != 0x29 {
            return None;
        }

        // SAFETY: 已检查数据长度和类型，指针有效且对齐
        Some(unsafe { &*(data.as_ptr() as *const HubDescriptor) })
    }
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub union HubDescriptorVariant {
    pub hs: HighSpeedHubDescriptorTail,
    pub ss: SuperSpeedHubDescriptorTail,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct HighSpeedHubDescriptorTail {
    pub device_removable: [u8; DEVICE_BITMAP_BYTES],
    pub port_pwr_ctrl_mask: [u8; DEVICE_BITMAP_BYTES],
}

#[allow(non_snake_case)]
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct SuperSpeedHubDescriptorTail {
    pub bHubHdrDecLat: u8,
    wHubDelay: u16,
    device_removable: u16,
}

impl SuperSpeedHubDescriptorTail {
    pub fn hub_delay(&self) -> u16 {
        u16::from_le(self.wHubDelay)
    }
    pub fn device_removable(&self) -> u16 {
        u16::from_le(self.device_removable)
    }
}

/// Transaction Translator 信息
///
/// 用于高速 Hub 与低速/全速设备的通信。
#[derive(Debug, Clone, Copy)]
pub struct TtInfo {
    /// TT 思考时间（单位：2 微秒）
    pub think_time: u8,

    /// 是否有多个 TT
    pub multi_tt: bool,

    /// TT 端口数量
    pub num_ports: u8,
}

/// Hub 特性
///
/// 参照 USB 2.0 规范图 11-16。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubCharacteristics {
    /// 电源切换模式
    pub power_switching: PowerSwitchingMode,

    /// 复合设备
    pub compound_device: bool,

    /// 过流保护模式
    pub over_current_mode: OverCurrentMode,

    /// 端口指示灯支持
    pub port_indicators: bool,
}

/// 电源切换模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSwitchingMode {
    /// 所有端口同时供电
    Ganged,

    /// 每个端口独立控制
    Individual,

    /// 无电源控制（总是供电）
    AlwaysPower,
}

/// 过流保护模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverCurrentMode {
    /// 全局过流保护
    Global,

    /// 每个端口独立保护
    Individual,
}

/// 端口状态
///
/// 参照 USB 2.0 规范表 11-21。
#[derive(Debug, Clone, Copy)]
pub struct PortStatus {
    /// 当前连接状态
    pub connected: bool,

    /// 端口已启用
    pub enabled: bool,

    /// 已挂起
    pub suspended: bool,

    /// 过流检测
    pub over_current: bool,

    /// 复位中
    pub resetting: bool,

    /// 电源已开启
    pub powered: bool,

    /// 低速设备连接
    pub low_speed: bool,

    /// 高速设备连接
    pub high_speed: bool,

    /// 端口速度
    pub speed: Speed,

    /// 端口状态变化标志
    pub change: PortStatusChange,
}

/// 端口状态变化标志
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortStatusChange {
    /// 连接状态变化
    pub connection_changed: bool,

    /// 启用状态变化
    pub enabled_changed: bool,

    /// 复位完成
    pub reset_complete: bool,

    /// 挂起状态变化
    pub suspend_changed: bool,

    /// 过流状态变化
    pub over_current_changed: bool,
}

/// USB 设备速度
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Speed {
    Low = 0,
    #[default]
    Full = 1,
    High = 2,
    Wireless = 3,
    SuperSpeed = 4,
    SuperSpeedPlus = 5,
}

impl From<u8> for Speed {
    fn from(value: u8) -> Self {
        match value {
            0 => Speed::Low,
            1 => Speed::Full,
            2 => Speed::High,
            3 => Speed::Wireless,
            4 => Speed::SuperSpeed,
            5 => Speed::SuperSpeedPlus,
            _ => Speed::Full,
        }
    }
}

impl Speed {
    /// 从 USB 2.0 Hub wPortStatus 解析速度
    ///
    /// 根据 USB 2.0 规范（第 11.24.2.7 节）：
    /// - Bit 9 (0x0200): Low Speed
    /// - Bit 10 (0x0400): High Speed
    /// - Bit 11 (0x0800): SuperSpeed (USB 3.0)
    /// - 默认: Full Speed
    pub fn from_usb2_hub_status(raw: u16) -> Self {
        if (raw & 0x0200) != 0 {
            Speed::Low
        } else if (raw & 0x0400) != 0 {
            Speed::High
        } else if (raw & 0x0600) != 0 {
            Speed::SuperSpeed
        } else {
            Speed::Full
        }
    }

    /// 从 xHCI PORTSC PortSpeed 字段解析速度
    ///
    /// 根据 xHCI 规范（第 4.19.2 节）：
    /// - 1 = Full Speed
    /// - 2 = Low Speed
    /// - 3 = High Speed
    /// - 4 = SuperSpeed
    /// - 5 = SuperSpeedPlus
    pub fn from_xhci_portsc(speed_value: u8) -> Self {
        match speed_value {
            1 => Speed::Full,
            2 => Speed::Low,
            3 => Speed::High,
            4 => Speed::SuperSpeed,
            5 => Speed::SuperSpeedPlus,
            _ => Speed::Full, // Reserved/Unknown
        }
    }

    /// 转换为 xHCI Slot Context 速度值
    ///
    /// 根据 xHCI 规范（第 6.2.2 节）Slot Context Speed 字段：
    /// - 1 = Full Speed
    /// - 2 = Low Speed
    /// - 3 = High Speed
    /// - 4 = Super Speed
    pub fn to_xhci_slot_value(&self) -> u8 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::SuperSpeed => 4,
            Speed::SuperSpeedPlus => 5,
            Speed::Wireless => 3,
        }
    }

    /// Convert to the raw PORTSC.PortSpeed encoding used by xHCI registers.
    ///
    /// Values follow xHCI 4.19.2: 1=FS, 2=LS, 3=HS, 4=SS, 5=SS+.
    pub fn to_xhci_portsc_value(&self) -> u8 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::SuperSpeed => 4,
            Speed::SuperSpeedPlus => 5,
            Speed::Wireless => 3,
        }
    }

    /// 判断是否需要 Transaction Translator
    ///
    /// 根据 xHCI 规范：
    /// - LS/FS 设备连接在 HS Hub 上需要 TT
    pub fn requires_tt(&self, hub_speed: Self) -> bool {
        // 设备是 LS 或 FS，且 Hub 是 HS
        matches!(self, Self::Low | Self::Full) && matches!(hub_speed, Self::High)
    }

    /// 判断设备是否为 Low Speed 或 Full Speed
    pub fn is_low_or_full_speed(&self) -> bool {
        matches!(self, Self::Low | Self::Full)
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

impl HubCharacteristics {
    /// 从描述符原始数据解析
    ///
    /// 参照 USB 2.0 规范图 11-16。
    pub fn from_descriptor(value: u16) -> Self {
        let power_switching = match value & 0x03 {
            0x01 => PowerSwitchingMode::Ganged,
            0x02 => PowerSwitchingMode::Individual,
            _ => PowerSwitchingMode::AlwaysPower,
        };

        let compound_device = (value & 0x04) != 0;
        let over_current_mode = if (value & 0x08) != 0 {
            OverCurrentMode::Individual
        } else {
            OverCurrentMode::Global
        };
        let port_indicators = (value & 0x10) != 0;

        Self {
            power_switching,
            compound_device,
            over_current_mode,
            port_indicators,
        }
    }

    /// 转换为描述符原始数据
    pub fn to_descriptor(&self) -> u16 {
        let mut value = 0u16;

        value |= match self.power_switching {
            PowerSwitchingMode::Ganged => 0x01,
            PowerSwitchingMode::Individual => 0x02,
            PowerSwitchingMode::AlwaysPower => 0x00,
        };

        if self.compound_device {
            value |= 0x04;
        }

        if matches!(self.over_current_mode, OverCurrentMode::Individual) {
            value |= 0x08;
        }

        if self.port_indicators {
            value |= 0x10;
        }

        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hub_characteristics_roundtrip() {
        let original = HubCharacteristics {
            power_switching: PowerSwitchingMode::Individual,
            compound_device: true,
            over_current_mode: OverCurrentMode::Global,
            port_indicators: true,
        };

        let descriptor = original.to_descriptor();
        let decoded = HubCharacteristics::from_descriptor(descriptor);

        assert_eq!(original.power_switching, decoded.power_switching);
        assert_eq!(original.compound_device, decoded.compound_device);
        assert_eq!(original.over_current_mode, decoded.over_current_mode);
        assert_eq!(original.port_indicators, decoded.port_indicators);
    }

    #[test]
    fn test_hub_descriptor_from_bytes() {
        // 测试数据：4 端口 Hub
        let data = [
            0x09, // bDescLength = 9
            0x29, // bDescriptorType = 0x29 (Hub)
            0x04, // bNbrPorts = 4
            0x12, 0x00, // wHubCharacteristics = 0x0012 (little-endian)
            // Bits 1:0 = 10b -> Individual power switching
            // Bit 2 = 0 -> Not a compound device
            // Bit 3 = 1 -> Individual over-current protection
            // Bit 4 = 0 -> No port indicators
            0x32, // bPwrOn2PwrGood = 50 * 2ms = 100ms
            0x64, // bHubContrCurrent = 100mA
            0x00, // DeviceRemovable (端口 0-7 位图)
            0x00, // Reserved
        ];

        let desc = HubDescriptor::from_bytes(&data).expect("Failed to parse");

        assert_eq!(desc.bNbrPorts, 4);
        assert_eq!(desc.bPwrOn2PwrGood, 50);
        assert_eq!(desc.bHubContrCurrent, 100);

        // 验证特性解析（wHubCharacteristics = 0x0012）
        // Bits 1:0 = 10b -> Individual power switching
        // Bit 2 = 0 -> Not a compound device
        // Bit 3 = 1 -> Individual over-current protection
        assert_eq!(desc.hub_characteristics(), 0x0012);
    }

    #[test]
    fn test_hub_descriptor_invalid_length() {
        let data = [0x09, 0x29]; // 太短
        assert!(HubDescriptor::from_bytes(&data).is_none());
    }

    #[test]
    fn test_hub_descriptor_invalid_type() {
        let mut data = [0x09u8; 7];
        data[1] = 0x01; // 错误的类型
        assert!(HubDescriptor::from_bytes(&data).is_none());
    }
}

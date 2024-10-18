use core::fmt::{Display, Formatter};

/// Enumeration representing the type of emulator devices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EmuDeviceType {
    /// Console device.
    EmuDeviceTConsole = 0,
    /// ARM interrupt controller V2 device.
    EmuDeviceTGicdV2 = 1,
    /// Partial passthrough interrupt controller device.
    EmuDeviceTGPPT = 2,
    /// Virtio block device.
    EmuDeviceTVirtioBlk = 3,
    /// Virtio net device.
    EmuDeviceTVirtioNet = 4,
    /// Virtio console device.
    EmuDeviceTVirtioConsole = 5,
    /// IOMMU device.
    EmuDeviceTIOMMU = 6,
    /// Interrupt ICC SRE device.
    EmuDeviceTICCSRE = 7,
    /// Interrupt ICC SGIR device.
    EmuDeviceTSGIR = 8,
    /// Interrupt controller GICR device.
    EmuDeviceTGICR = 9,
    /// Meta device.
    EmuDeviceTMeta = 10,
}

impl Display for EmuDeviceType {
    // Implementation of the Display trait for EmuDeviceType.
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            EmuDeviceType::EmuDeviceTConsole => write!(f, "console"),
            EmuDeviceType::EmuDeviceTGicdV2 => write!(f, "Arm interrupt controller V2"),
            EmuDeviceType::EmuDeviceTGPPT => write!(f, "partial passthrough interrupt controller"),
            EmuDeviceType::EmuDeviceTVirtioBlk => write!(f, "virtio block"),
            EmuDeviceType::EmuDeviceTVirtioNet => write!(f, "virtio net"),
            EmuDeviceType::EmuDeviceTVirtioConsole => write!(f, "virtio console"),
            EmuDeviceType::EmuDeviceTIOMMU => write!(f, "IOMMU"),
            EmuDeviceType::EmuDeviceTICCSRE => write!(f, "interrupt ICC SRE"),
            EmuDeviceType::EmuDeviceTSGIR => write!(f, "interrupt ICC SGIR"),
            EmuDeviceType::EmuDeviceTGICR => write!(f, "interrupt controller gicr"),
            EmuDeviceType::EmuDeviceTMeta => write!(f, "meta device"),
        }
    }
}

/// Implementation of methods for EmuDeviceType.
impl EmuDeviceType {
    /// Returns true if the device is removable.
    pub fn removable(&self) -> bool {
        matches!(
            *self,
            EmuDeviceType::EmuDeviceTGicdV2
                | EmuDeviceType::EmuDeviceTSGIR
                | EmuDeviceType::EmuDeviceTICCSRE
                | EmuDeviceType::EmuDeviceTGPPT
                | EmuDeviceType::EmuDeviceTVirtioBlk
                | EmuDeviceType::EmuDeviceTVirtioNet
                | EmuDeviceType::EmuDeviceTGICR
                | EmuDeviceType::EmuDeviceTVirtioConsole
        )
    }

    /// Converts a usize value to an EmuDeviceType.
    pub fn from_usize(value: usize) -> EmuDeviceType {
        match value {
            0 => EmuDeviceType::EmuDeviceTConsole,
            1 => EmuDeviceType::EmuDeviceTGicdV2,
            2 => EmuDeviceType::EmuDeviceTGPPT,
            3 => EmuDeviceType::EmuDeviceTVirtioBlk,
            4 => EmuDeviceType::EmuDeviceTVirtioNet,
            5 => EmuDeviceType::EmuDeviceTVirtioConsole,
            6 => EmuDeviceType::EmuDeviceTIOMMU,
            7 => EmuDeviceType::EmuDeviceTICCSRE,
            8 => EmuDeviceType::EmuDeviceTSGIR,
            9 => EmuDeviceType::EmuDeviceTGICR,
            10 => EmuDeviceType::EmuDeviceTMeta,
            _ => panic!("Unknown  EmuDeviceType value: {}", value),
        }
    }
}

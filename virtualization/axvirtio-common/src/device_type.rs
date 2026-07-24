use core::fmt;

/// VirtIO device types (simplified version)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum VirtioDeviceID {
    /// Invalid/Unknown device type
    #[default]
    Invalid = 0,

    /// Network card device
    Network = 1,

    /// Block device
    Block = 2,

    /// Console device
    Console = 3,
}

impl VirtioDeviceID {
    /// Convert device ID to device type
    pub fn from_device_id(device_id: u32) -> Self {
        match device_id {
            0 => Self::Invalid,
            1 => Self::Network,
            2 => Self::Block,
            3 => Self::Console,
            _ => Self::Invalid,
        }
    }

    /// Convert device type to device ID
    pub fn to_device_id(&self) -> u32 {
        *self as u32
    }

    /// Get the human-readable name of the device type
    pub fn name(&self) -> &'static str {
        match self {
            Self::Invalid => "Invalid",
            Self::Network => "Network",
            Self::Block => "Block",
            Self::Console => "Console",
        }
    }

    /// Check if the device type is valid (not Invalid)
    pub fn is_valid(&self) -> bool {
        !matches!(self, Self::Invalid)
    }

    /// Get all supported device types
    pub fn all_types() -> &'static [VirtioDeviceID] {
        &[Self::Network, Self::Block, Self::Console]
    }
}

impl From<u32> for VirtioDeviceID {
    fn from(value: u32) -> Self {
        Self::from_device_id(value)
    }
}

impl From<usize> for VirtioDeviceID {
    fn from(value: usize) -> Self {
        Self::from_device_id(value as u32)
    }
}

impl fmt::Display for VirtioDeviceID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.to_device_id())
    }
}

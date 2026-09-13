use crate::error::{Ext4Error, Ext4Result};

/// Linux ext4 device identifier stored in a character or block inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceNumber {
    major: u16,
    minor: u32,
}

impl DeviceNumber {
    pub const ZERO: Self = Self { major: 0, minor: 0 };

    const MAX_MAJOR: u32 = 0x0fff;
    const MAX_MINOR: u32 = 0x000f_ffff;

    pub fn new(major: u32, minor: u32) -> Ext4Result<Self> {
        if major > Self::MAX_MAJOR || minor > Self::MAX_MINOR {
            return Err(Ext4Error::invalid_input().with_operation("inode:device_number"));
        }
        Ok(Self {
            major: major as u16,
            minor,
        })
    }

    pub const fn major(self) -> u32 {
        self.major as u32
    }

    pub const fn minor(self) -> u32 {
        self.minor
    }

    pub(crate) const fn has_legacy_encoding(self) -> bool {
        self.major < 256 && self.minor < 256
    }

    pub(crate) const fn encode_legacy(self) -> u32 {
        (self.major as u32) << 8 | self.minor
    }

    pub(crate) const fn encode_modern(self) -> u32 {
        (self.minor & 0xff) | ((self.major as u32) << 8) | ((self.minor & !0xff) << 12)
    }

    pub(crate) const fn decode_legacy(encoded: u32) -> Self {
        let encoded = encoded as u16;
        Self {
            major: (encoded >> 8) & 0xff,
            minor: (encoded & 0xff) as u32,
        }
    }

    pub(crate) const fn decode_modern(encoded: u32) -> Self {
        Self {
            major: ((encoded & 0x000f_ff00) >> 8) as u16,
            minor: (encoded & 0xff) | ((encoded >> 12) & 0x000f_ff00),
        }
    }
}

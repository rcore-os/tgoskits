//! Linux DRM UAPI shared by the RKNPU companion node.
//!
//! Card0's common UAPI and ioctl handling live in kdrm-core; this module only
//! retains the version/unique structures and driver-ioctl decoder used by card1.

use core::ffi::c_int;

use bytemuck::{AnyBitPattern, NoUninit};

/// DRM version query used by the RKNPU companion node.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVersion {
    pub version_major: c_int,
    pub version_minor: c_int,
    pub version_patchlevel: c_int,
    /// Explicit 64-bit ABI alignment bytes before the pointer-sized fields.
    pub _padding: u32,
    pub name_len: usize,
    pub name: u64,
    pub date_len: usize,
    pub date: u64,
    pub desc_len: usize,
    pub desc: u64,
}

/// DRM unique query used by the RKNPU companion node.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmUnique {
    pub unique_len: usize,
    pub unique: u64,
}

#[cfg(feature = "rknpu")]
pub const DRM_COMMAND_BASE: u32 = 0x40;
#[cfg(feature = "rknpu")]
pub const DRM_COMMAND_END: u32 = 0xA0;

/// Extracts the command number (bits 7..0) from a packed ioctl request.
#[cfg(feature = "rknpu")]
#[inline]
pub const fn ioctl_nr(cmd: u32) -> u32 {
    cmd & 0xff
}

/// Extracts the struct size (bits 29..16) from a packed ioctl request.
#[cfg(feature = "rknpu")]
#[inline]
pub const fn io_size(cmd: u32) -> u32 {
    (cmd >> 16) & ((1 << 14) - 1)
}

/// Returns whether nr is in the driver's private ioctl range.
#[cfg(feature = "rknpu")]
#[inline]
pub fn is_driver_ioctl(nr: u32) -> bool {
    (DRM_COMMAND_BASE..DRM_COMMAND_END).contains(&nr)
}

#[cfg(all(test, not(axtest), feature = "rknpu"))]
mod tests {
    use super::*;

    const IOCTL_VERSION: u32 = 0xc038_6400;
    const IOCTL_GET_UNIQUE: u32 = 0xc010_6401;
    const IOCTL_PRIME_HANDLE_TO_FD: u32 = 0xc010_642d;
    const IOCTL_MODE_GETRESOURCES: u32 = 0xc010_64a0;

    #[test]
    fn ioctl_nr_recovers_command_number() {
        assert_eq!(ioctl_nr(IOCTL_VERSION), 0x00);
        assert_eq!(ioctl_nr(IOCTL_GET_UNIQUE), 0x01);
        assert_eq!(ioctl_nr(IOCTL_PRIME_HANDLE_TO_FD), 0x2d);
        assert_eq!(ioctl_nr(IOCTL_MODE_GETRESOURCES), 0xA0);
    }

    #[test]
    fn ioctl_nr_masks_to_low_byte() {
        assert_eq!(ioctl_nr(0xc010_6402), 0x02);
    }

    #[test]
    fn io_size_recovers_struct_size() {
        assert_eq!(
            io_size(IOCTL_VERSION),
            core::mem::size_of::<DrmVersion>() as u32
        );
        assert_eq!(
            io_size(IOCTL_GET_UNIQUE),
            core::mem::size_of::<DrmUnique>() as u32
        );
        assert_eq!(io_size(0x0000_641e), 0);
    }

    #[test]
    fn is_driver_ioctl_covers_command_range() {
        assert!(!is_driver_ioctl(DRM_COMMAND_BASE - 1));
        assert!(is_driver_ioctl(DRM_COMMAND_BASE));
        assert!(is_driver_ioctl(DRM_COMMAND_END - 1));
        assert!(!is_driver_ioctl(DRM_COMMAND_END));
        assert!(!is_driver_ioctl(ioctl_nr(IOCTL_VERSION)));
        assert!(!is_driver_ioctl(ioctl_nr(IOCTL_MODE_GETRESOURCES)));
        assert!(is_driver_ioctl(0x40));
        assert!(is_driver_ioctl(0x45));
    }
}

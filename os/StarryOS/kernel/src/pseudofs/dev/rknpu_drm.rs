use core::ffi::{c_char, c_int, c_ulong};

/// IOCTL number bits
const IOC_NRBITS: u32 = 8;
/// IOCTL number mask
const IOC_NRMASK: u32 = (1 << IOC_NRBITS) - 1;
/// IOCTL type bits
const IOC_TYPEBITS: u32 = 8;
/// IOCTL size bits
const IOC_SIZEBITS: u32 = 14;
/// IOCTL number shift
const IOC_NRSHIFT: u32 = 0;
/// IOCTL type shift
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
/// IOCTL size shift
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
/// IOCTL size mask
const IOC_SIZEMASK: u32 = (1 << IOC_SIZEBITS) - 1;
/// Base value for DRM ioctl commands
const DRM_COMMAND_BASE: u32 = 0x40;
/// End value for DRM ioctl commands
const DRM_COMMAND_END: u32 = 0xA0;

/// DRM version information structure, corresponds to Linux's `struct drm_version`
/// Used for ioctl: DRM_IOCTL_VERSION
#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct DrmVersion {
    /// Major version
    pub version_major: c_int,
    /// Minor version
    pub version_minor: c_int,
    /// Patch level
    pub version_patchlevel: c_int,
    /// Length of name buffer
    pub name_len: c_ulong,
    /// Pointer to user-space buffer holding driver name
    pub name: *mut c_char,
    /// Length of date buffer
    pub date_len: c_ulong,
    /// Pointer to user-space buffer holding build date
    pub date: *mut c_char,
    /// Length of description buffer
    pub desc_len: c_ulong,
    /// Pointer to user-space buffer holding description
    pub desc: *mut c_char,
}

/// Extracts the ioctl command number from a DRM ioctl command
pub fn ioctl_nr(cmd: u32) -> u32 {
    (cmd) & IOC_NRMASK
}

/// Checks if an ioctl command number is a driver-specific ioctl
pub fn is_driver_ioctl(nr: u32) -> bool {
    (DRM_COMMAND_BASE..DRM_COMMAND_END).contains(&nr)
}

/// Extracts the size of the data structure from a DRM ioctl command
pub fn io_size(cmd: u32) -> u32 {
    ((cmd) >> (IOC_SIZESHIFT)) & IOC_SIZEMASK
}

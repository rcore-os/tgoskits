use core::{
    any::Any,
    convert::TryFrom,
    ffi::{c_char, c_ulong},
};

use ax_hal::asm::user_copy;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};

use super::drm::DrmVersion;
use crate::pseudofs::{
    DeviceOps,
    dev::drm::{io_size, ioctl_nr, is_driver_ioctl},
};

/// Driver name for DRM device
const DRM0_NAME: &str = "rockchip";
/// Driver date for DRM device
const DRM0_DATE: &str = "20140818";
/// Driver description for DRM device
const DRM0_DESC: &str = "RockChip Soc DRM";

/// Device ID for /dev/rknpu (pick an unused major/minor)
pub const RKNPU_DEVICE_ID: DeviceId = DeviceId::new(251, 0);

/// Device ID for /dev/dri/card0
pub const CARD0_SYSTEM_DEVICE_ID: DeviceId = DeviceId::new(0xe2, 0);

/// DRM card0 device implementation
pub struct Card0;

impl Card0 {
    /// Creates a new /dev/dri/card0 device.
    pub fn new() -> Card0 {
        Self
    }
}

impl Default for Card0 {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for Card0 {
    /// Reads data from the device (not supported for card0)
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        trace!("card0: read_at called");
        // card0 devices are not meant to be read directly
        Err(VfsError::InvalidInput)
    }

    /// Writes data to the device (not supported for card0)
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        trace!("card0: write_at called");
        // card0 devices are not meant to be written directly
        Err(VfsError::InvalidInput)
    }

    /// Handles ioctl commands for the device
    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        if arg == 0 {
            warn!("[rknpu]: ioctl received null arg pointer");
            return Err(VfsError::InvalidData);
        }
        let nr = ioctl_nr(cmd);
        info!("card0: cmd {cmd:#x}, nr {nr:#x}, arg {arg:#x}");

        let is_driver_ioctl = is_driver_ioctl(ioctl_nr(cmd));
        info!("card0: is_driver_ioctl = {}", is_driver_ioctl);

        let mut stack_data = [0u8; 128];

        let in_size = io_size(cmd) as usize;
        let out_size = in_size;

        copy_from_user(stack_data.as_mut_ptr(), arg as _, in_size)?;

        if is_driver_ioctl {
            panic!("card0: driver ioctls are not supported");
        } else {
            assert!(nr <= 0xcf, "card0: unsupported ioctl nr {nr}");

            match nr {
                0 => {
                    info!("drm get version");
                    drm_version(&mut stack_data)?;
                }
                _ => {
                    panic!("card0: unsupported ioctl nr {nr}");
                }
            }
        }

        copy_to_user(arg as _, stack_data.as_mut_ptr(), out_size)?;

        Ok(0)
    }

    /// Returns a reference to the object as Any for dynamic type checking
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Returns the node flags for the device
    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

/// Rust implementation of Linux kernel's drm_copy_field function
///
/// This function safely copies a string value to user space buffer,
/// similar to the Linux kernel implementation with proper error handling.
unsafe fn drm_copy_field(
    buf: *mut u8,
    buf_len: &mut c_ulong,
    value: *const u8,
) -> Result<(), VfsError> {
    // Handle NULL value case - same as kernel's WARN_ONCE check
    if value.is_null() {
        warn!("[drm_copy_field] BUG: the value to copy was not set!");
        *buf_len = 0;
        return Ok(());
    }

    // Calculate actual string length using C string semantics
    let mut len = 0;
    unsafe {
        let mut ptr = value;
        while *ptr != 0 {
            len += 1;
            ptr = ptr.add(1);
        }
    }

    // Get the original buffer size
    let original_buf_len = *buf_len;

    // Update user's buffer length with actual string length (same as kernel)
    *buf_len = len;

    // Don't overflow user buffer - limit copy to available space
    let copy_len = if len > original_buf_len {
        original_buf_len as usize
    } else {
        len as usize
    };

    // Finally, try filling in the userbuf (same logic as kernel)
    if copy_len > 0 && !buf.is_null() {
        copy_to_user(buf as _, value, copy_len)?;
    }

    Ok(())
}

/// Sets the DRM version information for the device
fn drm_version(data: &mut [u8]) -> VfsResult<()> {
    let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmVersion) };
    info!("drm_version called: {:?}", data);

    // Set version information
    data.version_major = 3;
    data.version_minor = 0;
    data.version_patchlevel = 0;

    // Use drm_copy_field to handle string copying properly
    unsafe {
        // Copy driver name
        let ret = drm_copy_field(data.name as *mut u8, &mut data.name_len, DRM0_NAME.as_ptr());
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver name: {:?}", e);
            return Err(VfsError::InvalidData);
        }

        // Copy driver date
        let ret = drm_copy_field(data.date as *mut u8, &mut data.date_len, DRM0_DATE.as_ptr());
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver date: {:?}", e);
            return Err(VfsError::InvalidData);
        }

        // Copy driver description
        let ret = drm_copy_field(
            data.desc.cast(),
            &mut data.desc_len,
            DRM0_DESC.as_ptr().cast(),
        );
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver description: {:?}", e);
            return Err(VfsError::InvalidData);
        }
    }

    info!(
        "[drm_version] Set driver info: name_len={}, date_len={}, desc_len={}",
        data.name_len, data.date_len, data.desc_len
    );

    Ok(())
}

/// Copies data from user space to kernel space
pub fn copy_from_user(dst: *mut u8, src: *const u8, size: usize) -> Result<(), VfsError> {
    let ret = unsafe { user_copy(dst, src, size) };

    if ret != 0 {
        warn!("[rknpu]: copy_from_user failed, ret={}", ret);
        return Err(VfsError::InvalidData);
    }
    Ok(())
}

/// Copies data from kernel space to user space
pub fn copy_to_user(dst: *mut u8, src: *const u8, size: usize) -> Result<(), VfsError> {
    let ret = unsafe { user_copy(dst, src, size) };

    if ret != 0 {
        warn!("[rknpu]: copy_to_user failed, ret={}", ret);
        return Err(VfsError::InvalidData);
    }
    Ok(())
}

/// RKNPU command types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RknpuCmd {
    /// Action command
    Action     = 0x00,
    /// Submit command
    Submit     = 0x01,
    /// Memory create command
    MemCreate  = 0x02,
    /// Memory map command
    MemMap     = 0x03,
    /// Memory destroy command
    MemDestroy = 0x04,
    /// Memory sync command
    MemSync    = 0x05,
}

impl TryFrom<u32> for RknpuCmd {
    type Error = ();

    /// Tries to convert a u32 value to an RknpuCmd
    fn try_from(nr: u32) -> Result<Self, Self::Error> {
        match nr {
            0x00 | 0x40 => Ok(RknpuCmd::Action),
            0x01 | 0x41 => Ok(RknpuCmd::Submit),
            0x02 | 0x42 => Ok(RknpuCmd::MemCreate),
            0x03 | 0x43 => Ok(RknpuCmd::MemMap),
            0x04 | 0x44 => Ok(RknpuCmd::MemDestroy),
            0x05 | 0x45 => Ok(RknpuCmd::MemSync),
            _ => {
                warn!("Unknown ioctl nr: {nr:#x}",);
                Err(())
            }
        }
    }
}

/// DRM_IOCTL_GET_UNIQUE ioctl argument type.
///
/// This structure is used with DRM_IOCTL_GET_UNIQUE to retrieve the unique
/// identifier for a DRM device, typically the bus ID. This corresponds to the
/// Linux kernel's struct drm_unique.
///
/// \sa drmGetBusid() and drmSetBusid() in libdrm.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DrmUnique {
    /// Length of unique string identifier
    pub unique_len: c_ulong,
    /// Pointer to user-space buffer holding unique name for driver
    /// instantiation
    pub unique: *mut c_char,
}

impl DrmUnique {
    /// Creates a new DrmUnique with default values
    pub const fn new() -> Self {
        Self {
            unique_len: 0,
            unique: core::ptr::null_mut(),
        }
    }

    /// Creates a new DrmUnique with specified buffer and length
    pub fn with_buffer(buffer: *mut c_char, len: c_ulong) -> Self {
        Self {
            unique_len: len,
            unique: buffer,
        }
    }

    /// Returns true if the unique pointer is null
    pub fn is_null(&self) -> bool {
        self.unique.is_null()
    }

    /// Returns the length of the unique identifier
    pub fn len(&self) -> c_ulong {
        self.unique_len
    }

    /// Sets the length of the unique identifier
    pub fn set_len(&mut self, len: c_ulong) {
        self.unique_len = len;
    }
}

use alloc::borrow::Cow;
use core::{
    any::Any,
    convert::TryFrom,
    ffi::{CStr, c_char, c_ulong},
    mem,
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_hal::mem::virt_to_phys;
use ax_memory_addr::PhysAddrRange;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::O_CLOEXEC;
use rockchip_npu::{
    RknpuAction,
    ioctrl::{RknpuMemCreate, RknpuMemMap, RknpuSubmit},
};

use super::{
    card0::{RknpuCmd, copy_from_user, copy_to_user},
    drm::DrmVersion,
};
use crate::{
    file::FileLike,
    pseudofs::{
        DeviceOps,
        dev::drm::{io_size, ioctl_nr, is_driver_ioctl},
        device::DeviceMmap,
    },
};

/// Driver name for DRM device
const DRM1_NAME: &CStr = c"rknpu";
/// Driver date for DRM device
const DRM1_DATE: &CStr = c"20240828";
/// Driver description for DRM device
const DRM1_DESC: &CStr = c"RKNPU driver";

/// Device ID for /dev/dri/card1
pub const CARD1_SYSTEM_DEVICE_ID: DeviceId = DeviceId::new(0xe2, 1);

/// Device ID for /dev/rknpu (pick an unused major/minor)
pub const RKNPU_DEVICE_ID: DeviceId = DeviceId::new(251, 0);

/// Page shift constant (4KB pages)
const PAGE_SHIFT: u32 = 12;
/// Maximum ioctl command number
const MAX_IOCTL_NR: u32 = 0xcf;
/// Stack data buffer size
const STACK_DATA_SIZE: usize = 128;
/// DRM ioctl version command number
const DRM_IOCTL_VERSION_NR: u32 = 0;
/// DRM ioctl get unique command number
const DRM_IOCTL_GET_UNIQUE_NR: u32 = 1;
/// DRM ioctl gem flink command number
const DRM_IOCTL_GEM_FLINK_NR: u32 = 10;
/// DRM ioctl prime handle to fd command number
const DRM_IOCTL_PRIME_HANDLE_TO_FD_NR: u32 = 0x2d;

/// DRM_IOCTL_VERSION ioctl argument type
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DrmUnique {
    /// Length of unique string identifier
    pub unique_len: c_ulong,
    /// Pointer to user-space buffer holding unique name for driver
    /// instantiation
    pub unique: *mut c_char,
}

/// Represents an RKNPU user action with flags and value
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct RknpuUserAction {
    /// Action flags
    pub flags: RknpuAction,
    /// Action value
    pub value: u32,
}

impl RknpuUserAction {
    /// Creates a new RknpuUserAction with default values
    pub fn default() -> Self {
        Self {
            flags: RknpuAction::GetDrvVersion,
            value: 0,
        }
    }
}

/// DRM card1 device implementation
pub struct Card1;

impl Card1 {
    /// Creates a new /dev/dri/card1 device.
    pub fn new() -> Card1 {
        Self
    }
}

impl Default for Card1 {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for Card1 {
    /// Reads data from the device (not supported for card1)
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        trace!("dri: read_at called");
        // card1 heap devices are not meant to be read directly
        Err(VfsError::InvalidInput)
    }

    /// Writes data to the device (not supported for card1)
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        trace!("dri: write_at called");
        // card1 heap devices are not meant to be written directly
        Err(VfsError::InvalidInput)
    }

    /// Handles ioctl commands for the device
    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        if arg == 0 {
            warn!("[rknpu]: ioctl received null arg pointer");
            return Err(VfsError::InvalidData);
        }
        let nr = ioctl_nr(cmd);
        info!("card1: cmd {cmd:#x}, nr {nr:#x}, arg {arg:#x}");

        let is_driver_ioctl = is_driver_ioctl(ioctl_nr(cmd));
        info!("card1: is_driver_ioctl = {}", is_driver_ioctl);

        if is_driver_ioctl {
            if let Ok(op) = RknpuCmd::try_from(nr) {
                rknpu_driver_ioctl(op, arg)?;
            } else {
                warn!("Unknown RKNPU cmd: {:#x}", cmd);
                return Err(VfsError::NotATty);
            }
        } else {
            assert!(nr <= MAX_IOCTL_NR, "card1: unsupported ioctl nr {nr}");
            let mut stack_data = [0u8; STACK_DATA_SIZE];

            let in_size = io_size(cmd) as usize;
            let out_size = in_size;

            copy_from_user(stack_data.as_mut_ptr(), arg as _, in_size)?;
            match nr {
                DRM_IOCTL_VERSION_NR => {
                    info!("drm get version");
                    drm_version(&mut stack_data)?;
                }
                DRM_IOCTL_GET_UNIQUE_NR => {
                    info!("drm get unique");
                    drm_get_unique(&mut stack_data)?;
                }
                DRM_IOCTL_GEM_FLINK_NR => {
                    drm_gem_flink_ioctl(&mut stack_data)?;
                }
                DRM_IOCTL_PRIME_HANDLE_TO_FD_NR => {
                    drm_prime_handle_to_fd_ioctl(&mut stack_data)?;
                }

                _ => {
                    panic!("card1: unsupported ioctl nr {nr:#x}");
                }
            }
            copy_to_user(arg as _, stack_data.as_mut_ptr(), out_size)?;
        }

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

    /// Maps an exported GEM buffer selected by `handle << PAGE_SHIFT`.
    fn mmap(&self, offset: u64) -> DeviceMmap {
        let Some(handle) = map_handle_from_offset(offset) else {
            warn!("card1: mmap received invalid offset {offset:#x}");
            return DeviceMmap::None;
        };
        let Ok(exported) = exported_gem_buffer(handle) else {
            warn!("card1: mmap could not resolve handle {handle}");
            return DeviceMmap::None;
        };
        DeviceMmap::Physical(exported.range)
    }
}

struct ExportedGemBuffer {
    range: PhysAddrRange,
}

impl ExportedGemBuffer {
    fn new(range: PhysAddrRange) -> Self {
        Self { range }
    }
}

impl FileLike for ExportedGemBuffer {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[rknpu-gem]".into()
    }

    fn device_mmap(&self, _offset: u64) -> AxResult<DeviceMmap> {
        Ok(DeviceMmap::Physical(self.range))
    }
}

impl Pollable for ExportedGemBuffer {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

fn prime_fd_cloexec(flags: u32) -> bool {
    flags & O_CLOEXEC != 0
}

fn map_handle_from_offset(offset: u64) -> Option<u32> {
    if offset & ((1 << PAGE_SHIFT) - 1) != 0 {
        return None;
    }
    let handle = u32::try_from(offset >> PAGE_SHIFT).ok()?;
    (handle != 0).then_some(handle)
}

fn exported_gem_buffer(handle: u32) -> AxResult<ExportedGemBuffer> {
    let (obj_addr, size) = with_npu(|rknpu_dev| {
        rknpu_dev
            .get_obj_addr_and_size(handle)
            .ok_or(VfsError::NotFound)
    })
    .map_err(|_| AxError::NotFound)?;
    let paddr = virt_to_phys(obj_addr.into());
    Ok(ExportedGemBuffer::new(PhysAddrRange::from_start_size(
        paddr, size,
    )))
}

/// Gets a reference to the NPU device
pub fn npu() -> Result<rdrive::DeviceGuard<::rockchip_npu::Rknpu>, VfsError> {
    rdrive::get_one()
        .ok_or(VfsError::NotFound)?
        .try_lock()
        .map_err(|_| VfsError::AlreadyExists)
}

/// Executes a function with the NPU device
pub fn with_npu<F, R>(f: F) -> Result<R, VfsError>
where
    F: FnOnce(&mut ::rockchip_npu::Rknpu) -> Result<R, VfsError>,
{
    let mut npu = npu()?;
    f(&mut npu)
}

/// Handles RKNPU action ioctl commands
pub fn rknpu_driver_ioctl(op: RknpuCmd, arg: usize) -> VfsResult<usize> {
    info!("rknpu_driver_ioctl: op = {:?}", op);
    match op {
        RknpuCmd::Submit => {
            let mut submit_args = RknpuSubmit::default();
            copy_from_user(
                &mut submit_args as *mut _ as *mut u8,
                arg as *const u8,
                mem::size_of::<RknpuSubmit>(),
            )?;
            info!("rknpu submit ioctl {submit_args:#x?}");

            if let Err(e) = with_npu(|rknpu_dev| {
                rknpu_dev
                    .submit_ioctrl(&mut submit_args)
                    .map_err(|_| VfsError::InvalidData)
            }) {
                warn!("rknpu submit ioctl failed: {:?}", e);
            }
            debug!("rknpu submit ioctl result: {:#x?}", submit_args);

            copy_to_user(
                arg as *mut u8,
                &submit_args as *const _ as *const u8,
                mem::size_of::<RknpuSubmit>(),
            )?;
        }
        RknpuCmd::MemCreate => {
            info!("rknpu mem_create ioctl");
            let mut mem_create_args = RknpuMemCreate::default();

            copy_from_user(
                &mut mem_create_args as *mut _ as *mut u8,
                arg as *const u8,
                mem::size_of::<RknpuMemCreate>(),
            )?;

            if let Err(e) = with_npu(|rknpu_dev| {
                rknpu_dev
                    .create(&mut mem_create_args)
                    .map_err(|_| VfsError::InvalidData)
            }) {
                warn!("rknpu mem_create ioctl failed: {:?}", e);
            }

            copy_to_user(
                arg as *mut u8,
                &mem_create_args as *const _ as *const u8,
                mem::size_of::<RknpuMemCreate>(),
            )?;
        }
        RknpuCmd::MemMap => {
            info!("rknpu mem_map ioctl");
            let mut mem_map = RknpuMemMap::default();
            copy_from_user(
                &mut mem_map as *mut _ as *mut u8,
                arg as *const u8,
                mem::size_of::<RknpuMemMap>(),
            )?;

            if let Err(e) = with_npu(|rknpu_dev| {
                if rknpu_dev.get_phys_addr_and_size(mem_map.handle).is_some() {
                    mem_map.offset = (mem_map.handle as u64) << PAGE_SHIFT;

                    info!(
                        "mem_map: handle={} -> offset=0x{:x}",
                        mem_map.handle, mem_map.offset
                    );
                    Ok(())
                } else {
                    warn!("mem_map: invalid handle={}", mem_map.handle);
                    Err(VfsError::InvalidData)
                }
            }) {
                warn!("rknpu mem_map ioctl failed: {:?}", e);
                return Err(e);
            }

            copy_to_user(
                arg as *mut u8,
                &mem_map as *const _ as *const u8,
                mem::size_of::<RknpuMemMap>(),
            )?;
        }
        RknpuCmd::MemDestroy => {
            info!("rknpu mem_destroy ioctl");
        }
        RknpuCmd::MemSync => {
            info!("rknpu mem_sync ioctl");
        }
        _ => {
            info!("rknpu action ioctl");
            let mut action = RknpuUserAction::default();
            copy_from_user(
                &mut action as *mut _ as *mut u8,
                arg as *const u8,
                mem::size_of::<RknpuUserAction>(),
            )?;

            info!(
                "rknpu action ioctl: flags = {:?}, value = {}",
                action.flags, action.value
            );

            if let Err(e) = with_npu(|rknpu_dev| {
                let val = rknpu_dev
                    .action(action.flags)
                    .map_err(|_| VfsError::InvalidData)?;
                action.value = val;
                Ok(())
            }) {
                warn!("rknpu action ioctl failed: {:?}", e);
            }

            copy_to_user(
                arg as *mut u8,
                &action as *const _ as *const u8,
                mem::size_of::<RknpuUserAction>(),
            )?;
        }
    }
    Ok(0)
}

/// Handles RKNPU submit ioctl command
pub fn rknpu_submit_ioctl(arg: usize) -> VfsResult<usize> {
    let mut submit_args = RknpuSubmit::default();

    copy_from_user(
        &mut submit_args as *mut _ as *mut u8,
        arg as *const u8,
        mem::size_of::<RknpuSubmit>(),
    )?;

    if let Err(e) = with_npu(|rknpu_dev| {
        rknpu_dev
            .submit_ioctrl(&mut submit_args)
            .map_err(|_| VfsError::InvalidData)
    }) {
        warn!("rknpu submit ioctl failed: {:?}", e);
    }

    copy_to_user(
        arg as *mut u8,
        &submit_args as *const _ as *const u8,
        mem::size_of::<RknpuSubmit>(),
    )?;
    Ok(0)
}

/// Handles RKNPU memory create ioctl command
pub fn rknpu_mem_create_ioctl(arg: usize) -> VfsResult<usize> {
    let mut mem_create_args = RknpuMemCreate::default();

    copy_from_user(
        &mut mem_create_args as *mut _ as *mut u8,
        arg as *const u8,
        mem::size_of::<RknpuMemCreate>(),
    )?;

    if let Err(e) = with_npu(|rknpu_dev| {
        rknpu_dev
            .create(&mut mem_create_args)
            .map_err(|_| VfsError::InvalidData)
    }) {
        warn!("rknpu mem_create ioctl failed: {:?}", e);
    }

    copy_to_user(
        arg as *mut u8,
        &mem_create_args as *const _ as *const u8,
        mem::size_of::<RknpuMemCreate>(),
    )?;
    Ok(0)
}

/// DRM_IOCTL_GEM_FLINK ioctl argument type
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct DrmGemFlink {
    /// GEM handle
    handle: u32,
    /// GEM name
    name: u32,
}

/// Handles DRM GEM flink ioctl command
fn drm_gem_flink_ioctl(data: &mut [u8]) -> VfsResult<usize> {
    let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmGemFlink) };
    info!("drm_gem_flink_ioctl called: {:#?}", data);
    Err(VfsError::NotFound)
}

/// DRM prime handle structure
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct DrmPrimeHande {
    /// Handle
    handle: u32,
    /// Flags
    flags: u32,
    /// File descriptor
    fd: i32,
}

/// Handles DRM prime handle to fd ioctl command
fn drm_prime_handle_to_fd_ioctl(data: &mut [u8]) -> VfsResult<usize> {
    let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmPrimeHande) };
    info!("drm_prime_handle_to_fd_ioctl {data:#x?}");
    let exported = exported_gem_buffer(data.handle).map_err(|err| {
        warn!(
            "drm_prime_handle_to_fd_ioctl: invalid handle {}: {err}",
            data.handle
        );
        VfsError::NotFound
    })?;
    data.fd = exported
        .add_to_fd_table(prime_fd_cloexec(data.flags))
        .map_err(|err| {
            warn!("drm_prime_handle_to_fd_ioctl: failed to allocate fd: {err}");
            VfsError::NoMemory
        })?;
    Ok(0)
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
        original_buf_len
    } else {
        len
    };

    // Finally, try filling in the userbuf (same logic as kernel)
    if copy_len > 0 && !buf.is_null() {
        copy_to_user(buf as _, value, copy_len as _)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use ax_memory_addr::PhysAddrRange;

    use super::*;

    #[test]
    fn prime_export_honors_cloexec_flag() {
        assert!(prime_fd_cloexec(linux_raw_sys::general::O_CLOEXEC as u32));
        assert!(!prime_fd_cloexec(0));
    }

    #[test]
    fn mem_map_offset_decodes_to_handle() {
        assert_eq!(map_handle_from_offset(0x1000), Some(1));
        assert_eq!(map_handle_from_offset(0x2000), Some(2));
        assert_eq!(map_handle_from_offset(0), None);
        assert_eq!(map_handle_from_offset(0x1001), None);
    }

    #[test]
    fn exported_buffer_reports_physical_device_mmap() {
        let range = PhysAddrRange::from_start_size(0x1234_5000.into(), 0x4000);
        let exported = ExportedGemBuffer::new(range);

        assert!(
            matches!(exported.device_mmap(0).unwrap(), DeviceMmap::Physical(actual) if actual == range)
        );
    }
}

/// Sets the DRM version information for the device
pub fn drm_version(data: &mut [u8]) -> VfsResult<()> {
    let data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmVersion) };
    info!("drm_version called: {:?}", data);

    // Set version information
    data.version_major = 0;
    data.version_minor = 9;
    data.version_patchlevel = 8;

    // Use drm_copy_field to handle string copying properly
    unsafe {
        // Copy driver name
        let ret = drm_copy_field(
            data.name.cast(),
            &mut data.name_len,
            DRM1_NAME.as_ptr().cast(),
        );
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver name: {:?}", e);
            return Err(VfsError::InvalidData);
        }

        // Copy driver date
        let ret = drm_copy_field(
            data.date as *mut u8,
            &mut data.date_len,
            DRM1_DATE.as_ptr() as *const u8,
        );
        if let Err(e) = ret {
            warn!("[drm_version] Failed to copy driver date: {:?}", e);
            return Err(VfsError::InvalidData);
        }

        // Copy driver description
        let ret = drm_copy_field(
            data.desc.cast(),
            &mut data.desc_len,
            DRM1_DESC.as_ptr().cast(),
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

/// DRM_GET_UNIQUE ioctl handler
///
/// This function handles DRM_IOCTL_GET_UNIQUE requests, returning the unique
/// identifier for the DRM device (typically a bus ID or similar identifier).
pub fn drm_get_unique(data: &mut [u8]) -> VfsResult<()> {
    let unique_data = unsafe { &mut *(data.as_mut_ptr() as *mut DrmUnique) };
    info!("drm_get_unique called: {:?}", unique_data);

    unique_data.unique_len = 0;

    Ok(())
}

/// DRM_SET_UNIQUE ioctl handler (stub implementation)
///
/// This function handles DRM_IOCTL_SET_UNIQUE requests. For this
/// implementation, we return success but don't actually set the unique
/// identifier, as this is typically not used/needed in embedded systems.
pub fn drm_set_unique(data: &mut [u8]) -> VfsResult<()> {
    let unique_data = unsafe { &*(data.as_ptr() as *const DrmUnique) };
    info!("drm_set_unique called: {:?}", unique_data);

    // For this implementation, we just log the attempt and return success
    // In a real implementation, this would validate and store the unique ID
    warn!("[drm_set_unique] Setting unique identifier is not supported in this implementation");

    Ok(())
}

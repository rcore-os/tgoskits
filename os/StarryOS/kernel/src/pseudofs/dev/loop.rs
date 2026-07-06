use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::vfs::FileBackend;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsResult};
use linux_raw_sys::{
    general::{O_ACCMODE, O_RDONLY},
    ioctl::{
        BLKFLSBUF, BLKGETSIZE, BLKGETSIZE64, BLKIOMIN, BLKIOOPT, BLKPG, BLKRAGET, BLKRASET,
        BLKROGET, BLKROSET, BLKRRPART, BLKSSZGET,
    },
    loop_device::{
        LO_FLAGS_READ_ONLY, LOOP_CLR_FD, LOOP_CONFIGURE, LOOP_GET_STATUS, LOOP_GET_STATUS64,
        LOOP_SET_FD, LOOP_SET_STATUS, LOOP_SET_STATUS64, loop_config, loop_info, loop_info64,
    },
};
use starry_vm::{VmMutPtr, VmPtr};

#[cfg(feature = "ext4fs")]
use super::loop_block::BlockCache;
use crate::{
    file::{FileLike, get_file_like},
    pseudofs::{DeviceMmap, DeviceOps},
};

/// HDIO_GETGEO ioctl command (get drive geometry).
/// Not defined in linux-raw-sys, so we use the standard value directly.
const HDIO_GETGEO: u32 = 0x0301;

/// /dev/loopX devices
pub struct LoopDevice {
    number: u32,
    dev_id: DeviceId,
    /// Underlying file for the loop device, if any.
    pub file: Mutex<Option<FileBackend>>,
    /// Read-only flag for the loop device.
    pub ro: AtomicBool,
    /// Read-ahead size for the loop device, in bytes.
    pub ra: AtomicU32,
    /// Backing file name for the loop device.
    file_name: Mutex<[u8; 64]>,
    /// Bit mask of `LO_FLAGS_*` (READ_ONLY, AUTOCLEAR, PARTSCAN, DIRECT_IO).
    flags: AtomicU32,
    /// Whether the device is opened exclusively (O_EXCL).
    exclusive: AtomicBool,
    /// Block-device data cache.  Populated by `as_dyn_block_device()`,
    /// written back and cleared by `LOOP_CLR_FD`.
    #[cfg(feature = "ext4fs")]
    pub(super) block_cache: Mutex<BlockCache>,
}

impl LoopDevice {
    pub(crate) fn new(number: u32, dev_id: DeviceId) -> Self {
        Self {
            number,
            dev_id,
            file: Mutex::new(None),
            ro: AtomicBool::new(false),
            ra: AtomicU32::new(512),
            file_name: Mutex::new([0u8; 64]),
            flags: AtomicU32::new(0),
            exclusive: AtomicBool::new(false),
            #[cfg(feature = "ext4fs")]
            block_cache: Mutex::new(BlockCache::new()),
        }
    }

    /// Apply `lo_flags` from userspace, keeping `ro` and `flags` in sync.
    fn set_lo_flags(&self, lo_flags: u32) {
        self.flags.store(lo_flags, Ordering::Relaxed);
        self.ro
            .store(lo_flags & LO_FLAGS_READ_ONLY as u32 != 0, Ordering::Relaxed);
    }

    /// Get information about the loop device.
    pub fn get_info(&self) -> AxResult<loop_info> {
        if self.file.lock().is_none() {
            return Err(AxError::from(LinuxError::ENXIO));
        }
        let mut res: loop_info = unsafe { core::mem::zeroed() };
        res.lo_number = self.number as _;
        res.lo_rdevice = self.dev_id.0 as _;
        res.lo_flags = self.flags.load(Ordering::Relaxed) as _;
        let name = self.file_name.lock();
        for (i, &c) in name.iter().enumerate() {
            if i < 64 {
                res.lo_name[i] = c as _;
            }
            if c == 0 {
                break;
            }
        }
        Ok(res)
    }

    /// Set information for the loop device.
    pub fn set_info(&self, _src: loop_info) -> AxResult<()> {
        Ok(())
    }

    /// Get information about the loop device (64-bit variant).
    pub fn get_info64(&self) -> AxResult<loop_info64> {
        if self.file.lock().is_none() {
            return Err(AxError::from(LinuxError::ENXIO));
        }
        let mut res: loop_info64 = unsafe { core::mem::zeroed() };
        res.lo_number = self.number as _;
        res.lo_rdevice = self.dev_id.0 as _;
        res.lo_file_name = *self.file_name.lock();
        res.lo_flags = self.flags.load(Ordering::Relaxed);
        Ok(res)
    }

    /// Clone the underlying file of the loop device.
    pub fn clone_file(&self) -> VfsResult<FileBackend> {
        let file = self.file.lock().clone();
        file.ok_or(AxError::from(LinuxError::ENXIO))
    }
}

impl DeviceOps for LoopDevice {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let file = self.file.lock().clone();
        file.ok_or(AxError::OperationNotPermitted)?
            .read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if self.ro.load(Ordering::Relaxed) {
            return Err(AxError::ReadOnlyFilesystem);
        }
        let file = self.file.lock().clone();
        file.ok_or(AxError::OperationNotPermitted)?
            .write_at(buf, offset)
    }

    fn open(&self, exclusive: bool) -> VfsResult<()> {
        if exclusive {
            if self.exclusive.swap(true, Ordering::Acquire) {
                return Err(AxError::ResourceBusy);
            }
        } else if self.exclusive.load(Ordering::Acquire) {
            return Err(AxError::ResourceBusy);
        }
        Ok(())
    }

    fn close(&self, exclusive: bool) {
        if exclusive {
            self.exclusive.store(false, Ordering::Release);
        }
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            LOOP_SET_FD => {
                let fd = arg as i32;
                if fd < 0 {
                    return Err(AxError::BadFileDescriptor);
                }
                let f = get_file_like(fd)?;
                let Some(file) = f.downcast_ref::<crate::file::File>() else {
                    return Err(AxError::InvalidInput);
                };
                let mut guard = self.file.lock();
                if guard.is_some() {
                    return Err(AxError::ResourceBusy);
                }

                // Match Linux: if backing file opened O_RDONLY, device is read-only.
                let ro = (file.open_flags() & O_ACCMODE) == O_RDONLY;
                if ro {
                    self.set_lo_flags(LO_FLAGS_READ_ONLY as u32);
                } else {
                    self.set_lo_flags(0);
                }
                *guard = Some(file.inner().backend()?.clone());
            }
            LOOP_CLR_FD => {
                let mut guard = self.file.lock();
                if guard.is_none() {
                    return Err(AxError::from(LinuxError::ENXIO));
                }

                // Write back dirty data from the block cache before clearing.
                // This runs in normal syscall context so CachedFile VFS I/O
                // (page cache updates) is safe. The cache lock ensures no
                // concurrent write_block() can race.
                #[cfg(feature = "ext4fs")]
                self.detach_block_cache(guard.as_ref())?;

                *guard = None;
                *self.file_name.lock() = [0u8; 64];
                self.flags.store(0, Ordering::Relaxed);
            }
            LOOP_GET_STATUS => {
                (arg as *mut loop_info).vm_write(self.get_info()?)?;
            }
            LOOP_SET_STATUS => {
                // `loop_info` is a C ioctl payload copied from the guest ABI.
                let info = unsafe { (arg as *const loop_info).vm_read_uninit()?.assume_init() };
                self.set_info(info)?;
                let mut name = self.file_name.lock();
                for (i, &c) in info.lo_name.iter().enumerate() {
                    if i < 64 {
                        name[i] = c as _;
                    }
                    if c == 0 {
                        break;
                    }
                }
                self.set_lo_flags(info.lo_flags as u32);
            }
            LOOP_GET_STATUS64 => {
                (arg as *mut loop_info64).vm_write(self.get_info64()?)?;
            }
            LOOP_SET_STATUS64 => {
                // `loop_info64` is a C ioctl payload copied from the guest ABI.
                let info = unsafe { (arg as *const loop_info64).vm_read_uninit()?.assume_init() };
                *self.file_name.lock() = info.lo_file_name;
                self.set_lo_flags(info.lo_flags);
            }
            LOOP_CONFIGURE => {
                // `loop_config` is a C ioctl payload copied from the guest ABI.
                let cfg = unsafe { (arg as *const loop_config).vm_read_uninit()?.assume_init() };
                let fd = cfg.fd as i32;
                if fd < 0 {
                    return Err(AxError::BadFileDescriptor);
                }
                let f = get_file_like(fd)?;
                let Some(file) = f.downcast_ref::<crate::file::File>() else {
                    return Err(AxError::InvalidInput);
                };
                let mut guard = self.file.lock();
                if guard.is_some() {
                    return Err(AxError::ResourceBusy);
                }
                *guard = Some(file.inner().backend()?.clone());
                drop(guard);
                *self.file_name.lock() = cfg.info.lo_file_name;
                let mut flags = cfg.info.lo_flags;
                if (file.open_flags() & O_ACCMODE) == O_RDONLY {
                    flags |= LO_FLAGS_READ_ONLY as u32;
                }
                self.set_lo_flags(flags);
            }
            BLKGETSIZE | BLKGETSIZE64 => {
                let sectors = if let Ok(f) = self.clone_file() {
                    f.location().len()? / 512
                } else {
                    return Err(AxError::from(LinuxError::ENXIO));
                };
                if cmd == BLKGETSIZE {
                    (arg as *mut u32).vm_write(sectors as _)?;
                } else {
                    (arg as *mut u64).vm_write(sectors * 512)?;
                }
            }
            BLKSSZGET => {
                (arg as *mut u32).vm_write(512)?;
            }
            #[cfg(any(
                target_arch = "riscv64",
                target_arch = "aarch64",
                target_arch = "loongarch64"
            ))]
            linux_raw_sys::ioctl::BLKPBSZGET => {
                (arg as *mut u32).vm_write(512)?;
            }
            BLKROGET => {
                (arg as *mut u32).vm_write(self.ro.load(Ordering::Relaxed) as u32)?;
            }
            BLKROSET => {
                let ro = (arg as *const u32).vm_read()?;
                if ro != 0 && ro != 1 {
                    return Err(AxError::InvalidInput);
                }
                let mut flags = self.flags.load(Ordering::Relaxed);
                if ro != 0 {
                    flags |= LO_FLAGS_READ_ONLY as u32;
                } else {
                    flags &= !(LO_FLAGS_READ_ONLY as u32);
                }
                self.set_lo_flags(flags);
            }
            BLKRAGET => {
                (arg as *mut u32).vm_write(self.ra.load(Ordering::Relaxed))?;
            }
            BLKRASET => {
                self.ra
                    .store((arg as *const u32).vm_read()? as _, Ordering::Relaxed);
            }
            BLKRRPART => {
                // loop device has no physical partition table; no-op
            }
            BLKPG => {
                // partition manipulation not supported on loop devices
                return Err(AxError::from(LinuxError::ENOTTY));
            }
            BLKFLSBUF => {
                // Atomically claim the dirty flag so that a concurrent
                // write_block() will re-set dirty=true after our snapshot,
                // guaranteeing its data is flushed on the next attempt.
                #[cfg(feature = "ext4fs")]
                self.flush_block_cache_ioctl()?;
            }
            BLKIOMIN => {
                // minimum I/O size
                (arg as *mut u32).vm_write(512)?;
            }
            BLKIOOPT => {
                // optimal I/O size
                (arg as *mut u32).vm_write(512)?;
            }
            // HDIO_GETGEO: virtual CHS geometry for fdisk
            HDIO_GETGEO => {
                let size = match self.file.lock().clone() {
                    Some(f) => f.location().len().unwrap_or(0),
                    None => 0,
                };
                // hd_geometry: { u8 heads, u8 sectors, u16 cylinders, unsigned long start }
                // On 64-bit targets unsigned long is 8 bytes.
                let heads: u8 = 64;
                let sectors: u8 = 32;
                let cyl = if size > 0 {
                    (size / (heads as u64 * sectors as u64 * 512)) as u16
                } else {
                    0
                };
                #[repr(C)]
                struct HdGeometry {
                    heads: u8,
                    sectors: u8,
                    cylinders: u16,
                    start: u64,
                }
                let geo = HdGeometry {
                    heads,
                    sectors,
                    cylinders: cyl,
                    start: 0,
                };
                (arg as *mut HdGeometry).vm_write(geo)?;
            }
            _ => {
                warn!("unknown ioctl for loop device: {cmd}");
                return Err(AxError::NotATty);
            }
        }
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        if let Some(FileBackend::Cached(cache)) = self.file.lock().as_ref() {
            DeviceMmap::Cache(cache.clone())
        } else {
            DeviceMmap::None
        }
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

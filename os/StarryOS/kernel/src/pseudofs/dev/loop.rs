use alloc::boxed::Box;
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::FileBackend;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsResult};
use linux_raw_sys::{
    ioctl::{
        BLKFLSBUF, BLKGETSIZE, BLKGETSIZE64, BLKIOMIN, BLKIOOPT, BLKPG, BLKRAGET, BLKRASET,
        BLKROGET, BLKROSET, BLKRRPART, BLKSSZGET,
    },
    loop_device::{
        LOOP_CLR_FD, LOOP_CONFIGURE, LOOP_GET_STATUS, LOOP_GET_STATUS64, LOOP_SET_FD,
        LOOP_SET_STATUS, LOOP_SET_STATUS64, loop_config, loop_info, loop_info64,
    },
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    file::get_file_like,
    pseudofs::{DeviceMmap, DeviceOps},
};

/// HDIO_GETGEO ioctl command (get drive geometry).
/// Not defined in linux-raw-sys, so we use the standard value directly.
const HDIO_GETGEO: u32 = 0x0301;

/// Write `data` back to `file` starting at offset 0, then sync.
fn writeback_data(file: &FileBackend, data: &[u8]) {
    let mut offset: u64 = 0;
    while offset < data.len() as u64 {
        match file.write_at(&data[offset as usize..], offset) {
            Ok(0) => {
                warn!("LoopDevice: writeback stalled at {offset}");
                break;
            }
            Ok(n) => offset += n as u64,
            Err(e) => {
                warn!("LoopDevice: writeback err at {offset}: {e:?}");
                break;
            }
        }
    }
    if let Err(e) = file.sync(true) {
        warn!("LoopDevice: writeback sync failed: {e:?}");
    }
}

/// Adapter that wraps a LoopDevice's file backend as a block device.
///
/// This allows ext4 (or other filesystem drivers) to use a loop device as
/// backing storage by converting offset-based I/O to block-based I/O.
///
/// The entire backing file is loaded into memory at creation time.  The data
/// is owned by the **LoopDevice** (not by this adapter) so it remains
/// accessible for write-back even if this adapter is never dropped (which can
/// happen when ext4's internal DirEntry references prevent the filesystem
/// state from being freed on unmount).
///
/// Write-back happens in three places:
///   - `LOOP_CLR_FD` (losetup -d): writes back and clears the cache
///   - `as_dyn_block_device()` (re-mount): writes back before re-reading
///   - `BLKFLSBUF` ioctl: explicit flush
/// All three run in normal syscall context where VFS I/O is safe.
/// The `flush()` callback is intentionally a no-op because ext4 invokes it
/// inside `SpinNoPreempt`.
pub struct LoopBlockDevice {
    /// Pointer to the data slice inside `LoopDevice.block_cache`.
    /// Valid as long as `LoopDevice.block_cache` is `Some` and not re-initialised.
    data: *mut [u8],
    block_size: usize,
    /// Pointer to the dirty flag inside `LoopDevice.block_cache`.
    dirty: *mut bool,
}

// Safety: LoopBlockDevice is only used from the ext4 mount context, where
// exclusive access is guaranteed by `&mut self` on BlockDriverOps methods.
unsafe impl Send for LoopBlockDevice {}
unsafe impl Sync for LoopBlockDevice {}

impl LoopBlockDevice {
    /// Create a new block device adapter.
    ///
    /// `data` and `dirty` must point into storage owned by the calling
    /// `LoopDevice` that outlives this adapter (the LoopDevice lives in
    /// `/dev/loopX` for the entire system lifetime).
    pub unsafe fn new(data: *mut [u8], dirty: *mut bool) -> VfsResult<Self> {
        Ok(Self {
            data,
            block_size: 512,
            dirty,
        })
    }
}

impl ax_driver::prelude::BaseDriverOps for LoopBlockDevice {
    fn device_name(&self) -> &str {
        "loop"
    }

    fn device_type(&self) -> ax_driver::prelude::DeviceType {
        ax_driver::prelude::DeviceType::Block
    }
}

impl ax_driver::prelude::BlockDriverOps for LoopBlockDevice {
    fn num_blocks(&self) -> u64 {
        // SAFETY: data points into LoopDevice.block_cache which is valid
        // while the block device is in use.
        unsafe { (&*self.data).len() as u64 / self.block_size as u64 }
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_driver::prelude::DevResult {
        // SAFETY: see struct doc comment.
        let data = unsafe { &*self.data };
        let offset = block_id as usize * self.block_size;
        let end = offset + buf.len();
        if end > data.len() {
            return Err(ax_driver::prelude::DevError::Io);
        }
        buf.copy_from_slice(&data[offset..end]);
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_driver::prelude::DevResult {
        // SAFETY: see struct doc comment.
        let data = unsafe { &mut *self.data };
        let offset = block_id as usize * self.block_size;
        let end = offset + buf.len();
        if end > data.len() {
            return Err(ax_driver::prelude::DevError::Io);
        }
        data[offset..end].copy_from_slice(buf);
        unsafe { *self.dirty = true };
        Ok(())
    }

    fn flush(&mut self) -> ax_driver::prelude::DevResult {
        // Intentionally a no-op.  ext4 calls this from inside SpinNoPreempt
        // where sleeping VFS I/O would panic.  Dirty data is written back in
        // LOOP_CLR_FD (losetup -d) which runs in normal syscall context.
        Ok(())
    }
}

/// Block-device data cache owned by the LoopDevice.
struct BlockCache {
    data: Option<alloc::vec::Vec<u8>>,
    dirty: bool,
}

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
    /// Block-device data cache.  Populated by `as_dyn_block_device()`,
    /// written back and cleared by `LOOP_CLR_FD`.
    block_cache: Mutex<BlockCache>,
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
            block_cache: Mutex::new(BlockCache {
                data: None,
                dirty: false,
            }),
        }
    }

    /// Get information about the loop device.
    pub fn get_info(&self) -> AxResult<loop_info> {
        if self.file.lock().is_none() {
            return Err(AxError::from(LinuxError::ENXIO));
        }
        let mut res: loop_info = unsafe { core::mem::zeroed() };
        res.lo_number = self.number as _;
        res.lo_rdevice = self.dev_id.0 as _;
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
        Ok(res)
    }

    /// Clone the underlying file of the loop device.
    pub fn clone_file(&self) -> VfsResult<FileBackend> {
        let file = self.file.lock().clone();
        file.ok_or(AxError::from(LinuxError::ENXIO))
    }

    /// Create a boxed block device adapter from this loop device.
    ///
    /// Returns `Err` if no file is attached to the loop device.
    pub fn as_dyn_block_device(&self) -> VfsResult<Box<dyn ax_driver::prelude::BlockDriverOps>> {
        let file = self.file.lock().clone();
        let file = file.ok_or(AxError::from(LinuxError::ENXIO))?;

        // Write back any dirty cache before re-reading the backing file.
        // This covers the scenario: mount -> write -> umount -> mount again
        // (without losetup -d), where the in-memory writes must be persisted
        // to the backing file before we reload from it.
        {
            let mut cache = self.block_cache.lock();
            if cache.dirty {
                if let Some(ref data) = cache.data {
                    writeback_data(&file, data);
                }
                cache.dirty = false;
            }
        }

        // Read the entire backing file into the block cache.
        let len = file.location().len().unwrap_or(0) as usize;
        let mut data = alloc::vec![0u8; len];
        if len > 0 {
            let n = file.read_at(&mut data[..], 0)?;
            if n != len {
                warn!("LoopDevice: short read {n}/{len} bytes from backing file");
                return Err(AxError::Io);
            }
        }

        // Store in block_cache and obtain raw pointers for LoopBlockDevice.
        let mut cache = self.block_cache.lock();
        cache.data = Some(data);
        cache.dirty = false;
        // SAFETY: the pointers remain valid until block_cache is replaced
        // (in LOOP_CLR_FD or a subsequent as_dyn_block_device call), which
        // happens after the block device is no longer in use.  Mount
        // operations are serialised by FS_CONTEXT, preventing concurrent
        // replacement.
        let data_ptr = cache.data.as_mut().unwrap().as_mut_slice() as *mut [u8];
        let dirty_ptr = &mut cache.dirty as *mut bool;
        drop(cache);

        Ok(unsafe { Box::new(LoopBlockDevice::new(data_ptr, dirty_ptr)?) })
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

                *guard = Some(file.inner().backend()?.clone());
            }
            LOOP_CLR_FD => {
                let mut guard = self.file.lock();
                if guard.is_none() {
                    return Err(AxError::from(LinuxError::ENXIO));
                }

                // Write back dirty data from the block cache before clearing.
                // This runs in normal syscall context so CachedFile VFS I/O
                // (page cache updates) is safe.
                let mut cache = self.block_cache.lock();
                if cache.dirty
                    && let Some(ref data) = cache.data
                    && let Some(ref file) = *guard
                {
                    writeback_data(file, data);
                }
                cache.data = None;
                cache.dirty = false;
                drop(cache);

                *guard = None;
                *self.file_name.lock() = [0u8; 64];
            }
            LOOP_GET_STATUS => {
                (arg as *mut loop_info).vm_write(self.get_info()?)?;
            }
            LOOP_SET_STATUS => {
                // FIXME: AnyBitPattern
                let info = unsafe { (arg as *const loop_info).vm_read_uninit()?.assume_init() };
                self.set_info(info)?;
                let mut name = self.file_name.lock();
                for (i, &c) in info.lo_name.iter().enumerate() {
                    if i < 64 {
                        name[i] = c as u8;
                    }
                    if c == 0 {
                        break;
                    }
                }
            }
            LOOP_GET_STATUS64 => {
                (arg as *mut loop_info64).vm_write(self.get_info64()?)?;
            }
            LOOP_SET_STATUS64 => {
                // FIXME: AnyBitPattern
                let info = unsafe { (arg as *const loop_info64).vm_read_uninit()?.assume_init() };
                *self.file_name.lock() = info.lo_file_name;
            }
            LOOP_CONFIGURE => {
                // FIXME: AnyBitPattern
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
            }
            // TODO: the following should apply to any block devices
            BLKGETSIZE | BLKGETSIZE64 => {
                let file = self.clone_file()?;
                let sectors = file.location().len()? / 512;
                if cmd == BLKGETSIZE {
                    (arg as *mut u32).vm_write(sectors as _)?;
                } else {
                    (arg as *mut u64).vm_write(sectors * 512)?;
                }
            }
            BLKROGET => {
                (arg as *mut u32).vm_write(self.ro.load(Ordering::Relaxed) as u32)?;
            }
            BLKROSET => {
                let ro = (arg as *const u32).vm_read()?;
                if ro != 0 && ro != 1 {
                    return Err(AxError::InvalidInput);
                }
                self.ro.store(ro != 0, Ordering::Relaxed);
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
                // Flush dirty block cache back to the backing file.
                let file = self.file.lock().clone();
                let mut cache = self.block_cache.lock();
                if cache.dirty {
                    if let Some(ref data) = cache.data {
                        if let Some(ref file) = file {
                            writeback_data(file, data);
                        }
                    }
                    cache.dirty = false;
                }
            }
            BLKSSZGET => {
                // get logical block size
                (arg as *mut u32).vm_write(512)?;
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

    fn mmap(&self, _offset: u64) -> DeviceMmap {
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

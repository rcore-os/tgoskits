use alloc::{boxed::Box, sync::Arc};
use core::{
    any::Any,
    cell::UnsafeCell,
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
///
/// Returns `true` if the entire buffer was written and synced successfully.
/// On failure the caller should **not** clear the dirty flag so that
/// subsequent flush attempts (detach, re-mount, BLKFLSBUF) can retry.
fn writeback_data(file: &FileBackend, data: &[u8]) -> bool {
    let mut offset: u64 = 0;
    while offset < data.len() as u64 {
        match file.write_at(&data[offset as usize..], offset) {
            Ok(0) => {
                warn!("LoopDevice: writeback stalled at {offset}");
                return false;
            }
            Ok(n) => offset += n as u64,
            Err(e) => {
                warn!("LoopDevice: writeback err at {offset}: {e:?}");
                return false;
            }
        }
    }
    if let Err(e) = file.sync(true) {
        warn!("LoopDevice: writeback sync failed: {e:?}");
        return false;
    }
    true
}

/// Shared cache data backing a `LoopBlockDevice`.
///
/// Owning an `Arc<CacheData>` keeps the buffer alive even if the
/// `LoopDevice` replaces its cache slot (e.g. on re-mount).
///
/// # Synchronization (UnsafeCell safety)
///
/// `buffer_mut()` (write, via `LoopBlockDevice`) and `buffer()` (read,
/// in write-back) can never execute concurrently:
/// - `buffer_mut()` is only called through `LoopBlockDevice` which takes
///   `&mut self` (exclusive at device level).
/// - `LoopBlockDevice::new()` sets `mounted = true`; `Drop` clears it.
/// - Write-back paths check `mounted` and **skip** when it is true.
/// Therefore, while any `LoopBlockDevice` is alive, no write-back code
/// reaches `buffer()`; after the device is dropped, `buffer_mut()` can no
/// longer be called.  The two accesses are mutually exclusive.
struct CacheData {
    buffer: UnsafeCell<alloc::vec::Vec<u8>>,
    dirty: AtomicBool,
    /// `true` while a `LoopBlockDevice` referencing this cache is alive.
    mounted: AtomicBool,
}

// Safety: concurrent access to the `UnsafeCell` is prevented by the
// `mounted` flag — see struct-level Synchronization section above.
unsafe impl Send for CacheData {}
unsafe impl Sync for CacheData {}

impl CacheData {
    fn new(data: alloc::vec::Vec<u8>) -> Self {
        Self {
            buffer: UnsafeCell::new(data),
            dirty: AtomicBool::new(false),
            mounted: AtomicBool::new(false),
        }
    }

    /// Returns a shared reference to the buffer.
    ///
    /// Safe because the caller guarantees no concurrent `buffer_mut()`:
    /// write-back callers check `mounted` first; inside `LoopBlockDevice`
    /// the `&mut self` receiver prevents aliasing.
    fn buffer(&self) -> &alloc::vec::Vec<u8> {
        // see struct-level Synchronization section.
        unsafe { &*self.buffer.get() }
    }

    /// Returns a mutable reference to the buffer.
    ///
    /// Safe because this is only called from `LoopBlockDevice` methods
    /// that take `&mut self`.  Write-back paths skip when `mounted` is
    /// true, so no concurrent `buffer()` call can race.
    #[allow(clippy::mut_from_ref)]
    fn buffer_mut(&self) -> &mut alloc::vec::Vec<u8> {
        // see struct-level Synchronization section.
        unsafe { &mut *self.buffer.get() }
    }
}

/// Adapter that wraps a LoopDevice's file backend as a block device.
///
/// This allows ext4 (or other filesystem drivers) to use a loop device as
/// backing storage by converting offset-based I/O to block-based I/O.
///
/// The adapter holds an `Arc<CacheData>` so the buffer remains valid even if
/// the `LoopDevice`'s cache slot is later replaced (e.g. on re-mount while
/// old filesystem references still exist).  The old adapter keeps its buffer
/// alive through the Arc — no use-after-free.
///
/// Write-back happens in three places:
///   - `LOOP_CLR_FD` (losetup -d): writes back and clears the cache
///   - `as_dyn_block_device()` (re-mount): writes back before re-reading
///   - `BLKFLSBUF` ioctl: explicit flush
///
/// All three run in normal syscall context where VFS I/O is safe.
/// The `flush()` callback is intentionally a no-op because ext4 invokes it
/// inside `SpinNoPreempt`.
pub struct LoopBlockDevice {
    cache: Arc<CacheData>,
    block_size: usize,
}

// Safety: LoopBlockDevice is only used from the ext4 mount context, where
// exclusive access is guaranteed by `&mut self` on BlockDriverOps methods.
unsafe impl Send for LoopBlockDevice {}
unsafe impl Sync for LoopBlockDevice {}

impl LoopBlockDevice {
    /// Create a new block device adapter backed by the given `CacheData`.
    fn new(cache: Arc<CacheData>) -> VfsResult<Self> {
        cache.mounted.store(true, Ordering::Release);
        Ok(Self {
            cache,
            block_size: 512,
        })
    }
}

impl Drop for LoopBlockDevice {
    fn drop(&mut self) {
        self.cache.mounted.store(false, Ordering::Release);
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
        self.cache.buffer().len() as u64 / self.block_size as u64
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_driver::prelude::DevResult {
        let data = self.cache.buffer();
        let offset = block_id as usize * self.block_size;
        let end = offset + buf.len();
        if end > data.len() {
            return Err(ax_driver::prelude::DevError::Io);
        }
        buf.copy_from_slice(&data[offset..end]);
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_driver::prelude::DevResult {
        let data = self.cache.buffer_mut();
        let offset = block_id as usize * self.block_size;
        let end = offset + buf.len();
        if end > data.len() {
            return Err(ax_driver::prelude::DevError::Io);
        }
        data[offset..end].copy_from_slice(buf);
        self.cache.dirty.store(true, Ordering::Release);
        Ok(())
    }

    fn flush(&mut self) -> ax_driver::prelude::DevResult {
        // Intentionally a no-op.  ext4 calls this from inside SpinNoPreempt
        // where sleeping VFS I/O would panic.  Dirty data is written back in
        // LOOP_CLR_FD (losetup -d), BLKFLSBUF, or after umount — all in
        // normal syscall context.
        Ok(())
    }
}

/// Block-device data cache owned by the LoopDevice.
struct BlockCache {
    data: Option<Arc<CacheData>>,
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
            block_cache: Mutex::new(BlockCache { data: None }),
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
    ///
    /// If a previous `CacheData` still has outstanding `Arc` references from
    /// an old (unmounted) filesystem, those references keep the old buffer
    /// alive — no use-after-free.  The new adapter gets a fresh `CacheData`
    /// re-read from the backing file.
    pub fn as_dyn_block_device(&self) -> VfsResult<Box<dyn ax_driver::prelude::BlockDriverOps>> {
        let file = self.file.lock().clone();
        let file = file.ok_or(AxError::from(LinuxError::ENXIO))?;

        // Write back any dirty cache before re-reading the backing file.
        // This covers the scenario: mount -> write -> umount -> mount again
        // (without losetup -d), where the in-memory writes must be persisted
        // to the backing file before we reload from it.
        {
            let cache = self.block_cache.lock();
            #[allow(clippy::collapsible_if)]
            if let Some(ref cd) = cache.data
                && cd.dirty.load(Ordering::Acquire)
            {
                if cd.mounted.load(Ordering::Acquire) {
                    warn!("LoopDevice: re-mount writeback skipped — old cache still in use");
                } else if writeback_data(&file, cd.buffer()) {
                    cd.dirty.store(false, Ordering::Release);
                }
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

        // Store in block_cache.  Any outstanding Arc<CacheData> held by an
        // old (unmounted) filesystem keeps the old buffer alive independently;
        // the new adapter owns the fresh Arc and the two never alias.
        let mut cache = self.block_cache.lock();
        let cd = Arc::new(CacheData::new(data));
        cache.data = Some(cd.clone());
        drop(cache);

        Ok(Box::new(LoopBlockDevice::new(cd)?))
    }

    /// Write back dirty block-cache data to the backing file.
    ///
    /// Called after `unmount` in normal syscall context where VFS I/O is
    /// safe, so that data is persisted without requiring an explicit
    /// `losetup -d` or `BLKFLSBUF`.
    pub fn flush_cache_to_file(&self) {
        let file = self.file.lock().clone();
        let cache = self.block_cache.lock();
        #[allow(clippy::collapsible_if)]
        if let Some(ref cd) = cache.data
            && cd.dirty.load(Ordering::Acquire)
            && let Some(ref file) = file
        {
            if writeback_data(file, cd.buffer()) {
                cd.dirty.store(false, Ordering::Release);
            }
        }
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
                #[allow(clippy::collapsible_if)]
                if let Some(ref cd) = cache.data
                    && cd.dirty.load(Ordering::Acquire)
                {
                    if cd.mounted.load(Ordering::Acquire) {
                        warn!("LoopDevice: LOOP_CLR_FD writeback skipped — cache still in use");
                    } else if let Some(ref file) = *guard {
                        if !writeback_data(file, cd.buffer()) {
                            warn!("LoopDevice: writeback failed on LOOP_CLR_FD, data may be lost");
                        }
                    }
                    cd.dirty.store(false, Ordering::Release);
                }
                cache.data = None;
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
                let cache = self.block_cache.lock();
                #[allow(clippy::collapsible_if)]
                if let Some(ref cd) = cache.data
                    && cd.dirty.load(Ordering::Acquire)
                    && let Some(ref file) = file
                {
                    if cd.mounted.load(Ordering::Acquire) {
                        warn!(
                            "LoopDevice: BLKFLSBUF skipped — cache still in use by mounted \
                             filesystem"
                        );
                    } else if writeback_data(file, cd.buffer()) {
                        cd.dirty.store(false, Ordering::Release);
                    }
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

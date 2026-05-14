#[cfg(feature = "ext4")]
use alloc::{boxed::Box, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::FileBackend;
#[cfg(feature = "ext4")]
use ax_kspin::SpinNoPreempt;
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

use crate::{
    file::{FileLike, get_file_like},
    pseudofs::{DeviceMmap, DeviceOps},
};

/// HDIO_GETGEO ioctl command (get drive geometry).
/// Not defined in linux-raw-sys, so we use the standard value directly.
const HDIO_GETGEO: u32 = 0x0301;

#[cfg(feature = "ext4")]
const CACHE_BLK: usize = 4096;

#[cfg(feature = "ext4")]
fn writeback_buffer(file: &FileBackend, cd: &CacheData) -> bool {
    let total = cd.total_len;
    let nchunks = cd.blocks.lock().len();
    let mut offset: usize = 0;
    for i in 0..nchunks {
        let mut buf = [0u8; CACHE_BLK];
        let to_write = {
            let guard = cd.blocks.lock();
            let Some(chunk) = guard.get(i) else { break };
            let n = chunk.len().min(total.saturating_sub(offset));
            buf[..n].copy_from_slice(&chunk[..n]);
            n
        };
        if to_write == 0 {
            break;
        }
        let mut written = 0usize;
        while written < to_write {
            match file.write_at(&buf[written..to_write], (offset + written) as u64) {
                Ok(0) => {
                    warn!("LoopDevice: writeback stalled at {}", offset + written);
                    return false;
                }
                Ok(w) => written += w,
                Err(e) => {
                    warn!("LoopDevice: writeback err at {}: {e:?}", offset + written);
                    return false;
                }
            }
        }
        offset += to_write;
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
/// The buffer is protected by a `SpinNoPreempt` lock.  Both block-device
/// I/O (`read_block`/`write_block`, already under ext4's SpinNoPreempt)
/// and write-back paths (normal syscall context) acquire this lock,
/// so no concurrent access is possible regardless of mount state.
#[cfg(feature = "ext4")]
struct CacheData {
    blocks: SpinNoPreempt<alloc::vec::Vec<alloc::vec::Vec<u8>>>,
    total_len: usize,
    dirty: AtomicBool,
    /// `true` while a `LoopBlockDevice` referencing this cache is alive.
    mounted: AtomicBool,
}

#[cfg(feature = "ext4")]
impl CacheData {
    fn new(blocks: alloc::vec::Vec<alloc::vec::Vec<u8>>, total_len: usize) -> Self {
        Self {
            blocks: SpinNoPreempt::new(blocks),
            total_len,
            dirty: AtomicBool::new(false),
            mounted: AtomicBool::new(false),
        }
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
#[cfg(feature = "ext4")]
pub struct LoopBlockDevice {
    cache: Arc<CacheData>,
    block_size: usize,
    ro: bool,
}

#[cfg(feature = "ext4")]
impl LoopBlockDevice {
    /// Create a new block device adapter backed by the given `CacheData`.
    fn new(cache: Arc<CacheData>, ro: bool) -> VfsResult<Self> {
        cache.mounted.store(true, Ordering::Release);
        Ok(Self {
            cache,
            block_size: 512,
            ro,
        })
    }
}

#[cfg(feature = "ext4")]
impl Drop for LoopBlockDevice {
    fn drop(&mut self) {
        self.cache.mounted.store(false, Ordering::Release);
    }
}

#[cfg(feature = "ext4")]
impl ax_driver::prelude::BaseDriverOps for LoopBlockDevice {
    fn device_name(&self) -> &str {
        "loop"
    }

    fn device_type(&self) -> ax_driver::prelude::DeviceType {
        ax_driver::prelude::DeviceType::Block
    }
}

#[cfg(feature = "ext4")]
impl ax_driver::prelude::BlockDriverOps for LoopBlockDevice {
    fn num_blocks(&self) -> u64 {
        self.cache.total_len as u64 / self.block_size as u64
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> ax_driver::prelude::DevResult {
        let byte_off = block_id as usize * self.block_size;
        if byte_off
            .checked_add(buf.len())
            .is_none_or(|end| end > self.cache.total_len)
        {
            return Err(ax_driver::prelude::DevError::Io);
        }
        let blocks = self.cache.blocks.lock();
        let mut pos = 0;
        let mut cur = byte_off;
        while pos < buf.len() {
            let idx = cur / CACHE_BLK;
            let off = cur % CACHE_BLK;
            let Some(chunk) = blocks.get(idx) else {
                return Err(ax_driver::prelude::DevError::Io);
            };
            let to_copy = (buf.len() - pos).min(CACHE_BLK - off);
            buf[pos..pos + to_copy].copy_from_slice(&chunk[off..off + to_copy]);
            pos += to_copy;
            cur += to_copy;
        }
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> ax_driver::prelude::DevResult {
        if self.ro {
            return Err(ax_driver::prelude::DevError::Io);
        }
        let byte_off = block_id as usize * self.block_size;
        if byte_off
            .checked_add(buf.len())
            .is_none_or(|end| end > self.cache.total_len)
        {
            return Err(ax_driver::prelude::DevError::Io);
        }
        let mut blocks = self.cache.blocks.lock();
        let mut pos = 0;
        let mut cur = byte_off;
        while pos < buf.len() {
            let idx = cur / CACHE_BLK;
            let off = cur % CACHE_BLK;
            let Some(chunk) = blocks.get_mut(idx) else {
                return Err(ax_driver::prelude::DevError::Io);
            };
            let to_copy = (buf.len() - pos).min(CACHE_BLK - off);
            chunk[off..off + to_copy].copy_from_slice(&buf[pos..pos + to_copy]);
            pos += to_copy;
            cur += to_copy;
        }
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
#[cfg(feature = "ext4")]
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
    /// Bit mask of `LO_FLAGS_*` (READ_ONLY, AUTOCLEAR, PARTSCAN, DIRECT_IO).
    flags: AtomicU32,
    /// Whether the device is opened exclusively (O_EXCL).
    exclusive: AtomicBool,
    /// Block-device data cache.  Populated by `as_dyn_block_device()`,
    /// written back and cleared by `LOOP_CLR_FD`.
    #[cfg(feature = "ext4")]
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
            flags: AtomicU32::new(0),
            exclusive: AtomicBool::new(false),
            #[cfg(feature = "ext4")]
            block_cache: Mutex::new(BlockCache { data: None }),
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

    /// Create a boxed block device adapter from this loop device.
    ///
    /// Returns `Err` if no file is attached to the loop device.
    ///
    /// If a previous `CacheData` still has outstanding `Arc` references from
    /// an old (unmounted) filesystem, those references keep the old buffer
    /// alive — no use-after-free.  The new adapter gets a fresh `CacheData`
    /// re-read from the backing file.
    #[cfg(feature = "ext4")]
    pub fn as_dyn_block_device(&self) -> VfsResult<Box<dyn ax_driver::prelude::BlockDriverOps>> {
        let file = self.file.lock().clone();
        let file = file.ok_or(AxError::from(LinuxError::ENXIO))?;
        let len = file.location().len().unwrap_or(0) as usize;

        // Reject re-mount if an existing CacheData is still actively mounted.
        {
            let mut cache = self.block_cache.lock();
            if let Some(ref cd) = cache.data {
                if cd.mounted.load(Ordering::Acquire) {
                    return Err(AxError::from(LinuxError::EBUSY));
                }
                if cd.dirty.swap(false, Ordering::AcqRel) {
                    if self.ro.load(Ordering::Relaxed) {
                        cd.dirty.store(true, Ordering::Release);
                        return Err(AxError::Io);
                    }
                    if !writeback_buffer(&file, cd) {
                        cd.dirty.store(true, Ordering::Release);
                        return Err(AxError::Io);
                    }
                }
            }
            cache.data = None;
        }

        // Read the backing file in CACHE_BLK-sized chunks.  Each individual
        // allocation is only 4 KiB, small enough to satisfy even when physical
        // memory is fragmented after many mount/umount cycles.
        let num_chunks = if len == 0 {
            0
        } else {
            (len - 1) / CACHE_BLK + 1
        };
        let mut chunks = alloc::vec::Vec::with_capacity(num_chunks);
        let mut offset: usize = 0;
        for _ in 0..num_chunks {
            let to_read = CACHE_BLK.min(len - offset);
            let mut chunk = alloc::vec![0u8; CACHE_BLK];
            let n = file.read_at(&mut chunk[..to_read], offset as u64)?;
            if n != to_read {
                warn!("LoopDevice: short read {n}/{to_read} at offset {offset}");
                return Err(AxError::Io);
            }
            chunks.push(chunk);
            offset += to_read;
        }

        let mut cache = self.block_cache.lock();
        let cd = Arc::new(CacheData::new(chunks, len));
        cache.data = Some(cd.clone());
        drop(cache);

        Ok(Box::new(LoopBlockDevice::new(
            cd,
            self.ro.load(Ordering::Relaxed),
        )?))
    }

    /// Write back dirty block-cache data to the backing file.
    ///
    /// Called after `unmount` in normal syscall context where VFS I/O is
    /// safe, so that data is persisted without requiring an explicit
    /// `losetup -d` or `BLKFLSBUF`.
    ///
    /// Returns `Err(AxError::Io)` if writeback fails, so that callers
    /// (`sys_umount2`) can propagate the error to userspace.  On failure
    /// the dirty flag is preserved so that subsequent flush attempts
    /// (BLKFLSBUF, LOOP_CLR_FD) can retry.
    #[cfg(feature = "ext4")]
    pub fn flush_cache_to_file(&self) -> AxResult<()> {
        let file = self.file.lock().clone();
        let cache = self.block_cache.lock();
        if let Some(ref cd) = cache.data {
            let mut wb_err = false;
            if cd.dirty.swap(false, Ordering::AcqRel)
                && let Some(ref file) = file
            {
                let writeback_ok = !self.ro.load(Ordering::Relaxed) && writeback_buffer(file, cd);
                if !writeback_ok {
                    cd.dirty.store(true, Ordering::Release);
                    wb_err = true;
                }
            }
            cd.mounted.store(false, Ordering::Release);
            if wb_err {
                return Err(AxError::Io);
            }
        }
        Ok(())
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
                // (page cache updates) is safe.  The SpinNoPreempt lock
                // ensures no concurrent write_block() can race.
                #[cfg(feature = "ext4")]
                {
                    let cache = self.block_cache.lock();
                    // If a LoopBlockDevice is still alive (mounted), refuse to
                    // detach — the old Arc<CacheData> would keep receiving
                    // write_block calls but the backing file would be gone,
                    // causing silent data loss on umount.
                    if let Some(ref cd) = cache.data
                        && cd.mounted.load(Ordering::Acquire)
                    {
                        return Err(AxError::from(LinuxError::EBUSY));
                    }

                    if let Some(ref cd) = cache.data
                        && cd.dirty.load(Ordering::Acquire)
                    {
                        if self.ro.load(Ordering::Relaxed) {
                            return Err(AxError::Io);
                        }
                        if let Some(ref file) = *guard
                            && !writeback_buffer(file, cd)
                        {
                            warn!("LoopDevice: writeback failed on LOOP_CLR_FD, data may be lost");
                            return Err(AxError::Io);
                        }
                        cd.dirty.store(false, Ordering::Release);
                    }
                    drop(cache);
                    self.block_cache.lock().data = None;
                }

                *guard = None;
                *self.file_name.lock() = [0u8; 64];
                self.flags.store(0, Ordering::Relaxed);
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
                self.set_lo_flags(info.lo_flags as u32);
            }
            LOOP_GET_STATUS64 => {
                (arg as *mut loop_info64).vm_write(self.get_info64()?)?;
            }
            LOOP_SET_STATUS64 => {
                // FIXME: AnyBitPattern
                let info = unsafe { (arg as *const loop_info64).vm_read_uninit()?.assume_init() };
                *self.file_name.lock() = info.lo_file_name;
                self.set_lo_flags(info.lo_flags);
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
                let mut flags = cfg.info.lo_flags;
                if (file.open_flags() & O_ACCMODE) == O_RDONLY {
                    flags |= LO_FLAGS_READ_ONLY as u32;
                }
                self.set_lo_flags(flags);
            }
            // TODO: the following should apply to any block devices
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
                #[cfg(feature = "ext4")]
                {
                    let file = self.file.lock().clone();
                    let cache = self.block_cache.lock();
                    if let Some(ref cd) = cache.data
                        && let Some(ref file) = file
                    {
                        if self.ro.load(Ordering::Relaxed) {
                            return Err(AxError::Io);
                        }
                        if !cd.dirty.swap(false, Ordering::AcqRel) {
                            return Ok(0);
                        }
                        if !writeback_buffer(file, cd) {
                            cd.dirty.store(true, Ordering::Release);
                            return Err(AxError::Io);
                        }
                    }
                }
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

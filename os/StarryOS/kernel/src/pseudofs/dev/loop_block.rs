use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::FileBackend;
use ax_kspin::SpinNoIrq;
use axfs_ng_vfs::VfsResult;

use super::r#loop::LoopDevice;

const CACHE_BLK: usize = 4096;

pub(super) struct BlockCache {
    data: Option<Arc<CacheData>>,
}

impl BlockCache {
    pub(super) const fn new() -> Self {
        Self { data: None }
    }
}

struct CacheData {
    blocks: SpinNoIrq<alloc::vec::Vec<alloc::vec::Vec<u8>>>,
    total_len: usize,
    dirty: AtomicBool,
    mounted: AtomicBool,
}

impl CacheData {
    fn new(blocks: alloc::vec::Vec<alloc::vec::Vec<u8>>, total_len: usize) -> Self {
        Self {
            blocks: SpinNoIrq::new(blocks),
            total_len,
            dirty: AtomicBool::new(false),
            mounted: AtomicBool::new(false),
        }
    }
}

struct LoopBlockDevice {
    cache: Arc<CacheData>,
    block_size: usize,
    ro: bool,
}

impl LoopBlockDevice {
    fn new(cache: Arc<CacheData>, ro: bool) -> VfsResult<Self> {
        cache.mounted.store(true, Ordering::Release);
        Ok(Self {
            cache,
            block_size: 512,
            ro,
        })
    }
}

impl Drop for LoopBlockDevice {
    fn drop(&mut self) {
        self.cache.mounted.store(false, Ordering::Release);
    }
}

impl ax_fs::FsBlockDevice for LoopBlockDevice {
    fn name(&self) -> &str {
        "loop"
    }

    fn num_blocks(&self) -> u64 {
        self.cache.total_len as u64 / self.block_size as u64
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let byte_off = block_id as usize * self.block_size;
        if byte_off
            .checked_add(buf.len())
            .is_none_or(|end| end > self.cache.total_len)
        {
            return Err(AxError::Io);
        }
        let blocks = self.cache.blocks.lock();
        let mut pos = 0;
        let mut cur = byte_off;
        while pos < buf.len() {
            let idx = cur / CACHE_BLK;
            let off = cur % CACHE_BLK;
            let Some(chunk) = blocks.get(idx) else {
                return Err(AxError::Io);
            };
            let to_copy = (buf.len() - pos).min(CACHE_BLK - off);
            buf[pos..pos + to_copy].copy_from_slice(&chunk[off..off + to_copy]);
            pos += to_copy;
            cur += to_copy;
        }
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        if self.ro {
            return Err(AxError::Io);
        }
        let byte_off = block_id as usize * self.block_size;
        if byte_off
            .checked_add(buf.len())
            .is_none_or(|end| end > self.cache.total_len)
        {
            return Err(AxError::Io);
        }
        let mut blocks = self.cache.blocks.lock();
        let mut pos = 0;
        let mut cur = byte_off;
        while pos < buf.len() {
            let idx = cur / CACHE_BLK;
            let off = cur % CACHE_BLK;
            let Some(chunk) = blocks.get_mut(idx) else {
                return Err(AxError::Io);
            };
            let to_copy = (buf.len() - pos).min(CACHE_BLK - off);
            chunk[off..off + to_copy].copy_from_slice(&buf[pos..pos + to_copy]);
            pos += to_copy;
            cur += to_copy;
        }
        self.cache.dirty.store(true, Ordering::Release);
        Ok(())
    }

    fn flush(&mut self) -> AxResult {
        Ok(())
    }
}

impl LoopDevice {
    pub fn as_dyn_block_device(&self) -> VfsResult<Box<dyn ax_fs::FsBlockDevice>> {
        let file = self.file.lock().clone();
        let file = file.ok_or(AxError::from(LinuxError::ENXIO))?;
        let len = file.location().len().unwrap_or(0) as usize;

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

        let chunks = read_file_chunks(&file, len)?;
        let mut cache = self.block_cache.lock();
        let cd = Arc::new(CacheData::new(chunks, len));
        cache.data = Some(cd.clone());
        drop(cache);

        Ok(Box::new(LoopBlockDevice::new(
            cd,
            self.ro.load(Ordering::Relaxed),
        )?))
    }

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

    pub(super) fn detach_block_cache(&self, file: Option<&FileBackend>) -> AxResult<()> {
        let cache = self.block_cache.lock();
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
            if let Some(file) = file
                && !writeback_buffer(file, cd)
            {
                warn!("LoopDevice: writeback failed on LOOP_CLR_FD, data may be lost");
                return Err(AxError::Io);
            }
            cd.dirty.store(false, Ordering::Release);
        }
        drop(cache);
        self.block_cache.lock().data = None;
        Ok(())
    }

    pub(super) fn flush_block_cache_ioctl(&self) -> AxResult<()> {
        let file = self.file.lock().clone();
        let cache = self.block_cache.lock();
        if let Some(ref cd) = cache.data
            && let Some(ref file) = file
        {
            if self.ro.load(Ordering::Relaxed) {
                return Err(AxError::Io);
            }
            if !cd.dirty.swap(false, Ordering::AcqRel) {
                return Ok(());
            }
            if !writeback_buffer(file, cd) {
                cd.dirty.store(true, Ordering::Release);
                return Err(AxError::Io);
            }
        }
        Ok(())
    }
}

fn read_file_chunks(
    file: &FileBackend,
    len: usize,
) -> VfsResult<alloc::vec::Vec<alloc::vec::Vec<u8>>> {
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
    Ok(chunks)
}

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

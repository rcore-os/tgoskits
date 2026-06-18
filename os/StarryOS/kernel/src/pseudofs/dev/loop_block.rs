use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs_ng::{
    block::runtime::{BlockDeviceHandle, BlockDrainWake, BlockIrqBridge, BlockRuntimeConfig},
    vfs::FileBackend,
};
use ax_kspin::SpinNoIrq;
use axfs_ng_vfs::VfsResult;
use rdif_block::{
    BlkError, DeviceInfo, IQueue, QueueInfo, QueueLimits, Request, RequestId, RequestOp,
    RequestStatus,
};

use super::r#loop::LoopDevice;

const LOOP_BLOCK_SIZE: usize = 512;
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
    blocks: SpinNoIrq<Vec<Vec<u8>>>,
    total_len: usize,
    dirty: AtomicBool,
    mounted: AtomicBool,
}

impl CacheData {
    fn new(blocks: Vec<Vec<u8>>, total_len: usize) -> Self {
        Self {
            blocks: SpinNoIrq::new(blocks),
            total_len,
            dirty: AtomicBool::new(false),
            mounted: AtomicBool::new(false),
        }
    }
}

struct LoopDrainWake;

impl BlockDrainWake for LoopDrainWake {
    fn wake_drain(&self) {}
}

struct LoopQueue {
    cache: Arc<CacheData>,
    ro: bool,
    info: QueueInfo,
    next_request_id: usize,
    pending: BTreeMap<RequestId, RequestStatus>,
}

impl LoopQueue {
    fn new(cache: Arc<CacheData>, ro: bool) -> Self {
        cache.mounted.store(true, Ordering::Release);
        let total_len = cache.total_len;
        let mut limits = QueueLimits::simple(LOOP_BLOCK_SIZE, u64::MAX);
        limits.supports_flush = true;
        let mut device = DeviceInfo::new((total_len / LOOP_BLOCK_SIZE) as u64, LOOP_BLOCK_SIZE);
        device.read_only = ro;
        Self {
            cache,
            ro,
            info: QueueInfo {
                id: 0,
                device,
                limits,
            },
            next_request_id: 1,
            pending: BTreeMap::new(),
        }
    }

    fn copy_from_cache(&self, offset: usize, dst: &mut [u8]) -> Result<(), BlkError> {
        if offset
            .checked_add(dst.len())
            .is_none_or(|end| end > self.cache.total_len)
        {
            return Err(BlkError::InvalidRequest);
        }
        let blocks = self.cache.blocks.lock();
        let mut pos = 0;
        let mut cur = offset;
        while pos < dst.len() {
            let idx = cur / CACHE_BLK;
            let off = cur % CACHE_BLK;
            let Some(chunk) = blocks.get(idx) else {
                return Err(BlkError::InvalidRequest);
            };
            let to_copy = (dst.len() - pos).min(CACHE_BLK - off);
            dst[pos..pos + to_copy].copy_from_slice(&chunk[off..off + to_copy]);
            pos += to_copy;
            cur += to_copy;
        }
        Ok(())
    }

    fn copy_into_cache(&self, offset: usize, src: &[u8]) -> Result<(), BlkError> {
        if offset
            .checked_add(src.len())
            .is_none_or(|end| end > self.cache.total_len)
        {
            return Err(BlkError::InvalidRequest);
        }
        let mut blocks = self.cache.blocks.lock();
        let mut pos = 0;
        let mut cur = offset;
        while pos < src.len() {
            let idx = cur / CACHE_BLK;
            let off = cur % CACHE_BLK;
            let Some(chunk) = blocks.get_mut(idx) else {
                return Err(BlkError::InvalidRequest);
            };
            let to_copy = (src.len() - pos).min(CACHE_BLK - off);
            chunk[off..off + to_copy].copy_from_slice(&src[pos..pos + to_copy]);
            pos += to_copy;
            cur += to_copy;
        }
        self.cache.dirty.store(true, Ordering::Release);
        Ok(())
    }

    fn block_offset(lba: u64) -> Result<usize, BlkError> {
        let lba = usize::try_from(lba).map_err(|_| BlkError::InvalidRequest)?;
        lba.checked_mul(LOOP_BLOCK_SIZE)
            .ok_or(BlkError::InvalidRequest)
    }

    fn execute_request(&self, request: &mut Request<'_>) -> Result<(), BlkError> {
        let base = Self::block_offset(request.lba)?;
        match request.op {
            RequestOp::Read => {
                let mut offset = 0usize;
                for segment in request.segments.iter_mut() {
                    let len = segment.len;
                    self.copy_from_cache(base + offset, &mut segment[..len])?;
                    offset += len;
                }
                Ok(())
            }
            RequestOp::Write => {
                if self.ro {
                    return Err(BlkError::Io);
                }
                let mut offset = 0usize;
                for segment in request.segments.iter() {
                    let len = segment.len;
                    self.copy_into_cache(base + offset, &segment[..len])?;
                    offset += len;
                }
                Ok(())
            }
            RequestOp::Flush => Ok(()),
            _ => Err(BlkError::NotSupported),
        }
    }
}

impl Drop for LoopQueue {
    fn drop(&mut self) {
        self.cache.mounted.store(false, Ordering::Release);
    }
}

// SAFETY: The queue executes all operations synchronously while holding
// exclusive `&mut self` access and only touches the cached file bytes.
unsafe impl IQueue for LoopQueue {
    fn id(&self) -> usize {
        self.info.id
    }

    fn info(&self) -> QueueInfo {
        self.info
    }

    fn submit_request(&mut self, mut request: Request<'_>) -> Result<RequestId, BlkError> {
        self.execute_request(&mut request)?;
        let request_id = RequestId::new(self.next_request_id);
        self.next_request_id += 1;
        self.pending.insert(request_id, RequestStatus::Complete);
        Ok(request_id)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        self.pending
            .remove(&request)
            .ok_or(BlkError::InvalidRequest)
    }
}

impl LoopDevice {
    pub fn block_handle(&self) -> VfsResult<Arc<BlockDeviceHandle>> {
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

        let handle = BlockDeviceHandle::new(
            "loop",
            [Box::new(LoopQueue::new(cd, self.ro.load(Ordering::Relaxed))) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            BlockRuntimeConfig::new(Arc::new(LoopDrainWake)),
        )
        .map_err(|_| AxError::Io)?;
        Ok(handle)
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

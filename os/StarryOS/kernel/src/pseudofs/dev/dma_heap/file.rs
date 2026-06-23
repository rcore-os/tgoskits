//! The dma-buf fd object returned by `DMA_HEAP_IOCTL_ALLOC`. Implements `FileLike`: its mmap maps
//! the backing buffer (uncached, via `DeviceMmap::Physical`) and anchors the VMA to the buffer's
//! `Arc`, so closing the fd before `munmap` is safe.

use alloc::{borrow::Cow, sync::Arc};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, Pollable};
use starry_vm::VmPtr;

use super::{object::DmaBufObject, uapi};
use crate::{
    file::{FileLike, Kstat},
    pseudofs::DeviceMmap,
};

/// A dma-buf fd backing object.
pub struct DmaBufFile {
    buf: Arc<DmaBufObject>,
}

impl DmaBufFile {
    pub fn new(buf: Arc<DmaBufObject>) -> Self {
        Self { buf }
    }

    /// The underlying buffer (for `resolve_dmabuf_fd` / RGA import).
    pub fn buffer(&self) -> &Arc<DmaBufObject> {
        &self.buf
    }

    fn handle_sync(&self, arg: usize) -> AxResult<usize> {
        // SAFETY: `arg` is a userspace pointer to a `dma_buf_sync` (8-byte aligned, `#[repr(C)]`);
        // `vm_read_uninit` faults safely on a bad address.
        let sync = unsafe {
            (arg as *const uapi::DmaBufSync)
                .vm_read_uninit()?
                .assume_init()
        };
        if sync.flags & !uapi::DMA_BUF_SYNC_VALID_FLAGS_MASK != 0 {
            return Err(AxError::InvalidInput);
        }
        // Uncached mapping (design §7): cache maintenance is a no-op on this platform. We still
        // drive the dma-api sync entry points so enabling a cached mode later is a one-line change.
        if sync.flags & uapi::DMA_BUF_SYNC_END == uapi::DMA_BUF_SYNC_START {
            // START phase: CPU is about to access; invalidate so it observes device writes.
            self.buf.sync_for_cpu();
        } else {
            // END phase: CPU is done; flush so the device observes CPU writes.
            self.buf.sync_for_device();
        }
        Ok(0)
    }
}

impl Pollable for DmaBufFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {
        // A dma-buf fd is always ready; no waker registration is needed.
    }
}

impl FileLike for DmaBufFile {
    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed("/dev/dma_heap:buffer")
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat {
            size: self.buf.len() as u64,
            ..Default::default()
        })
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            uapi::DMA_BUF_IOCTL_SYNC => self.handle_sync(arg),
            _ => Err(AxError::NotATty),
        }
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> AxResult<DeviceMmap> {
        // Single contiguous physical range. The mmap syscall applies the byte offset, forces
        // UNCACHED (mmap.rs), and pins the VMA to this `Arc<DmaBufObject>` anchor so the pages
        // survive an fd close before munmap.
        Ok(DeviceMmap::Physical(
            self.buf.phys_range(),
            Some(self.buf.clone()),
        ))
    }
}

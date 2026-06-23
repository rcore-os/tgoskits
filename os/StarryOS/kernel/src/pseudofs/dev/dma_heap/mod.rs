mod device;
mod file;
mod object;
mod uapi;

use alloc::sync::Arc;
use core::ffi::c_int;

use ax_errno::AxResult;
use axfs_ng_vfs::DeviceId;
pub use device::DmaHeap;
use file::DmaBufFile;
pub use object::DmaBufObject;

use crate::file::FileLike;

/// Allocate a dma-buf-backed contiguous buffer directly from the kernel (no userspace fd).
/// Used by the RGA selftest's imported-buffer case.
pub fn alloc(len: usize) -> AxResult<Arc<DmaBufObject>> {
    DmaBufObject::alloc(len)
}

/// Resolve a dma-buf fd to its backing object (fd → phys+len). Phase E `/dev/rga` uses this to
/// import a buffer for RGA submission.
pub fn resolve_dmabuf_fd(fd: c_int) -> AxResult<Arc<DmaBufObject>> {
    let file = DmaBufFile::from_fd(fd)?;
    Ok(file.buffer().clone())
}

/// Device ID for /dev/dma_heap/system
pub const DMA_HEAP_SYSTEM_DEVICE_ID: DeviceId = DeviceId::new(252, 0);

/// `/dev/dma_heap/cma` device id (alias node; same contiguous allocator as `system`).
pub const DMA_HEAP_CMA_DEVICE_ID: DeviceId = DeviceId::new(252, 1);

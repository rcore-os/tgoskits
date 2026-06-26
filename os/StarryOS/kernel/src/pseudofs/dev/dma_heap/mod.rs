mod device;
mod file;
mod object;
mod uapi;

// The items below back the in-kernel helper API consumed only by the RGA driver
// (`resolve_dmabuf_fd` for /dev/rga) and its selftest (`alloc`). They compile out when
// dma-heap is built without rga (e.g. for rknpu, which uses its own allocator).
#[cfg(feature = "rga")]
use alloc::sync::Arc;
#[cfg(feature = "rga")]
use core::ffi::c_int;

#[cfg(feature = "rga")]
use ax_errno::AxResult;
use axfs_ng_vfs::DeviceId;
pub use device::DmaHeap;
#[cfg(feature = "rga")]
use file::DmaBufFile;
#[cfg(feature = "rga")]
pub use object::DmaBufObject;

#[cfg(feature = "rga")]
use crate::file::FileLike;

/// Allocate a dma-buf-backed contiguous buffer directly from the kernel (no userspace fd).
/// Used by the RGA selftest's imported-buffer case.
#[cfg(feature = "rga-selftest")]
pub fn alloc(len: usize) -> AxResult<Arc<DmaBufObject>> {
    DmaBufObject::alloc(len)
}

/// Resolve a dma-buf fd to its backing object (fd → phys+len). `/dev/rga` uses this to
/// import a buffer for RGA submission.
#[cfg(feature = "rga")]
pub fn resolve_dmabuf_fd(fd: c_int) -> AxResult<Arc<DmaBufObject>> {
    let file = DmaBufFile::from_fd(fd)?;
    Ok(file.buffer().clone())
}

/// Device ID for /dev/dma_heap/system
pub const DMA_HEAP_SYSTEM_DEVICE_ID: DeviceId = DeviceId::new(252, 0);

/// `/dev/dma_heap/cma` device id (alias node; same contiguous allocator as `system`).
pub const DMA_HEAP_CMA_DEVICE_ID: DeviceId = DeviceId::new(252, 1);

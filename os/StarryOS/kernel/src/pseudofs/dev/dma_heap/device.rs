//! The `/dev/dma_heap/{system,cma}` character device. `DMA_HEAP_IOCTL_ALLOC` allocates a
//! contiguous buffer and returns a real, mmap-able, RGA-importable dma-buf fd.

use core::any::Any;

use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use linux_raw_sys::general::O_CLOEXEC;
use starry_vm::{VmMutPtr, VmPtr};

use super::{file::DmaBufFile, object::DmaBufObject, uapi};
use crate::{file::FileLike, pseudofs::DeviceOps};

/// dma-heap device. Stateless; allocates on demand. One instance backs both heap nodes.
pub struct DmaHeap;

impl DmaHeap {
    pub fn new() -> Self {
        Self
    }

    fn handle_alloc(&self, arg: usize) -> VfsResult<usize> {
        // SAFETY: `arg` is a userspace pointer to a `dma_heap_allocation_data` (8-byte aligned,
        // `#[repr(C)]`); `vm_read_uninit` faults safely on a bad address.
        let mut data = unsafe {
            (arg as *const uapi::DmaHeapAllocationData)
                .vm_read_uninit()?
                .assume_init()
        };
        if data.len == 0 || data.len > u32::MAX as u64 {
            return Err(VfsError::InvalidInput);
        }
        let buf = DmaBufObject::alloc(data.len as usize).map_err(|_| VfsError::NoMemory)?;
        let cloexec = data.fd_flags & O_CLOEXEC != 0;
        let fd = DmaBufFile::new(buf)
            .add_to_fd_table(cloexec)
            .map_err(|_| VfsError::TooManyOpenFiles)?;
        data.fd = fd as u32;
        (arg as *mut uapi::DmaHeapAllocationData).vm_write(data)?;
        Ok(0)
    }
}

impl Default for DmaHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for DmaHeap {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            uapi::DMA_HEAP_IOCTL_ALLOC => self.handle_alloc(arg),
            _ => Err(VfsError::NotATty),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

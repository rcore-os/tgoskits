//! Real Linux dma-heap nodes (`/dev/dma_heap/<name>`) backing MPP buffer
//! allocation for the JPEG decoder.
//!
//! `librockchip_mpp` prefers the dma-heap allocator: it checks that the
//! `/dev/dma_heap` directory exists, opens a per-heap node (e.g. `cma`,
//! `system`), and allocates with `DMA_HEAP_IOCTL_ALLOC`, receiving a dma-buf fd.
//! Here every heap maps to one contiguous, DMA-coherent allocator
//! ([`crate::file::dmabuf::DmaBufFile`]); the returned fd resolves to a physical
//! base that `/dev/mpp_service` programs into the decoder.

use alloc::sync::Arc;
use core::any::Any;

use axfs_ng_vfs::{DeviceId, VfsError, VfsResult};
use bytemuck::{AnyBitPattern, NoUninit};
use linux_raw_sys::general::O_CLOEXEC;

use crate::{
    file::{add_file_like, close_file_like, dmabuf::DmaBufFile},
    mm::{UserConstPtr, UserPtr},
    pseudofs::DeviceOps,
};

/// Char-device id for the dma-heap nodes (opened by path; id is informational).
pub const DMA_HEAP_DEVICE_ID: DeviceId = DeviceId::new(0xF1, 0x20);

/// Heap node names exposed under `/dev/dma_heap/`. `librockchip_mpp` enumerates
/// all of these (including the `-dma32` variants) at startup and picks one by
/// buffer flags; exposing each as a node avoids the lib's fragile dup-a-sibling
/// fallback. All map to the same contiguous coherent allocator, which always
/// allocates below 4 GiB ([`DmaBufFile::alloc`] uses the dma32 page path) so the
/// 32-bit IOMMU-bypassed accelerators can address every buffer.
pub const HEAP_NAMES: &[&str] = &[
    "system",
    "system-uncached",
    "system-dma32",
    "system-uncached-dma32",
    "cma",
    "cma-uncached",
    "cma-dma32",
    "cma-uncached-dma32",
];

/// `DMA_HEAP_IOCTL_ALLOC = _IOWR('H', 0, struct dma_heap_allocation_data)` (24B).
const DMA_HEAP_IOCTL_ALLOC: u32 = 0xC018_4800;

/// `struct dma_heap_allocation_data` (Linux dma-buf heaps UAPI).
#[repr(C)]
#[derive(Clone, Copy, Default, AnyBitPattern, NoUninit)]
struct DmaHeapAllocData {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

/// A contiguous dma-buf heap node.
pub struct DmaHeap;

impl DeviceOps for DmaHeap {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        if cmd != DMA_HEAP_IOCTL_ALLOC {
            return Err(VfsError::NotATty);
        }
        if arg == 0 {
            return Err(VfsError::InvalidInput);
        }

        let mut data = copy_in(arg)?;

        // Linux dma-heap rejects a zero-length allocation with EINVAL.
        if data.len == 0 {
            return Err(VfsError::InvalidInput);
        }

        let buf = DmaBufFile::alloc(data.len as usize)?;
        // Honour O_CLOEXEC from fd_flags, matching the dma-heap UAPI (otherwise the
        // fd would leak across exec).
        let cloexec = data.fd_flags & O_CLOEXEC != 0;
        let fd = add_file_like(Arc::new(buf), cloexec)?;

        data.fd = fd as u32;
        if let Err(e) = copy_out(&data, arg) {
            // Userspace never learns this fd, so close it here; otherwise the fd
            // slot and its contiguous DMA buffer leak for the process lifetime.
            let _ = close_file_like(fd);
            return Err(e);
        }
        Ok(0)
    }
}

fn copy_in(uaddr: usize) -> VfsResult<DmaHeapAllocData> {
    UserConstPtr::<DmaHeapAllocData>::from(uaddr)
        .read()
        .map_err(|_| VfsError::InvalidData)
}

fn copy_out(src: &DmaHeapAllocData, uaddr: usize) -> VfsResult<()> {
    UserPtr::<DmaHeapAllocData>::from(uaddr)
        .write(*src)
        .map_err(|_| VfsError::InvalidData)
}

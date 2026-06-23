mod uapi;

use core::any::Any;

use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use starry_vm::VmMutPtr;

use crate::pseudofs::DeviceOps;

/// Device ID for /dev/dma_heap/system
pub const DMA_HEAP_SYSTEM_DEVICE_ID: DeviceId = DeviceId::new(252, 0);

/// DMA heap system device
pub struct DmaHeapSystem;

impl DmaHeapSystem {
    /// Creates a new DMA heap system device.
    pub fn new() -> Self {
        warn!("dma_heap: Creating new DmaHeapSystem instance");
        Self
    }
}

impl Default for DmaHeapSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for DmaHeapSystem {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        warn!("dma_heap: read_at called");
        // DMA heap devices are not meant to be read directly
        Err(VfsError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        warn!("dma_heap: write_at called");
        // DMA heap devices are not meant to be written directly
        Err(VfsError::InvalidInput)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        warn!("dma_heap: ioctl called cmd={:#x}, arg={:#x}", cmd, arg);

        // For now, return success for all ioctls and zero the first u32 if arg
        // is a user pointer, similar to the rknpu implementation.
        if arg != 0
            && let Err(e) = (arg as *mut u32).vm_write(0u32)
        {
            warn!("dma_heap: ioctl vm_write failed: {:?}", e);
            return Err(VfsError::InvalidInput);
        }
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        info!("dma_heap: as_any called - used for dynamic type checking");
        self
    }

    fn flags(&self) -> NodeFlags {
        info!("dma_heap: flags called - returning NON_CACHEABLE flag");
        NodeFlags::NON_CACHEABLE
    }
}

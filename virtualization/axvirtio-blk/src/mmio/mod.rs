mod device;
use alloc::sync::Arc;
use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};
use axvirtio_common::{VirtioError, VirtioResult};
pub use device::VirtioMmioBlockDevice;

/// VirtIO block request header structure
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VirtioBlockHeader {
    /// Request type (VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT, etc.)
    pub request_type: u32,
    /// I/O priority (currently unused)
    pub ioprio: u32,
    /// Starting sector number
    pub sector: u64,
}

impl VirtioBlockHeader {
    /// Size of the VirtIO block header in bytes
    pub const SIZE: u32 = 16; // type (4) + ioprio (4) + sector (8)

    /// Read VirtIO block header from guest memory
    pub fn read_from_guest<T>(addr: GuestPhysAddr, accessor: Arc<T>) -> VirtioResult<Self>
    where
        T: GuestMemoryAccessor,
    {
        accessor
            .read_obj(addr)
            .map_err(|_| VirtioError::InvalidAddress)
    }
}

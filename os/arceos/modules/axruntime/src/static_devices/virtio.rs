use core::{marker::PhantomData, ptr::NonNull};

use ax_alloc::{UsageKind, global_allocator};
use ax_hal::mem::{phys_to_virt, virt_to_phys};
use virtio_drivers::{
    BufferDirection, Error as VirtIoError, Hal as VirtIoHal, PhysAddr as VirtIoPhysAddr,
    transport::{DeviceType, Transport, mmio::MmioTransport},
};

pub(super) const MMIO_DEVICE_NAME: &str = "virtio-mmio";

pub(super) struct VirtIoHalImpl(PhantomData<()>);

unsafe impl VirtIoHal for VirtIoHalImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtIoPhysAddr, NonNull<u8>) {
        let vaddr = if let Ok(vaddr) = global_allocator().alloc_pages(pages, 0x1000, UsageKind::Dma)
        {
            vaddr
        } else {
            return (0, NonNull::dangling());
        };
        let paddr = virt_to_phys(vaddr.into()).as_usize() as VirtIoPhysAddr;
        let ptr = NonNull::new(vaddr as _).expect("DMA allocator returned a null address");
        (paddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: VirtIoPhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        global_allocator().dealloc_pages(vaddr.as_ptr() as usize, pages, UsageKind::Dma);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: VirtIoPhysAddr, _size: usize) -> NonNull<u8> {
        let vaddr = phys_to_virt((paddr as usize).into()).as_mut_ptr();
        NonNull::new(vaddr).expect("MMIO mapping returned a null address")
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> VirtIoPhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        virt_to_phys(vaddr.into()).as_usize() as VirtIoPhysAddr
    }

    unsafe fn unshare(_paddr: VirtIoPhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
    }
}

pub(super) fn probe_mmio_device(
    reg_base: *mut u8,
    reg_size: usize,
) -> Option<(DeviceType, MmioTransport<'static>)> {
    if reg_base.is_null() || reg_size == 0 {
        return None;
    }

    let header = NonNull::new(reg_base as *mut virtio_drivers::transport::mmio::VirtIOHeader)?;
    let transport = unsafe { MmioTransport::new(header, reg_size) }.ok()?;
    Some((transport.device_type(), transport))
}

pub(super) fn map_virtio_error(err: VirtIoError) -> &'static str {
    match err {
        VirtIoError::QueueFull => "virtio queue full",
        VirtIoError::NotReady => "virtio device not ready",
        VirtIoError::WrongToken => "virtio queue returned a wrong token",
        VirtIoError::AlreadyUsed => "virtio resource is already used",
        VirtIoError::InvalidParam => "virtio invalid parameter",
        VirtIoError::DmaError => "virtio DMA error",
        VirtIoError::IoError => "virtio I/O error",
        VirtIoError::Unsupported => "virtio operation unsupported",
        VirtIoError::ConfigSpaceTooSmall => "virtio config space too small",
        VirtIoError::ConfigSpaceMissing => "virtio config space missing",
        VirtIoError::SocketDeviceError(_) => "virtio socket device error",
    }
}

pub(super) trait VirtIoTransport: Transport + 'static {}

impl<T: Transport + 'static> VirtIoTransport for T {}

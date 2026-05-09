use core::{marker::PhantomData, ptr::NonNull};

use ax_alloc::{UsageKind, global_allocator};
use ax_driver_virtio::{BufferDirection, PhysAddr as VirtIoPhysAddr, VirtIoHal};

use crate::drivers::iomap;

pub(crate) struct VirtIoHalImpl(PhantomData<()>);

unsafe impl VirtIoHal for VirtIoHalImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (VirtIoPhysAddr, NonNull<u8>) {
        let vaddr = if let Ok(vaddr) = global_allocator().alloc_pages(pages, 0x1000, UsageKind::Dma)
        {
            vaddr
        } else {
            return (0, NonNull::dangling());
        };
        let paddr = somehal::mem::virt_to_phys(vaddr as *mut u8) as VirtIoPhysAddr;
        let ptr = NonNull::new(vaddr as _).unwrap();
        (paddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: VirtIoPhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        global_allocator().dealloc_pages(vaddr.as_ptr() as usize, pages, UsageKind::Dma);
        0
    }

    #[inline]
    unsafe fn mmio_phys_to_virt(paddr: VirtIoPhysAddr, size: usize) -> NonNull<u8> {
        iomap((paddr as usize).into(), size).expect("failed to map virtio MMIO region")
    }

    #[inline]
    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> VirtIoPhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        somehal::mem::virt_to_phys(vaddr as *mut u8) as VirtIoPhysAddr
    }

    #[inline]
    unsafe fn unshare(_paddr: VirtIoPhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
    }
}

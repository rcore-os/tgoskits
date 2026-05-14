#![cfg(target_os = "none")]
#![no_std]

extern crate alloc;

use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull, time::Duration};

use bare_test::{
    mem::{PhysAddr, VirtAddr, alloc_with_mask, page_size},
    time::spin_delay,
};
use crab_usb::*;

pub struct KernelImpl;

impl DmaOp for KernelImpl {
    fn page_size(&self) -> usize {
        page_size()
    }

    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let size = size.get();
        let orig_phys = PhysAddr::from(VirtAddr::from(addr)).raw() as u64;

        let layout = Layout::from_size_align(size, align).unwrap();

        if orig_phys + size as u64 > dma_mask || !orig_phys.is_multiple_of(align as u64) {
            // 需要重新分配内存
            let ptr = unsafe { alloc_with_mask(layout, dma_mask) };
            if ptr.is_null() {
                return Err(DmaError::NoMemory);
            }

            let new_virt = NonNull::new(ptr).unwrap();
            let new_phys = PhysAddr::from(VirtAddr::from(new_virt)).raw() as u64;

            log::debug!(
                "DMA remap: orig_virt={:#p}, orig_phys={:#x} -> new_virt={:#p}, new_phys={:#x}, size={:#x}",
                addr,
                orig_phys,
                new_virt,
                new_phys,
                size
            );

            unsafe {
                let dst = core::slice::from_raw_parts_mut(new_virt.as_ptr(), size);
                let src = core::slice::from_raw_parts(addr.as_ptr(), size);
                dst.copy_from_slice(src);

                // important: flush cache to make sure DMA can see the latest data
                self.flush_invalidate(new_virt, size);
            }

            Ok(unsafe { DmaMapHandle::new(addr, new_phys.into(), layout, Some(new_virt)) })
        } else {
            // ✅ 原始地址可以使用，直接返回
            Ok(unsafe { DmaMapHandle::new(addr, orig_phys.into(), layout, None) })
        }
    }

    unsafe fn unmap_single(&self, handle: DmaMapHandle) {
        if let Some(virt) = handle.alloc_virt() {
            // 重新分配过，需要释放新分配的内存
            unsafe {
                alloc::alloc::dealloc(virt.as_ptr(), handle.layout());
            }
        }
        // 没有重新分配，原始 buffer 由调用者管理，不需要释放
    }

    unsafe fn alloc_coherent(
        &self,
        dma_mask: u64,
        layout: core::alloc::Layout,
    ) -> Option<DmaHandle> {
        let ptr = unsafe { alloc_with_mask(layout, dma_mask) };
        let ptr = NonNull::new(ptr)?;
        let virt = VirtAddr::from(ptr);
        let phys = PhysAddr::from(virt).raw() as u64;

        Some(unsafe { DmaHandle::new(ptr, phys.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        unsafe { alloc::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) }
    }

    // unsafe fn map_single(
    //     &self,
    //     dma_mask: u64,
    //     origin_virt: NonNull<u8>,
    //     size: NonZeroUsize,
    //     align: usize,
    //     _direction: crate::DmaDirection,
    // ) -> Result<DmaHandle, DmaError> {
    //     let size = size.get();
    //     let orig_phys = PhysAddr::from(VirtAddr::from(origin_virt)).raw() as u64;

    //     let layout = Layout::from_size_align(size, align).unwrap();

    //     if orig_phys + size as u64 > dma_mask || !orig_phys.is_multiple_of(align as u64) {
    //         // 需要重新分配内存
    //         let ptr = unsafe { alloc_with_mask(layout, dma_mask) };
    //         if ptr.is_null() {
    //             return Err(DmaError::NoMemory);
    //         }

    //         let new_virt = NonNull::new(ptr).unwrap();
    //         let new_phys = PhysAddr::from(VirtAddr::from(new_virt)).raw() as u64;

    //         log::debug!(
    //             "DMA remap: orig_virt={:#x}, orig_phys={:#x} -> new_virt={:#x}, new_phys={:#x}, size={:#x}",
    //             origin_virt.as_ptr() as usize,
    //             orig_phys,
    //             new_virt.as_ptr() as usize,
    //             new_phys,
    //             size
    //         );

    //         unsafe {
    //             core::ptr::copy_nonoverlapping(origin_virt.as_ptr(), new_virt.as_ptr(), size);
    //         }

    //         Ok(DmaHandle {
    //             dma_addr: new_phys,
    //             origin_virt,
    //             alloc_virt: Some(new_virt),
    //             layout,
    //         })
    //     } else {
    //         // ✅ 原始地址可以使用，直接返回
    //         Ok(DmaHandle {
    //             dma_addr: orig_phys,
    //             origin_virt,
    //             layout,
    //             alloc_virt: None,
    //         })
    //     }
    // }

    // unsafe fn unmap_single(&self, handle: DmaHandle) {
    //     if let Some(virt) = handle.alloc_virt {
    //         // 重新分配过，需要释放新分配的内存
    //         unsafe {
    //             alloc::alloc::dealloc(virt.as_ptr(), handle.layout);
    //         }
    //     }
    //     // 没有重新分配，原始 buffer 由调用者管理，不需要释放
    // }

    // unsafe fn alloc_coherent(&self, dma_mask: u64, layout: Layout) -> Option<DmaHandle> {
    //     let ptr = unsafe { alloc_with_mask(layout, dma_mask) };
    //     let ptr = NonNull::new(ptr)?;
    //     let virt = VirtAddr::from(ptr);
    //     let phys = PhysAddr::from(virt).raw() as u64;

    //     Some(crab_usb::DmaHandle {
    //         origin_virt: ptr,
    //         dma_addr: phys,
    //         layout,
    //         alloc_virt: Some(ptr),
    //     })
    // }

    // unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
    //     unsafe { alloc::alloc::dealloc(handle.as_ptr(), handle.layout) }
    // }
}

impl KernelOp for KernelImpl {
    fn delay(&self, duration: Duration) {
        spin_delay(duration);
    }
}

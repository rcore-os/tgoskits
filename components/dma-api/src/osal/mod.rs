use core::{num::NonZeroUsize, ptr::NonNull};

use mbarrier::mb;

use crate::{DmaDirection, DmaError, DmaHandle, DmaMapHandle};

cfg_if::cfg_if! {
    if #[cfg(target_arch = "aarch64")] {
        #[path = "aarch64.rs"]
        pub mod arch;
    } else{
        #[path = "nop.rs"]
        pub mod arch;
    }
}

pub trait DmaOp: Sync + Send + 'static {
    fn page_size(&self) -> usize;

    /// 将虚拟地址映射到 DMA 地址
    ///
    /// # Safety
    /// 只能是单个连续内存块
    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError>;

    /// 解除 DMA 映射
    ///
    /// # Safety
    /// 必须与 map_single 配对使用
    unsafe fn unmap_single(&self, handle: DmaMapHandle);

    /// 写回缓存到内存 (clean)
    fn flush(&self, addr: NonNull<u8>, size: usize) {
        mb();
        arch::flush(addr, size)
    }

    /// 使缓存无效 (invalidate)
    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        arch::invalidate(addr, size);
        mb();
    }

    fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        mb();
        arch::flush_invalidate(addr, size);
        mb();
    }

    /// 分配 DMA 可访问内存
    /// # Safety
    ///
    /// - 调用者必须确保 layout 合法
    /// - 返回的内存必须保证连续
    unsafe fn alloc_coherent(
        &self,
        dma_mask: u64,
        layout: core::alloc::Layout,
    ) -> Option<DmaHandle>;

    /// 释放 DMA 内存
    /// # Safety
    /// 调用者必须确保 ptr 和 layout 与 alloc 时匹配
    unsafe fn dealloc_coherent(&self, handle: DmaHandle);

    fn prepare_read(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if matches!(
            direction,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) {
            let origin_ptr = unsafe { handle.cpu_addr.add(offset) };

            if let Some(virt) = handle.map_alloc_virt
                && virt != handle.cpu_addr
            {
                let map_ptr = unsafe { virt.add(offset) };
                self.invalidate(map_ptr, size);
                unsafe {
                    origin_ptr.copy_from_nonoverlapping(map_ptr, size);
                }
            } else {
                self.invalidate(origin_ptr, size);
            }
        }
    }

    fn confirm_write(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if matches!(
            direction,
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ) {
            let ptr = unsafe { handle.cpu_addr.add(offset) };

            if let Some(virt) = handle.map_alloc_virt
                && virt != handle.cpu_addr
            {
                unsafe {
                    let src = core::slice::from_raw_parts(ptr.as_ptr(), size);
                    let dst = core::slice::from_raw_parts_mut(virt.as_ptr().add(offset), size);

                    dst.copy_from_slice(src);
                }
            }

            self.flush(ptr, size)
        }
    }
}

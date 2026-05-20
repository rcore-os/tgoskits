//! 测试辅助工具模块
//!
//! 提供用于跟踪和验证 DMA 操作的辅助工具

use std::{
    num::NonZeroUsize,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

use dma_api::*;

/// DMA 操作记录
#[derive(Debug, Clone, PartialEq)]
pub enum DmaOperation {
    /// Flush 操作 (写回缓存到内存)
    Flush { addr: usize, size: usize },
    /// Invalidate 操作 (使缓存无效)
    Invalidate { addr: usize, size: usize },
    /// MapSingle 操作
    MapSingle {
        virt_addr: usize,
        size: usize,
        direction: DmaDirection,
    },
    /// UnmapSingle 操作
    UnmapSingle { size: usize },
    /// AllocCoherent 操作
    AllocCoherent { size: usize, align: usize },
    /// DeallocCoherent 操作
    DeallocCoherent { size: usize },
}

/// 记录所有 DMA 操作的测试辅助工具
///
/// 这个结构体实现了 `DmaOp` trait,可以拦截并记录所有 DMA 操作,
/// 用于验证地址计算、缓存同步等关键行为。
pub struct TrackingDmaOp {
    operations: Arc<Mutex<Vec<DmaOperation>>>,
    base_addr: usize,
}

impl TrackingDmaOp {
    /// 创建新的跟踪器
    ///
    /// # 参数
    ///
    /// * `base_addr` - DMA 地址的基地址,用于计算相对偏移
    pub fn new(base_addr: usize) -> Self {
        Self {
            operations: Arc::new(Mutex::new(Vec::new())),
            base_addr,
        }
    }

    /// 获取所有操作记录的副本
    pub fn get_operations(&self) -> Vec<DmaOperation> {
        self.operations.lock().unwrap().clone()
    }

    /// 清空所有操作记录
    pub fn clear(&self) {
        self.operations.lock().unwrap().clear();
    }

    /// 统计 flush 操作的次数
    pub fn count_flush(&self) -> usize {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DmaOperation::Flush { .. }))
            .count()
    }

    /// 统计 invalidate 操作的次数
    pub fn count_invalidate(&self) -> usize {
        self.operations
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DmaOperation::Invalidate { .. }))
            .count()
    }

    /// 查找指定偏移和大小的 flush 操作
    ///
    /// # 参数
    ///
    /// * `offset` - 相对于 base_addr 的偏移量
    /// * `size` - 操作大小
    pub fn find_flush_at(&self, offset: usize, size: usize) -> bool {
        let expected_addr = self.base_addr + offset;
        self.operations.lock().unwrap().iter().any(|op| {
            matches!(op, DmaOperation::Flush { addr, size: s }
                    if *addr == expected_addr && *s == size)
        })
    }

    /// 查找指定偏移和大小的 invalidate 操作
    ///
    /// # 参数
    ///
    /// * `offset` - 相对于 base_addr 的偏移量
    /// * `size` - 操作大小
    pub fn find_inv_at(&self, offset: usize, size: usize) -> bool {
        let expected_addr = self.base_addr + offset;
        self.operations.lock().unwrap().iter().any(|op| {
            matches!(op, DmaOperation::Invalidate { addr, size: s }
                    if *addr == expected_addr && *s == size)
        })
    }

    /// 获取最后一次 flush 操作的地址和大小
    pub fn last_flush(&self) -> Option<(usize, usize)> {
        self.operations.lock().unwrap().iter().rev().find_map(|op| {
            if let DmaOperation::Flush { addr, size } = op {
                Some((*addr, *size))
            } else {
                None
            }
        })
    }

    /// 获取最后一次 invalidate 操作的地址和大小
    pub fn last_invalidate(&self) -> Option<(usize, usize)> {
        self.operations.lock().unwrap().iter().rev().find_map(|op| {
            if let DmaOperation::Invalidate { addr, size } = op {
                Some((*addr, *size))
            } else {
                None
            }
        })
    }
}

// 实现 DmaOp trait
impl DmaOp for TrackingDmaOp {
    fn page_size(&self) -> usize {
        0x1000
    }

    unsafe fn map_single(
        &self,
        _dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _align: usize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::MapSingle {
                virt_addr: addr.as_ptr() as usize,
                size: size.get(),
                direction,
            });

        let layout = core::alloc::Layout::from_size_align(size.get(), 8)?;
        Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
    }

    unsafe fn unmap_single(&self, handle: DmaMapHandle) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::UnmapSingle {
                size: handle.size(),
            });
    }

    fn flush(&self, addr: NonNull<u8>, size: usize) {
        self.operations.lock().unwrap().push(DmaOperation::Flush {
            addr: addr.as_ptr() as usize,
            size,
        });
    }

    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::Invalidate {
                addr: addr.as_ptr() as usize,
                size,
            });
    }

    unsafe fn alloc_coherent(
        &self,
        _dma_mask: u64,
        layout: core::alloc::Layout,
    ) -> Option<DmaHandle> {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::AllocCoherent {
                size: layout.size(),
                align: layout.align(),
            });

        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { DmaHandle::new(NonNull::new(ptr).unwrap(), (ptr as u64).into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        self.operations
            .lock()
            .unwrap()
            .push(DmaOperation::DeallocCoherent {
                size: handle.size(),
            });
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }
}

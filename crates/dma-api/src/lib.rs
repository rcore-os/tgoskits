#![cfg_attr(not(any(windows, unix)), no_std)]
#![doc = include_str!("../README.md")]

extern crate alloc;

use core::{ops::Deref, ptr::NonNull};

use alloc::sync::Arc;

mod osal;

mod array;
mod common;
mod dbox;
mod slice;

pub use array::*;
pub use dbox::*;
pub use slice::*;

// mod stream;

// pub use stream::*;

/// DMA 传输方向
///
/// 参考 Linux `enum dma_data_direction`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum Direction {
    /// 数据从 CPU 传输到设备 (DMA_TO_DEVICE)
    ToDevice,
    /// 数据从设备传输到 CPU (DMA_FROM_DEVICE)
    FromDevice,
    /// 双向传输 (DMA_BIDIRECTIONAL)
    Bidirectional,
}

/// DMA 地址类型
pub type DmaAddr = u64;

/// 物理地址类型
pub type PhysAddr = u64;

/// DMA 错误类型
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    #[error("DMA allocation failed")]
    NoMemory,
    #[error("Invalid layout for DMA allocation")]
    LayoutError,
    #[error("DMA address {addr:#x} does not match device mask {mask:#x}")]
    DmaMaskNotMatch { addr: DmaAddr, mask: u64 },
}

impl From<core::alloc::LayoutError> for DmaError {
    fn from(_: core::alloc::LayoutError) -> Self {
        DmaError::LayoutError
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaHandle {
    pub virt_addr: NonNull<u8>,
    pub dma_addr: DmaAddr,
    pub layout: core::alloc::Layout,
}

impl DmaHandle {
    pub fn new(virt_addr: NonNull<u8>, dma_addr: DmaAddr, layout: core::alloc::Layout) -> Self {
        Self {
            virt_addr,
            dma_addr,
            layout,
        }
    }
}

unsafe impl Send for DmaHandle {}

impl Deref for DmaHandle {
    type Target = core::alloc::Layout;
    fn deref(&self) -> &Self::Target {
        &self.layout
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapHandle {
    pub virt_addr: NonNull<u8>,
    pub dma_addr: DmaAddr,
    pub size: usize,
}

/// 操作系统抽象层 trait
///
/// 用于适配不同的 OS/平台
pub trait DeviceDmaOps: Sync + Send + 'static {
    fn page_size(&self) -> usize;

    /// 获取设备支持的最大 DMA 地址掩码
    fn dma_mask(&self) -> u64 {
        u64::MAX
    }

    /// 将虚拟地址映射到 DMA 地址
    /// # Safety
    /// 只能是单个连续内存块
    unsafe fn map_single(
        &self,
        addr: NonNull<u8>,
        size: usize,
        direction: Direction,
    ) -> Result<MapHandle, DmaError>;

    /// 解除 DMA 映射
    /// # Safety
    /// 必须与 map_single 配对使用
    unsafe fn unmap_single(&self, handle: MapHandle);

    /// 写回缓存到内存 (clean)
    fn flush(&self, addr: NonNull<u8>, size: usize) {
        osal::arch::flush(addr, size)
    }

    /// 使缓存无效 (invalidate)
    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        osal::arch::invalidate(addr, size)
    }

    /// 分配 DMA 可访问内存
    /// # Safety
    ///
    /// - 调用者必须确保 layout 合法
    /// - 返回的内存必须保证连续
    unsafe fn alloc_coherent(&self, layout: core::alloc::Layout) -> Option<DmaHandle>;

    /// 释放 DMA 内存
    /// # Safety
    /// 调用者必须确保 ptr 和 layout 与 alloc 时匹配
    unsafe fn dealloc_coherent(&self, handle: DmaHandle);

    fn prepare_read(&self, ptr: NonNull<u8>, size: usize, direction: Direction) {
        if matches!(direction, Direction::FromDevice | Direction::Bidirectional) {
            self.invalidate(ptr, size);
        }
    }

    fn confirm_write(&self, ptr: NonNull<u8>, size: usize, direction: Direction) {
        if matches!(direction, Direction::ToDevice | Direction::Bidirectional) {
            self.flush(ptr, size)
        }
    }
}

#[derive(Clone)]
pub struct DeviceDma {
    inner: Arc<dyn DeviceDmaOps>,
}

impl DeviceDma {
    pub fn new(osal: impl DeviceDmaOps) -> Self {
        Self {
            inner: Arc::new(osal),
        }
    }

    pub fn new_array<T>(
        &self,
        size: usize,
        align: usize,
        direction: Direction,
    ) -> Result<array::DArray<T>, DmaError> {
        array::DArray::new_zero(&self.inner, size, align, direction)
    }

    pub fn new_box<T>(
        &self,
        align: usize,
        direction: Direction,
    ) -> Result<dbox::DBox<T>, DmaError> {
        dbox::DBox::new_zero(&self.inner, align, direction)
    }

    pub fn map_single<'a, T>(
        &self,
        s: &'a [T],
        direction: Direction,
    ) -> Result<DSliceSingle<'a, T>, DmaError> {
        DSliceSingle::new(&self.inner, s, direction)
    }

    pub fn map_single_mut<'a, T>(
        &self,
        s: &'a mut [T],
        direction: Direction,
    ) -> Result<DSliceSingleMut<'a, T>, DmaError> {
        DSliceSingleMut::new(&self.inner, s, direction)
    }
}

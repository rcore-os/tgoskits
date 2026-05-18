#![cfg_attr(target_os = "none", no_std)]
#![doc = include_str!("../README.md")]

extern crate alloc;

use core::{num::NonZeroUsize, ops::Deref, ptr::NonNull};

mod osal;

mod array;
mod common;
mod dbox;
mod def;
mod map_single;
mod pool;

pub use array::*;
pub use dbox::*;
pub use def::*;
pub use map_single::*;
pub use osal::DmaOp;
pub use pool::*;

impl Deref for DmaHandle {
    type Target = core::alloc::Layout;
    fn deref(&self) -> &Self::Target {
        &self.layout
    }
}

/// DMA 设备操作接口。
///
/// `DeviceDma` 是用于执行 DMA 操作的主要入口点，封装了平台特定的
/// `DmaOp` 实现，并提供了分配、映射和管理 DMA 内存的方法。
///
/// # 创建
///
/// 使用 [`DeviceDma::new()`] 创建实例，需要提供：
/// - `dma_mask`: 设备可寻址的地址掩码（如 `0xFFFFFFFF` 表示 32 位设备）
/// - `osal`: 实现 `DmaOp` trait 的平台抽象层
///
/// # 示例
///
/// ```rust,ignore
/// use dma_api::DeviceDma;
///
/// let device = DeviceDma::new(0xFFFFFFFF, &my_dma_impl);
/// ```
#[derive(Clone)]
pub struct DeviceDma {
    os: &'static dyn DmaOp,
    mask: u64,
}

impl DeviceDma {
    /// 创建新的 DMA 设备实例。
    ///
    /// # 参数
    ///
    /// - `dma_mask`: 设备 DMA 地址掩码，指定设备可寻址的地址范围
    ///   - `0xFFFFFFFF`: 32 位设备（最多 4GB）
    ///   - `0xFFFFFFFFFFFFFFFF`: 64 位设备（全地址空间）
    /// - `osal`: 实现 `DmaOp` trait 的平台抽象层引用
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// use dma_api::DeviceDma;
    ///
    /// let device = DeviceDma::new(0xFFFFFFFF, &my_dma_impl);
    /// ```
    pub fn new(dma_mask: u64, osal: &'static dyn DmaOp) -> Self {
        Self {
            mask: dma_mask,
            os: osal,
        }
    }

    /// 获取设备的 DMA 地址掩码。
    ///
    /// # 返回
    ///
    /// 返回设备的 DMA 掩码值，表示设备可寻址的最大地址范围。
    pub fn dma_mask(&self) -> u64 {
        self.mask
    }

    /// 刷新 CPU 缓存到内存（clean 操作）。
    ///
    /// 将指定地址范围的 CPU 缓存数据写回到内存，确保设备可以读取到最新数据。
    /// 用于 `ToDevice` 和 `Bidirectional` 方向的 DMA 传输前。
    ///
    /// # 参数
    ///
    /// - `addr`: 内存起始地址
    /// - `size`: 内存大小（字节）
    pub fn flush(&self, addr: NonNull<u8>, size: usize) {
        self.os.flush(addr, size)
    }

    /// 使 CPU 缓存失效（invalidate 操作）。
    ///
    /// 使指定地址范围的 CPU 缓存失效，强制 CPU 从内存重新读取数据。
    /// 用于 `FromDevice` 和 `Bidirectional` 方向的 DMA 传输后。
    ///
    /// # 参数
    ///
    /// - `addr`: 内存起始地址
    /// - `size`: 内存大小（字节）
    pub fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.os.invalidate(addr, size)
    }

    /// 刷新并使 CPU 缓存失效（clean and invalidate 操作）。
    ///
    /// 同时执行刷新和失效操作，用于确保缓存和内存完全同步。
    ///
    /// # 参数
    ///
    /// - `addr`: 内存起始地址
    /// - `size`: 内存大小（字节）
    pub fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.os.flush_invalidate(addr, size)
    }

    /// 获取系统页大小。
    ///
    /// # 返回
    ///
    /// 返回系统的页大小（字节），通常为 4096。
    pub fn page_size(&self) -> usize {
        self.os.page_size()
    }

    fn prepare_read(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.os.prepare_read(handle, offset, size, direction)
    }

    fn confirm_write(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.os.confirm_write(handle, offset, size, direction)
    }

    unsafe fn alloc_coherent(&self, layout: core::alloc::Layout) -> Result<DmaHandle, DmaError> {
        let res = unsafe { self.os.alloc_coherent(self.mask, layout) }.ok_or(DmaError::NoMemory)?;
        match self.check_handle(&res) {
            Ok(()) => Ok(res),
            Err(e) => {
                unsafe { self.dealloc_coherent(res) };
                Err(e)
            }
        }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        unsafe { self.os.dealloc_coherent(handle) }
    }

    fn check_handle(&self, handle: &DmaHandle) -> Result<(), DmaError> {
        let addr: u64 = handle.dma_addr.into();

        let in_mask = if handle.size() == 0 {
            addr <= self.dma_mask()
        } else {
            addr.checked_add(handle.size().saturating_sub(1) as u64)
                .map(|end| end <= self.dma_mask())
                .unwrap_or(false)
        };

        if !in_mask {
            return Err(DmaError::DmaMaskNotMatch {
                addr: handle.dma_addr,
                mask: self.dma_mask(),
            });
        }

        let is_aligned = handle
            .dma_addr
            .as_u64()
            .is_multiple_of(handle.align() as u64);
        if !is_aligned {
            return Err(DmaError::AlignMismatch {
                address: handle.dma_addr,
                required: handle.align(),
            });
        }

        Ok(())
    }

    unsafe fn _map_single(
        &self,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let res = unsafe { self.os.map_single(self.mask, addr, size, align, direction) }?;
        match self.check_handle(&res) {
            Ok(()) => Ok(res),
            Err(e) => {
                unsafe { self.unmap_single(res) };
                Err(e)
            }
        }
    }

    unsafe fn unmap_single(&self, handle: DmaMapHandle) {
        unsafe { self.os.unmap_single(handle) }
    }

    /// 创建默认对齐的 DMA 数组。
    ///
    /// 分配一个指定大小的 DMA 可访问数组，内存初始化为零。
    /// 数组的对齐方式使用类型 `T` 的默认对齐值。
    ///
    /// # 类型参数
    ///
    /// - `T`: 数组元素类型，必须是 `Sized` 并且实现了 `Default`
    ///
    /// # 参数
    ///
    /// - `size`: 数组长度（元素个数）
    /// - `direction`: DMA 传输方向，决定缓存同步策略
    ///
    /// # 返回
    ///
    /// 成功时返回 `DArray<T>` 容器，失败时返回 `DmaError`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let dma_array = device.array_zero::<u32>(100, DmaDirection::FromDevice)?;
    /// ```
    pub fn array_zero<T>(
        &self,
        size: usize,
        direction: DmaDirection,
    ) -> Result<array::DArray<T>, DmaError> {
        array::DArray::new_zero(self, size, direction)
    }

    /// 创建指定对齐的 DMA 数组。
    ///
    /// 分配一个指定大小和对齐要求的 DMA 可访问数组，内存初始化为零。
    ///
    /// # 类型参数
    ///
    /// - `T`: 数组元素类型，必须是 `Sized` 并且实现了 `Default`
    ///
    /// # 参数
    ///
    /// - `size`: 数组长度（元素个数）
    /// - `align`: 对齐字节数（至少等于 `core::mem::align_of::<T>()`）
    /// - `direction`: DMA 传输方向，决定缓存同步策略
    ///
    /// # 返回
    ///
    /// 成功时返回 `DArray<T>` 容器，失败时返回 `DmaError`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// // 创建 64 字节对齐的数组
    /// let dma_array = device
    ///     .array_zero_with_align::<u32>(100, 64, DmaDirection::FromDevice)?;
    /// ```
    pub fn array_zero_with_align<T>(
        &self,
        size: usize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<array::DArray<T>, DmaError> {
        array::DArray::new_zero_with_align(self, size, align, direction)
    }

    /// 创建默认对齐的 DMA Box。
    ///
    /// 分配一个 DMA 可访问的单值容器，内存初始化为零。
    /// 适合存储 DMA 描述符、配置结构等单个对象。
    ///
    /// # 类型参数
    ///
    /// - `T`: 存储的值类型，必须是 `Sized` 并且实现了 `Default`
    ///
    /// # 参数
    ///
    /// - `direction`: DMA 传输方向，决定缓存同步策略
    ///
    /// # 返回
    ///
    /// 成功时返回 `DBox<T>` 容器，失败时返回 `DmaError`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// #[derive(Default)]
    /// struct Descriptor {
    ///     addr: u64,
    ///     length: u32,
    /// }
    ///
    /// let dma_desc = device.box_zero::<Descriptor>(DmaDirection::ToDevice)?;
    /// ```
    pub fn box_zero<T>(&self, direction: DmaDirection) -> Result<dbox::DBox<T>, DmaError> {
        dbox::DBox::new_zero(self, direction)
    }

    /// 创建指定对齐的 DMA Box。
    ///
    /// 分配一个指定对齐要求的 DMA 可访问单值容器，内存初始化为零。
    ///
    /// # 类型参数
    ///
    /// - `T`: 存储的值类型，必须是 `Sized` 并且实现了 `Default`
    ///
    /// # 参数
    ///
    /// - `align`: 对齐字节数（至少等于 `core::mem::align_of::<T>()`）
    /// - `direction`: DMA 传输方向，决定缓存同步策略
    ///
    /// # 返回
    ///
    /// 成功时返回 `DBox<T>` 容器，失败时返回 `DmaError`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let dma_desc = device
    ///     .box_zero_with_align::<Descriptor>(64, DmaDirection::ToDevice)?;
    /// ```
    pub fn box_zero_with_align<T>(
        &self,
        align: usize,
        direction: DmaDirection,
    ) -> Result<dbox::DBox<T>, DmaError> {
        dbox::DBox::new_zero_with_align(self, align, direction)
    }

    /// 映射现有缓冲区为 DMA 可访问。
    ///
    /// 将已存在的缓冲区（如栈数组或堆分配的 slice）映射为 DMA 可访问区域。
    /// 返回的 `SArrayPtr` 在离开作用域时自动解除映射。
    ///
    /// # 缓存同步
    ///
    /// **重要**: 此方法创建的映射**不会**自动同步缓存。
    /// 你必须手动调用 `SArrayPtr` 的方法进行缓存同步：
    /// - `to_vec()`: 读取前自动失效整个范围
    /// - `copy_from_slice()`: 写入后自动刷新整个范围
    ///
    /// # 类型参数
    ///
    /// - `T`: 数组元素类型
    ///
    /// # 参数
    ///
    /// - `buff`: 要映射的缓冲区切片
    /// - `align`: 对齐字节数
    /// - `direction`: DMA 传输方向，决定手动缓存同步的行为
    ///
    /// # 返回
    ///
    /// 成功时返回 `SArrayPtr<T>` 映射句柄，失败时返回 `DmaError`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let mut buffer = [0u8; 4096];
    ///
    /// // 映射用于 DMA 写入
    /// let mapping = device.map_single_array(&buffer, 64, DmaDirection::ToDevice)?;
    ///
    /// // 必须手动刷新缓存
    /// mapping.copy_from_slice(&data);
    ///
    /// // ... 启动 DMA 传输 ...
    ///
    /// // 映射在作用域结束时自动解映射
    /// ```
    pub fn map_single_array<T>(
        &self,
        buff: &[T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<SArrayPtr<T>, DmaError> {
        SArrayPtr::map_single(self, buff, align, direction)
    }

    pub fn new_pool(
        &self,
        layout: core::alloc::Layout,
        direction: DmaDirection,
        cap: usize,
    ) -> DArrayPool {
        let config = DArrayConfig {
            size: layout.size(),
            align: layout.align(),
            direction,
        };
        DArrayPool::new_pool(self.clone(), config, cap)
    }
}

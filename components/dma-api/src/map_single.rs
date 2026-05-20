use alloc::vec::Vec;
use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{DeviceDma, DmaDirection, DmaError, DmaMapHandle};

/// 映射单个连续内存区域的 DMA 数组。
///
/// `SArrayPtr<T>` 将现有的缓冲区（如栈数组或堆分配的 slice）映射为 DMA 可访问区域。
/// 与 `DArray<T>` 不同，此类型提供手动缓存同步控制。
///
/// # 缓存同步
///
/// **重要**: `SArrayPtr` **不会**在每次访问时自动同步缓存。
/// 你必须使用特定方法进行缓存同步：
/// - `to_vec()`: 读取前自动失效整个范围
/// - `copy_from_slice()`: 写入后自动刷新整个范围
/// - `read()`/`set()`: **不**执行自动缓存同步
///
/// # 生命周期
///
/// `SArrayPtr` 在离开作用域时自动解除 DMA 映射，但**不会**自动同步缓存。
/// 必须在映射解除前手动完成必要的缓存同步操作。
///
/// # 类型参数
///
/// - `T`: 数组元素类型
///
/// # 示例
///
/// ```rust,ignore
/// use dma_api::{DeviceDma, DmaDirection};
///
/// let mut buffer = [0u8; 4096];
/// let device = DeviceDma::new(0xFFFFFFFF, &my_dma_impl);
///
/// // 映射用于 DMA 写入
/// let mut mapping = device.map_single_array(&buffer, 64, DmaDirection::ToDevice)?;
///
/// // 必须手动刷新缓存
/// mapping.copy_from_slice(&data);
///
/// // ... 启动 DMA 传输 ...
///
/// // 映射在作用域结束时自动解映射
/// ```
pub struct SArrayPtr<T> {
    handle: DmaMapHandle,
    osal: DeviceDma,
    pub direction: DmaDirection,
    _marker: core::marker::PhantomData<*mut T>,
}

impl<T> SArrayPtr<T> {
    /// Create a new SArrayPtr from a raw pointer and size.
    pub(crate) fn map_single(
        os: &DeviceDma,
        buff: &[T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let addr = NonNull::new(buff.as_ptr() as *mut u8).ok_or(DmaError::NullPointer)?;
        let size =
            NonZeroUsize::new(core::mem::size_of_val(buff)).ok_or(DmaError::ZeroSizedBuffer)?;
        let handle = unsafe { os._map_single(addr, size, align, direction)? };

        Ok(Self {
            handle,
            osal: os.clone(),
            direction,
            _marker: core::marker::PhantomData,
        })
    }

    /// 从 slice 复制数据到映射的缓冲区。
    ///
    /// 复制完成后刷新整个缓冲区的 CPU 缓存（ToDevice/Bidirectional）。
    /// 这是手动同步缓存的主要方式之一。
    ///
    /// # 参数
    ///
    /// - `src`: 源 slice
    ///
    /// # Panics
    ///
    /// 如果源 slice 大于 DMA 缓冲区大小则 panic
    pub fn copy_from_slice(&mut self, src: &[T]) {
        assert!(
            core::mem::size_of_val(src) <= self.handle.size(),
            "Source slice is larger than DMA buffer"
        );
        unsafe {
            let dest_ptr = self.handle.cpu_addr.cast::<T>();
            dest_ptr
                .as_ptr()
                .copy_from_nonoverlapping(src.as_ptr(), src.len());
        }
        self.osal
            .confirm_write(&self.handle, 0, self.handle.size(), self.direction);
    }

    /// 获取 DMA 地址。
    ///
    /// 返回设备用于访问此 DMA 缓冲区的物理/DMA 地址。
    ///
    /// # 返回
    ///
    /// DMA 地址
    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.handle.dma_addr
    }

    /// 获取数组长度（元素个数）。
    ///
    /// # 返回
    ///
    /// 数组中的元素个数
    pub fn len(&self) -> usize {
        self.handle.size() / core::mem::size_of::<T>()
    }

    /// 检查数组是否为空。
    ///
    /// # 返回
    ///
    /// 如果数组长度为 0 返回 `true`，否则返回 `false`
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 读取指定索引的元素（不自动同步缓存）。
    ///
    /// **注意**: 此方法**不会**自动同步缓存。
    /// 如果需要缓存同步，请使用 `to_vec()` 方法。
    ///
    /// # 参数
    ///
    /// - `index`: 元素索引
    ///
    /// # 返回
    ///
    /// 如果索引有效返回 `Some(T)`，否则返回 `None`
    pub fn read(&self, index: usize) -> Option<T> {
        if index >= self.len() {
            return None;
        }

        unsafe {
            let offset = index * core::mem::size_of::<T>();
            self.osal.prepare_read(
                &self.handle,
                offset,
                core::mem::size_of::<T>(),
                self.direction,
            );
            let ptr = self.handle.cpu_addr.cast::<T>().add(index);
            Some(ptr.read())
        }
    }

    /// 设置指定索引的元素值（不自动同步缓存）。
    ///
    /// **注意**: 此方法**不会**自动同步缓存。
    /// 写入后请使用 `copy_from_slice()` 或手动刷新缓存。
    ///
    /// # 参数
    ///
    /// - `index`: 元素索引
    /// - `value`: 要写入的值
    ///
    /// # Panics
    ///
    /// 如果 `index >= self.len()` 则 panic
    pub fn set(&mut self, index: usize, value: T) {
        assert!(
            index < self.len(),
            "index out of range, index: {},len: {}",
            index,
            self.len()
        );

        unsafe {
            let ptr = self.handle.cpu_addr.cast::<T>().add(index);
            ptr.write(value);
        }

        self.osal.confirm_write(
            &self.handle,
            index * core::mem::size_of::<T>(),
            core::mem::size_of::<T>(),
            self.direction,
        );
    }

    /// 将整个数组转换为 Vec（自动同步缓存）。
    ///
    /// 这是读取映射数据并同步缓存的主要方式。
    /// 读取前会自动使 CPU 缓存失效（FromDevice/Bidirectional）。
    ///
    /// # 返回
    ///
    /// 包含所有元素的 Vec
    pub fn to_vec(&self) -> Vec<T> {
        let mut vec: Vec<T> = Vec::with_capacity(self.len());
        self.osal
            .prepare_read(&self.handle, 0, self.handle.size(), self.direction);
        unsafe {
            let src_ptr = self.handle.cpu_addr.as_ptr().cast::<T>();
            let dst_ptr = vec.as_mut_ptr();
            dst_ptr.copy_from_nonoverlapping(src_ptr, self.len());
            vec.set_len(self.len());
        }
        vec
    }

    pub fn prepare_read_all(&self) {
        self.osal
            .prepare_read(&self.handle, 0, self.handle.size(), self.direction);
    }

    pub fn confirm_write_all(&self) {
        self.osal
            .confirm_write(&self.handle, 0, self.handle.size(), self.direction);
    }
}

impl<T> Drop for SArrayPtr<T> {
    fn drop(&mut self) {
        unsafe {
            self.osal.unmap_single(self.handle);
        }
    }
}

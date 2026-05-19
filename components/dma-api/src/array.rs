use core::{alloc::Layout, ptr::NonNull};

use crate::{DeviceDma, DmaDirection, DmaError, common::DCommon};

/// DMA 可访问的数组容器。
///
/// `DArray<T>` 提供固定大小的 DMA 可访问数组，支持自动缓存同步。
/// 每次访问元素（`read`/`set`）时都会根据 DMA 方向自动处理缓存操作。
///
/// # 类型参数
///
/// - `T`: 数组元素类型
///
/// # 缓存同步
///
/// 缓存同步在每次元素访问时自动执行：
/// - `read(index)`: 读取前使 CPU 缓存失效（FromDevice/Bidirectional）
/// - `set(index, value)`: 写入后刷新 CPU 缓存（ToDevice/Bidirectional）
/// - `copy_from_slice(slice)`: 写入后刷新整个范围
///
/// # 示例
///
/// ```rust,ignore
/// use dma_api::{DeviceDma, DmaDirection};
///
/// let device = DeviceDma::new(0xFFFFFFFF, &my_dma_impl);
///
/// // 创建 100 个 u32 的 DMA 数组
/// let mut dma_array = device
///     .array_zero_with_align::<u32>(100, 64, DmaDirection::FromDevice)
///     .expect("Failed to allocate");
///
/// dma_array.set(0, 0x12345678);  // 写入（自动刷新缓存）
/// let value = dma_array.read(0);  // 读取（自动失效缓存）
///
/// let dma_addr = dma_array.dma_addr(); // 获取 DMA 地址给硬件
/// ```
///
/// # 生命周期
///
/// `DArray` 拥有其分配的 DMA 内存，在离开作用域时自动释放。
pub struct DArray<T> {
    data: DCommon,
    _phantom: core::marker::PhantomData<T>,
}

unsafe impl<T> Send for DArray<T> where T: Send {}

impl<T> DArray<T> {
    pub(crate) fn new_zero_with_align(
        os: &DeviceDma,
        size: usize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let layout = Layout::from_size_align(
            size * core::mem::size_of::<T>(),
            align.max(core::mem::align_of::<T>()),
        )?;
        let data = DCommon::new_zero(os, layout, direction)?;
        Ok(Self {
            data,
            _phantom: core::marker::PhantomData,
        })
    }

    pub(crate) fn new_zero(
        os: &DeviceDma,
        size: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        Self::new_zero_with_align(os, size, core::mem::align_of::<T>(), direction)
    }

    /// 获取 DMA 地址。
    ///
    /// 返回设备用于访问此 DMA 缓冲区的物理/DMA 地址。
    /// 将此地址传递给硬件设备以配置 DMA 操作。
    ///
    /// # 返回
    ///
    /// DMA 地址
    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.data.handle.dma_addr
    }

    /// 获取数组长度（元素个数）。
    ///
    /// # 返回
    ///
    /// 数组中的元素个数
    pub fn len(&self) -> usize {
        self.data.handle.size() / core::mem::size_of::<T>()
    }

    /// 检查数组是否为空。
    ///
    /// # 返回
    ///
    /// 如果数组长度为 0 返回 `true`，否则返回 `false`
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 获取数组的字节长度。
    ///
    /// # 返回
    ///
    /// 数组占用的总字节数
    pub fn bytes_len(&self) -> usize {
        self.data.handle.size()
    }

    /// 读取指定索引的元素。
    ///
    /// 根据 DMA 方向自动处理缓存同步：
    /// - `FromDevice`/`Bidirectional`: 读取前使 CPU 缓存失效
    /// - `ToDevice`: 无缓存操作
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
            self.data.prepare_read(offset, core::mem::size_of::<T>());
            Some(self.data.handle.cpu_addr.cast().add(index).read())
        }
    }

    /// 设置指定索引的元素值。
    ///
    /// 根据 DMA 方向自动处理缓存同步：
    /// - `ToDevice`/`Bidirectional`: 写入后刷新 CPU 缓存
    /// - `FromDevice`: 无缓存操作
    ///
    /// # 参数
    ///
    /// - `index`: 元素索引，必须在范围内
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
            let offset = index * core::mem::size_of::<T>();
            let ptr = self.data.handle.cpu_addr.cast::<T>().add(index);
            ptr.write(value);
            self.data.confirm_write(offset, core::mem::size_of::<T>());
        }
    }

    /// 创建迭代器。
    ///
    /// 返回一个迭代器，按顺序读取数组元素。
    /// 每次读取都会自动处理缓存同步。
    ///
    /// # 返回
    ///
    /// `DArrayIter` 迭代器
    pub fn iter(&self) -> DArrayIter<'_, T> {
        DArrayIter {
            array: self,
            index: 0,
        }
    }

    /// 从 slice 复制数据到数组。
    ///
    /// 复制完成后刷新整个数组的 CPU 缓存（ToDevice/Bidirectional）。
    ///
    /// # 参数
    ///
    /// - `src`: 源 slice，长度必须 `<= self.len()`
    ///
    /// # Panics
    ///
    /// 如果 `src.len() > self.len()` 则 panic
    pub fn copy_from_slice(&mut self, src: &[T]) {
        assert!(
            src.len() <= self.len(),
            "source slice is larger than DArray, src len: {}, DArray len: {}",
            src.len(),
            self.len()
        );
        unsafe {
            let dst_ptr = self.data.handle.cpu_addr.as_ptr();
            let len = core::mem::size_of_val(src);
            dst_ptr.copy_from_nonoverlapping(src.as_ptr() as *const u8, len);
        }
        self.data.confirm_write_all();
    }

    /// 在 CPU 读取前同步指定字节范围。
    ///
    /// 对 `FromDevice` 和 `Bidirectional` 方向，这会使对应缓存范围失效。
    pub fn prepare_read(&self, offset: usize, size: usize) {
        assert!(
            offset <= self.bytes_len() && size <= self.bytes_len().saturating_sub(offset),
            "range out of bounds, offset: {}, size: {}, bytes_len: {}",
            offset,
            size,
            self.bytes_len()
        );
        self.data.prepare_read(offset, size);
    }

    /// 在设备读取前同步指定字节范围。
    ///
    /// 对 `ToDevice` 和 `Bidirectional` 方向，这会将对应缓存范围刷回内存。
    pub fn confirm_write(&self, offset: usize, size: usize) {
        assert!(
            offset <= self.bytes_len() && size <= self.bytes_len().saturating_sub(offset),
            "range out of bounds, offset: {}, size: {}, bytes_len: {}",
            offset,
            size,
            self.bytes_len()
        );
        self.data.confirm_write(offset, size);
    }

    /// 在 CPU 读取前同步整个数组。
    pub fn prepare_read_all(&self) {
        self.data.prepare_read(0, self.bytes_len());
    }

    /// 在设备读取前同步整个数组。
    pub fn confirm_write_all(&self) {
        self.data.confirm_write_all();
    }

    /// 直接借出一段可写切片，并在闭包返回后自动同步缓存。
    pub fn write_with<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        assert!(
            len <= self.len(),
            "range out of bounds, len: {}, array len: {}",
            len,
            self.len()
        );
        let ret = {
            let data = unsafe { self.as_mut_slice() };
            f(&mut data[..len])
        };
        self.confirm_write(0, len * core::mem::size_of::<T>());
        ret
    }

    /// 直接借出一段只读切片，并在闭包调用前自动同步缓存。
    pub fn read_with<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        assert!(
            len <= self.len(),
            "range out of bounds, len: {}, array len: {}",
            len,
            self.len()
        );
        self.prepare_read(0, len * core::mem::size_of::<T>());
        let data = unsafe { core::slice::from_raw_parts(self.as_ptr().as_ptr(), len) };
        f(data)
    }

    /// # Safety
    ///
    /// slice will not auto do cache sync operations.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [T] {
        let ptr = self.data.handle.cpu_addr;
        unsafe {
            core::slice::from_raw_parts_mut(
                ptr.as_ptr() as *mut T,
                self.bytes_len() / core::mem::size_of::<T>(),
            )
        }
    }

    pub fn as_ptr(&self) -> NonNull<T> {
        self.data.handle.as_ptr().cast::<T>()
    }
}

pub struct DArrayIter<'a, T> {
    array: &'a DArray<T>,
    index: usize,
}

impl<'a, T> Iterator for DArrayIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.array.len() {
            return None;
        }
        let value = self.array.read(self.index);
        self.index += 1;
        value
    }
}

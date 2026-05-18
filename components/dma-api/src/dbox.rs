use core::ptr::NonNull;

use crate::{DeviceDma, DmaAddr, DmaDirection, DmaError, common::DCommon};

/// DMA 可访问的单值容器。
///
/// `DBox<T>` 提供单个值的 DMA 可访问存储，支持自动缓存同步。
/// 每次访问值（`read`/`write`/`modify`）时都会根据 DMA 方向自动处理缓存操作。
///
/// # 类型参数
///
/// - `T`: 存储的值类型
///
/// # 缓存同步
///
/// 缓存同步在每次访问时自动执行：
/// - `read()`: 读取前使 CPU 缓存失效（FromDevice/Bidirectional）
/// - `write(value)`: 写入后刷新 CPU 缓存（ToDevice/Bidirectional）
/// - `modify(f)`: 先失效缓存，执行闭包，再刷新缓存
///
/// # 示例
///
/// ```rust,ignore
/// use dma_api::{DeviceDma, DmaDirection};
///
/// #[derive(Default)]
/// struct Descriptor {
///     addr: u64,
///     length: u32,
/// }
///
/// let device = DeviceDma::new(0xFFFFFFFF, &my_dma_impl);
///
/// // 分配描述符
/// let mut dma_desc = device
///     .box_zero_with_align::<Descriptor>(64, DmaDirection::ToDevice)
///     .expect("Failed to allocate");
///
/// // 配置描述符（自动刷新缓存）
/// dma_desc.modify(|d| d.length = 4096);
///
/// // 获取 DMA 地址给硬件
/// let desc_addr = dma_desc.dma_addr();
/// ```
///
/// # 生命周期
///
/// `DBox` 拥有其分配的 DMA 内存，在离开作用域时自动释放。
pub struct DBox<T> {
    data: DCommon,
    _marker: core::marker::PhantomData<T>,
}

unsafe impl<T> Send for DBox<T> where T: Send {}

impl<T> DBox<T> {
    pub(crate) fn new_zero(os: &DeviceDma, direction: DmaDirection) -> Result<Self, DmaError> {
        let layout = core::alloc::Layout::from_size_align(
            core::mem::size_of::<T>(),
            core::mem::align_of::<T>(),
        )?;
        let data = DCommon::new_zero(os, layout, direction)?;
        Ok(Self {
            data,
            _marker: core::marker::PhantomData,
        })
    }

    pub(crate) fn new_zero_with_align(
        os: &DeviceDma,
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let layout = core::alloc::Layout::from_size_align(
            core::mem::size_of::<T>(),
            align.max(core::mem::align_of::<T>()),
        )?;
        let data = DCommon::new_zero(os, layout, direction)?;
        Ok(Self {
            data,
            _marker: core::marker::PhantomData,
        })
    }

    /// 获取 DMA 地址。
    ///
    /// 返回设备用于访问此 DMA 缓冲区的物理/DMA 地址。
    /// 将此地址传递给硬件设备以配置 DMA 操作。
    ///
    /// # 返回
    ///
    /// DMA 地址
    pub fn dma_addr(&self) -> DmaAddr {
        self.data.handle.dma_addr
    }

    /// 读取存储的值。
    ///
    /// 根据 DMA 方向自动处理缓存同步：
    /// - `FromDevice`/`Bidirectional`: 读取前使 CPU 缓存失效
    /// - `ToDevice`: 无缓存操作
    ///
    /// # 返回
    ///
    /// 存储的值
    pub fn read(&self) -> T {
        unsafe {
            self.data.prepare_read(0, core::mem::size_of::<T>());
            let ptr = self.data.handle.cpu_addr.cast::<T>();
            ptr.read()
        }
    }

    /// 写入新值。
    ///
    /// 根据 DMA 方向自动处理缓存同步：
    /// - `ToDevice`/`Bidirectional`: 写入后刷新 CPU 缓存
    /// - `FromDevice`: 无缓存操作
    ///
    /// # 参数
    ///
    /// - `value`: 要写入的值
    pub fn write(&mut self, value: T) {
        unsafe {
            let ptr = self.data.handle.cpu_addr.cast::<T>();
            ptr.write(value);
            self.data.confirm_write(0, core::mem::size_of::<T>());
        }
    }

    /// 修改值（read-modify-write 模式）。
    ///
    /// 此方法等价于先调用 `read()`，然后对值执行闭包，最后调用 `write()`。
    /// 缓存同步操作：读取前失效缓存，写入后刷新缓存。
    ///
    /// # 参数
    ///
    /// - `f`: 修改值的闭包
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// dma_box.modify(|v| v.field += 10);
    /// ```
    pub fn modify(&mut self, f: impl FnOnce(&mut T)) {
        let mut value = self.read();
        f(&mut value);
        self.write(value);
    }

    /// 获取指向存储值的指针。
    ///
    /// # 返回
    ///
    /// 指向存储值的非空指针
    pub fn as_ptr(&self) -> NonNull<T> {
        self.data.handle.as_ptr().cast::<T>()
    }

    /// 获取底层缓冲区的可变切片。
    ///
    /// # Safety
    ///
    /// - 调用者必须确保在使用该切片期间，设备不会访问此内存区域
    /// - 调用者必须手动处理缓存同步（flush/invalidate）
    pub unsafe fn as_buff_mut(&mut self) -> &mut [u8] {
        self.data.as_mut_slice()
    }
}

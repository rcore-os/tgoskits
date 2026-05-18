use core::{alloc::Layout, cmp::PartialOrd, ops::Deref, ptr::NonNull};

use derive_more::{
    Add, AddAssign, Debug, Display, Div, From, Into, Mul, MulAssign, Sub, SubAssign,
};

#[derive(
    Debug,
    Display,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Hash,
    From,
    Into,
    Add,
    AddAssign,
    Mul,
    MulAssign,
    Sub,
    SubAssign,
    Div,
)]
#[debug("{}", format_args!("{_0:#X}"))]
#[display("{}", format_args!("{_0:#X}"))]
pub struct DmaAddr(u64);

impl DmaAddr {
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn checked_add(&self, rhs: u64) -> Option<Self> {
        self.0.checked_add(rhs).map(DmaAddr)
    }
}

impl PartialEq<u64> for DmaAddr {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialOrd<u64> for DmaAddr {
    fn partial_cmp(&self, other: &u64) -> Option<core::cmp::Ordering> {
        self.0.partial_cmp(other)
    }
}

/// 物理地址类型
#[derive(Debug, Display, Clone, Copy, PartialEq, Eq, Hash, From, Into, Add, Mul, Sub)]
#[debug("{}", format_args!("{_0:#X}"))]
#[display("{}", format_args!("{_0:#X}"))]
pub struct PhysAddr(u64);

impl PhysAddr {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// DMA 传输方向
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmaDirection {
    /// 数据从 CPU 传输到设备 (DMA_TO_DEVICE)
    ToDevice,
    /// 数据从设备传输到 CPU (DMA_FROM_DEVICE)
    FromDevice,
    /// 双向传输 (DMA_BIDIRECTIONAL)
    Bidirectional,
}

/// DMA 错误类型
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum DmaError {
    #[error("DMA allocation failed")]
    NoMemory,
    #[error("Invalid layout")]
    LayoutError(#[from] core::alloc::LayoutError),
    #[error("DMA address {addr} does not match device mask {mask:#X}")]
    DmaMaskNotMatch { addr: DmaAddr, mask: u64 },
    #[error("DMA align mismatch: required={required:#X}, but address={address}")]
    AlignMismatch { required: usize, address: DmaAddr },
    #[error("Null pointer provided for DMA mapping")]
    NullPointer,
    #[error("Zero-sized buffer cannot be used for DMA")]
    ZeroSizedBuffer,
}

/// Handle for DMA memory allocation.
///
/// Manages DMA memory buffers that may require special alignment or DMA address mask
/// constraints. When the original virtual address doesn't meet alignment or mask
/// requirements, an additional aligned buffer is allocated and stored in `alloc_virt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaHandle {
    /// Original virtual address provided by the user
    pub(crate) cpu_addr: NonNull<u8>,
    /// DMA address visible to devices
    pub(crate) dma_addr: DmaAddr,
    /// Memory layout specification (size and alignment)
    pub(crate) layout: Layout,
    // /// Additional allocated virtual address if the original doesn't satisfy
    // /// alignment or DMA mask requirements when mapping for DMA.
    // pub(crate) map_alloc_virt: Option<NonNull<u8>>,
}

impl DmaHandle {
    /// 为 `alloc_coherent` 操作创建 `DmaHandle`。
    ///
    /// 此构造函数专门用于 DMA 一致性内存分配场景，其中：
    /// - 内存是专门为 DMA 分配的（零初始化）
    /// - CPU 和设备看到同一个虚拟地址
    /// - 不需要额外的对齐缓冲区
    ///
    /// # 特性保证
    ///
    /// - 内存已被零初始化
    ///
    /// # Safety
    ///
    /// 调用者必须确保：
    /// - `origin_virt` 指向有效内存，生命周期与 handle 相同
    /// - `dma_addr` 是与 `origin_virt` 对应的设备可访问地址
    /// - `layout` 正确描述内存的大小和对齐
    /// - 内存必须保持有效直到被正确释放
    pub unsafe fn new(cpu_addr: NonNull<u8>, dma_addr: DmaAddr, layout: Layout) -> Self {
        Self {
            cpu_addr,
            dma_addr,
            layout,
        }
    }

    /// Returns the size of the DMA buffer in bytes.
    pub fn size(&self) -> usize {
        self.layout.size()
    }

    /// Returns the alignment requirement of the DMA buffer in bytes.
    pub fn align(&self) -> usize {
        self.layout.align()
    }

    /// Returns the virtual address to access data.
    pub fn as_ptr(&self) -> NonNull<u8> {
        self.cpu_addr
    }

    /// Returns the DMA address visible to devices.
    pub fn dma_addr(&self) -> DmaAddr {
        self.dma_addr
    }

    /// Returns the memory layout used for this DMA allocation.
    pub fn layout(&self) -> Layout {
        self.layout
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaMapHandle {
    pub(crate) handle: DmaHandle,
    pub(crate) map_alloc_virt: Option<NonNull<u8>>,
}

impl Deref for DmaMapHandle {
    type Target = DmaHandle;
    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl DmaMapHandle {
    /// 为 `map_single` 操作创建 `DmaMapHandle`。
    ///
    /// 此构造函数用于将现有缓冲区映射为 DMA 可访问的场景，其中：
    /// - 缓冲区可能已经存在于用户空间
    /// - 如果原地址不满足对齐或掩码要求，会分配额外的对齐缓冲区
    /// - `alloc_virt` 存储额外的对齐缓冲区地址（如果分配了）
    ///
    /// # 特性保证
    ///
    /// - 如果原地址满足要求，`alloc_virt` 为 `None`
    /// - 如果分配了对齐缓冲区，`alloc_virt` 包含其地址
    ///
    /// # Safety
    ///
    /// 调用者必须确保：
    /// - `cpu_addr` 指向有效内存，生命周期与 handle 相同
    /// - `dma_addr` 是与 `cpu_addr` 对应的设备可访问地址
    /// - `layout` 正确描述内存的大小和对齐
    /// - `alloc_virt`（如果提供）必须指向有效分配的内存
    /// - 内存必须保持有效直到 `unmap_single` 被调用
    /// - 必须与 `DmaOp::unmap_single` 配对使用以防止内存泄漏
    pub unsafe fn new(
        cpu_addr: NonNull<u8>,
        dma_addr: DmaAddr,
        layout: Layout,
        alloc_virt: Option<NonNull<u8>>,
    ) -> Self {
        let handle = DmaHandle {
            cpu_addr,
            dma_addr,
            layout,
        };
        Self {
            handle,
            map_alloc_virt: alloc_virt,
        }
    }

    pub fn alloc_virt(&self) -> Option<NonNull<u8>> {
        self.map_alloc_virt
    }
}

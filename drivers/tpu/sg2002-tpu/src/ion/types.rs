//! Ion 驱动数据结构定义

use core::{
    alloc::Layout,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_dma::DMAInfo;
use ax_memory_addr::PAGE_SIZE_4K;

/// Ion 堆类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IonHeapType {
    /// 系统堆，使用普通的系统内存
    System      = 0,
    /// DMA 堆，使用 DMA coherent 内存
    DmaCoherent = 1,
    /// Carveout 堆，预留的物理内存区域
    Carveout    = 2,
}

impl TryFrom<u32> for IonHeapType {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::System),
            1 => Ok(Self::DmaCoherent),
            2 => Ok(Self::Carveout),
            _ => Err(()),
        }
    }
}

/// Ion 缓冲区句柄
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct IonHandle(pub u32);

impl Default for IonHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl IonHandle {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(1);
        Self(COUNTER.fetch_add(1, Ordering::SeqCst))
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Ion 缓冲区信息
#[derive(Debug)]
pub struct IonBuffer {
    /// 缓冲区句柄
    pub handle: IonHandle,
    /// DMA 信息（包含虚拟地址和总线地址）
    pub dma_info: DMAInfo,
    /// 缓冲区大小
    pub size: usize,
}

impl IonBuffer {
    pub fn new(dma_info: DMAInfo, size: usize) -> Self {
        Self {
            handle: IonHandle::new(),
            dma_info,
            size,
        }
    }
}

impl Drop for IonBuffer {
    fn drop(&mut self) {
        // 最后一个 `Arc<IonBuffer>` 被释放时，物理页才交还给 DMA 分配器，
        // 以避免 fd / mmap 还存活时交还后被另一路 DMA 者重复占用。
        match Layout::from_size_align(self.size, PAGE_SIZE_4K) {
            Ok(layout) => unsafe {
                ax_dma::dealloc_coherent_pages(self.dma_info, layout);
            },
            Err(err) => {
                error!(
                    "IonBuffer drop: invalid layout (size={}, align={}): {:?}",
                    self.size, PAGE_SIZE_4K, err
                );
            }
        }
    }
}

// 手动实现 Send 和 Sync，因为 DMAInfo 中的 NonNull<u8> 默认不实现 Sync
// 但是在我们的使用场景中，DMA 内存地址是安全的，可以在线程间共享
unsafe impl Send for IonBuffer {}
unsafe impl Sync for IonBuffer {}

/// Ion 分配请求
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IonAllocData {
    /// 请求的大小
    pub len: u64,
    /// 堆掩码
    pub heap_id_mask: u32,
    /// 标志
    pub flags: u32,
    /// 返回的文件描述符
    pub fd: u32,
    /// 未使用字段
    pub unused: u32,
    /// 物理地址
    pub paddr: u64,
    /// 缓冲区名称
    pub name: [u8; MAX_ION_BUFFER_NAME],
}

/// Ion FD 数据（用于导入外部 fd）
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IonFdData {
    /// 外部文件描述符
    pub fd: i32,
    /// 返回的 Ion 句柄
    pub handle: u32,
}

/// Ion 句柄数据（用于释放缓冲区）
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IonHandleData {
    /// Ion 句柄
    pub handle: u32,
}

pub const MAX_HEAP_NAME: usize = 32;
pub const MAX_ION_BUFFER_NAME: usize = 32;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IonHeapData {
    pub name: [u8; MAX_HEAP_NAME],
    pub type_: u32,
    pub heap_id: u32,
    pub reserved0: u32,
    pub reserved1: u32,
    pub reserved2: u32,
}

/// Ion 堆查询数据
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IonHeapQuery {
    /// 堆计数（输入：要查询的堆数量，输出：实际堆数量）
    pub cnt: u32,
    /// 保留字段
    pub reserved0: u32,
    /// 堆数据指针（用户空间地址）
    pub heaps: u64,
    /// 保留字段
    pub reserved1: u32,
    /// 保留字段
    pub reserved2: u32,
}

/// Ion IOCTL 命令
pub mod ioctl {
    pub use super::*;

    /// 魔数
    pub const ION_IOC_MAGIC: u8 = b'I';

    /// 分配内存
    pub const ION_IOC_ALLOC: u32 = ioctl_iowr!(ION_IOC_MAGIC, 0, IonAllocData);
    /// 查询堆信息
    pub const ION_IOC_HEAP_QUERY: u32 = ioctl_iowr!(ION_IOC_MAGIC, 8, IonHeapQuery);

    /// 释放内存
    pub const ION_IOC_FREE: u32 = ioctl_iow!(ION_IOC_MAGIC, 1, IonHandleData);
    /// 导入 fd
    pub const ION_IOC_IMPORT: u32 = ioctl_iowr!(ION_IOC_MAGIC, 5, IonFdData);
}

/// IOCTL 宏定义
macro_rules! ioctl_iowr {
    ($magic:expr, $nr:expr, $ty:ty) => {
        (3u32 << 30)
            | (($magic as u32) << 8)
            | ($nr as u32)
            | ((core::mem::size_of::<$ty>() as u32) << 16)
    };
}

macro_rules! ioctl_iow {
    ($magic:expr, $nr:expr, $ty:ty) => {
        (1u32 << 30)
            | (($magic as u32) << 8)
            | ($nr as u32)
            | ((core::mem::size_of::<$ty>() as u32) << 16)
    };
}

#[allow(unused_macros)]
macro_rules! ioctl_ior {
    ($magic:expr, $nr:expr, $ty:ty) => {
        (2u32 << 30)
            | (($magic as u32) << 8)
            | ($nr as u32)
            | ((core::mem::size_of::<$ty>() as u32) << 16)
    };
}

pub(crate) use ioctl_iow;
pub(crate) use ioctl_iowr;

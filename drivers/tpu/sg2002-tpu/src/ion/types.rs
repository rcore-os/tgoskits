//! Ion 驱动数据结构定义

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use dma_api::{CoherentArray, DmaAddr};

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
pub struct IonBuffer {
    /// 缓冲区句柄
    pub handle: IonHandle,
    /// Owned DMA-coherent storage.
    dma: CoherentArray<u8>,
    /// 缓冲区大小
    pub size: usize,
}

impl IonBuffer {
    pub fn new(dma: CoherentArray<u8>, size: usize) -> Self {
        Self {
            handle: IonHandle::new(),
            dma,
            size,
        }
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.dma.dma_addr()
    }

    pub fn cpu_ptr(&self) -> NonNull<u8> {
        self.dma.as_ptr()
    }
}

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

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use dma_api::{
        DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
    };

    use self::std::alloc::{alloc_zeroed, dealloc};
    use super::*;

    struct TestDma;

    static TEST_DMA: TestDma = TestDma;
    static RELEASES: AtomicUsize = AtomicUsize::new(0);

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_contiguous(&self, _handle: DmaAllocHandle) {}

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            Some(unsafe { DmaAllocHandle::new(ptr, 0x4000_u64.into(), layout) })
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
            RELEASES.fetch_add(1, Ordering::SeqCst);
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            Err(DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    #[test]
    fn ion_buffer_preserves_size_address_and_last_arc_release() {
        RELEASES.store(0, Ordering::SeqCst);
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA)
            .coherent_array_zero_with_align::<u8>(123, 8)
            .unwrap();
        let cpu_ptr = dma.as_ptr();
        let buffer = Arc::new(IonBuffer::new(dma, 123));

        assert_eq!(buffer.size, 123);
        assert_eq!(buffer.dma_addr().as_u64(), 0x4000);
        assert_eq!(buffer.cpu_ptr(), cpu_ptr);

        let mmap_owner = buffer.clone();
        drop(buffer);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 0);
        drop(mmap_owner);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
    }
}

//! dma-heap / dma-buf userspace ABI (mainline-stable Linux uapi).
//!
//! ioctl numbers are derived via the `_IOC` macros below and compile-time-checked against the
//! canonical Linux values, so the encoding cannot silently drift. Structs are `#[repr(C)]`.

/// `_IOWR(type, nr, T)` — read/write ioctl number (dir bits = 3).
macro_rules! ioc_iowr {
    ($magic:expr, $nr:expr, $ty:ty) => {
        (3u32 << 30)
            | (($magic as u32) << 8)
            | ($nr as u32)
            | ((core::mem::size_of::<$ty>() as u32) << 16)
    };
}

/// `_IOW(type, nr, T)` — write ioctl number (dir bits = 1).
macro_rules! ioc_iow {
    ($magic:expr, $nr:expr, $ty:ty) => {
        (1u32 << 30)
            | (($magic as u32) << 8)
            | ($nr as u32)
            | ((core::mem::size_of::<$ty>() as u32) << 16)
    };
}

/// `struct dma_heap_allocation_data` (linux/dma-heap.h). 24 bytes.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DmaHeapAllocationData {
    /// Requested size in bytes (input).
    pub len: u64,
    /// Returned dma-buf fd (output).
    pub fd: u32,
    /// Flags for the returned fd, e.g. `O_CLOEXEC` (input).
    pub fd_flags: u32,
    /// Heap-specific flags (input; unused).
    pub heap_flags: u64,
}

/// `struct dma_buf_sync` (linux/dma-buf.h). 8 bytes.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DmaBufSync {
    /// Sync direction and phase flags; valid bits defined by `DMA_BUF_SYNC_VALID_FLAGS_MASK`.
    pub flags: u64,
}

const DMA_HEAP_IOCTL_MAGIC: u8 = b'H';
const DMA_BUF_IOCTL_MAGIC: u8 = b'b';

/// `DMA_HEAP_IOCTL_ALLOC = _IOWR('H', 0x0, struct dma_heap_allocation_data)`.
pub const DMA_HEAP_IOCTL_ALLOC: u32 = ioc_iowr!(DMA_HEAP_IOCTL_MAGIC, 0, DmaHeapAllocationData);
/// `DMA_BUF_IOCTL_SYNC = _IOW('b', 0x0, struct dma_buf_sync)`.
pub const DMA_BUF_IOCTL_SYNC: u32 = ioc_iow!(DMA_BUF_IOCTL_MAGIC, 0, DmaBufSync);

// dma_buf_sync.flags bits (linux/dma-buf.h).
pub const DMA_BUF_SYNC_READ: u64 = 1 << 0;
pub const DMA_BUF_SYNC_WRITE: u64 = 2 << 0;
pub const DMA_BUF_SYNC_RW: u64 = DMA_BUF_SYNC_READ | DMA_BUF_SYNC_WRITE;
/// Begin-CPU-access phase: the absence of the `END` bit. Value `0`.
pub const DMA_BUF_SYNC_START: u64 = 0 << 2;
pub const DMA_BUF_SYNC_END: u64 = 1 << 2;
/// Valid bits a caller may set in `dma_buf_sync.flags`.
pub const DMA_BUF_SYNC_VALID_FLAGS_MASK: u64 = DMA_BUF_SYNC_RW | DMA_BUF_SYNC_END;

// Compile-time guards: the macro math MUST equal the canonical Linux uapi values and sizes.
const _: () = assert!(DMA_HEAP_IOCTL_ALLOC == 0xC018_4800);
const _: () = assert!(DMA_BUF_IOCTL_SYNC == 0x4008_6200);
const _: () = assert!(core::mem::size_of::<DmaHeapAllocationData>() == 24);
const _: () = assert!(core::mem::size_of::<DmaBufSync>() == 8);

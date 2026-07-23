//! Ion (Android ION) memory allocator driver
//!
//! Ion 是一个用于 Android 系统的内存分配器，用于在不同的硬件组件
//! （如 GPU、摄像头、显示器等）之间共享内存缓冲区。
//!
//! DMA memory is obtained through an injected `dma-api::DeviceDma` capability.

pub mod buffer;
pub mod error;
pub mod heap;
pub mod types;

pub use buffer::IonBufferManager;
pub use error::{IonError, IonResult};
pub use heap::IonHeapManager;
pub use types::{
    IonAllocData, IonBuffer, IonFdData, IonHandle, IonHandleData, IonHeapData, IonHeapQuery,
    IonHeapType, MAX_HEAP_NAME, MAX_ION_BUFFER_NAME, ioctl,
};

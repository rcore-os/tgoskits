//! 每个 buffer 的元数据——对应 Linux 的 `struct vb2_buffer`。

use alloc::vec::Vec;
use core::ptr::NonNull;

use crate::interface::{Timeval, buffer::BufFlags};

/// 队列中单个 buffer 的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    Free,   // buffer 处于用户空间控制下
    Ready,  // buffer 已被用户空间入队，等待交给驱动
    Active, // buffer 已通过 buf_queue 交给驱动，驱动正在处理它
    Done,   // 驱动已处理完毕，可供 DQBUF
    Error,  // 驱动处理此 buffer 时遇到错误
}

/// buffer 中一个 plane 的内存句柄。
///
/// 对应 Linux 的 `vb2_plane.mem_priv`：由 [`VbMemOps::alloc`](super::VbMemOps::alloc)
/// 返回的分配器私有句柄。句柄的 CPU 可写地址由 [`MemPlane::as_ptr`] 暴露，
/// 驱动侧通过 [`ActiveFrame::as_mut_slice`] 独占访问，不再直接 `cookie as *mut u8` 强转。
#[derive(Debug, Clone, Copy)]
pub struct MemPlane {
    ptr: NonNull<u8>,
    pub offset: usize,
    pub length: u32,
}

impl MemPlane {
    /// 构造一个平面句柄。
    ///
    /// `ptr` 必须指向长度为 `length` 的有效 vmalloc 段，且在 `release` 前保持稳定。
    pub fn new(ptr: NonNull<u8>, offset: usize, length: u32) -> Self {
        Self {
            ptr,
            offset,
            length,
        }
    }

    /// 以裸地址构造（宿主机测试/特殊分配器辅助）。
    ///
    /// `addr == 0` 时返回 `None`。
    pub fn from_addr(addr: usize, offset: usize, length: u32) -> Option<Self> {
        NonNull::new(addr as *mut u8).map(|ptr| Self {
            ptr,
            offset,
            length,
        })
    }

    /// CPU 可写虚地址（裸指针）。
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// 虚地址的整数视图（用于 `mmap` 页表查找与调试）。
    #[inline]
    pub fn addr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }
}

// SAFETY: `ptr` 指向由 `VbMemOps` 分配的稳定 vmalloc 段，在 `release` 前
// 保持有效；`MemPlane` 仅作为句柄在队列锁保护下共享或经 `ActiveFrame` 独占
// 访问，与 Linux `vb2_plane.mem_priv` 的跨线程共享语义一致。原 `usize` 形式的
// cookie 同为 `Send + Sync`，此封装保持相同线程安全契约。
unsafe impl Send for MemPlane {}
unsafe impl Sync for MemPlane {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Timestamp {
    #[default]
    Unset, // 尚未设置时间戳（`Dequeued`/`Queued`/`Active` 阶段）。
    Monotonic(u64), // 单调时钟时间戳（`Done`/`Error` 阶段），纳秒。
}

impl Timestamp {
    /// 是否已设置时间戳。
    #[inline]
    pub fn is_set(&self) -> bool {
        matches!(self, Self::Monotonic(_))
    }

    /// 返回纳秒值，未设置时为 0。
    #[inline]
    pub fn nanos(&self) -> u64 {
        match *self {
            Self::Unset => 0,
            Self::Monotonic(n) => n,
        }
    }

    /// 对应的 `V4L2_BUF_FLAG_TIMESTAMP_*` 标志。
    #[inline]
    pub fn flags(&self) -> BufFlags {
        match *self {
            Self::Unset => BufFlags::empty(),
            Self::Monotonic(_) => BufFlags::TIMESTAMP_MONOTONIC,
        }
    }

    /// 转换为 `v4l2_buffer.timestamp` 的 `Timeval` 表示。
    #[inline]
    pub fn timeval(&self) -> Timeval {
        let n = self.nanos();
        Timeval {
            tv_sec: (n / 1_000_000_000) as i64,
            tv_usec: ((n / 1_000) % 1_000_000) as i64,
        }
    }
}

/// 队列中的单个 buffer——对应 Linux 的 `struct vb2_buffer`。
#[derive(Clone)]
pub struct VbBuffer {
    pub state: BufferState,
    pub planes: Vec<MemPlane>,
    pub bytesused: u32,
    pub sequence: u32,
    pub timestamp: Timestamp,
}

/// 活动缓冲句柄——仅含裸指针与索引
#[must_use]
pub(crate) struct ActiveFrame {
    pub(crate) buffer_index: u32,
    pub(crate) data_ptr: *mut u8,
    pub(crate) len: usize,
}

// SAFETY: `ptr` 指向的 vmalloc 段在 Guard 期间独占且稳定。
unsafe impl Send for ActiveFrame {}
unsafe impl Sync for ActiveFrame {}

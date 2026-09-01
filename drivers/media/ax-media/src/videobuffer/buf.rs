//! 每个 buffer 的元数据——对应 Linux 的 `struct vb2_buffer`。

use alloc::vec::Vec;
use core::ptr::NonNull;

/// 队列中单个 buffer 的状态。
///
/// 对应 Linux 的 `enum vb2_buffer_state`：
///
/// ```text
/// DEQUEUED ──(QBUF)──► QUEUED ──(buf_queue)──► ACTIVE
///     ▲                    │                        │
///     │                    │                        │ (vb2_buffer_done)
///     │                    │                        ▼
///     └──(DQBUF)───────────┴────────────────── DONE / ERROR
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// buffer 处于用户空间控制下（之前是 Free）。
    Dequeued,
    /// buffer 已被用户空间入队，等待交给驱动。
    Queued,
    /// buffer 已通过 buf_queue 交给驱动，驱动正在处理它。
    Active,
    /// 驱动已处理完毕，可供 DQBUF。
    Done,
    /// 驱动处理此 buffer 时遇到错误。
    Error,
}

/// buffer 中一个 plane 的内存句柄。
///
/// 对应 Linux 的 `vb2_plane.mem_priv`：由 [`Vb2MemOps::alloc`](super::Vb2MemOps::alloc)
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

// SAFETY: `ptr` 指向由 `Vb2MemOps` 分配的稳定 vmalloc 段，在 `release` 前
// 保持有效；`MemPlane` 仅作为句柄在队列锁保护下共享或经 `ActiveFrame` 独占
// 访问，与 Linux `vb2_plane.mem_priv` 的跨线程共享语义一致。原 `usize` 形式的
// cookie 同为 `Send + Sync`，此封装保持相同线程安全契约。
unsafe impl Send for MemPlane {}
unsafe impl Sync for MemPlane {}

/// 队列中的单个 buffer——对应 Linux 的 `struct vb2_buffer`。
#[derive(Clone)]
pub struct Vb2Buffer {
    pub state: BufferState,
    pub planes: Vec<MemPlane>,
    pub bytesused: u32,
    pub sequence: u32,
    pub timestamp: u64,
    pub timestamp_flags: u32,
    pub(crate) prepared: bool,
    pub(crate) driver_owned: bool,
}

/// 驱动独占的活动缓冲句柄——仅含裸指针与索引，不持有队列。
///
/// 由 `Vb2Queue::acquire` 返回，通过 `Vb2Queue::commit` 完成，避免
/// `queue: *const Vb2Queue` 的所有权纠缠（`FrameAssembler` 已持有 `queue`）。
#[must_use]
pub struct ActiveFrame {
    pub(crate) index: u32,
    pub(crate) ptr: *mut u8,
    pub(crate) len: usize,
}

impl ActiveFrame {
    /// 缓冲容量（`plane.length`，页对齐）。
    pub fn capacity(&self) -> usize {
        self.len
    }

    /// 队列下标（仅供测试/调试）。
    pub fn index(&self) -> u32 {
        self.index
    }

    /// 以独占 `&mut [u8]` 暴露缓冲平面。
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

// SAFETY: `ptr` 指向的 vmalloc 段在 Guard 期间独占且稳定。
unsafe impl Send for ActiveFrame {}
unsafe impl Sync for ActiveFrame {}

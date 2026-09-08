//! V4L2 buffer 队列框架（vb2——Video Buffer 2）。
//!
//! 对应 Linux 的 `drivers/media/common/videobuf2/`。
//! 实现 buffer 状态机（DEQUEUED → QUEUED → ACTIVE → DONE），
//! 通过 [`VbMemOps`] 提供可插拔的内存分配

mod allocator;
mod buf;
mod pool;

pub use allocator::{VbMemOps, VirtualAllocator};
pub use buf::{BufferState, MemPlane, Timestamp, VbBuffer};
pub use pool::{FrameGuard, VbPool, VbPoolLease};

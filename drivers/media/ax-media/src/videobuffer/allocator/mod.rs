//! 内存分配策略——[`VbMemOps`]（对应 Linux 的 `struct vb2_mem_ops`）及其后端。

mod vmalloc;

use alloc::vec::Vec;

pub use vmalloc::VirtualAllocator;

use crate::{V4l2Error, videobuffer::buf::MemPlane};

/// 内存分配策略：VbPool 通过它分配/释放缓冲平面，不依赖具体后端。
pub trait VbMemOps: Send + Sync {
    fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, V4l2Error>;
    fn release(&self, planes: &[MemPlane]);
    fn mmap(&self, plane: &MemPlane) -> Vec<usize>;
}

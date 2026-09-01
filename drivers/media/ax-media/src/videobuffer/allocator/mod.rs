//! 内存分配策略——[`Vb2MemOps`]（对应 Linux 的 `struct vb2_mem_ops`）及其后端。

mod vmalloc;

use alloc::vec::Vec;

pub use vmalloc::VirtualAllocator;

use crate::{V4l2Error, videobuffer::buf::MemPlane};

/// 内存分配策略：Vb2Queue 通过它分配/释放缓冲平面，不依赖具体后端。
///
/// 对齐 Linux `struct vb2_mem_ops`：**布局（虚拟/物理连续与否、mmap
/// 偏移编码）是分配器的内部实现**——队列只调生命周期与映射回调。
/// [`MemPlane`] 封装分配器私有句柄（`NonNull<u8>` 虚拟地址，vmalloc 风格——
/// Linux `vb2_vmalloc` 的 `mem_priv` 同为虚拟地址），通过 [`MemPlane::as_ptr`]
/// 暴露 CPU 可写地址，驱动侧经 [`crate::videobuffer::ActiveFrame`] 独占访问，不再裸 `as *mut u8`
/// 强转。所有方法都接受 `&self`（后端自同步）——采集回调可在 IRQ 上下文访问。
pub trait Vb2MemOps: Send + Sync {
    fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, V4l2Error>;
    fn release(&self, planes: &[MemPlane]);
    /// 平面的逐页物理地址（4K 粒度）——glue 建用户 vma 用
    /// （Linux mem_ops 的 `mmap` 回调的对应：分配器声明自己的映射）。
    /// 物理连续后端按基址逐页生成；vmalloc 风格返回离散页列表。
    fn mmap(&self, plane: &MemPlane) -> Vec<usize>;
}

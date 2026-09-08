//! VirtualAllocator——实现 VbMemOps 的 vmalloc 风格内核页分配器。
//!
//! 对齐 Linux `vb2_vmalloc`：**虚拟连续、物理离散**——在内核地址空间
//! （`axmm::kernel_aspace`）分配连续虚拟段，逐页从 axalloc 堆分配物理
//! 帧并 `map_linear` 建立映射（不要求物理连续大块，长时运行系统不易
//! 碎片失败）。
//!
//! 物理页快照由分配器自管（`alloc` 时逐页分配并记录）——不需要页表
//! 查询。注意 `virt_to_phys` 只对**线性映射区**有效（ax-plat mem 契约），
//! 不能翻译 vmalloc 段任意虚拟地址——所以物理页从 axalloc 堆分配
//! （其 vaddr 在线性映射区，换算有效），再映射到 vmalloc 段。
//!
//! 布局完全内聚：`alloc` 时计算 UAPI mmap 偏移（stride）；[`MemPlane`] 的
//! 私有句柄即虚拟段基址（`as_ptr()` 暴露 CPU 直写地址）。供 vivid 及 CPU 搬运
//! 场景（uvc 拼帧）。

use alloc::{vec, vec::Vec};
use core::ptr::NonNull;

use ax_alloc::{UsageKind, global_allocator};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange, align_up_4k};
use ax_mm::kernel_aspace;
use ax_runtime::hal::{
    mem::{phys_to_virt, virt_to_phys},
    paging::MappingFlags,
};

use crate::{
    V4l2Error,
    videobuffer::{MemPlane, VbMemOps},
};

/// 跟踪一个已分配 buffer 的虚拟段与物理页（自管，release 时归还）。
struct AllocEntry {
    /// 虚拟段基址（与 [`MemPlane::as_ptr`] 一致——CPU 直写地址）。
    vaddr: usize,
    size: usize,
    /// 逐页物理地址（axalloc 堆分配；unmap/归还前不变）。
    pages: Vec<usize>,
}

/// 用于 V4L2 buffer 内存的 vmalloc 风格 allocator。
///
/// 分配流程（每次 `alloc`）：在内核地址空间中找空闲虚拟段
/// （4K 对齐）→ 逐页 `global_allocator().alloc_pages(1, 4K)` 分配物理
/// 帧（vaddr 在线性映射区，`virt_to_phys` 换算有效）→ `map_linear`
/// 逐页映射到虚拟段（虚拟连续、物理离散）。`mmap` 偏移按 stride
/// （页对齐 plane 大小）在 alloc 时计算，buffer 间不重叠。
pub struct VirtualAllocator {
    entries: ax_sync::Mutex<Vec<AllocEntry>>,
}

impl VirtualAllocator {
    pub fn new() -> Self {
        Self {
            entries: ax_sync::Mutex::new(Vec::new()),
        }
    }
}

impl Default for VirtualAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// 逐页分配 `size`（页对齐）字节的物理帧，返回物理地址列表。
///
/// 必须在可睡上下文调用：全局分配失败路径会触发页缓存回收。
/// 中途失败时已分配的帧随回收逻辑一起归还，不泄漏。
fn alloc_frames(size: usize) -> Option<Vec<usize>> {
    let mut pages = Vec::with_capacity(size / PAGE_SIZE_4K);
    for _ in 0..size / PAGE_SIZE_4K {
        let vaddr = match global_allocator().alloc_pages(1, PAGE_SIZE_4K, UsageKind::VirtMem) {
            Ok(vaddr) => vaddr,
            Err(_) => {
                free_frames(&pages);
                return None;
            }
        };
        let pa = virt_to_phys(VirtAddr::from(vaddr)).as_usize();
        pages.push(pa);
    }
    Some(pages)
}

/// 归还物理帧（无锁；不触发回收）。
fn free_frames(pages: &[usize]) {
    for &pa in pages {
        let vaddr = phys_to_virt(PhysAddr::from_usize(pa));
        global_allocator().dealloc_pages(vaddr.as_usize(), 1, UsageKind::VirtMem);
    }
}

/// 在内核地址空间查找空闲虚拟段并逐页映射已分配的物理帧。
fn map_frames(pages: &[usize]) -> Option<usize> {
    let size = pages.len() * PAGE_SIZE_4K;
    let mut aspace = kernel_aspace().lock();
    let limit = VirtAddrRange::new(aspace.base(), aspace.end());
    let start = aspace.find_free_area(aspace.base(), size, limit)?;
    for (i, &pa) in pages.iter().enumerate() {
        if aspace
            .map_linear(
                start + i * PAGE_SIZE_4K,
                PhysAddr::from_usize(pa),
                PAGE_SIZE_4K,
                MappingFlags::READ | MappingFlags::WRITE,
            )
            .is_err()
        {
            for j in 0..i {
                let _ = aspace.unmap(start + j * PAGE_SIZE_4K, PAGE_SIZE_4K);
            }
            return None;
        }
    }
    Some(start.as_usize())
}

impl VbMemOps for VirtualAllocator {
    fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, V4l2Error> {
        if sizes.len() != 1 || sizes[0] == 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let aligned = align_up_4k(sizes[0] as usize);
        // 仅取登记数量用于 UAPI 偏移编码，不跨锁持有。
        let buf_index = self.entries.lock().len();
        // 物理帧在可睡上下文（无锁）分配：分配失败路径的页缓存回收
        // 需要睡锁，不得在 entries/kernel_aspace 自旋锁内进行。
        let Some(pages) = alloc_frames(aligned) else {
            return Err(V4l2Error::NoMemory);
        };
        // find→map 原子窗口（aspace 自旋锁；页表扩展分配失败时
        // 回收回调在原子上下文跳过，仅返回 ENOMEM，不 panic）。
        let Some(vaddr) = map_frames(&pages) else {
            free_frames(&pages);
            return Err(V4l2Error::NoMemory);
        };
        self.entries.lock().push(AllocEntry {
            vaddr,
            size: aligned,
            pages,
        });
        // 记录页对齐后的实际大小：用户态 mmap 的 length 是页对齐的，
        // 若记录未对齐的 size 会导致 mmap 越界检查（offset+length>end）
        // 误拒绝（Linux vb2 的 plane.length 同样是页对齐值）。
        let ptr = NonNull::new(vaddr as *mut u8).ok_or(V4l2Error::NoMemory)?;
        Ok(vec![MemPlane::new(
            ptr,
            buf_index * aligned,
            aligned as u32,
        )])
    }

    fn release(&self, planes: &[MemPlane]) {
        for plane in planes {
            let addr = plane.addr();
            // 摘除 entry（entries 锁只覆盖登记表窗口）。
            let entry = {
                let mut entries = self.entries.lock();
                let Some(pos) = entries.iter().position(|e| e.vaddr == addr) else {
                    continue;
                };
                entries.swap_remove(pos)
            };
            // 摘除虚拟映射（aspace 锁只覆盖 unmap 窗口，不跨物理帧归还）。
            let _ = kernel_aspace()
                .lock()
                .unmap(VirtAddr::from(entry.vaddr), entry.size);
            // 归还物理帧（无锁）。
            free_frames(&entry.pages);
        }
    }

    fn mmap(&self, plane: &MemPlane) -> Vec<usize> {
        let addr = plane.addr();
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|e| e.vaddr == addr)
            .map(|e| e.pages.clone())
            .unwrap_or_default()
    }
}

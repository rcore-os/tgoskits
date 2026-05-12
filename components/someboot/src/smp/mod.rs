use kernutil::memory::MemoryType;

use crate::{
    ArchTrait, DCacheOp,
    arch::Arch,
    kernel_page_table_paddr,
    mem::{__percpu, dcache_range, page_size, phys_to_virt},
};

mod cpu_iter;
#[cfg(not(feature = "percpu-prealloc"))]
mod legacy;
#[cfg(feature = "percpu-prealloc")]
mod prealloc;

#[cfg(not(feature = "percpu-prealloc"))]
use legacy as layout;
#[cfg(feature = "percpu-prealloc")]
use prealloc as layout;

static mut PERCPU_START: usize = 0;
static mut PERCPU_END: usize = 0;

fn __cpu_id_list() -> impl Iterator<Item = usize> {
    cpu_iter::cpu_id_list()
}

fn align_up_pow2(value: usize, align: usize) -> usize {
    assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

fn meta_align() -> usize {
    core::mem::align_of::<PerCpuMeta>().max(64)
}

fn percpu_region_align() -> usize {
    page_size().max(meta_align())
}

pub fn alloc_percpu() {
    layout::alloc_percpu();
}

pub(crate) fn init_percpu() {
    let boot_table = crate::mem::mmu::boot_table_addr();
    let primary_table = kernel_page_table_paddr();
    for meta in cpu_meta_list_mut() {
        meta.boot_table_paddr = boot_table;
        meta.primary_table_paddr = primary_table;
    }

    let start = __percpu(unsafe { PERCPU_START });
    let size = unsafe { PERCPU_END - PERCPU_START };
    dcache_range(DCacheOp::CleanInvalidate, start, size);
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PerCpuMeta {
    pub stack_top: usize,
    /// The hardware ID of the CPU, e.g. hart id in RISC-V or MPIDR in ARM
    pub cpu_id: usize,
    /// The logical index of the CPU, assigned by the bootloader or determined by the OS
    pub cpu_idx: usize,

    pub stack_top_virt: usize,
    pub entry_virt: usize,

    pub boot_table_paddr: usize,
    pub primary_table_paddr: usize,
}

#[allow(dead_code)]
/// Physical RAM allocated for per-CPU data should be mapped to this virtual address range in the kernel
pub(crate) fn percpu_range() -> core::ops::Range<usize> {
    unsafe { PERCPU_START..PERCPU_END }
}

#[allow(dead_code)]
pub(crate) fn percpu_va_range() -> core::ops::Range<usize> {
    let start = __percpu(unsafe { PERCPU_START });
    let end = __percpu(unsafe { PERCPU_END });
    start as usize..end as usize
}

pub fn cpu_meta_list() -> impl Iterator<Item = PerCpuMeta> {
    CpuMetaIter { next: 0 }
}

pub fn cpu_meta(idx: usize) -> Option<PerCpuMeta> {
    let meta_start = cpu_meta_addr(idx)?;
    let meta_va = phys_to_virt(meta_start);
    debug_assert_eq!((meta_va as usize) % meta_align(), 0);
    Some(unsafe { *(meta_va as *const PerCpuMeta) })
}

/// Physical address of cpu meta
pub(crate) fn cpu_meta_addr(idx: usize) -> Option<usize> {
    layout::cpu_meta_addr(idx)
}

pub fn percpu_data_ptr(idx: usize) -> Option<*mut u8> {
    layout::percpu_data_ptr(idx)
}

pub fn cpu_hart_id() -> usize {
    Arch::cpu_current_hartid()
}

pub fn cpu_idx() -> usize {
    let hart_id = cpu_hart_id();
    for (idx, id) in __cpu_id_list().enumerate() {
        if id == hart_id {
            return idx;
        }
    }
    panic!("Current CPU hart id {hart_id:#x} not found in CPU list");
}

pub fn cpu_id_to_idx(hart_id: usize) -> Option<usize> {
    for (idx, id) in __cpu_id_list().enumerate() {
        if id == hart_id {
            return Some(idx);
        }
    }
    None
}

pub fn cpu_idx_to_id(idx: usize) -> Option<usize> {
    __cpu_id_list().nth(idx)
}

pub fn cpu_count() -> usize {
    __cpu_id_list().count()
}

struct CpuMetaIter {
    next: usize,
}

impl Iterator for CpuMetaIter {
    type Item = PerCpuMeta;

    fn next(&mut self) -> Option<Self::Item> {
        let meta = cpu_meta(self.next)?;
        self.next += 1;
        Some(meta)
    }
}

fn cpu_meta_list_mut() -> impl Iterator<Item = &'static mut PerCpuMeta> {
    CpuMetaIterMutable { next: 0 }
}

struct CpuMetaIterMutable {
    next: usize,
}

impl Iterator for CpuMetaIterMutable {
    type Item = &'static mut PerCpuMeta;

    fn next(&mut self) -> Option<Self::Item> {
        let meta_start = cpu_meta_addr(self.next)?;
        let meta_va = phys_to_virt(meta_start);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);
        let meta = unsafe { &mut *(meta_va as *mut PerCpuMeta) };
        self.next += 1;
        Some(meta)
    }
}

fn percpu_link_range() -> core::ops::Range<usize> {
    unsafe extern "C" {
        fn __percpu_start();
        fn __percpu_end();
    }
    let start = __percpu_start as *const () as usize;
    let end = __percpu_end as *const () as usize;
    start..end
}

fn set_percpu_range(start: usize, end: usize) {
    unsafe {
        PERCPU_START = start;
        PERCPU_END = end;
    }
}

fn percpu_data_range() -> core::ops::Range<usize> {
    unsafe { PERCPU_START..PERCPU_END }
}

fn alloc_percpu_region(size: usize) -> usize {
    unsafe { crate::mem::ram::flush_to_memory_map(MemoryType::Reserved) };

    unsafe {
        crate::mem::ram::alloc_and_flush_to_memory_map(
            core::alloc::Layout::from_size_align(size, percpu_region_align()).unwrap(),
            MemoryType::PerCpuData,
        )
        .unwrap()
    }
}

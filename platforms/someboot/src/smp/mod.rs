use core::{
    alloc::Layout,
    mem::size_of,
    sync::atomic::{AtomicUsize, Ordering},
};

use kernutil::memory::MemoryType;

use crate::{
    ArchTrait, DCacheOp,
    arch::Arch,
    kernel_page_table_paddr,
    mem::{cpu_area_phys_to_virt, dcache_range, page_size, phys_to_virt},
};

mod cpu_iter;
mod layout;

static mut CPU_AREA_REGION_START: usize = 0;
static mut CPU_AREA_REGION_END: usize = 0;
static CPU_AREA_LAYOUT_COUNT: AtomicUsize = AtomicUsize::new(0);
static CPU_AREA_RUNTIME_COUNT: AtomicUsize = AtomicUsize::new(0);

const PERCPU_INIT_OK: u32 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
enum PerCpuLayoutError {
    #[error("firmware did not provide any usable CPU")]
    EmptyCpuSet,
    #[error("per-CPU layout alignment {alignment:#x} is not a nonzero power of two")]
    InvalidAlignment { alignment: usize },
    #[error("per-CPU layout address arithmetic overflowed")]
    AddressOverflow,
    #[error("per-CPU linker template range {start:#x}..{end:#x} is malformed")]
    MalformedTemplateRange { start: usize, end: usize },
    #[error("per-CPU allocation size {size:#x} and alignment {alignment:#x} are invalid")]
    InvalidAllocationLayout { size: usize, alignment: usize },
}

fn __cpu_id_list() -> impl Iterator<Item = usize> {
    cpu_iter::cpu_id_list()
}

fn checked_align_up_pow2(value: usize, alignment: usize) -> Result<usize, PerCpuLayoutError> {
    if !alignment.is_power_of_two() {
        return Err(PerCpuLayoutError::InvalidAlignment { alignment });
    }
    let mask = alignment - 1;
    value
        .checked_add(mask)
        .map(|aligned| aligned & !mask)
        .ok_or(PerCpuLayoutError::AddressOverflow)
}

fn checked_allocation_layout(size: usize, alignment: usize) -> Result<Layout, PerCpuLayoutError> {
    Layout::from_size_align(size, alignment)
        .map_err(|_| PerCpuLayoutError::InvalidAllocationLayout { size, alignment })
}

fn meta_align() -> usize {
    core::mem::align_of::<PerCpuMeta>().max(64)
}

fn cpu_area_region_alignment() -> Result<usize, PerCpuLayoutError> {
    let alignment = page_size()
        .max(meta_align())
        .max(cpu_area_template_alignment()?);
    if !alignment.is_power_of_two() {
        return Err(PerCpuLayoutError::InvalidAlignment { alignment });
    }
    Ok(alignment)
}

fn cpu_area_template_alignment() -> Result<usize, PerCpuLayoutError> {
    unsafe extern "C" {
        static __PERCPU_TEMPLATE_ALIGN_START: u8;
        static __PERCPU_TEMPLATE_ALIGN_END: u8;
    }
    let start = core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_START) as usize;
    let end = core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_END) as usize;
    let alignment = end
        .checked_sub(start)
        .ok_or(PerCpuLayoutError::MalformedTemplateRange { start, end })?;
    if !alignment.is_power_of_two() {
        return Err(PerCpuLayoutError::InvalidAlignment { alignment });
    }
    Ok(alignment)
}

pub fn alloc_percpu() {
    layout::allocate_cpu_areas();
}

/// Constructs the final CPU-area values and publishes platform metadata.
///
/// Early boot reserves only raw physical storage. This function must run from
/// the final high-address image, after relocation reset, and before any CPU is
/// bound or made visible to runtime placement. The external ABI is scalar-only
/// so someboot does not acquire a semantic dependency on `ax-percpu`.
pub(crate) fn initialize_percpu_layout() {
    unsafe extern "C" {
        fn __percpu_initialize_layout(
            runtime_base: usize,
            area_stride: usize,
            area_count: u32,
        ) -> u32;
    }

    let cpu_count = allocated_cpu_count();
    let area_count =
        u32::try_from(cpu_count).expect("reserved per-CPU area count must fit the value-only ABI");
    assert_ne!(area_count, 0, "per-CPU storage must contain CPU zero");
    let runtime_base =
        percpu_data_ptr(0).expect("reserved CPU zero data area must remain addressable") as usize;
    let area_stride = layout::cpu_area_stride();
    let last_offset = area_stride
        .checked_mul(cpu_count - 1)
        .expect("reserved per-CPU area offset must not overflow");
    runtime_base
        .checked_add(last_offset)
        .expect("reserved per-CPU runtime layout must not wrap");

    // SAFETY: prime_entry is the unique final-high caller. Early allocation
    // reserved, zeroed, and mapped every area for the kernel lifetime; runtime
    // metadata and online count remain unpublished until construction and
    // cache maintenance complete below.
    let status = unsafe { __percpu_initialize_layout(runtime_base, area_stride, area_count) };
    assert_eq!(
        status, PERCPU_INIT_OK,
        "final CPU-local typed initialization rejected the reserved layout with status {status}"
    );

    initialize_runtime_metadata();
    let allocation = cpu_area_region();
    let allocation_size = allocation
        .end
        .checked_sub(allocation.start)
        .expect("reserved per-CPU range must remain ordered");
    dcache_range(
        DCacheOp::CleanInvalidate,
        cpu_area_phys_to_virt(allocation.start),
        allocation_size,
    );
    publish_runtime_cpu_areas(cpu_count);
}

/// Publishes the page-table facts consumed by secondary boot trampolines.
///
/// Final-high initialization has already constructed and exposed every
/// CPU-local Rust value. This late phase may update only the separate boot
/// metadata records; touching the complete allocation would also invalidate
/// live CPU data and primary/secondary stacks.
pub(crate) fn finalize_secondary_boot_metadata() {
    let boot_table = crate::mem::mmu::boot_table_addr();
    let primary_table = kernel_page_table_paddr();
    for meta in cpu_meta_list_mut() {
        meta.boot_table_paddr = boot_table;
        meta.primary_table_paddr = primary_table;
        dcache_range(
            DCacheOp::Clean,
            core::ptr::from_ref(meta).cast::<u8>(),
            size_of::<PerCpuMeta>(),
        );
    }
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

/// Immutable CPU identity resolved from the allocated per-CPU metadata table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCpuTarget {
    logical_index: usize,
    hardware_id: usize,
}

impl RuntimeCpuTarget {
    /// Returns the dense logical CPU index used by kernel data structures.
    pub const fn logical_index(self) -> usize {
        self.logical_index
    }

    /// Returns the firmware/hardware CPU identity used by architecture IPIs.
    pub const fn hardware_id(self) -> usize {
        self.hardware_id
    }
}

/// Failure to resolve one runtime CPU target without firmware parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeCpuTargetError {
    /// The logical CPU has no allocated metadata slot.
    #[error("logical CPU index has no allocated metadata slot")]
    Missing,
    /// The requested slot contains metadata for a different logical CPU.
    #[error("per-CPU metadata logical index mismatch")]
    IndexMismatch,
}

#[allow(dead_code)]
pub(crate) fn cpu_area_virtual_region() -> core::ops::Range<usize> {
    let start = cpu_area_phys_to_virt(unsafe { CPU_AREA_REGION_START });
    let end = cpu_area_phys_to_virt(unsafe { CPU_AREA_REGION_END });
    start as usize..end as usize
}

pub fn cpu_meta_list() -> impl Iterator<Item = PerCpuMeta> {
    CpuMetaIter { next: 0 }
}

pub fn cpu_meta(idx: usize) -> Option<PerCpuMeta> {
    if idx >= runtime_cpu_count() {
        return None;
    }
    let meta_start = cpu_meta_addr(idx)?;
    let meta_va = phys_to_virt(meta_start);
    debug_assert_eq!((meta_va as usize) % meta_align(), 0);
    Some(unsafe { *(meta_va as *const PerCpuMeta) })
}

/// Resolves one logical CPU through shutdown-lifetime per-CPU metadata.
///
/// This path performs one bounds check and one metadata load. It never falls
/// back to ACPI/FDT discovery and is therefore safe to use from bounded IPI
/// send paths after [`alloc_percpu`] completes.
pub fn runtime_cpu_target(idx: usize) -> Result<RuntimeCpuTarget, RuntimeCpuTargetError> {
    if idx >= runtime_cpu_count() {
        return Err(RuntimeCpuTargetError::Missing);
    }
    let meta = cpu_meta(idx).ok_or(RuntimeCpuTargetError::Missing)?;
    if meta.cpu_idx != idx {
        return Err(RuntimeCpuTargetError::IndexMismatch);
    }
    Ok(RuntimeCpuTarget {
        logical_index: idx,
        hardware_id: meta.cpu_id,
    })
}

/// Returns the number of CPU slots published by [`alloc_percpu`].
///
/// Unlike [`cpu_count`], this accessor never revisits firmware tables.
pub fn runtime_cpu_count() -> usize {
    CPU_AREA_RUNTIME_COUNT.load(Ordering::Acquire)
}

/// Physical address of cpu meta
pub(crate) fn cpu_meta_addr(idx: usize) -> Option<usize> {
    layout::cpu_meta_addr(idx)
}

pub(crate) fn cpu_area_phys(idx: usize) -> Option<usize> {
    layout::cpu_area_phys(idx)
}

pub fn percpu_data_ptr(idx: usize) -> Option<*mut u8> {
    cpu_area_phys(idx).map(cpu_area_phys_to_virt)
}

/// Contiguous runtime layout of the platform-owned CPU-local data areas.
///
/// The platform publishes this value only after [`initialize_percpu_layout`]
/// has constructed every typed value and immutable prefix in CPU-lifetime
/// storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PerCpuDataLayout {
    /// Virtual address of logical CPU zero's data area.
    pub runtime_base: usize,
    /// Byte distance between adjacent logical CPU data areas.
    pub area_stride: usize,
    /// Number of allocated logical CPU data areas.
    pub area_count: u32,
}

/// Returns the platform-owned contiguous CPU-local data layout.
pub fn percpu_data_layout() -> Option<PerCpuDataLayout> {
    let area_count = u32::try_from(runtime_cpu_count()).ok()?;
    if area_count == 0 {
        return None;
    }
    let runtime_base = percpu_data_ptr(0)? as usize;
    let area_stride = layout::cpu_area_stride();
    let last_offset = area_stride.checked_mul(area_count as usize - 1)?;
    runtime_base.checked_add(last_offset)?;
    Some(PerCpuDataLayout {
        runtime_base,
        area_stride,
        area_count,
    })
}

/// Returns the final mapped stack top without reading unpublished metadata.
///
/// Primary MMU transitions use this pure reserved-layout calculation before
/// [`initialize_percpu_layout`] constructs and publishes [`PerCpuMeta`].
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "x86_64"
))]
pub(crate) fn primary_stack_top_virtual(cpu_index: usize) -> Option<usize> {
    layout::cpu_stack_top(cpu_index).map(|stack_top| cpu_area_phys_to_virt(stack_top) as usize)
}

/// Returns the current hardware CPU ID from the early boot register convention.
///
/// On RISC-V, `sscratch` points to the versioned boot record that owns the hart
/// ID. Before online publication, the platform binder selects LinuxCurrent
/// (`tp` is the boot/current header and `sscratch=0`) or UnikernelTls
/// (`sscratch` is the CPU-area prefix and `tp` is TLS).
pub fn early_current_hart_id() -> usize {
    Arch::cpu_current_hartid()
}

pub fn early_current_cpu_idx() -> usize {
    let hart_id = early_current_hart_id();
    cpu_id_to_idx(hart_id)
        .unwrap_or_else(|| panic!("Current CPU hart id {hart_id:#x} not found in CPU list"))
}

pub fn try_early_cpu_idx() -> Option<usize> {
    cpu_id_to_idx(early_current_hart_id())
}

fn cpu_index_from_mappings<R, F, I>(
    hardware_id: usize,
    runtime_cpu_ids: R,
    early_cpu_ids: F,
) -> Option<usize>
where
    R: Iterator<Item = usize>,
    F: FnOnce() -> I,
    I: Iterator<Item = usize>,
{
    let mut runtime_cpu_ids = runtime_cpu_ids.peekable();
    if runtime_cpu_ids.peek().is_some() {
        return runtime_cpu_ids.position(|id| id == hardware_id);
    }

    early_cpu_ids().position(|id| id == hardware_id)
}

pub fn cpu_id_to_idx(hart_id: usize) -> Option<usize> {
    cpu_index_from_mappings(
        hart_id,
        cpu_meta_list().map(|meta| meta.cpu_id),
        __cpu_id_list,
    )
}

pub fn cpu_idx_to_id(idx: usize) -> Option<usize> {
    if runtime_cpu_count() != 0 {
        return cpu_meta(idx).map(|meta| meta.cpu_id);
    }

    __cpu_id_list().nth(idx)
}

pub fn cpu_count() -> usize {
    let runtime_cpu_count = runtime_cpu_count();
    if runtime_cpu_count != 0 {
        return runtime_cpu_count;
    }

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

fn cpu_area_template_range() -> core::ops::Range<usize> {
    unsafe extern "C" {
        static __CPU_LOCAL_AREA_PREFIX: u8;
        static __CPU_LOCAL_TEMPLATE_END: u8;
    }
    let start = core::ptr::addr_of!(__CPU_LOCAL_AREA_PREFIX) as usize;
    let end = core::ptr::addr_of!(__CPU_LOCAL_TEMPLATE_END) as usize + 1;
    start..end
}

fn cpu_area_template_size() -> Result<usize, PerCpuLayoutError> {
    let range = cpu_area_template_range();
    range
        .end
        .checked_sub(range.start)
        .ok_or(PerCpuLayoutError::MalformedTemplateRange {
            start: range.start,
            end: range.end,
        })
}

fn set_cpu_area_region(start: usize, size: usize, cpu_count: usize) {
    debug_assert_eq!(CPU_AREA_LAYOUT_COUNT.load(Ordering::Relaxed), 0);
    let end = start
        .checked_add(size)
        .expect("the allocator returned a wrapping per-CPU region");
    unsafe {
        CPU_AREA_REGION_START = start;
        CPU_AREA_REGION_END = end;
    }
    CPU_AREA_LAYOUT_COUNT.store(cpu_count, Ordering::Relaxed);
}

fn publish_runtime_cpu_areas(cpu_count: usize) {
    debug_assert_eq!(CPU_AREA_LAYOUT_COUNT.load(Ordering::Relaxed), cpu_count);
    CPU_AREA_RUNTIME_COUNT.store(cpu_count, Ordering::Release);
}

fn initialize_runtime_metadata() {
    let entry_phys =
        crate::mem::virt_to_phys(crate::entry::secondary_entry as *const () as *const u8);
    let entry_virt = crate::mem::__kimage_va(entry_phys) as usize;
    for (cpu_index, hardware_id) in __cpu_id_list().enumerate() {
        let meta_start = cpu_meta_addr(cpu_index)
            .expect("reserved per-CPU metadata slot must remain addressable");
        let stack_top = layout::cpu_stack_top(cpu_index)
            .expect("reserved per-CPU stack slot must remain addressable");
        let meta = PerCpuMeta {
            stack_top,
            cpu_id: hardware_id,
            cpu_idx: cpu_index,
            stack_top_virt: cpu_area_phys_to_virt(stack_top) as usize,
            entry_virt,
            boot_table_paddr: 0,
            primary_table_paddr: 0,
        };
        let meta_va = phys_to_virt(meta_start);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);
        // SAFETY: early allocation reserved this unique raw metadata slot and
        // no consumer can observe it before runtime count publication.
        unsafe { meta_va.cast::<PerCpuMeta>().write(meta) };
    }
}

pub(crate) fn allocated_cpu_count() -> usize {
    CPU_AREA_LAYOUT_COUNT.load(Ordering::Relaxed)
}

/// Physical region that owns every runtime CPU area, metadata record, and stack.
pub(crate) fn cpu_area_region() -> core::ops::Range<usize> {
    unsafe { CPU_AREA_REGION_START..CPU_AREA_REGION_END }
}

fn allocate_cpu_area_region(layout: Layout) -> usize {
    unsafe { crate::mem::ram::flush_to_memory_map(MemoryType::Reserved) };

    let physical_base = unsafe {
        crate::mem::ram::alloc_and_flush_to_memory_map(layout, MemoryType::PerCpuData)
            .expect("validated per-CPU allocation must fit available boot memory")
    };
    // SAFETY: the early bump allocator uniquely owns this complete allocation,
    // and the existing early physical mapping makes it writable. Clearing raw
    // storage prevents stale firmware bytes from being mistaken for values;
    // final-high typed initialization still constructs every Rust object.
    unsafe { crate::mem::phys_to_virt(physical_base).write_bytes(0, layout.size()) };
    physical_base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_alignment_input_does_not_wrap_or_panic() {
        assert_eq!(
            checked_align_up_pow2(usize::MAX, 4096),
            Err(PerCpuLayoutError::AddressOverflow)
        );
    }

    #[test]
    fn runtime_cpu_lookup_does_not_revisit_early_firmware_mapping() {
        let runtime_cpu_ids = [1, 0x103, 0x101, 2];

        let cpu_index = cpu_index_from_mappings(
            0x101,
            runtime_cpu_ids.into_iter(),
            || -> core::iter::Empty<usize> {
                panic!("runtime CPU lookup must not revisit early firmware state")
            },
        );

        assert_eq!(cpu_index, Some(2));
    }
}

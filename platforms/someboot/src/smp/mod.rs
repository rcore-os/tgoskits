use core::{
    alloc::Layout,
    mem::size_of,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
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

const CPU_BOOT_DEAD: u32 = 0;
const CPU_BOOT_KICKED: u32 = 1;
const CPU_BOOT_ALIVE: u32 = 2;
const CPU_BOOT_SHOULD_ONLINE: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CpuBootStatus {
    WaitingForAlive,
    Alive,
}

/// Per-CPU synchronization owned by the generic secondary-boot core.
///
/// This object is deliberately separate from [`PerCpuMeta`]. Metadata is an
/// immutable trampoline ABI after publication, while this object is the sole
/// mutable owner of the `DEAD -> KICKED -> ALIVE -> SHOULD_ONLINE` handshake.
#[repr(C)]
pub(crate) struct CpuBootSync {
    state: AtomicU32,
}

impl CpuBootSync {
    const fn new() -> Self {
        Self {
            state: AtomicU32::new(CPU_BOOT_DEAD),
        }
    }

    fn prepare_kick(&self) -> Result<(), CpuBootPrepareError> {
        self.state
            .compare_exchange(
                CPU_BOOT_DEAD,
                CPU_BOOT_KICKED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|state| CpuBootPrepareError::UnexpectedState { state })
    }

    fn report_alive(&self) -> Result<(), CpuBootPrepareError> {
        self.state
            .compare_exchange(
                CPU_BOOT_KICKED,
                CPU_BOOT_ALIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|state| CpuBootPrepareError::UnexpectedState { state })
    }

    fn status(&self) -> Result<CpuBootStatus, CpuBootPrepareError> {
        match self.state.load(Ordering::Acquire) {
            CPU_BOOT_KICKED => Ok(CpuBootStatus::WaitingForAlive),
            CPU_BOOT_ALIVE => Ok(CpuBootStatus::Alive),
            state => Err(CpuBootPrepareError::UnexpectedState { state }),
        }
    }

    fn release_alive(&self) -> Result<(), CpuBootPrepareError> {
        self.state
            .compare_exchange(
                CPU_BOOT_ALIVE,
                CPU_BOOT_SHOULD_ONLINE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|state| CpuBootPrepareError::UnexpectedState { state })
    }

    fn wait_until_released(&self) {
        while self.state.load(Ordering::Acquire) != CPU_BOOT_SHOULD_ONLINE {
            core::hint::spin_loop();
        }
    }

    #[cfg(test)]
    fn state(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CpuBootPrepareError {
    #[error("logical CPU {cpu_index} has no published boot synchronization")]
    Missing { cpu_index: usize },
    #[error("CPU boot synchronization is in unexpected state {state}")]
    UnexpectedState { state: u32 },
}

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
    cpu_meta_slot(idx)
}

fn cpu_meta_slot(idx: usize) -> Option<PerCpuMeta> {
    let meta_start = cpu_meta_addr(idx)?;
    let meta_va = phys_to_virt(meta_start);
    debug_assert_eq!((meta_va as usize) % meta_align(), 0);
    // SAFETY: callers reach this publication-independent reader only after an
    // Acquire load observed a nonzero runtime count and bound `idx` by that
    // count. The matching Release store happens after every aligned metadata
    // slot is initialized and cache-maintained. CPU identity fields remain
    // immutable after publication.
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

fn cpu_boot_sync(idx: usize) -> Option<&'static CpuBootSync> {
    if idx >= runtime_cpu_count() {
        return None;
    }
    let sync_start = layout::cpu_boot_sync_addr(idx)?;
    let sync_va = phys_to_virt(sync_start);
    // SAFETY: initialization constructs one CpuBootSync in every reserved
    // slot before the Release publication of CPU_AREA_RUNTIME_COUNT. Its
    // address remains stable until shutdown and all mutation is atomic.
    Some(unsafe { &*sync_va.cast::<CpuBootSync>() })
}

pub(crate) fn prepare_secondary_boot(cpu_index: usize) -> Result<(), CpuBootPrepareError> {
    cpu_boot_sync(cpu_index)
        .ok_or(CpuBootPrepareError::Missing { cpu_index })?
        .prepare_kick()
}

pub(crate) fn secondary_boot_status(
    cpu_index: usize,
) -> Result<CpuBootStatus, CpuBootPrepareError> {
    cpu_boot_sync(cpu_index)
        .ok_or(CpuBootPrepareError::Missing { cpu_index })?
        .status()
}

pub(crate) fn release_secondary_boot(cpu_index: usize) -> Result<(), CpuBootPrepareError> {
    release_secondary_boot_from(cpu_boot_sync(cpu_index), cpu_index)
}

fn release_secondary_boot_from(
    sync: Option<&CpuBootSync>,
    cpu_index: usize,
) -> Result<(), CpuBootPrepareError> {
    sync.ok_or(CpuBootPrepareError::Missing { cpu_index })?
        .release_alive()
}

pub(crate) fn synchronize_secondary_boot(cpu_index: usize) {
    let sync = cpu_boot_sync(cpu_index)
        .unwrap_or_else(|| panic!("missing boot synchronization for CPU {cpu_index}"));
    sync.report_alive().unwrap_or_else(|error| {
        panic!("CPU {cpu_index} reported alive from an invalid boot state: {error}")
    });
    sync.wait_until_released();
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

fn cpu_id_to_idx_from_sources<I>(
    hardware_id: usize,
    runtime_count: usize,
    mut meta_at: impl FnMut(usize) -> Option<PerCpuMeta>,
    early_ids: impl FnOnce() -> I,
) -> Option<usize>
where
    I: Iterator<Item = usize>,
{
    if runtime_count == 0 {
        return early_ids().position(|id| id == hardware_id);
    }

    let mut matching_index = None;
    for cpu_index in 0..runtime_count {
        let meta = meta_at(cpu_index)?;
        if meta.cpu_idx != cpu_index {
            return None;
        }
        if meta.cpu_id == hardware_id {
            matching_index = Some(cpu_index);
        }
    }
    matching_index
}

fn cpu_idx_to_id_from_sources<I>(
    cpu_index: usize,
    runtime_count: usize,
    meta_at: impl FnOnce(usize) -> Option<PerCpuMeta>,
    early_ids: impl FnOnce() -> I,
) -> Option<usize>
where
    I: Iterator<Item = usize>,
{
    if runtime_count == 0 {
        return early_ids().nth(cpu_index);
    }
    if cpu_index >= runtime_count {
        return None;
    }

    let meta = meta_at(cpu_index)?;
    (meta.cpu_idx == cpu_index).then_some(meta.cpu_id)
}

fn cpu_count_from_sources<I>(runtime_count: usize, early_ids: impl FnOnce() -> I) -> usize
where
    I: Iterator<Item = usize>,
{
    if runtime_count == 0 {
        early_ids().count()
    } else {
        runtime_count
    }
}

pub fn cpu_id_to_idx(hardware_id: usize) -> Option<usize> {
    let runtime_count = runtime_cpu_count();
    cpu_id_to_idx_from_sources(hardware_id, runtime_count, cpu_meta_slot, __cpu_id_list)
}

pub fn cpu_idx_to_id(cpu_index: usize) -> Option<usize> {
    let runtime_count = runtime_cpu_count();
    cpu_idx_to_id_from_sources(cpu_index, runtime_count, cpu_meta_slot, __cpu_id_list)
}

pub fn cpu_count() -> usize {
    let runtime_count = runtime_cpu_count();
    cpu_count_from_sources(runtime_count, __cpu_id_list)
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
        let sync_start = layout::cpu_boot_sync_addr(cpu_index)
            .expect("reserved per-CPU boot synchronization slot must remain addressable");
        let meta_va = phys_to_virt(meta_start);
        let sync_va = phys_to_virt(sync_start);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);
        // SAFETY: early allocation reserved this unique raw metadata slot and
        // no consumer can observe it before runtime count publication.
        unsafe { meta_va.cast::<PerCpuMeta>().write(meta) };
        // SAFETY: every CPU area owns exactly one disjoint synchronization
        // slot and runtime publication has not occurred yet.
        unsafe { sync_va.cast::<CpuBootSync>().write(CpuBootSync::new()) };
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

    fn runtime_metadata<const N: usize>(hardware_ids: [usize; N]) -> [PerCpuMeta; N] {
        core::array::from_fn(|cpu_index| PerCpuMeta {
            stack_top: 0,
            cpu_id: hardware_ids[cpu_index],
            cpu_idx: cpu_index,
            stack_top_virt: 0,
            entry_virt: 0,
            boot_table_paddr: 0,
            primary_table_paddr: 0,
        })
    }

    #[test]
    fn boot_sync_release_requires_the_matching_cpu_to_report_alive() {
        let first = CpuBootSync::new();
        let second = CpuBootSync::new();

        first.prepare_kick().unwrap();
        second.prepare_kick().unwrap();
        second.report_alive().unwrap();

        assert_eq!(
            first.release_alive(),
            Err(CpuBootPrepareError::UnexpectedState {
                state: CPU_BOOT_KICKED
            })
        );
        second.release_alive().unwrap();
        assert_eq!(first.state(), CPU_BOOT_KICKED);
        assert_eq!(second.state(), CPU_BOOT_SHOULD_ONLINE);
    }

    #[test]
    fn boot_sync_observation_does_not_release_an_alive_cpu() {
        let sync = CpuBootSync::new();

        sync.prepare_kick().unwrap();
        assert_eq!(sync.status(), Ok(CpuBootStatus::WaitingForAlive));

        sync.report_alive().unwrap();
        assert_eq!(sync.status(), Ok(CpuBootStatus::Alive));
        assert_eq!(sync.state(), CPU_BOOT_ALIVE);
    }

    #[test]
    fn boot_sync_rejects_a_cpu_already_released_to_online_startup() {
        let sync = CpuBootSync::new();
        sync.prepare_kick().unwrap();
        sync.report_alive().unwrap();
        sync.release_alive().unwrap();

        assert_eq!(
            sync.prepare_kick(),
            Err(CpuBootPrepareError::UnexpectedState {
                state: CPU_BOOT_SHOULD_ONLINE
            })
        );
    }

    #[test]
    fn boot_sync_rejects_alive_before_the_cpu_is_kicked() {
        let sync = CpuBootSync::new();

        assert_eq!(
            sync.report_alive(),
            Err(CpuBootPrepareError::UnexpectedState {
                state: CPU_BOOT_DEAD
            })
        );
    }

    #[test]
    fn releasing_an_unpublished_cpu_returns_a_typed_error() {
        assert_eq!(
            release_secondary_boot_from(None, usize::MAX),
            Err(CpuBootPrepareError::Missing {
                cpu_index: usize::MAX
            })
        );
    }

    #[test]
    fn extreme_alignment_input_does_not_wrap_or_panic() {
        assert_eq!(
            checked_align_up_pow2(usize::MAX, 4096),
            Err(PerCpuLayoutError::AddressOverflow)
        );
    }

    #[test]
    fn unpublished_cpu_count_uses_early_ids() {
        assert_eq!(cpu_count_from_sources(0, || [2, 0, 1, 3].into_iter()), 4);
    }

    #[test]
    fn published_cpu_count_does_not_query_early_ids() {
        assert_eq!(
            cpu_count_from_sources(4, || -> core::array::IntoIter<usize, 0> {
                panic!("early CPU IDs queried after publication")
            }),
            4
        );
    }

    #[test]
    fn unpublished_hardware_id_lookup_uses_early_ids() {
        assert_eq!(
            cpu_id_to_idx_from_sources(
                1,
                0,
                |_| panic!("runtime metadata queried before publication"),
                || [2, 0, 1, 3].into_iter(),
            ),
            Some(2)
        );
    }

    #[test]
    fn published_hardware_id_lookup_does_not_query_early_ids() {
        let metadata = runtime_metadata([2, 0, 1, 3]);

        assert_eq!(
            cpu_id_to_idx_from_sources(
                1,
                metadata.len(),
                |slot| metadata.get(slot).copied(),
                || -> core::array::IntoIter<usize, 0> {
                    panic!("early CPU IDs queried after publication")
                },
            ),
            Some(2)
        );
    }

    #[test]
    fn published_hardware_id_lookup_rejects_missing_or_inconsistent_metadata() {
        let metadata = runtime_metadata([2, 0, 1, 3]);
        assert_eq!(
            cpu_id_to_idx_from_sources(
                7,
                metadata.len(),
                |slot| metadata.get(slot).copied(),
                || -> core::array::IntoIter<usize, 0> {
                    panic!("early CPU IDs queried after publication")
                },
            ),
            None
        );

        let mut inconsistent = metadata;
        inconsistent[1].cpu_idx = 2;
        assert_eq!(
            cpu_id_to_idx_from_sources(
                3,
                inconsistent.len(),
                |slot| inconsistent.get(slot).copied(),
                || -> core::array::IntoIter<usize, 0> {
                    panic!("early CPU IDs queried after publication")
                },
            ),
            None
        );
    }

    #[test]
    fn published_hardware_id_lookup_validates_slots_after_the_match() {
        let mut metadata = runtime_metadata([2, 0, 1, 3]);
        metadata[3].cpu_idx = 2;

        assert_eq!(
            cpu_id_to_idx_from_sources(
                2,
                metadata.len(),
                |slot| metadata.get(slot).copied(),
                || -> core::array::IntoIter<usize, 0> {
                    panic!("early CPU IDs queried after publication")
                },
            ),
            None
        );
    }

    #[test]
    fn unpublished_logical_index_lookup_uses_early_ids() {
        assert_eq!(
            cpu_idx_to_id_from_sources(
                2,
                0,
                |_| panic!("runtime metadata queried before publication"),
                || [2, 0, 1, 3].into_iter(),
            ),
            Some(1)
        );
    }

    #[test]
    fn published_logical_index_lookup_does_not_query_early_ids() {
        let metadata = runtime_metadata([2, 0, 1, 3]);

        assert_eq!(
            cpu_idx_to_id_from_sources(
                2,
                metadata.len(),
                |slot| metadata.get(slot).copied(),
                || -> core::array::IntoIter<usize, 0> {
                    panic!("early CPU IDs queried after publication")
                },
            ),
            Some(1)
        );
    }

    #[test]
    fn published_logical_index_lookup_rejects_inconsistent_metadata() {
        let mut metadata = runtime_metadata([2, 0, 1, 3]);
        metadata[2].cpu_idx = 1;

        assert_eq!(
            cpu_idx_to_id_from_sources(
                2,
                metadata.len(),
                |slot| metadata.get(slot).copied(),
                || -> core::array::IntoIter<usize, 0> {
                    panic!("early CPU IDs queried after publication")
                },
            ),
            None
        );
    }
}

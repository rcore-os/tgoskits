//! Cache, TLB, and modified-text synchronization helpers.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};

static KERNEL_TLB_GENERATION: AtomicU64 = AtomicU64::new(0);
static KERNEL_TLB_READY_CPUS: AtomicUsize = AtomicUsize::new(0);
static ADDRESS_SPACE_TAG_CAPACITY: AtomicU32 = AtomicU32::new(u32::MAX);
static FROZEN_ADDRESS_SPACE_TAG_CAPACITY: AtomicU32 = AtomicU32::new(0);

// The range API is normalized to 4 KiB pages. x86_64 and RISC-V use the
// current Linux defaults; the other backends keep the page-table engine's
// existing 32-entry bound until an architecture-specific cost model exists.
#[cfg(target_arch = "x86_64")]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 33;
#[cfg(target_arch = "riscv64")]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 64;
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 32;
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
)))]
const TLB_SINGLE_PAGE_FLUSH_CEILING: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TlbRangeFlushMode {
    Pages,
    Full,
}

fn tlb_range_flush_mode(size: usize) -> TlbRangeFlushMode {
    if size.div_ceil(PAGE_SIZE_4K) > TLB_SINGLE_PAGE_FLUSH_CEILING {
        TlbRangeFlushMode::Full
    } else {
        TlbRangeFlushMode::Pages
    }
}

/// Failure while synchronously invalidating a kernel TLB range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TlbShootdownError {
    /// The target CPU is offline.
    #[error("target CPU is offline")]
    CpuOffline,
    /// The synchronous cross-CPU call timed out.
    #[error("cross-CPU TLB shootdown timed out")]
    Timeout,
    /// This configuration has no cross-CPU invalidation backend.
    #[error("cross-CPU TLB shootdown is not supported")]
    Unsupported,
    /// The platform rejected the cross-CPU operation.
    #[error("platform rejected the cross-CPU TLB shootdown")]
    Platform,
    /// The monotonic kernel TLB generation can no longer advance.
    #[error("kernel TLB generation is exhausted")]
    GenerationExhausted,
}

fn advance_kernel_tlb_generation() -> Result<u64, TlbShootdownError> {
    KERNEL_TLB_GENERATION
        .try_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
            generation.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| TlbShootdownError::GenerationExhausted)
}

fn publish_cpu_tlb_ready_with(
    cpu_id: usize,
    generation: &AtomicU64,
    ready_cpus: &AtomicUsize,
    mut flush_all: impl FnMut(),
) -> Result<(), TlbShootdownError> {
    let cpu_bit = 1usize
        .checked_shl(cpu_id as u32)
        .ok_or(TlbShootdownError::Platform)?;
    loop {
        let observed = generation.load(Ordering::Acquire);
        flush_all();
        ready_cpus.fetch_or(cpu_bit, Ordering::AcqRel);
        if generation.load(Ordering::Acquire) == observed {
            return Ok(());
        }
        // A page-table publisher raced with this transition and may have
        // snapshotted the ready mask before our bit became visible. Withdraw
        // the bit, flush the newer generation, and publish again.
        ready_cpus.fetch_and(!cpu_bit, Ordering::AcqRel);
    }
}

fn withdraw_cpu_tlb_ready_with(
    cpu_id: usize,
    generation: &AtomicU64,
    ready_cpus: &AtomicUsize,
    mut flush_all: impl FnMut(),
) -> Result<(), TlbShootdownError> {
    let cpu_bit = 1usize
        .checked_shl(cpu_id as u32)
        .ok_or(TlbShootdownError::Platform)?;
    ready_cpus.fetch_and(!cpu_bit, Ordering::AcqRel);
    loop {
        let observed = generation.load(Ordering::Acquire);
        flush_all();
        if generation.load(Ordering::Acquire) == observed
            && ready_cpus.load(Ordering::Acquire) & cpu_bit == 0
        {
            return Ok(());
        }
        // A re-online transition or a kernel mapping publication raced with
        // the flush. Keep the CPU excluded and cover the newer generation.
        ready_cpus.fetch_and(!cpu_bit, Ordering::AcqRel);
    }
}

fn publish_address_space_tag_capacity_with(
    capacity: u32,
    aggregate: &AtomicU32,
) -> Result<u32, TlbShootdownError> {
    if capacity == 0 || !capacity.is_power_of_two() {
        return Err(TlbShootdownError::Platform);
    }
    let previous = aggregate.fetch_min(capacity, Ordering::AcqRel);
    Ok(previous.min(capacity))
}

fn publish_current_cpu_address_space_tag_capacity() -> Result<u32, TlbShootdownError> {
    let local_capacity = ax_cpu::asm::address_space_tag_capacity(crate::cpu_num());
    let aggregate =
        publish_address_space_tag_capacity_with(local_capacity, &ADDRESS_SPACE_TAG_CAPACITY)?;
    let frozen = FROZEN_ADDRESS_SPACE_TAG_CAPACITY.load(Ordering::Acquire);
    if frozen != 0 && local_capacity < frozen {
        // The allocator may already have issued tags that this CPU cannot
        // represent. Linux rejects an ASID-width mismatch instead of silently
        // truncating a live context; keep this CPU outside the ready mask.
        return Err(TlbShootdownError::Platform);
    }
    Ok(aggregate)
}

/// A current-CPU capability probe completed while the CPU was still
/// unavailable to normal tasks and cross-CPU TLB requests.
#[must_use = "the CPU remains unavailable for TLB requests until this token is published"]
pub struct CurrentCpuTlbPreparation {
    cpu_id: usize,
}

impl CurrentCpuTlbPreparation {
    /// Returns the logical CPU covered by this preparation.
    pub const fn cpu_id(&self) -> usize {
        self.cpu_id
    }
}

/// Probes the current CPU's address-space-tag capability before it is online.
///
/// Architectures such as RISC-V discover the implemented ASID width by
/// temporarily writing the address-space register. The caller must therefore
/// invoke this after per-CPU state exists but before enabling local interrupts
/// or making the CPU available to the scheduler.
pub fn prepare_current_cpu_tlb() -> Result<CurrentCpuTlbPreparation, TlbShootdownError> {
    let cpu_id = crate::percpu::this_cpu_id();
    let _ = 1usize
        .checked_shl(cpu_id as u32)
        .ok_or(TlbShootdownError::Platform)?;
    publish_current_cpu_address_space_tag_capacity()?;
    Ok(CurrentCpuTlbPreparation { cpu_id })
}

/// A CPU has left the kernel TLB-ready set after switching away from every
/// userspace root and covering a stable kernel mapping generation.
#[must_use = "dropping this token deliberately leaves the CPU offline"]
pub struct CurrentCpuTlbOffline {
    cpu_id: usize,
}

impl CurrentCpuTlbOffline {
    /// Returns the logical CPU withdrawn by this token.
    pub const fn cpu_id(&self) -> usize {
        self.cpu_id
    }

    /// Re-probes this CPU before a future re-online transition.
    ///
    /// The caller must satisfy the same interrupt and scheduler exclusion
    /// requirements as [`prepare_current_cpu_tlb`].
    pub fn prepare_online(self) -> Result<CurrentCpuTlbPreparation, TlbShootdownError> {
        if crate::percpu::this_cpu_id() != self.cpu_id {
            return Err(TlbShootdownError::Platform);
        }
        prepare_current_cpu_tlb()
    }
}

/// Returns the address-space-tag capacity shared by every prepared CPU.
///
/// Capacity includes reserved tag zero. A value of one selects the portable
/// full-flush mode. Before any CPU publishes a capability, this function also
/// returns one rather than exposing the internal uninitialized sentinel.
pub fn address_space_tag_capacity() -> u32 {
    let frozen = FROZEN_ADDRESS_SPACE_TAG_CAPACITY.load(Ordering::Acquire);
    if frozen != 0 {
        return frozen;
    }
    match ADDRESS_SPACE_TAG_CAPACITY.load(Ordering::Acquire) {
        u32::MAX => 1,
        capacity => capacity,
    }
}

/// Freezes the system-wide tag capacity before the first MM tag allocation.
///
/// CPUs prepared after this point must support at least this many tags or they
/// cannot enter the TLB-ready set. This mirrors Linux's rule that one live ASID
/// allocator cannot mix incompatible CPU ASID widths.
pub fn freeze_address_space_tag_capacity() -> u32 {
    let discovered = address_space_tag_capacity();
    match FROZEN_ADDRESS_SPACE_TAG_CAPACITY.compare_exchange(
        0,
        discovered,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => discovered,
        Err(frozen) => frozen,
    }
}

/// Publishes that the current CPU may access dynamic kernel mappings.
///
/// The runtime must initialize synchronous IPI delivery after obtaining
/// `preparation` and before calling this function. A full local flush is
/// performed before the ready bit becomes visible, and a generation recheck
/// closes the race with an in-progress kernel mapping mutation. CPUs not
/// present in the ready mask are excluded from shootdown snapshots.
pub fn publish_current_cpu_tlb_ready(
    preparation: CurrentCpuTlbPreparation,
) -> Result<(), TlbShootdownError> {
    if crate::percpu::this_cpu_id() != preparation.cpu_id {
        return Err(TlbShootdownError::Platform);
    }
    publish_cpu_tlb_ready_with(
        preparation.cpu_id,
        &KERNEL_TLB_GENERATION,
        &KERNEL_TLB_READY_CPUS,
        || ax_cpu::asm::flush_tlb(None),
    )
}

/// Withdraws the current CPU from kernel TLB shootdown snapshots.
///
/// # Safety
///
/// The caller must already have installed the permanent kernel root, released
/// every userspace activation lease for this CPU, and disabled migration. The
/// CPU must not access mappings that can be retired after this function. IPI
/// delivery must remain operational until the stable-generation flush returns.
pub unsafe fn withdraw_current_cpu_tlb_ready() -> Result<CurrentCpuTlbOffline, TlbShootdownError> {
    let cpu_id = crate::percpu::this_cpu_id();
    withdraw_cpu_tlb_ready_with(
        cpu_id,
        &KERNEL_TLB_GENERATION,
        &KERNEL_TLB_READY_CPUS,
        || ax_cpu::asm::flush_tlb(None),
    )?;
    Ok(CurrentCpuTlbOffline { cpu_id })
}

/// Flushes the TLB entries covering a virtual-address range on the current CPU.
pub fn flush_tlb_range(start: VirtAddr, size: usize) {
    if size == 0 {
        return;
    }
    if tlb_range_flush_mode(size) == TlbRangeFlushMode::Full {
        ax_cpu::asm::flush_tlb(None);
        return;
    }
    for offset in (0..size).step_by(PAGE_SIZE_4K) {
        ax_cpu::asm::flush_tlb(Some(start + offset));
    }
}

fn update_mmu_cache_with(vaddr: VirtAddr, update: impl FnOnce(VirtAddr)) {
    update(vaddr.align_down_4k());
}

/// Synchronizes a page-table update performed by the local page-fault handler.
///
/// This is the architecture boundary corresponding to Linux's
/// `update_mmu_cache()`: it is intentionally local and must not be replaced by
/// a cross-CPU shootdown. Architectures that do not cache invalid translations
/// implement it as a no-op.
#[inline]
pub fn update_mmu_cache(vaddr: VirtAddr) {
    update_mmu_cache_with(vaddr, ax_cpu::asm::update_mmu_cache);
}

/// Flushes a virtual-address range on the caller and every TLB-ready CPU.
///
/// The caller advances the kernel mapping generation before selecting the
/// ready mask. A CPU publishes itself ready only after a local full flush and
/// rechecks that generation, so a CPU racing with this update cannot miss it.
pub fn flush_tlb_range_all_cpus(start: VirtAddr, size: usize) -> Result<(), TlbShootdownError> {
    #[cfg(feature = "ipi")]
    let _guard = ax_sync::PreemptGuard::new();
    advance_kernel_tlb_generation()?;
    let current_cpu = crate::percpu::this_cpu_id();
    let current_bit = 1usize
        .checked_shl(current_cpu as u32)
        .ok_or(TlbShootdownError::Platform)?;
    let cpu_mask = KERNEL_TLB_READY_CPUS.load(Ordering::Acquire) | current_bit;
    flush_tlb_range_on_cpus_with(&AxHalTlbShootdown, cpu_mask, start, size)
}

/// Flushes a TLB range on the CPUs selected by `cpu_mask`.
///
/// Bit `n` targets logical CPU `n`. Every selected CPU must be online. An
/// offline target is rejected before any invalidation is performed so callers
/// cannot acknowledge an address-space receipt for a CPU that did not flush.
pub fn flush_tlb_range_on_cpus(
    cpu_mask: usize,
    start: VirtAddr,
    size: usize,
) -> Result<(), TlbShootdownError> {
    #[cfg(feature = "ipi")]
    let _guard = ax_sync::PreemptGuard::new();
    flush_tlb_range_on_cpus_with(&AxHalTlbShootdown, cpu_mask, start, size)
}

/// Flushes every address translation on the CPUs selected by `cpu_mask`.
///
/// Keeping a distinct entry point makes a full-flush obligation explicit to
/// callers that have no finite virtual range (for example a root replacement
/// or an address-space tag rollover).  The implementation still goes through
/// the same synchronous shootdown protocol, so timeout/offline/unsupported
/// errors remain observable.
pub fn flush_tlb_all_on_cpus(cpu_mask: usize) -> Result<(), TlbShootdownError> {
    flush_tlb_range_on_cpus(cpu_mask, VirtAddr::from(0), usize::MAX)
}

trait TlbShootdown {
    fn cpu_count(&self) -> usize;
    fn current_cpu(&self) -> usize;
    fn cpu_online(&self, cpu_id: usize) -> bool;
    fn flush_remote(
        &self,
        cpu_id: usize,
        start: VirtAddr,
        size: usize,
    ) -> Result<(), TlbShootdownError>;
    fn flush_local(&self, start: VirtAddr, size: usize);
}

struct AxHalTlbShootdown;

impl TlbShootdown for AxHalTlbShootdown {
    fn cpu_count(&self) -> usize {
        crate::cpu_num()
    }

    fn current_cpu(&self) -> usize {
        crate::percpu::this_cpu_id()
    }

    fn cpu_online(&self, cpu_id: usize) -> bool {
        crate::irq::is_cpu_online(cpu_id)
    }

    fn flush_remote(
        &self,
        cpu_id: usize,
        start: VirtAddr,
        size: usize,
    ) -> Result<(), TlbShootdownError> {
        #[cfg(feature = "ipi")]
        {
            let arg = FlushRangeArg {
                start: start.as_usize(),
                size,
            };
            let arg_ptr = &arg as *const FlushRangeArg as *mut ();
            unsafe {
                crate::irq::run_on_cpu_sync(
                    crate::irq::CpuId(cpu_id),
                    flush_tlb_range_thunk,
                    arg_ptr,
                )
            }
            .map_err(|err| match err {
                crate::irq::IrqError::CpuOffline => TlbShootdownError::CpuOffline,
                crate::irq::IrqError::Timeout => TlbShootdownError::Timeout,
                crate::irq::IrqError::Unsupported => TlbShootdownError::Unsupported,
                _ => TlbShootdownError::Platform,
            })
        }
        #[cfg(not(feature = "ipi"))]
        {
            let _ = (cpu_id, start, size);
            Err(TlbShootdownError::Unsupported)
        }
    }

    fn flush_local(&self, start: VirtAddr, size: usize) {
        flush_tlb_range(start, size);
    }
}

fn flush_tlb_range_on_cpus_with(
    runtime: &impl TlbShootdown,
    cpu_mask: usize,
    start: VirtAddr,
    size: usize,
) -> Result<(), TlbShootdownError> {
    let current_cpu = runtime.current_cpu();
    for cpu_id in 0..runtime.cpu_count() {
        let selected = cpu_id < usize::BITS as usize && cpu_mask & (1usize << cpu_id) != 0;
        if selected && !runtime.cpu_online(cpu_id) {
            return Err(TlbShootdownError::CpuOffline);
        }
    }
    for cpu_id in 0..runtime.cpu_count() {
        let selected = cpu_id < usize::BITS as usize && cpu_mask & (1usize << cpu_id) != 0;
        if !selected || cpu_id == current_cpu {
            continue;
        }
        runtime.flush_remote(cpu_id, start, size)?;
    }
    if current_cpu < usize::BITS as usize && cpu_mask & (1usize << current_cpu) != 0 {
        runtime.flush_local(start, size);
    }
    Ok(())
}

#[cfg(feature = "ipi")]
struct FlushRangeArg {
    start: usize,
    size: usize,
}

#[cfg(feature = "ipi")]
unsafe fn flush_tlb_range_thunk(arg: *mut ()) {
    let arg = unsafe { &*(arg as *const FlushRangeArg) };
    flush_tlb_range(VirtAddr::from(arg.start), arg.size);
}

/// Flushes the entire instruction cache on the current CPU.
pub fn flush_icache_all() {
    ax_cpu::asm::flush_icache_all();
}

/// Flushes the entire instruction cache on all available CPUs.
pub fn flush_icache_all_cpus() {
    #[cfg(feature = "ipi")]
    {
        let _guard = ax_sync::PreemptGuard::new();
        let current_cpu = crate::percpu::this_cpu_id();

        for cpu_id in 0..crate::cpu_num() {
            if cpu_id == current_cpu {
                continue;
            }
            let _ = unsafe {
                crate::irq::run_on_cpu_sync(
                    crate::irq::CpuId(cpu_id),
                    flush_icache_all_thunk,
                    core::ptr::null_mut(),
                )
            };
        }
        flush_icache_all();
    }
    #[cfg(not(feature = "ipi"))]
    {
        flush_icache_all();
    }
}

#[cfg(feature = "ipi")]
unsafe fn flush_icache_all_thunk(_arg: *mut ()) {
    flush_icache_all();
}

/// Cleans a data-cache range to the point of unification when needed.
pub fn clean_dcache_to_pou(vaddr: VirtAddr, size: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        ax_cpu::asm::clean_dcache_range_to_pou(vaddr, size);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (vaddr, size);
    }
}

/// Synchronizes modified kernel text with the local execution pipeline.
pub fn sync_kernel_text(start: VirtAddr, size: usize) {
    flush_tlb_range(start, size);
    flush_icache_all();
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn local_mmu_cache_update_aligns_the_fault_address_once() {
        let calls = Cell::new(0);
        let observed = Cell::new(VirtAddr::from(0));

        update_mmu_cache_with(VirtAddr::from(0x4567), |vaddr| {
            calls.set(calls.get() + 1);
            observed.set(vaddr);
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(observed.get(), VirtAddr::from(0x4000));
    }

    struct ModelShootdown {
        online: [bool; 3],
        remote_error: Option<TlbShootdownError>,
        remote_cpu: Cell<Option<usize>>,
        local_flushed: Cell<bool>,
    }

    impl TlbShootdown for ModelShootdown {
        fn cpu_count(&self) -> usize {
            self.online.len()
        }

        fn current_cpu(&self) -> usize {
            0
        }

        fn cpu_online(&self, cpu_id: usize) -> bool {
            self.online[cpu_id]
        }

        fn flush_remote(
            &self,
            cpu_id: usize,
            _start: VirtAddr,
            _size: usize,
        ) -> Result<(), TlbShootdownError> {
            self.remote_cpu.set(Some(cpu_id));
            self.remote_error.map_or(Ok(()), Err)
        }

        fn flush_local(&self, _start: VirtAddr, _size: usize) {
            self.local_flushed.set(true);
        }
    }

    #[test]
    fn all_cpu_tlb_shootdown_propagates_remote_failure() {
        let runtime = ModelShootdown {
            online: [true; 3],
            remote_error: Some(TlbShootdownError::Timeout),
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, usize::MAX, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Err(TlbShootdownError::Timeout));
        assert_eq!(runtime.remote_cpu.get(), Some(1));
        assert!(!runtime.local_flushed.get());
    }

    #[test]
    fn selected_offline_cpu_cannot_be_silently_acknowledged() {
        let runtime = ModelShootdown {
            online: [true, false, true],
            remote_error: None,
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, usize::MAX, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Err(TlbShootdownError::CpuOffline));
        assert_eq!(runtime.remote_cpu.get(), None);
        assert!(!runtime.local_flushed.get());
    }

    #[test]
    fn targeted_tlb_shootdown_rejects_selected_offline_cpu() {
        let runtime = ModelShootdown {
            online: [true, false, true],
            remote_error: None,
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result = flush_tlb_range_on_cpus_with(
            &runtime,
            (1usize << 0) | (1usize << 1),
            VirtAddr::from(0x4000),
            0x2000,
        );

        assert_eq!(result, Err(TlbShootdownError::CpuOffline));
        assert_eq!(runtime.remote_cpu.get(), None);
        assert!(!runtime.local_flushed.get());
    }

    #[test]
    fn targeted_tlb_shootdown_skips_unselected_remote_and_local_cpus() {
        let runtime = ModelShootdown {
            online: [true; 3],
            remote_error: None,
            remote_cpu: Cell::new(None),
            local_flushed: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, 1usize << 2, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Ok(()));
        assert_eq!(runtime.remote_cpu.get(), Some(2));
        assert!(!runtime.local_flushed.get());
    }

    #[test]
    fn large_tlb_ranges_switch_to_one_full_invalidation() {
        assert_eq!(tlb_range_flush_mode(0), TlbRangeFlushMode::Pages);
        assert_eq!(
            tlb_range_flush_mode(TLB_SINGLE_PAGE_FLUSH_CEILING * PAGE_SIZE_4K),
            TlbRangeFlushMode::Pages
        );
        assert_eq!(
            tlb_range_flush_mode((TLB_SINGLE_PAGE_FLUSH_CEILING + 1) * PAGE_SIZE_4K),
            TlbRangeFlushMode::Full
        );
    }

    #[test]
    fn cpu_ready_publication_reflushes_a_racing_generation() {
        let generation = AtomicU64::new(7);
        let ready_cpus = AtomicUsize::new(0);
        let flushes = Cell::new(0);

        publish_cpu_tlb_ready_with(1, &generation, &ready_cpus, || {
            let current = flushes.get();
            flushes.set(current + 1);
            if current == 0 {
                generation.fetch_add(1, Ordering::Release);
            }
        })
        .unwrap();

        assert_eq!(flushes.get(), 2);
        assert_eq!(ready_cpus.load(Ordering::Acquire), 1usize << 1);
    }

    #[test]
    fn cpu_ready_publication_rejects_unrepresentable_cpu_ids() {
        let generation = AtomicU64::new(0);
        let ready_cpus = AtomicUsize::new(0);
        assert_eq!(
            publish_cpu_tlb_ready_with(usize::BITS as usize, &generation, &ready_cpus, || {}),
            Err(TlbShootdownError::Platform)
        );
    }

    #[test]
    fn cpu_offline_withdrawal_reflushes_a_racing_generation() {
        let generation = AtomicU64::new(11);
        let ready_cpus = AtomicUsize::new(1usize << 1);
        let flushes = Cell::new(0);

        withdraw_cpu_tlb_ready_with(1, &generation, &ready_cpus, || {
            let current = flushes.get();
            flushes.set(current + 1);
            if current == 0 {
                generation.fetch_add(1, Ordering::Release);
            }
        })
        .unwrap();

        assert_eq!(flushes.get(), 2);
        assert_eq!(ready_cpus.load(Ordering::Acquire), 0);
    }

    #[test]
    fn tag_capacity_uses_the_smallest_online_cpu_capability() {
        let capacity = AtomicU32::new(u32::MAX);
        assert_eq!(
            publish_address_space_tag_capacity_with(1 << 16, &capacity),
            Ok(1 << 16)
        );
        assert_eq!(
            publish_address_space_tag_capacity_with(1 << 8, &capacity),
            Ok(1 << 8)
        );
        assert_eq!(capacity.load(Ordering::Acquire), 1 << 8);
    }
}

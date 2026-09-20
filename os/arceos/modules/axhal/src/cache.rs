//! Cache, TLB, and modified-text synchronization helpers.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use ax_cpu::cache::flush_icache_all;
pub use ax_memory_addr::VirtAddr;

static KERNEL_TLB_GENERATION: AtomicU64 = AtomicU64::new(0);
static KERNEL_TLB_READY_CPUS: AtomicUsize = AtomicUsize::new(0);
static ADDRESS_SPACE_TAG_CAPACITY: AtomicU32 = AtomicU32::new(u32::MAX);
static FROZEN_ADDRESS_SPACE_TAG_CAPACITY: AtomicU32 = AtomicU32::new(0);

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
    let local_capacity = crate::KernelMmu::address_space_tag_capacity();
    // Linux's RISC-V allocator requires more than twice the possible CPU
    // count. This is runtime allocation policy, not a CPU capability probe.
    #[cfg(target_arch = "riscv64")]
    let local_capacity = if local_capacity as usize > crate::cpu_num().saturating_mul(2) {
        local_capacity
    } else {
        1
    };
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
        || crate::KernelMmu::flush_tlb(None),
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
        || crate::KernelMmu::flush_tlb(None),
    )?;
    Ok(CurrentCpuTlbOffline { cpu_id })
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
    fn synchronize_page_table_writes(&self);
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

    fn synchronize_page_table_writes(&self) {
        #[cfg(target_arch = "aarch64")]
        ax_cpu::barrier::synchronize_page_table_writes();
        #[cfg(not(target_arch = "aarch64"))]
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
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
        crate::KernelMmu::flush_tlb_range(start, size);
    }
}

fn flush_tlb_range_on_cpus_with(
    runtime: &impl TlbShootdown,
    cpu_mask: usize,
    start: VirtAddr,
    size: usize,
) -> Result<(), TlbShootdownError> {
    // Publish the initiating CPU's PTE writes before any local invalidation or
    // remote IPI. In particular, an AArch64 DSB executed by the remote CPU
    // cannot order stores performed here. Reclaim is legal only after this
    // publication edge and every selected invalidation has completed.
    runtime.synchronize_page_table_writes();
    let current_cpu = runtime.current_cpu();

    let mut first_error = None;
    for cpu_id in 0..runtime.cpu_count() {
        let selected = cpu_id < usize::BITS as usize && cpu_mask & (1usize << cpu_id) != 0;
        if selected && !runtime.cpu_online(cpu_id) {
            return Err(TlbShootdownError::CpuOffline);
        }
    }
    if current_cpu < usize::BITS as usize && cpu_mask & (1usize << current_cpu) != 0 {
        runtime.flush_local(start, size);
    }
    for cpu_id in 0..runtime.cpu_count() {
        let selected = cpu_id < usize::BITS as usize && cpu_mask & (1usize << cpu_id) != 0;
        if !selected || cpu_id == current_cpu {
            continue;
        }
        if let Err(error) = runtime.flush_remote(cpu_id, start, size)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(feature = "ipi")]
struct FlushRangeArg {
    start: usize,
    size: usize,
}

#[cfg(feature = "ipi")]
unsafe fn flush_tlb_range_thunk(arg: *mut ()) {
    let arg = unsafe { &*(arg as *const FlushRangeArg) };
    crate::KernelMmu::flush_tlb_range(VirtAddr::from(arg.start), arg.size);
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

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    struct ModelShootdown {
        online: [bool; 3],
        remote_error: Option<TlbShootdownError>,
        remote_mask: Cell<usize>,
        local_flushed: Cell<bool>,
        writes_synchronized: Cell<bool>,
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

        fn synchronize_page_table_writes(&self) {
            assert!(
                !self.writes_synchronized.replace(true),
                "one shootdown transaction must publish PTE writes exactly once"
            );
        }

        fn flush_remote(
            &self,
            cpu_id: usize,
            _start: VirtAddr,
            _size: usize,
        ) -> Result<(), TlbShootdownError> {
            assert!(
                self.writes_synchronized.get(),
                "PTE writes must be published before a remote invalidation"
            );
            self.remote_mask
                .set(self.remote_mask.get() | (1usize << cpu_id));
            self.remote_error.map_or(Ok(()), Err)
        }

        fn flush_local(&self, _start: VirtAddr, _size: usize) {
            assert!(
                self.writes_synchronized.get(),
                "PTE writes must be published before a local invalidation"
            );
            self.local_flushed.set(true);
        }
    }

    #[test]
    fn all_cpu_tlb_shootdown_propagates_remote_failure() {
        let runtime = ModelShootdown {
            online: [true; 3],
            remote_error: Some(TlbShootdownError::Timeout),
            remote_mask: Cell::new(0),
            local_flushed: Cell::new(false),
            writes_synchronized: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, usize::MAX, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Err(TlbShootdownError::Timeout));
        assert_eq!(runtime.remote_mask.get(), (1usize << 1) | (1usize << 2));
        assert!(
            runtime.local_flushed.get(),
            "a remote failure must not skip the current CPU invalidation"
        );
        assert!(runtime.writes_synchronized.get());
    }

    #[test]
    fn selected_offline_cpu_cannot_be_silently_acknowledged() {
        let runtime = ModelShootdown {
            online: [true, false, true],
            remote_error: None,
            remote_mask: Cell::new(0),
            local_flushed: Cell::new(false),
            writes_synchronized: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, usize::MAX, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Err(TlbShootdownError::CpuOffline));
        assert_eq!(runtime.remote_mask.get(), 0);
        assert!(!runtime.local_flushed.get());
        assert!(runtime.writes_synchronized.get());
    }

    #[test]
    fn targeted_tlb_shootdown_skips_unselected_remote_and_local_cpus() {
        let runtime = ModelShootdown {
            online: [true; 3],
            remote_error: None,
            remote_mask: Cell::new(0),
            local_flushed: Cell::new(false),
            writes_synchronized: Cell::new(false),
        };

        let result =
            flush_tlb_range_on_cpus_with(&runtime, 1usize << 2, VirtAddr::from(0x4000), 0x2000);

        assert_eq!(result, Ok(()));
        assert_eq!(runtime.remote_mask.get(), 1usize << 2);
        assert!(!runtime.local_flushed.get());
        assert!(runtime.writes_synchronized.get());
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

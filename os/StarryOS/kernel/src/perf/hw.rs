//! Hardware-PMU `perf` events (ARM PMUv3): counting (M1, `perf stat`) and
//! sampling (M2, `perf record`).
//!
//! Counting events are one or more concurrent `PERF_TYPE_HARDWARE` /
//! `PERF_TYPE_RAW` events, each backed by either the dedicated 64-bit cycle
//! counter (`PMCCNTR_EL0`) or one of the programmable 32-bit event counters
//! (`PMEVCNTRn_EL0`). PMU capability probing is exposed through `ax_hal::pmu`;
//! the per-CPU sysreg operations remain in `ax_cpu::pmu`. This module allocates
//! counters, configures the requested event, drives
//! `ioctl(ENABLE/DISABLE/RESET)`, and serves `read(perf_fd)` with the timing
//! fields `perf stat` expects.
//!
//! A *sampling* event (`attr.sample_period > 0`) always takes a programmable
//! counter (even for CPU_CYCLES → ARM event `0x11`) and additionally owns an
//! mmap ring buffer plus a deferred poll worker. `mmap(perf_fd)` allocates the
//! ring (mirroring [`super::bpf`]); `enable()` preloads the counter to overflow
//! after `period` events, registers a [`super::sampling::SampleSlot`] for the
//! PMU overflow IRQ, and enables the overflow interrupt. The IRQ handler
//! ([`super::sampling::pmu_overflow_handler`]) writes one `PERF_RECORD_SAMPLE`
//! into the ring and wakes the worker, which delivers `POLLIN`. Sampling accepts
//! the scalar `PERF_SAMPLE_*` fields in
//! [`super::sampling::SUPPORTED_SAMPLE_TYPE`].
//!
//! Events are owned either by one task context or one explicit CPU context.
//! Fixed CPU workers serialize task-context control operations with PMU
//! register access on the owner CPU. There is no multiplexing, so
//! `time_running` equals `time_enabled`.

#[cfg(target_arch = "aarch64")]
use alloc::sync::Arc;
use core::any::Any;
#[cfg(target_arch = "aarch64")]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_arch = "aarch64")]
use ax_alloc::GlobalPage;
use ax_errno::{AxError, AxResult};
#[cfg(target_arch = "aarch64")]
use ax_hal::mem::virt_to_phys;
#[cfg(target_arch = "aarch64")]
use ax_memory_addr::PhysAddr;
#[cfg(target_arch = "aarch64")]
use ax_sync::PiMutex;
#[cfg(target_arch = "aarch64")]
use axpoll::PollSet;
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::perf_event_attr;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::perf_event_mmap_page;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::{perf_hw_id, perf_type_id};

#[cfg(target_arch = "aarch64")]
use super::PerfReadValues;
#[cfg(target_arch = "aarch64")]
use super::control::PerfControl;
#[cfg(target_arch = "aarch64")]
use super::target::{PerfCpuId, PerfTaskTarget};
use super::{PerfEventOps, target::PerfTarget};
#[cfg(target_arch = "aarch64")]
use super::{
    cpu_worker,
    inheritance::PerfInheritanceFamily,
    output::{PerfOutputRoute, PerfOutputScope, PerfRingOutput},
    sampling::{self, SampleOutput, SampleSlot, SampleSlotConfig},
    sampling_lifecycle::SampleRegistration,
};
#[cfg(target_arch = "aarch64")]
use crate::task::future::IrqNotify;

/// Dynamically-assigned `perf_event_attr.type` for the ARM PMUv3 CPU PMU,
/// exposed at `/sys/bus/event_source/devices/armv8_pmuv3_0/type`.
///
/// Linux assigns PMU type ids dynamically, starting after the fixed
/// `perf_type_id` range (`0..=5`). This workspace already hands out the next
/// two ids to the tracing event sources (kprobe = 6, uprobe = 7; see
/// `PERF_EVENT_SOURCES` in `pseudofs::sysfs`), so the first free id is 8.
///
/// The real `perf` tool reads this id from sysfs and puts it in
/// `perf_event_attr.type` for named events such as `armv8_pmuv3_0/cpu_cycles/`.
/// The dispatcher in [`super::perf_event_open`] routes it here, and
/// [`perf_event_open_hw`] then treats it exactly like `PERF_TYPE_RAW`: the low
/// 16 bits of `config` are the ARM event number on a programmable counter.
pub const ARMV8_PMUV3_PERF_TYPE: u32 = 8;

/// Required instruction-pointer bit in a hardware sampling event.
/// A sampling event with any other `sample_type` is rejected at open.
#[cfg(target_arch = "aarch64")]
const PERF_SAMPLE_IP: u64 = 1;

/// `data_offset` for our ring buffers: the data region starts after the single
/// `perf_event_mmap_page` header page (`PAGE_SIZE`).
#[cfg(target_arch = "aarch64")]
const RING_DATA_OFFSET: usize = ax_memory_addr::PAGE_SIZE_4K;

/// The hardware counter a [`HwPerfEvent`] is bound to.
///
/// `Cycle` is the dedicated 64-bit cycle counter (`PMCCNTR_EL0`);
/// `Programmable(n)` is one of the 32-bit event counters at logical index
/// `n` in `0..num_counters`.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy)]
enum Counter {
    Cycle,
    Programmable(usize),
}

#[cfg(target_arch = "aarch64")]
impl Counter {
    fn enable(self) {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::enable(),
            Self::Programmable(n) => ax_cpu::pmu::counter::enable(n),
        }
    }

    fn disable(self) {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::disable(),
            Self::Programmable(n) => ax_cpu::pmu::counter::disable(n),
        }
    }

    fn reset(self) {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::reset(),
            Self::Programmable(n) => ax_cpu::pmu::counter::reset(n),
        }
    }

    fn read(self) -> u64 {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::read(),
            Self::Programmable(n) => ax_cpu::pmu::counter::read(n),
        }
    }
}

/// Value-only request to configure a system-wide PMU event on its owner CPU.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuConfigure {
    counter: Counter,
    event: Option<u16>,
    exclude_user: bool,
    exclude_kernel: bool,
}

/// Owner-CPU enable request. A sampling slot owns every IRQ-visible reference.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuEnable {
    counter: Counter,
    sampling: Option<(u32, SampleSlot)>,
}

/// State published only after the owner CPU has committed enable.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuEnableResult {
    registration: Option<SampleRegistration>,
    started_at: u64,
}

/// Value-only owner-CPU disable request.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuDisable {
    counter: Counter,
    registration: Option<SampleRegistration>,
}

/// Value-only owner-CPU read request.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuRead {
    counter: Counter,
}

/// Owner-consistent raw count and timestamp.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuReadResult {
    value: u64,
    observed_at: u64,
}

/// Value-only owner-CPU reset request.
#[cfg(target_arch = "aarch64")]
pub(super) struct SystemPmuReset {
    counter: Counter,
    sampling_period: Option<u32>,
}

/// Configures one reserved counter on the current owner CPU.
#[cfg(target_arch = "aarch64")]
pub(super) fn configure_system_on_owner(request: SystemPmuConfigure) -> AxResult<()> {
    ax_cpu::pmu::init_cpu();
    match (request.counter, request.event) {
        (Counter::Cycle, None) => {
            ax_cpu::pmu::cycles::configure(request.exclude_user, request.exclude_kernel);
        }
        (Counter::Programmable(n), Some(event)) => {
            ax_cpu::pmu::counter::configure(n, event, request.exclude_user, request.exclude_kernel);
        }
        _ => return Err(AxError::BadState),
    }
    Ok(())
}

/// Commits enable on the current owner CPU and returns its publication state.
#[cfg(target_arch = "aarch64")]
pub(super) fn enable_system_on_owner(request: SystemPmuEnable) -> AxResult<SystemPmuEnableResult> {
    let registration = if let Some((period, slot)) = request.sampling {
        let Counter::Programmable(n) = request.counter else {
            return Err(AxError::BadState);
        };
        sampling::enable_local_pmu_irq().map_err(|_| AxError::NoSuchDevice)?;
        ax_cpu::pmu::counter::preload(n, period);
        let registration = sampling::register(n, slot).map_err(|_| AxError::ResourceBusy)?;
        ax_cpu::pmu::overflow::enable_irq(n);
        ax_cpu::pmu::counter::enable(n);
        Some(registration)
    } else {
        request.counter.enable();
        None
    };
    Ok(SystemPmuEnableResult {
        registration,
        started_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Quiesces one system-wide event on the current owner CPU.
#[cfg(target_arch = "aarch64")]
pub(super) fn disable_system_on_owner(request: SystemPmuDisable) -> AxResult<u64> {
    if let Some(registration) = request.registration {
        let Counter::Programmable(n) = request.counter else {
            return Err(AxError::BadState);
        };
        if registration.counter() != n {
            return Err(AxError::BadState);
        }
        ax_cpu::pmu::overflow::disable_irq(n);
        ax_cpu::pmu::counter::disable(n);
        ax_cpu::pmu::overflow::clear(1 << n);
        sampling::unregister(registration).map_err(|_| AxError::BadState)?;
    } else {
        request.counter.disable();
    }
    Ok(ax_runtime::hal::time::monotonic_time_nanos())
}

/// Reads one system-wide event on the current owner CPU.
#[cfg(target_arch = "aarch64")]
pub(super) fn read_system_on_owner(request: SystemPmuRead) -> AxResult<SystemPmuReadResult> {
    Ok(SystemPmuReadResult {
        value: request.counter.read(),
        observed_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Resets one system-wide event on the current owner CPU.
#[cfg(target_arch = "aarch64")]
pub(super) fn reset_system_on_owner(request: SystemPmuReset) -> AxResult<()> {
    match (request.counter, request.sampling_period) {
        (Counter::Programmable(n), Some(period)) => {
            ax_cpu::pmu::counter::preload(n, period);
        }
        (counter, None) => counter.reset(),
        (Counter::Cycle, Some(_)) => return Err(AxError::BadState),
    }
    Ok(())
}

/// Conservative process-wide PMU counter reservation.
///
/// Hardware slots are physically per CPU, but one global bitmap deliberately
/// prevents two owners from reserving the same logical slot until per-CPU
/// allocation and multiplexing are modeled. This limits concurrency across
/// CPUs without weakening ownership or teardown correctness.
#[cfg(target_arch = "aarch64")]
struct HwAlloc {
    /// Number of programmable counters (`PMCR_EL0.N`), from `ax_hal::pmu::info`.
    num_counters: usize,
    /// Bitmask of allocated programmable counters (bit `n` ⇒ index `n` in use).
    used: u32,
    /// Whether the dedicated cycle counter is allocated.
    cycle_used: bool,
}

#[cfg(target_arch = "aarch64")]
impl HwAlloc {
    const fn new() -> Self {
        HwAlloc {
            num_counters: 0,
            used: 0,
            cycle_used: false,
        }
    }

    /// Allocate the dedicated cycle counter, if free.
    fn alloc_cycle(&mut self) -> Option<Counter> {
        if self.cycle_used {
            return None;
        }
        self.cycle_used = true;
        Some(Counter::Cycle)
    }

    /// Allocate the lowest free programmable counter, if any.
    fn alloc_counter(&mut self) -> Option<Counter> {
        for n in 0..self.num_counters.min(32) {
            if self.used & (1 << n) == 0 {
                self.used |= 1 << n;
                return Some(Counter::Programmable(n));
            }
        }
        None
    }

    /// Release a previously allocated counter.
    fn free(&mut self, counter: Counter) {
        match counter {
            Counter::Cycle => self.cycle_used = false,
            Counter::Programmable(n) => {
                if n < 32 {
                    self.used &= !(1 << n);
                }
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
static ALLOC: ax_kspin::SpinNoPreempt<HwAlloc> = ax_kspin::SpinNoPreempt::new(HwAlloc::new());

/// Reserve a programmable counter for the per-task path ([`super::task`]).
///
/// The system-wide path reaches the allocator through [`alloc_programmable`],
/// which also configures and validates the event; the per-task path keeps the
/// slot unconfigured (the scheduler hook configures it per slice), so it needs a
/// bare reservation. Returns the logical counter index, or `None` if no
/// programmable counter is free.
#[cfg(target_arch = "aarch64")]
pub(crate) fn alloc_programmable_counter() -> Option<usize> {
    match ALLOC.lock().alloc_counter() {
        Some(Counter::Programmable(n)) => Some(n),
        // `alloc_counter` only ever yields `Programmable`; the cycle counter is
        // not handed to the per-task path.
        _ => None,
    }
}

/// Release a programmable counter previously reserved via
/// [`alloc_programmable_counter`]. Called by [`super::task::free_hw`].
#[cfg(target_arch = "aarch64")]
pub(crate) fn free_programmable_counter(n: usize) {
    ALLOC.lock().free(Counter::Programmable(n));
}

/// Sampling state attached to a `HwPerfEvent` when `attr.sample_period > 0`.
///
/// Holds the period and `sample_type`, the deferred poll machinery (mirroring
/// [`super::bpf::BpfPerfEventWrapper`]: a `PollSet` woken by an `IrqNotify` via a
/// background worker), and — once `mmap(perf_fd)` runs — the ring buffer.
///
/// Registered [`SampleSlot`] values clone their ring and notification owners.
/// Teardown generation-unregisters the slot before this state is reclaimed.
#[cfg(target_arch = "aarch64")]
struct SamplingState {
    /// Sampling period (events between overflows). Always `> 0`. In frequency
    /// mode this is the initial estimate; the overflow handler adapts it.
    period: u32,
    /// Frequency mode (`attr.freq`): the handler re-derives the period after each
    /// sample to converge on `target_freq` Hz. Fixed `-c` period when false.
    freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    target_freq: u32,
    /// Validated scalar `attr.sample_type`.
    sample_type: u64,
    /// Readiness set readers wait on; woken (with `IoEvents::IN`) by the worker.
    poll_ready: Arc<PollSet>,
    /// IRQ-safe notification the overflow handler pokes; drained by the worker.
    notify: Arc<IrqNotify>,
    /// Liveness flag for the worker; cleared on drop to stop it.
    poll_alive: Arc<AtomicBool>,
    /// Own mmap ring (weak) plus an optional strongly-owned redirect.
    output: PerfOutputRoute,
}

#[cfg(target_arch = "aarch64")]
impl core::fmt::Debug for SamplingState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SamplingState")
            .field("period", &self.period)
            .field("sample_type", &self.sample_type)
            .field("has_own_ring", &self.output.owned().is_some())
            .field(
                "redirected",
                &self
                    .output
                    .effective()
                    .is_some_and(|(_, redirected)| redirected),
            )
            .finish()
    }
}

/// Spawn the deferred worker that turns IRQ-context `notify_irq` pokes into
/// `axpoll` wakeups. Mirrors `bpf::start_bpf_perf_notify_worker`.
#[cfg(target_arch = "aarch64")]
fn start_sampling_notify_worker(
    poll_ready: Arc<PollSet>,
    notify: Arc<IrqNotify>,
    poll_alive: Arc<AtomicBool>,
) {
    crate::task::spawn_kernel_thread(
        move || loop {
            notify.wait();
            if !poll_alive.load(Ordering::Acquire) {
                break;
            }
            // The overflow handler writes the ring record before `notify_irq`.
            unsafe { poll_ready.wake(IoEvents::IN) };
        },
        "hw-perf-sample-notify".into(),
    );
}

/// Allocate, zero, and header-initialize one sampling mmap ring of `len` bytes.
///
/// Shared by the M2 system-wide path ([`HwPerfEvent::device_mmap`]) and the
/// per-task sampling path. Validates the libbpf/`perf` mmap geometry
/// (`(1 + 2^N) * PAGE_SIZE`), allocates contiguous pages, zeros them, writes the
/// `perf_event_mmap_page` header's data-region geometry, and returns the sole
/// strong `Arc<GlobalPage>` (the caller threads it into the VMA retainer and/or
/// keeps an anchor), the ring's kernel vaddr, and its physical start.
#[cfg(target_arch = "aarch64")]
fn alloc_sampling_ring(len: usize) -> AxResult<(Arc<GlobalPage>, usize, PhysAddr)> {
    // libbpf/`perf` require `(1 + 2^N) * PAGE_SIZE`: one header page plus a
    // power-of-two-page data ring. Reject anything else.
    if len == 0 || !len.is_multiple_of(ax_memory_addr::PAGE_SIZE_4K) {
        return Err(AxError::InvalidInput);
    }
    let num_pages = len / ax_memory_addr::PAGE_SIZE_4K;
    if num_pages < 2 || !(num_pages - 1).is_power_of_two() {
        return Err(AxError::InvalidInput);
    }

    // Allocate and zero the contiguous ring pages (mirror `bpf.rs`).
    let mut pages = GlobalPage::alloc_contiguous(num_pages, ax_memory_addr::PAGE_SIZE_4K)?;
    pages.zero();
    let kvirt = pages.start_vaddr();
    let paddr = virt_to_phys(kvirt);

    // Initialize the `perf_event_mmap_page` header in page 0. The pages are
    // already zeroed, so only the data-region geometry must be set: the data
    // ring starts after the header page and spans the rest of the mapping.
    // `data_head`/`data_tail` stay 0 (empty ring).
    let header = kvirt.as_usize() as *mut perf_event_mmap_page;
    let data_size = (len - RING_DATA_OFFSET) as u64;
    // SAFETY: `header` points at the freshly allocated, zeroed header page,
    // which is `>= size_of::<perf_event_mmap_page>()` (≥ 1 page = 4096 B, and
    // the struct is < 4096 B). No reader sees it until the VMA maps it.
    unsafe {
        core::ptr::addr_of_mut!((*header).version).write(1); // v1 protocol
        core::ptr::addr_of_mut!((*header).compat_version).write(0);
        core::ptr::addr_of_mut!((*header).data_offset).write(RING_DATA_OFFSET as u64);
        core::ptr::addr_of_mut!((*header).data_size).write(data_size);
        core::ptr::addr_of_mut!((*header).data_head).write(0);
        core::ptr::addr_of_mut!((*header).data_tail).write(0);
    }

    Ok((Arc::new(pages), kvirt.as_usize(), paddr))
}

/// A hardware-PMU perf event: one allocated counter plus the timing
/// accumulators `perf stat` reads back through `read_format`, and — for sampling
/// events — the [`SamplingState`] driving the overflow-IRQ ring buffer.
///
/// Timing follows Linux semantics: `time_enabled` accumulates wall time the
/// event has spent enabled and `time_running` the time it was actually
/// scheduled onto hardware. With no multiplexing the two are equal.
#[cfg(target_arch = "aarch64")]
#[derive(Debug)]
struct HwPerfEventState {
    /// The physical counter backing this event.
    counter: Counter,
    /// CPU that owns a system-wide event; task-bound events use scheduler leases.
    system_owner: Option<PerfCpuId>,
    /// Context used to validate `PERF_EVENT_IOC_SET_OUTPUT`.
    output_scope: PerfOutputScope,
    /// Unique event id emitted in `PERF_SAMPLE_ID` / `PERF_SAMPLE_IDENTIFIER`
    /// records (the same id `PERF_EVENT_IOC_ID` reports), so a reader can tell
    /// apart events sharing one ring. Set by [`set_sample_id`](PerfEventOps::set_sample_id);
    /// `0` until then.
    sample_id: u64,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// Monotonic ns timestamp of the last `enable`, or `None` while disabled.
    enabled_since: Option<u64>,
    /// Accumulated enabled time across past enabled windows (ns).
    time_enabled: u64,
    /// Accumulated running time across past enabled windows (ns). Equal to
    /// `time_enabled` in M1 (no multiplexing).
    time_running: u64,
    /// Sampling machinery, `Some` iff `attr.sample_period > 0`.
    sampling: Option<SamplingState>,
    /// Exact owner-CPU registry generation while sampling is armed.
    sampling_registration: Option<SampleRegistration>,
    /// Fd-owned task event and its inherited descendants.
    ///
    /// When set, this is the *only* live state: the system-wide fields are inert
    /// placeholders and the family delegates each CPU-affine member to
    /// [`super::task`]. The root fd owns the family; scheduler lists own member
    /// counters independently.
    per_task: Option<Arc<PerfInheritanceFamily>>,
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEventState {
    /// `device_mmap` for a counting event: the single-page `perf_event_mmap_page`
    /// userspace maps for `rdpmc` self-monitoring.
    ///
    /// No ring buffer — the page only carries the metadata a userspace reader
    /// needs to read this event's hardware counter directly: `cap_user_rdpmc`,
    /// the 1-based `index` selecting the counter, and its `pmc_width`. `offset`
    /// stays 0: with no multiplexing the raw counter value *is* the count, so
    /// `count = rdpmc(index - 1)` masked to `pmc_width` bits. The page is never
    /// updated after this, so `lock` stays 0 (the userspace seqlock reads once).
    /// EL0 read access to the counters is enabled globally in
    /// [`ax_cpu::pmu::init_cpu`] via `PMUSERENR_EL0`.
    fn device_mmap_rdpmc(&self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        if len < ax_memory_addr::PAGE_SIZE_4K {
            return Err(AxError::InvalidInput);
        }
        let mut pages = GlobalPage::alloc_contiguous(1, ax_memory_addr::PAGE_SIZE_4K)?;
        pages.zero();
        let kvirt = pages.start_vaddr();
        let paddr = virt_to_phys(kvirt);

        // Encode which hardware counter backs this event. The mmap-page `index`
        // is 1-based (0 ⇒ rdpmc unusable); `index - 1` is the ARM counter the
        // reader accesses — `PMEVCNTR(index-1)_EL0`, or `PMCCNTR_EL0` for the
        // dedicated cycle counter (ARM index 31 ⇒ page index 32).
        let (index, pmc_width): (u32, u16) = match self.counter {
            Counter::Cycle => (32, 64),
            Counter::Programmable(n) => (n as u32 + 1, 32),
        };

        let header = kvirt.as_usize() as *mut perf_event_mmap_page;
        // SAFETY: freshly allocated, zeroed page, `>= size_of::<perf_event_mmap_page>()`
        // (≥ 1 page = 4096 B); no reader sees it until the VMA maps it.
        unsafe {
            core::ptr::addr_of_mut!((*header).version).write(1);
            core::ptr::addr_of_mut!((*header).compat_version).write(0);
            core::ptr::addr_of_mut!((*header).index).write(index);
            core::ptr::addr_of_mut!((*header).offset).write(0);
            core::ptr::addr_of_mut!((*header).pmc_width).write(pmc_width);
            // `capabilities` is a union over a bitfield; `cap_user_rdpmc` is bit 2
            // (after `cap_bit0` and `cap_bit0_is_deprecated`). Write the `u64` arm.
            core::ptr::addr_of_mut!((*header).__bindgen_anon_1.capabilities).write(1u64 << 2);
        }

        let anchor: Arc<dyn Any + Send + Sync> = Arc::new(pages);
        Ok((paddr, anchor))
    }
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEventState {
    /// Releases owner-visible PMU state before output anchors are dropped.
    fn close(&mut self) -> AxResult<()> {
        // Per-task events do not own a system-wide counter or sampling state:
        // release the HW counter through the per-task path (idempotent — the
        // task-exit hook may have freed it already) and stop here.
        if let Some(family) = &self.per_task {
            return family.close();
        }
        let owner = self.system_owner.ok_or(AxError::BadState)?;
        let stopped_at = cpu_worker::disable_system(
            owner,
            SystemPmuDisable {
                counter: self.counter,
                registration: self.sampling_registration,
            },
        )?;
        self.sampling_registration = None;
        if let Some(since) = self.enabled_since.take() {
            let elapsed = stopped_at.saturating_sub(since);
            self.time_enabled = self.time_enabled.saturating_add(elapsed);
            self.time_running = self.time_running.saturating_add(elapsed);
        }
        ALLOC.lock().free(self.counter);
        if let Some(sampling) = &mut self.sampling {
            sampling.poll_alive.store(false, Ordering::Release);
            sampling.notify.notify();
            sampling.output.clear();
        }
        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEventState {
    fn poll(&self) -> IoEvents {
        // Per-task events: a sampling one is readable when its ring (on the
        // shared `PerTaskCounter`) has unread bytes; a counting one is always
        // readable (`read(perf_fd)` returns the current value without blocking).
        if let Some(family) = &self.per_task {
            let ptc = family.root();
            if ptc.is_sampling() {
                return if ptc.ring_has_data() {
                    IoEvents::IN
                } else {
                    IoEvents::empty()
                };
            }
            return IoEvents::IN;
        }
        match &self.sampling {
            // Sampling events are readable only when the ring has unread bytes
            // (`data_tail != data_head`): that is what `perf record`'s poll
            // waits on. Before the first mmap there is no ring ⇒ not readable.
            Some(sampling) => {
                if sampling.output.owned().as_ref().is_some_and(ring_has_data) {
                    IoEvents::IN
                } else {
                    IoEvents::empty()
                }
            }
            // A counting event is always readable: `read(perf_fd)` returns the
            // current value without blocking.
            None => IoEvents::IN,
        }
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        // Per-task sampling events register a waker on the ptc's `PollSet` (the
        // one the per-task notify worker wakes). Counting events (per-task or
        // system-wide) never transition readiness, so they register nothing.
        if let Some(family) = &self.per_task {
            let ptc = family.root();
            if ptc.is_sampling() && events.contains(IoEvents::IN) {
                ptc.register_poll(context);
            }
            return;
        }
        // Counting events never transition readiness, so only sampling events
        // register a waker — on the same `PollSet` the notify worker wakes.
        if let Some(sampling) = &self.sampling
            && events.contains(IoEvents::IN)
        {
            unsafe { sampling.poll_ready.register(context.waker(), IoEvents::IN) };
        }
    }
}

/// Whether a sampling ring currently has unread bytes (`data_head != data_tail`).
///
/// The owned output snapshot pins the header page for the complete read.
#[cfg(target_arch = "aarch64")]
fn ring_has_data(ring: &PerfRingOutput) -> bool {
    let header = ring.ring_vaddr() as *const perf_event_mmap_page;
    // SAFETY: `ring` pins the initialized header page. These plain `u64` reads
    // are only a readiness hint.
    let (head, tail) = unsafe {
        (
            core::ptr::addr_of!((*header).data_head).read_volatile(),
            core::ptr::addr_of!((*header).data_tail).read_volatile(),
        )
    };
    head != tail
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEventState {
    fn enable(&mut self) -> AxResult<()> {
        // Per-task: just record userspace intent. The target task's next
        // `perf_sched_in` programs the counter onto HW (or an immediate one if
        // it is the running task at the next switch).
        if let Some(family) = &self.per_task {
            return family.enable();
        }
        if self.enabled_since.is_some() {
            return Ok(());
        }
        let owner = self.system_owner.ok_or(AxError::BadState)?;
        let sampling = if let Some(sampling) = &self.sampling {
            let Counter::Programmable(_) = self.counter else {
                return Err(AxError::BadState);
            };
            let period = sampling.period;
            let (ring, redirected) = sampling
                .output
                .effective()
                .map_or((None, false), |(ring, redirected)| (Some(ring), redirected));
            let notify = (!redirected).then(|| Arc::clone(&sampling.notify));
            Some((
                period,
                SampleSlot::new(
                    SampleOutput::new(ring, notify),
                    SampleSlotConfig {
                        period,
                        sample_type: sampling.sample_type,
                        id: self.sample_id,
                        freq: sampling.freq,
                        target_freq: sampling.target_freq,
                        last_time: 0,
                    },
                ),
            ))
        } else {
            None
        };
        let result = cpu_worker::enable_system(
            owner,
            SystemPmuEnable {
                counter: self.counter,
                sampling,
            },
        )?;
        self.sampling_registration = result.registration;
        self.enabled_since = Some(result.started_at);
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        if let Some(family) = &self.per_task {
            return family.disable();
        }
        let Some(since) = self.enabled_since else {
            return Ok(());
        };
        let owner = self.system_owner.ok_or(AxError::BadState)?;
        let stopped_at = cpu_worker::disable_system(
            owner,
            SystemPmuDisable {
                counter: self.counter,
                registration: self.sampling_registration,
            },
        )?;
        self.sampling_registration = None;
        self.enabled_since = None;
        let elapsed = stopped_at.saturating_sub(since);
        self.time_enabled = self.time_enabled.saturating_add(elapsed);
        self.time_running = self.time_running.saturating_add(elapsed);
        Ok(())
    }

    fn reset(&mut self) -> AxResult<()> {
        if let Some(family) = &self.per_task {
            return family.reset();
        }
        let owner = self.system_owner.ok_or(AxError::BadState)?;
        cpu_worker::reset_system(
            owner,
            SystemPmuReset {
                counter: self.counter,
                sampling_period: self.sampling.as_ref().map(|sampling| sampling.period),
            },
        )
    }

    fn read_values(&mut self) -> AxResult<PerfReadValues> {
        if let Some(family) = &self.per_task {
            let (value, time_enabled, time_running) = family.read()?;
            let root = family.root();
            return Ok(PerfReadValues {
                value,
                time_enabled,
                time_running,
                read_format: root.read_format(),
            });
        }
        let owner = self.system_owner.ok_or(AxError::BadState)?;
        let snapshot = cpu_worker::read_system(
            owner,
            SystemPmuRead {
                counter: self.counter,
            },
        )?;
        let (mut time_enabled, mut time_running) = (self.time_enabled, self.time_running);
        if let Some(since) = self.enabled_since {
            let elapsed = snapshot.observed_at.saturating_sub(since);
            time_enabled = time_enabled.saturating_add(elapsed);
            time_running = time_running.saturating_add(elapsed);
        }
        Ok(PerfReadValues {
            value: snapshot.value,
            time_enabled,
            time_running,
            read_format: self.read_format,
        })
    }

    fn set_sample_id(&mut self, id: u64) {
        self.sample_id = id;
        // Per-task: mirror onto the shared counter the scheduler hook reads.
        if let Some(family) = &self.per_task {
            family.set_sample_id(id);
        }
    }

    fn output_ring(&self) -> Option<PerfRingOutput> {
        // Per-task: the ring lives on the shared `PerTaskCounter`.
        if let Some(family) = &self.per_task {
            return family.root().output_ring();
        }
        self.sampling.as_ref()?.output.owned()
    }

    fn redirect_output(&mut self, output: PerfRingOutput) -> AxResult<()> {
        if self.output_ring().is_some() {
            return Err(AxError::InvalidInput);
        }
        if let Some(family) = &self.per_task {
            return family.redirect_output(output);
        }
        let was_enabled = self.enabled_since.is_some();
        if was_enabled {
            self.disable()?;
        }
        if let Some(sampling) = &mut self.sampling {
            sampling.output.redirect(output);
        }
        if was_enabled {
            self.enable()?;
        }
        Ok(())
    }

    fn detach_output(&mut self) -> AxResult<()> {
        if self.output_ring().is_some() {
            return Err(AxError::InvalidInput);
        }
        if let Some(family) = &self.per_task {
            return family.detach_output();
        }
        let was_enabled = self.enabled_since.is_some();
        if was_enabled {
            self.disable()?;
        }
        if let Some(sampling) = &mut self.sampling {
            sampling.output.detach();
        }
        if was_enabled {
            self.enable()?;
        }
        Ok(())
    }

    fn device_mmap(&mut self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        // Per-task sampling: the ring + notify/poll machinery live on the shared
        // `PerTaskCounter` (the scheduler hook builds the IRQ slot from there).
        // Allocate the ring, spawn the notify worker, and hand both to the ptc.
        if let Some(family) = &self.per_task {
            let ptc = family.root();
            if ptc.is_sampling() {
                return device_mmap_per_task(family, len);
            }
            return self.device_mmap_rdpmc(len);
        }

        // A counting event has no ring; it exposes a single-page
        // `perf_event_mmap_page` for `rdpmc` (userspace reads the counter
        // directly via `mrs`). Only sampling events allocate a ring below.
        let Some(sampling) = &mut self.sampling else {
            return self.device_mmap_rdpmc(len);
        };

        // One live mapping per perf fd (Linux semantics). A stale `Weak` from an
        // abandoned/munmap'd previous attempt does not count (its pages are
        // already freed), so the fd stays mmap-able. Mirrors `bpf.rs`.
        if sampling.output.owned().is_some() {
            return Err(AxError::ResourceBusy);
        }

        // Allocate + zero + header-init the ring (shared with the per-task path).
        let (pages, ring_vaddr, paddr) = alloc_sampling_ring(len)?;

        let page_anchor: Arc<dyn Any + Send + Sync> = pages;
        let output = PerfRingOutput::new(ring_vaddr, len, page_anchor);
        sampling.output.publish_owned(&output);
        Ok((paddr, output.mapping_anchor()))
    }
}

/// Sleepable control plane for one ARM PMU perf event.
#[cfg(target_arch = "aarch64")]
struct HwPerfControl {
    state: PiMutex<HwPerfEventState>,
}

#[cfg(target_arch = "aarch64")]
impl core::fmt::Debug for HwPerfControl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HwPerfControl").finish_non_exhaustive()
    }
}

#[cfg(target_arch = "aarch64")]
impl HwPerfControl {
    fn task_family(&self) -> Option<Arc<PerfInheritanceFamily>> {
        self.state.lock().per_task.clone()
    }
}

#[cfg(target_arch = "aarch64")]
impl Pollable for HwPerfControl {
    fn poll(&self) -> IoEvents {
        if let Some(family) = self.task_family() {
            let root = family.root();
            return if root.is_sampling() {
                if root.ring_has_data() {
                    IoEvents::IN
                } else {
                    IoEvents::empty()
                }
            } else {
                IoEvents::IN
            };
        }
        self.state.lock().poll()
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        if let Some(family) = self.task_family() {
            let root = family.root();
            if root.is_sampling() && events.contains(IoEvents::IN) {
                root.register_poll(context);
            }
            return;
        }
        self.state.lock().register(context, events);
    }
}

#[cfg(target_arch = "aarch64")]
impl PerfControl for HwPerfControl {
    fn enable(&self) -> AxResult<()> {
        if let Some(family) = self.task_family() {
            return family.enable();
        }
        self.state.lock().enable()
    }

    fn disable(&self) -> AxResult<()> {
        if let Some(family) = self.task_family() {
            return family.disable();
        }
        self.state.lock().disable()
    }

    fn reset(&self) -> AxResult<()> {
        if let Some(family) = self.task_family() {
            return family.reset();
        }
        self.state.lock().reset()
    }

    fn read_values(&self) -> AxResult<PerfReadValues> {
        if let Some(family) = self.task_family() {
            let (value, time_enabled, time_running) = family.read()?;
            return Ok(PerfReadValues {
                value,
                time_enabled,
                time_running,
                read_format: family.root().read_format(),
            });
        }
        self.state.lock().read_values()
    }

    fn device_mmap(&self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        self.state.lock().device_mmap(len)
    }

    fn output_ring(&self) -> Option<PerfRingOutput> {
        if let Some(family) = self.task_family() {
            return family.root().output_ring();
        }
        self.state.lock().output_ring()
    }

    fn output_scope(&self) -> Option<PerfOutputScope> {
        Some(self.state.lock().output_scope)
    }

    fn redirect_output(&self, output: PerfRingOutput) -> AxResult<()> {
        if let Some(family) = self.task_family() {
            return family.redirect_output(output);
        }
        self.state.lock().redirect_output(output)
    }

    fn detach_output(&self) -> AxResult<()> {
        if let Some(family) = self.task_family() {
            return family.detach_output();
        }
        self.state.lock().detach_output()
    }
}

/// Hardware-PMU event wrapper exposed through the generic perf fd layer.
#[cfg(target_arch = "aarch64")]
pub struct HwPerfEvent {
    control: Arc<HwPerfControl>,
    enable_at_open: bool,
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEvent {
    fn new(state: HwPerfEventState, enable_at_open: bool) -> Self {
        Self {
            control: Arc::new(HwPerfControl {
                state: PiMutex::new(state),
            }),
            enable_at_open,
        }
    }

    pub(super) fn control_handle(&self) -> Arc<dyn PerfControl> {
        self.control.clone()
    }
}

#[cfg(target_arch = "aarch64")]
impl core::fmt::Debug for HwPerfEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HwPerfEvent").finish_non_exhaustive()
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for HwPerfEvent {
    fn drop(&mut self) {
        let result = if let Some(family) = self.control.task_family() {
            family.close()
        } else {
            self.control.state.lock().close()
        };
        if let Err(error) = result {
            warn!("perf: owner-CPU PMU teardown failed, retaining resources: {error}");
            // Keep every IRQ-visible anchor and the reserved counter alive.
            // Reclamation without a completed owner-CPU grace period would
            // permit a stale overflow slot to reach freed memory.
            core::mem::forget(Arc::clone(&self.control));
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Pollable for HwPerfEvent {
    fn poll(&self) -> IoEvents {
        self.control.poll()
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        self.control.register(context, events);
    }
}

#[cfg(target_arch = "aarch64")]
impl PerfEventOps for HwPerfEvent {
    fn finish_open(&mut self) -> AxResult<()> {
        if core::mem::take(&mut self.enable_at_open) {
            self.control.enable()
        } else {
            Ok(())
        }
    }

    fn enable(&mut self) -> AxResult<()> {
        self.control.enable()
    }

    fn disable(&mut self) -> AxResult<()> {
        self.control.disable()
    }

    fn reset(&mut self) -> AxResult<()> {
        self.control.reset()
    }

    fn read_values(&mut self) -> AxResult<PerfReadValues> {
        self.control.read_values()
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_sample_id(&mut self, id: u64) {
        self.control.state.lock().set_sample_id(id);
    }

    fn device_mmap(&mut self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        self.control.device_mmap(len)
    }
}

/// `device_mmap` for a per-task sampling event.
///
/// Allocates the ring (via [`alloc_sampling_ring`]), spawns the deferred notify
/// worker, and publishes the ring plus page/notify/poll anchors through the
/// fd-owned [`PerfInheritanceFamily`]. The next
/// [`super::task::perf_sched_in`] for any family member sees that output and
/// arms the overflow IRQ. The returned anchor is the ring pages `Arc`, threaded
/// into the user VMA so the mapping outlives `close(perf_fd)`.
///
/// Rejecting a second mmap: a per-task event is opened once and mmap'd once by
/// `perf record`; a second attempt while the ring is still set is rejected.
#[cfg(target_arch = "aarch64")]
fn device_mmap_per_task(
    family: &Arc<PerfInheritanceFamily>,
    len: usize,
) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
    let ptc = family.root();
    // Only sampling per-task events have a ring; counting events reject mmap.
    if !ptc.is_sampling() {
        return Err(AxError::Unsupported);
    }
    // One live ring per fd: refuse if a ring is already mapped.
    if ptc.ring_mapped() {
        return Err(AxError::ResourceBusy);
    }

    let (pages, ring_vaddr, paddr) = alloc_sampling_ring(len)?;

    // Spawn the deferred worker (mirrors the M2 path): it turns IRQ-context
    // `notify_irq` pokes into `axpoll` `IoEvents::IN` wakeups.
    let poll_ready = Arc::new(PollSet::new());
    let notify = Arc::new(IrqNotify::new());
    let poll_alive = Arc::new(AtomicBool::new(true));
    start_sampling_notify_worker(poll_ready.clone(), notify.clone(), poll_alive.clone());

    let page_anchor: Arc<dyn Any + Send + Sync> = pages;
    let output = PerfRingOutput::new(ring_vaddr, len, page_anchor);

    // Publish a weak own-ring route plus the notification anchors. The returned
    // VMA retainer owns the complete output and its shared producer gate.
    family.publish_root_output(
        &output,
        super::task::SamplingAnchors::new(notify, poll_ready, poll_alive),
    )?;
    Ok((paddr, output.mapping_anchor()))
}

/// Resolve the `(period, target_freq)` a sampling event runs with, from the raw
/// `sample_period`/`sample_freq` union value and the `attr.freq` flag.
///
/// Fixed mode (`!is_freq`): the raw value is the period (range-checked to fit 32
/// bits by the caller); `target_freq` is `0`. Frequency mode (`is_freq`): the raw
/// value is a target rate (Hz), clamped to [`sampling::MAX_TARGET_FREQ`]; the
/// returned period is an initial estimate the overflow handler then adapts.
#[cfg(target_arch = "aarch64")]
fn resolve_sampling(raw: u64, is_freq: bool) -> (u32, u32) {
    if is_freq {
        let freq = raw.clamp(1, sampling::MAX_TARGET_FREQ as u64) as u32;
        (sampling::initial_period_for_freq(freq), freq)
    } else {
        (raw.min(u32::MAX as u64) as u32, 0)
    }
}

/// Open a hardware-PMU perf event from a user `perf_event_attr`.
///
/// Supports `PERF_TYPE_HARDWARE` (cycles via the dedicated counter, every
/// other mapped `perf_hw_id` via a programmable counter) and `PERF_TYPE_RAW`
/// (the low 16 bits of `config` as the raw ARM event number on a programmable
/// counter). The counter is configured (event programmed, `exclude_*` applied,
/// value reset to 0) but left disabled: the attr carries `disabled = 1`, and
/// the caller drives it with `ioctl(PERF_EVENT_IOC_ENABLE)`.
#[cfg(target_arch = "aarch64")]
pub fn perf_event_open_hw(attr: &perf_event_attr, target: PerfTarget) -> AxResult<HwPerfEvent> {
    // No PMUv3 → no hardware events.
    let Some(info) = ax_hal::pmu::info() else {
        return Err(AxError::Unsupported);
    };

    // Refresh the conservative global reservation width. Hardware programming
    // itself is dispatched to the selected owner CPU below.
    ALLOC.lock().num_counters = info.num_counters;

    let owner_cpu = match target {
        PerfTarget::Task { task, cpu } => {
            return perf_event_open_hw_per_task(attr, task, cpu);
        }
        PerfTarget::Cpu(cpu) => cpu,
    };
    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    // `sample_period` shares a union with `sample_freq`: `attr.freq` selects which
    // arm is live. A non-zero value (period or freq) selects the sampling path;
    // zero is counting. `resolve_sampling` turns either into the (initial period,
    // target_freq) pair the backend uses.
    // SAFETY: `perf_event_attr` is a `repr(C)` POD copied bytewise from user
    // space; both union arms are `u64`, so reading the field is sound.
    let raw = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_freq = attr.freq() != 0;
    let is_sampling = raw > 0;

    if is_sampling {
        // The IRQ handler (build_sample) emits the scalar sample_type fields perf
        // requests, so accept any combination of SUPPORTED bits — but IP must be
        // set (real perf always sets it for samples) and no unsupported bit
        // (CALLCHAIN/RAW/READ/REGS/…) may be present.
        if attr.sample_type & PERF_SAMPLE_IP == 0
            || attr.sample_type & !super::sampling::SUPPORTED_SAMPLE_TYPE != 0
        {
            warn!(
                "perf_event_open: sampling sample_type {:#x} unsupported (need PERF_SAMPLE_IP and \
                 only scalar fields)",
                attr.sample_type
            );
            return Err(AxError::Unsupported);
        }
        // A fixed period must fit the 32-bit programmable counter (the preload is
        // 32-bit). Frequency mode carries a (small) rate here, not a period.
        if !is_freq && raw > u32::MAX as u64 {
            warn!("perf_event_open: sample_period {raw} exceeds 32-bit counter");
            return Err(AxError::InvalidInput);
        }
    }
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);
    if is_sampling {
        sampling::ensure_pmu_irq_registered().map_err(|_| AxError::NoSuchDevice)?;
    }

    // Select the ARM event and counter. Sampling events ALWAYS take a
    // programmable counter — even CPU_CYCLES maps to ARM event 0x11 — because
    // the dedicated cycle counter is not used by the M2 overflow path.
    let (counter, event) = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        if attr.config == perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u64 && !is_sampling {
            // Counting CPU_CYCLES: the dedicated 64-bit cycle counter.
            let Some(counter) = ALLOC.lock().alloc_cycle() else {
                return Err(AxError::NoMemory);
            };
            (counter, None)
        } else {
            // Map the generic hardware event to an ARM PMUv3 event number.
            // (CPU_CYCLES → 0x11 here for the sampling case.)
            let Some(event) = ax_cpu::pmu::hw_event_to_arm(attr.config as u32) else {
                warn!(
                    "perf_event_open: unsupported hardware config {:#x}",
                    attr.config
                );
                return Err(AxError::Unsupported);
            };
            (alloc_programmable(event)?, Some(event))
        }
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32
        || attr.type_ == ARMV8_PMUV3_PERF_TYPE
    {
        // Raw events (`PERF_TYPE_RAW`) and dynamic ARM PMUv3 events
        // (`ARMV8_PMUV3_PERF_TYPE`, the sysfs-advertised PMU type) are decoded
        // identically: the low 16 bits of `config` are the ARM event number.
        // The real `perf` tool resolves a named event like
        // `armv8_pmuv3_0/cpu_cycles/` to (type = ARMV8_PMUV3_PERF_TYPE,
        // config = 0x11) via sysfs, so it lands here.
        let event = (attr.config & 0xFFFF) as u16;
        (alloc_programmable(event)?, Some(event))
    } else {
        // HW_CACHE / BREAKPOINT and anything else are not supported.
        warn!(
            "perf_event_open: unsupported hardware type {:#x}",
            attr.type_
        );
        return Err(AxError::Unsupported);
    };
    if let Err(error) = cpu_worker::configure_system(
        owner_cpu,
        SystemPmuConfigure {
            counter,
            event,
            exclude_user,
            exclude_kernel,
        },
    ) {
        ALLOC.lock().free(counter);
        return Err(error);
    }

    // Build sampling machinery for sampling events. The deferred poll worker is
    // spawned here (mirroring `BpfPerfEventWrapper::new`); the ring buffer is
    // allocated lazily on the first `mmap(perf_fd)`.
    //
    // ORDERING NOTE: `perf record` / libbpf always `mmap(perf_fd)` before
    // `ioctl(ENABLE)`, so the ring exists by the time `enable` registers the
    // slot. Enabling before mapping registers a zero ring (overflows are no-ops
    // until a mapping appears); this matches the M2 scope.
    let sampling = if is_sampling {
        let poll_ready = Arc::new(PollSet::new());
        let notify = Arc::new(IrqNotify::new());
        let poll_alive = Arc::new(AtomicBool::new(true));
        start_sampling_notify_worker(poll_ready.clone(), notify.clone(), poll_alive.clone());
        Some(SamplingState {
            period: sample_period,
            freq: is_freq,
            target_freq,
            sample_type: attr.sample_type,
            poll_ready,
            notify,
            poll_alive,
            output: PerfOutputRoute::new(),
        })
    } else {
        None
    };

    Ok(HwPerfEvent::new(
        HwPerfEventState {
            counter,
            system_owner: Some(owner_cpu),
            output_scope: PerfOutputScope::Cpu(owner_cpu.as_usize()),
            // Assigned by `set_sample_id` once the `PerfEvent` wrapper is built.
            sample_id: 0,
            read_format: attr.read_format,
            // `disabled = 1`: do not enable; timing accumulators start empty.
            enabled_since: None,
            time_enabled: 0,
            time_running: 0,
            sampling,
            sampling_registration: None,
            // System-wide / self event: not per-task.
            per_task: None,
        },
        attr.disabled() == 0,
    ))
}

/// Open a task-bound hardware-PMU event (`perf_event_open` with `pid >= 0`):
/// counting (`perf stat -- cmd`) or sampling (`perf record -- cmd`).
///
/// Resolves the target task, decodes the requested ARM event onto a
/// *programmable* counter (per-task never uses the dedicated cycle counter, so a
/// system-wide cycle event can run alongside it), reserves the slot from the M1
/// allocator without programming it, and attaches a shared
/// [`super::task::PerTaskCounter`] to the target [`crate::task::Thread`]. The HW
/// counter is programmed lazily by the scheduler hook the next time the target
/// runs (or by [`super::task::on_exec`] for `enable_on_exec`).
///
/// When `attr.sample_period > 0` (and `sample_type == PERF_SAMPLE_IP`) the event
/// is a per-task *sampling* event: the scheduler hooks arm the M2 overflow-IRQ
/// path for the slices the task runs, so samples are attributed to the task. The
/// ring buffer is allocated lazily in [`HwPerfEvent::device_mmap`] (perf mmaps
/// before enabling). The returned `HwPerfEvent` carries no `sampling` state of
/// its own — for per-task events the ring/notify live on the `PerTaskCounter`.
#[cfg(target_arch = "aarch64")]
fn perf_event_open_hw_per_task(
    attr: &perf_event_attr,
    target: PerfTaskTarget,
    cpu_filter: Option<PerfCpuId>,
) -> AxResult<HwPerfEvent> {
    // The Starry task table contains user tasks only.
    let task = match target {
        PerfTaskTarget::Current => crate::task::current_user_task(),
        PerfTaskTarget::Tid(tid) => crate::task::get_task(tid)?,
    };
    let thr = task.as_thread();
    let scheduler_id = thr.scheduler_id().ok_or(AxError::BadState)?;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    // `sample_period` shares a union with `sample_freq`; `attr.freq` selects the
    // arm. A non-zero value (period or rate) selects sampling. Frequency mode is
    // supported: `resolve_sampling` yields the initial period + target rate, and
    // the scheduler hook arms the adaptive overflow path per slice.
    // SAFETY: both union arms are `u64` in a `repr(C)` POD copied from user space.
    let raw = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_freq = attr.freq() != 0;
    let is_sampling = raw > 0;
    if is_sampling {
        // Same sample_type rule as the system-wide path: IP must be set and only
        // SUPPORTED scalar bits may be present (build_sample emits them).
        if attr.sample_type & PERF_SAMPLE_IP == 0
            || attr.sample_type & !super::sampling::SUPPORTED_SAMPLE_TYPE != 0
        {
            warn!(
                "perf_event_open: per-task sampling sample_type {:#x} unsupported (need \
                 PERF_SAMPLE_IP and only scalar fields)",
                attr.sample_type
            );
            return Err(AxError::Unsupported);
        }
        if !is_freq && raw > u32::MAX as u64 {
            warn!("perf_event_open: per-task sample_period {raw} exceeds 32-bit");
            return Err(AxError::InvalidInput);
        }
    }
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);
    if is_sampling {
        sampling::ensure_pmu_irq_registered().map_err(|_| AxError::NoSuchDevice)?;
    }

    // Decode the ARM event. Per-task always uses a programmable counter, so even
    // CPU_CYCLES maps to ARM event 0x11 (never the dedicated cycle counter).
    let event = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        match ax_cpu::pmu::hw_event_to_arm(attr.config as u32) {
            Some(event) => event,
            None => {
                warn!(
                    "perf_event_open: unsupported per-task hardware config {:#x}",
                    attr.config
                );
                return Err(AxError::Unsupported);
            }
        }
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32
        || attr.type_ == ARMV8_PMUV3_PERF_TYPE
    {
        (attr.config & 0xFFFF) as u16
    } else {
        warn!(
            "perf_event_open: unsupported per-task hardware type {:#x}",
            attr.type_
        );
        return Err(AxError::Unsupported);
    };

    if !ax_cpu::pmu::event_supported(event) {
        warn!(
            "perf_event_open: per-task ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(AxError::Unsupported);
    }

    // Reserve a programmable counter slot, but do NOT configure/enable HW now:
    // the scheduler hook configures it per slice when the target runs.
    let Some(n) = alloc_programmable_counter() else {
        return Err(AxError::NoMemory);
    };

    // `disabled = 0` ⇒ count from the next sched-in; `disabled = 1` ⇒ wait for
    // `enable_on_exec` / `ioctl(ENABLE)`. `perf stat -- cmd` sets both
    // `disabled` and `enable_on_exec`, so it starts counting at the child's exec.
    let enabled = attr.disabled() == 0;
    let enable_on_exec = attr.enable_on_exec() != 0;

    let ptc = Arc::new(super::task::PerTaskCounter::new(
        super::task::PerTaskConfig {
            n,
            event,
            exclude_user,
            exclude_kernel,
            read_format: attr.read_format,
            enabled,
            enable_on_exec,
            cpu_filter,
            // `0` ⇒ counting; `> 0` ⇒ per-task sampling.
            sample_period,
            sample_type: attr.sample_type,
            freq: is_freq,
            target_freq,
            // Side-band records for `perf report` symbolization.
            want_comm: attr.comm() != 0,
            want_mmap2: attr.mmap2() != 0,
            want_task: attr.task() != 0,
            sample_id_all: attr.sample_id_all() != 0,
            // Follow forked children into the same ring (`perf record` default).
            inherit: attr.inherit() != 0,
        },
    ));
    let family = PerfInheritanceFamily::new(Arc::clone(&ptc), enabled);
    super::task::attach(thr, ptc.clone());

    Ok(HwPerfEvent::new(
        HwPerfEventState {
            // Inert placeholders: the per-task path drives `ptc`, not these fields.
            counter: Counter::Programmable(n),
            system_owner: None,
            output_scope: PerfOutputScope::Task(scheduler_id.as_u64()),
            // Mirrors the wrapper id onto the ptc via `set_sample_id`; 0 until then.
            sample_id: 0,
            read_format: attr.read_format,
            enabled_since: None,
            time_enabled: 0,
            time_running: 0,
            sampling: None,
            sampling_registration: None,
            per_task: Some(family),
        },
        false,
    ))
}

/// Allocate a programmable counter, validate the event, and program it.
///
/// Common events (`< 0x40`) are gated through [`ax_cpu::pmu::event_supported`];
/// IMPLEMENTATION DEFINED events (`>= 0x40`) cannot be validated and are let
/// through. The counter is configured but left disabled.
#[cfg(target_arch = "aarch64")]
fn alloc_programmable(event: u16) -> AxResult<Counter> {
    if !ax_cpu::pmu::event_supported(event) {
        warn!(
            "perf_event_open: ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(AxError::Unsupported);
    }
    let Some(Counter::Programmable(n)) = ALLOC.lock().alloc_counter() else {
        return Err(AxError::NoMemory);
    };
    Ok(Counter::Programmable(n))
}

/// Non-aarch64 fallback: no hardware PMU support outside ARM PMUv3.
///
/// A pub unit struct keeps the dispatcher in `mod.rs` arch-agnostic; the
/// `PerfEventOps` methods all report `Unsupported`, and `perf_event_open_hw`
/// rejects the open before one is ever constructed.
#[cfg(not(target_arch = "aarch64"))]
#[derive(Debug)]
pub struct HwPerfEvent;

#[cfg(not(target_arch = "aarch64"))]
impl Pollable for HwPerfEvent {
    fn poll(&self) -> IoEvents {
        IoEvents::IN
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {}
}

#[cfg(not(target_arch = "aarch64"))]
impl PerfEventOps for HwPerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    fn disable(&mut self) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Non-aarch64 fallback: no hardware PMU support outside ARM PMUv3.
#[cfg(not(target_arch = "aarch64"))]
pub fn perf_event_open_hw(_attr: &perf_event_attr, _target: PerfTarget) -> AxResult<HwPerfEvent> {
    Err(AxError::Unsupported)
}

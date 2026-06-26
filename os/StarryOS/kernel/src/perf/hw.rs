//! Hardware-PMU `perf` events (ARM PMUv3): counting (M1, `perf stat`) and
//! sampling (M2, `perf record`).
//!
//! Counting events are one or more concurrent `PERF_TYPE_HARDWARE` /
//! `PERF_TYPE_RAW` events, each backed by either the dedicated 64-bit cycle
//! counter (`PMCCNTR_EL0`) or one of the programmable 32-bit event counters
//! (`PMEVCNTRn_EL0`). The per-CPU sysreg layer lives in [`ax_cpu::pmu`]; this
//! module allocates counters, configures the requested event, drives
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
//! into the ring and wakes the worker, which delivers `POLLIN`. M2 supports only
//! `PERF_SAMPLE_IP`.
//!
//! Scope: single CPU (the current one), no multiplexing. Because there is no
//! multiplexing, `time_running` always equals `time_enabled`.

#[cfg(target_arch = "aarch64")]
use alloc::sync::{Arc, Weak};
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
use ax_task::IrqNotify;
#[cfg(target_arch = "aarch64")]
use axpoll::PollSet;
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::perf_event_attr;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::perf_event_mmap_page;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::{perf_hw_id, perf_type_id};

use super::PerfEventOps;
#[cfg(target_arch = "aarch64")]
use super::PerfReadValues;
#[cfg(target_arch = "aarch64")]
use super::sampling::{self, SampleSlot};

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

/// `sample_type` value M2 supports: `perf_event_sample_format::PERF_SAMPLE_IP`.
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

/// Per-CPU counter allocator. M1 is single-core, so a single global allocator
/// (mirroring the cycle-only PMU state already living in sysregs) tracks which
/// physical counters are in use. `used` is a bitmask over programmable counter
/// indices `0..num_counters`; `cycle_used` guards the dedicated cycle counter.
#[cfg(target_arch = "aarch64")]
struct HwAlloc {
    /// Number of programmable counters (`PMCR_EL0.N`), from [`ax_cpu::pmu::probe`].
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

/// The backing pages of a sampling event's mmap ring buffer, after the first
/// `mmap(perf_fd)`.
///
/// Ownership mirrors [`super::bpf::BpfPerfEventWrapper`]: the strong
/// `Arc<GlobalPage>` is handed to the user VMA via `DeviceMmap::Physical`'s
/// retainer, and the event keeps only a `Weak`. `ring_vaddr` / `ring_len`
/// describe the kernel mapping the IRQ handler writes into; they are valid for
/// as long as some VMA pins the pages (i.e. while [`RingState::is_mapped`]).
#[cfg(target_arch = "aarch64")]
#[derive(Debug)]
struct RingState {
    /// Weak handle to the contiguous ring pages; strong refs live in the VMA(s).
    pages: Weak<GlobalPage>,
    /// Kernel virtual address of the ring's first page (`perf_event_mmap_page`).
    ring_vaddr: usize,
    /// Total ring length in bytes (header page + data region).
    ring_len: usize,
}

#[cfg(target_arch = "aarch64")]
impl RingState {
    /// Whether a live user mapping of the ring still pins the pages.
    fn is_mapped(&self) -> bool {
        self.pages.strong_count() > 0
    }
}

/// Sampling state attached to a `HwPerfEvent` when `attr.sample_period > 0`.
///
/// Holds the period and `sample_type`, the deferred poll machinery (mirroring
/// [`super::bpf::BpfPerfEventWrapper`]: a `PollSet` woken by an `IrqNotify` via a
/// background worker), and — once `mmap(perf_fd)` runs — the ring buffer.
///
/// The `notify` `Arc` is the strong reference that keeps the `IrqNotify` alive
/// for the registered [`SampleSlot`]'s raw pointer (see [`super::sampling`]):
/// teardown unregisters the slot before this `SamplingState` (and thus the
/// `Arc`) drops.
#[cfg(target_arch = "aarch64")]
struct SamplingState {
    /// Sampling period (events between overflows). Always `> 0`.
    period: u32,
    /// `attr.sample_type`. M2 requires exactly `PERF_SAMPLE_IP`.
    sample_type: u64,
    /// Readiness set readers wait on; woken (with `IoEvents::IN`) by the worker.
    poll_ready: Arc<PollSet>,
    /// IRQ-safe notification the overflow handler pokes; drained by the worker.
    notify: Arc<IrqNotify>,
    /// Liveness flag for the worker; cleared on drop to stop it.
    poll_alive: Arc<AtomicBool>,
    /// The ring buffer pages, `Some` after the first `mmap(perf_fd)`.
    ring: Option<RingState>,
}

#[cfg(target_arch = "aarch64")]
impl core::fmt::Debug for SamplingState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SamplingState")
            .field("period", &self.period)
            .field("sample_type", &self.sample_type)
            .field("ring", &self.ring)
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
    ax_task::spawn_with_name(
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

/// A hardware-PMU perf event: one allocated counter plus the timing
/// accumulators `perf stat` reads back through `read_format`, and — for sampling
/// events — the [`SamplingState`] driving the overflow-IRQ ring buffer.
///
/// Timing follows Linux semantics: `time_enabled` accumulates wall time the
/// event has spent enabled and `time_running` the time it was actually
/// scheduled onto hardware. With no multiplexing the two are equal.
#[cfg(target_arch = "aarch64")]
#[derive(Debug)]
pub struct HwPerfEvent {
    /// The physical counter backing this event.
    counter: Counter,
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
    /// Per-task counting state, `Some` iff this event was opened with `pid > 0`.
    ///
    /// When set, this is the *only* live state: `counter` / `enabled_since` /
    /// `time_*` / `sampling` are inert placeholders, the counter is driven from
    /// the scheduler hooks in [`super::task`] (not from this fd's `enable`), and
    /// the `PerfEventOps` methods + `Drop` delegate to the per-task path. The
    /// `Arc` is shared with the target [`crate::task::Thread`]'s counter list.
    per_task: Option<Arc<super::task::PerTaskCounter>>,
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEvent {
    /// Reads the current raw counter value (cycle ⇒ 64-bit, programmable ⇒
    /// 32-bit zero-extended).
    fn raw_value(&self) -> u64 {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::read(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::read(n),
        }
    }

    /// The programmable counter index backing this event, if any. Sampling
    /// events are always programmable, so this is `Some` for them.
    fn programmable_index(&self) -> Option<usize> {
        match self.counter {
            Counter::Programmable(n) => Some(n),
            Counter::Cycle => None,
        }
    }

    /// Tears down the overflow-IRQ sampling path for this event, in the strict
    /// order required for `notify`-pointer soundness:
    ///
    /// 1. mask the overflow interrupt (`disable_irq`) — no new IRQs reference it,
    /// 2. stop the counter (`disable`) — it can no longer overflow,
    /// 3. clear the per-CPU `SampleSlot` (`unregister`) — the handler can no
    ///    longer reach the `notify` pointer,
    ///
    /// after which it is safe for the owning `Arc<IrqNotify>` / `Arc<GlobalPage>`
    /// to drop. Idempotent: safe to call from both `disable` and `Drop`.
    fn teardown_sampling_irq(&self) {
        if self.sampling.is_none() {
            return;
        }
        if let Some(n) = self.programmable_index() {
            ax_cpu::pmu::overflow::disable_irq(n);
            ax_cpu::pmu::counter::disable(n);
            sampling::unregister(n);
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for HwPerfEvent {
    fn drop(&mut self) {
        // Per-task events do not own a system-wide counter or sampling state:
        // release the HW counter through the per-task path (idempotent — the
        // task-exit hook may have freed it already) and stop here.
        if let Some(ptc) = &self.per_task {
            super::task::free_hw(ptc);
            return;
        }
        // For sampling events, mask the IRQ, stop the counter, and clear the
        // registry slot BEFORE the `Arc<IrqNotify>`/`Arc<GlobalPage>` held in
        // `sampling` drop, so the overflow handler can never dereference a
        // freed `notify` pointer or write into freed ring pages.
        self.teardown_sampling_irq();
        // Stop the cycle counter too (sampling already disabled its
        // programmable counter above; `disable` is idempotent), then release the
        // counter back to the allocator for reuse.
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::disable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::disable(n),
        }
        ALLOC.lock().free(self.counter);
        // Stop the deferred worker (mirrors `BpfPerfEventWrapper::drop`). The
        // `Arc`s in `sampling` drop after this returns.
        if let Some(sampling) = &self.sampling {
            sampling.poll_alive.store(false, Ordering::Release);
            sampling.notify.notify();
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Pollable for HwPerfEvent {
    fn poll(&self) -> IoEvents {
        match &self.sampling {
            // Sampling events are readable only when the ring has unread bytes
            // (`data_tail != data_head`): that is what `perf record`'s poll
            // waits on. Before the first mmap there is no ring ⇒ not readable.
            Some(sampling) => {
                if sampling.ring.as_ref().is_some_and(ring_has_data) {
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
/// Reads the two head/tail fields from the header page only while a live
/// mapping still pins the pages; an unmapped ring reports "no data".
#[cfg(target_arch = "aarch64")]
fn ring_has_data(ring: &RingState) -> bool {
    if !ring.is_mapped() {
        return false;
    }
    let header = ring.ring_vaddr as *const perf_event_mmap_page;
    // SAFETY: the header page is live (a VMA pins it) and was initialized by
    // `device_mmap`; these are plain `u64` fields read non-atomically, which is
    // fine for a readiness hint.
    let (head, tail) = unsafe {
        (
            core::ptr::addr_of!((*header).data_head).read_volatile(),
            core::ptr::addr_of!((*header).data_tail).read_volatile(),
        )
    };
    head != tail
}

#[cfg(target_arch = "aarch64")]
impl PerfEventOps for HwPerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        // Per-task: just record userspace intent. The target task's next
        // `perf_sched_in` programs the counter onto HW (or an immediate one if
        // it is the running task at the next switch).
        if let Some(ptc) = &self.per_task {
            ptc.set_enabled();
            return Ok(());
        }
        if self.enabled_since.is_none() {
            self.enabled_since = Some(ax_runtime::hal::time::monotonic_time_nanos());
        }
        // Sampling events: arm the overflow IRQ path before starting the
        // counter. A programmable counter is guaranteed (see `perf_event_open_hw`).
        if let Some(sampling) = &self.sampling {
            let Counter::Programmable(n) = self.counter else {
                // Should be unreachable: sampling always takes a programmable
                // counter. Fail loudly rather than silently never sampling.
                return Err(AxError::Unsupported);
            };
            let period = sampling.period;
            let sample_type = sampling.sample_type;
            // Capture the ring (if mmap'd) and the notify pointer for the slot.
            let ring = sampling.ring.as_ref();
            let (ring_vaddr, ring_len) = match ring {
                Some(r) => (r.ring_vaddr, r.ring_len),
                // No ring yet (enable before mmap): nothing for the handler to
                // write into. Use a zero slot so a stray overflow is a no-op.
                None => (0, 0),
            };
            let notify_ptr = Arc::as_ptr(&sampling.notify) as *const ();

            // 1. Make sure the PMU overflow IRQ handler is registered AND the
            //    PMU PPI is enabled on this core.
            sampling::ensure_pmu_irq_registered();
            // 2. Preload the counter so it overflows after `period` events.
            ax_cpu::pmu::counter::preload(n, period);
            // 3. Publish the slot so the handler can find this event's ring.
            sampling::register(
                n,
                SampleSlot {
                    ring_vaddr,
                    ring_len,
                    period,
                    sample_type,
                    notify: notify_ptr,
                },
            );
            // 4. Arm the per-counter overflow interrupt, then start counting.
            ax_cpu::pmu::overflow::enable_irq(n);
            ax_cpu::pmu::counter::enable(n);
            return Ok(());
        }

        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::enable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::enable(n),
        }
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        // Per-task: clear userspace intent. The next `perf_sched_out` folds the
        // live slice and stops the HW counter; future slices skip it.
        if let Some(ptc) = &self.per_task {
            ptc.set_disabled();
            return Ok(());
        }
        // Sampling events: strict teardown (mask IRQ → stop counter → unregister
        // slot) so the handler can no longer touch this event, then accrue time.
        if self.sampling.is_some() {
            self.teardown_sampling_irq();
        } else {
            match self.counter {
                Counter::Cycle => ax_cpu::pmu::cycles::disable(),
                Counter::Programmable(n) => ax_cpu::pmu::counter::disable(n),
            }
        }
        if let Some(since) = self.enabled_since.take() {
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            let elapsed = now.saturating_sub(since);
            self.time_enabled += elapsed;
            self.time_running += elapsed;
        }
        Ok(())
    }

    fn reset(&mut self) -> AxResult<()> {
        // Per-task: zero the accumulated count only (Linux `PERF_EVENT_IOC_RESET`
        // semantics); timing is preserved.
        if let Some(ptc) = &self.per_task {
            ptc.reset();
            return Ok(());
        }
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::reset(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::reset(n),
        }
        Ok(())
    }

    fn read_values(&mut self) -> AxResult<PerfReadValues> {
        // Per-task: the accumulated count + live slice lives on the shared
        // `PerTaskCounter`; serialize it per this fd's `read_format`.
        if let Some(ptc) = &self.per_task {
            let (value, time_enabled, time_running) = super::task::read_values(ptc);
            return Ok(PerfReadValues {
                value,
                time_enabled,
                time_running,
                id: 0,
                read_format: ptc.read_format(),
            });
        }
        // Current timing = accumulated past windows + the live window, if any.
        let (mut time_enabled, mut time_running) = (self.time_enabled, self.time_running);
        if let Some(since) = self.enabled_since {
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            let elapsed = now.saturating_sub(since);
            time_enabled += elapsed;
            time_running += elapsed;
        }
        Ok(PerfReadValues {
            value: self.raw_value(),
            time_enabled,
            time_running,
            // M1 does not assign per-event ids (no event groups); report 0.
            id: 0,
            read_format: self.read_format,
        })
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn device_mmap(&mut self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        // Only sampling events expose a ring buffer; counting events have
        // nothing to map.
        let Some(sampling) = &mut self.sampling else {
            return Err(AxError::Unsupported);
        };

        // One live mapping per perf fd (Linux semantics). A stale `Weak` from an
        // abandoned/munmap'd previous attempt does not count (its pages are
        // already freed), so the fd stays mmap-able. Mirrors `bpf.rs`.
        if sampling.ring.as_ref().is_some_and(RingState::is_mapped) {
            return Err(AxError::ResourceBusy);
        }

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
        // which is `>= size_of::<perf_event_mmap_page>()` (≥ 1 page = 4096 B,
        // and the struct is < 4096 B). No reader sees it until the VMA maps it.
        unsafe {
            core::ptr::addr_of_mut!((*header).data_offset).write(RING_DATA_OFFSET as u64);
            core::ptr::addr_of_mut!((*header).data_size).write(data_size);
            core::ptr::addr_of_mut!((*header).data_head).write(0);
            core::ptr::addr_of_mut!((*header).data_tail).write(0);
        }

        // Hand the sole strong ref to the caller (threaded into the VMA via
        // `DeviceMmap::Physical`'s retainer); keep only a `Weak`. See `bpf.rs`
        // for the ownership/UAF rationale.
        let pages = Arc::new(pages);
        sampling.ring = Some(RingState {
            pages: Arc::downgrade(&pages),
            ring_vaddr: kvirt.as_usize(),
            ring_len: len,
        });
        let anchor: Arc<dyn Any + Send + Sync> = pages;
        Ok((paddr, anchor))
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
pub fn perf_event_open_hw(attr: &perf_event_attr, pid: i32) -> AxResult<HwPerfEvent> {
    // No PMUv3 → no hardware events.
    let Some(info) = ax_cpu::pmu::probe() else {
        return Err(AxError::Unsupported);
    };

    // Idempotent per-CPU global enable (`PMCR_EL0.E`).
    ax_cpu::pmu::init_cpu();

    // Refresh the counter count the allocator sizes its bitmask against. Safe
    // to set every open: M1 is single-core so `num_counters` is invariant.
    ALLOC.lock().num_counters = info.num_counters;

    // `pid > 0`: attach a per-task counter to that task. `pid <= 0` (0 = self,
    // -1 = system-wide) keeps the existing M1/M2 behaviour untouched below.
    if pid > 0 {
        return perf_event_open_hw_per_task(attr, pid);
    }

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    // `sample_period` shares a union with `sample_freq` (frequency mode). M2
    // supports only fixed-period sampling, so read the period variant directly.
    // A non-zero value selects the sampling (M2) path; zero is counting (M1).
    // SAFETY: `perf_event_attr` is a `repr(C)` POD copied bytewise from user
    // space; both union arms are `u64`, so reading `sample_period` is sound.
    let sample_period = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_sampling = sample_period > 0;

    if is_sampling {
        // M2 supports exactly one record layout: PERF_SAMPLE_IP. Reject anything
        // else up front so the IRQ handler's fixed 16-byte record is always
        // valid. A period that does not fit a 32-bit programmable counter is
        // also rejected (the counter preload is 32-bit).
        if attr.sample_type != PERF_SAMPLE_IP {
            warn!(
                "perf_event_open: sampling sample_type {:#x} unsupported (only PERF_SAMPLE_IP in \
                 M2)",
                attr.sample_type
            );
            return Err(AxError::Unsupported);
        }
        if sample_period > u32::MAX as u64 {
            warn!("perf_event_open: sample_period {sample_period} exceeds 32-bit counter");
            return Err(AxError::InvalidInput);
        }
    }

    // Select the ARM event and counter. Sampling events ALWAYS take a
    // programmable counter — even CPU_CYCLES maps to ARM event 0x11 — because
    // the dedicated cycle counter is not used by the M2 overflow path.
    let counter = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        if attr.config == perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u64 && !is_sampling {
            // Counting CPU_CYCLES: the dedicated 64-bit cycle counter.
            let Some(counter) = ALLOC.lock().alloc_cycle() else {
                return Err(AxError::NoMemory);
            };
            // `exclude_*` map onto the cycle filter; `configure` also resets.
            ax_cpu::pmu::cycles::configure(exclude_user, exclude_kernel);
            counter
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
            alloc_programmable(event, exclude_user, exclude_kernel)?
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
        alloc_programmable(event, exclude_user, exclude_kernel)?
    } else {
        // HW_CACHE / BREAKPOINT and anything else are not supported.
        warn!(
            "perf_event_open: unsupported hardware type {:#x}",
            attr.type_
        );
        return Err(AxError::Unsupported);
    };

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
            period: sample_period as u32,
            sample_type: attr.sample_type,
            poll_ready,
            notify,
            poll_alive,
            ring: None,
        })
    } else {
        None
    };

    Ok(HwPerfEvent {
        counter,
        read_format: attr.read_format,
        // `disabled = 1`: do not enable; timing accumulators start empty.
        enabled_since: None,
        time_enabled: 0,
        time_running: 0,
        sampling,
        // System-wide / self event: not per-task.
        per_task: None,
    })
}

/// Open a per-task hardware-PMU counting event (`perf_event_open` with `pid >
/// 0`).
///
/// Resolves the target task, decodes the requested ARM event onto a
/// *programmable* counter (per-task never uses the dedicated cycle counter, so a
/// system-wide cycle event can run alongside it), reserves the slot from the M1
/// allocator without programming it, and attaches a shared
/// [`super::task::PerTaskCounter`] to the target [`crate::task::Thread`]. The HW
/// counter is programmed lazily by the scheduler hook the next time the target
/// runs (or by [`super::task::on_exec`] for `enable_on_exec`).
///
/// Per-task *sampling* is not implemented; a `sample_period > 0` per-task event
/// still counts (it just never produces samples).
#[cfg(target_arch = "aarch64")]
fn perf_event_open_hw_per_task(attr: &perf_event_attr, pid: i32) -> AxResult<HwPerfEvent> {
    use crate::task::AsThread;

    // Resolve the target task and its `Thread` (kernel tasks have none).
    let task = crate::task::get_task(pid as u32)?;
    let thr = task.try_as_thread().ok_or(AxError::NoSuchProcess)?;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

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
        n,
        event,
        exclude_user,
        exclude_kernel,
        attr.read_format,
        enabled,
        enable_on_exec,
    ));
    super::task::attach(thr, ptc.clone());

    Ok(HwPerfEvent {
        // Inert placeholders: the per-task path drives `ptc`, not these fields.
        counter: Counter::Programmable(n),
        read_format: attr.read_format,
        enabled_since: None,
        time_enabled: 0,
        time_running: 0,
        sampling: None,
        per_task: Some(ptc),
    })
}

/// Allocate a programmable counter, validate the event, and program it.
///
/// Common events (`< 0x40`) are gated through [`ax_cpu::pmu::event_supported`];
/// IMPLEMENTATION DEFINED events (`>= 0x40`) cannot be validated and are let
/// through. The counter is configured but left disabled.
#[cfg(target_arch = "aarch64")]
fn alloc_programmable(event: u16, exclude_user: bool, exclude_kernel: bool) -> AxResult<Counter> {
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
    // `configure` applies the event + filter and resets the counter to 0.
    ax_cpu::pmu::counter::configure(n, event, exclude_user, exclude_kernel);
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
pub fn perf_event_open_hw(_attr: &perf_event_attr, _pid: i32) -> AxResult<HwPerfEvent> {
    Err(AxError::Unsupported)
}

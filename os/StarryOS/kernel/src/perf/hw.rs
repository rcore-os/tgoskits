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

#[cfg(target_arch = "aarch64")]
use super::PerfReadValues;
#[cfg(target_arch = "aarch64")]
use super::sampling::{self, SampleSlot};
#[cfg(target_arch = "aarch64")]
use super::percpu;
use super::{PerfEventOps, target::ResolvedPerfTarget};
use crate::{StarryError, StarryResult};

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
/// Sysfs-discovered type for the Cortex-A55 PMU instance.
pub const ARMV8_CORTEX_A55_PERF_TYPE: u32 = 9;
/// Sysfs-discovered type for the Cortex-A76 PMU instance.
pub const ARMV8_CORTEX_A76_PERF_TYPE: u32 = 10;

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

/// Architecture-neutral request retained by task events until they are
/// scheduled on a concrete CPU. This is required on heterogeneous systems:
/// Linux resolves generic branch events against each CPU's PMCEID bitmap, so a
/// task migrating between clusters must not retain an encoding chosen on the
/// CPU that happened to execute `perf_event_open`.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy)]
pub(crate) enum PmuEventSpec {
    Hardware(u32),
    Cache(u64),
    Raw {
        event: u16,
        cluster: Option<ax_cpu::pmu::ClusterId>,
    },
}

#[cfg(target_arch = "aarch64")]
impl PmuEventSpec {
    pub(crate) fn resolve(self, info: ax_cpu::pmu::PmuInfo) -> StarryResult<u16> {
        match self {
            Self::Hardware(config) => ax_cpu::pmu::hw_event_to_arm_with(info, config)
                .ok_or(StarryError::NotFound),
            Self::Cache(config) => match ax_cpu::pmu::hw_cache_to_arm(config) {
                Ok(event) if info.event_supported(event) => Ok(event),
                Ok(_) | Err(ax_cpu::pmu::CacheEventError::Unsupported) => {
                    Err(StarryError::NotFound)
                }
                Err(ax_cpu::pmu::CacheEventError::Invalid) => Err(StarryError::InvalidInput),
            },
            Self::Raw { event, cluster }
                if cluster.is_none_or(|cluster| ax_cpu::pmu::classify_midr(info.midr) == cluster)
                    && info.event_supported(event) =>
            {
                Ok(event)
            }
            Self::Raw { .. } => Err(StarryError::NotFound),
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn event_spec(attr: &perf_event_attr) -> StarryResult<PmuEventSpec> {
    if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        Ok(PmuEventSpec::Hardware(attr.config as u32))
    } else if attr.type_ == perf_type_id::PERF_TYPE_HW_CACHE as u32 {
        Ok(PmuEventSpec::Cache(attr.config))
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32
        || attr.type_ == ARMV8_PMUV3_PERF_TYPE
        || attr.type_ == ARMV8_CORTEX_A55_PERF_TYPE
        || attr.type_ == ARMV8_CORTEX_A76_PERF_TYPE
    {
        let cluster = match attr.type_ {
            ARMV8_CORTEX_A55_PERF_TYPE => Some(ax_cpu::pmu::ClusterId::CortexA55),
            ARMV8_CORTEX_A76_PERF_TYPE => Some(ax_cpu::pmu::ClusterId::CortexA76),
            _ => None,
        };
        Ok(PmuEventSpec::Raw {
            event: (attr.config & 0xffff) as u16,
            cluster,
        })
    } else {
        Err(StarryError::OperationNotSupported)
    }
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
    /// Sampling period (events between overflows). Always `> 0`. In frequency
    /// mode this is the initial estimate; the overflow handler adapts it.
    period: u32,
    /// Frequency mode (`attr.freq`): the handler re-derives the period after each
    /// sample to converge on `target_freq` Hz. Fixed `-c` period when false.
    freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    target_freq: u32,
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
    /// `PERF_EVENT_IOC_SET_OUTPUT` redirect: when `Some((vaddr, len, anchor))`,
    /// this event's overflow handler writes into *another* event's ring
    /// (`vaddr`/`len`) instead of `ring`, so `perf record -e a,b` lands both
    /// events in one mmap buffer. `anchor` pins the target ring's pages for as
    /// long as this event may write into them.
    redirect: Option<(usize, usize, Arc<dyn Any + Send + Sync>)>,
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

/// Allocate, zero, and header-initialize one sampling mmap ring of `len` bytes.
///
/// Shared by the M2 system-wide path ([`HwPerfEvent::device_mmap`]) and the
/// per-task sampling path. Validates the libbpf/`perf` mmap geometry
/// (`(1 + 2^N) * PAGE_SIZE`), allocates contiguous pages, zeros them, writes the
/// `perf_event_mmap_page` header's data-region geometry, and returns the sole
/// strong `Arc<GlobalPage>` (the caller threads it into the VMA retainer and/or
/// keeps an anchor), the ring's kernel vaddr, and its physical start.
#[cfg(target_arch = "aarch64")]
fn alloc_sampling_ring(len: usize) -> StarryResult<(Arc<GlobalPage>, usize, PhysAddr)> {
    // libbpf/`perf` require `(1 + 2^N) * PAGE_SIZE`: one header page plus a
    // power-of-two-page data ring. Reject anything else.
    if len == 0 || !len.is_multiple_of(ax_memory_addr::PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }
    let num_pages = len / ax_memory_addr::PAGE_SIZE_4K;
    if num_pages < 2 || !(num_pages - 1).is_power_of_two() {
        return Err(StarryError::InvalidInput);
    }

    // Allocate and zero the contiguous ring pages (mirror `bpf.rs`).
    let mut pages = GlobalPage::alloc_contiguous(num_pages, ax_memory_addr::PAGE_SIZE_4K)
        .map_err(|_| StarryError::NoMemory)?;
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
pub struct HwPerfEvent {
    /// Logical CPU that owns `counter` and every PMU register operation.
    owner_cpu: usize,
    /// The physical counter backing this event.
    counter: Counter,
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
    fn raw_value_on_owner(&self) -> u64 {
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
    fn teardown_sampling_irq_on_owner(&self) {
        if self.sampling.is_none() {
            return;
        }
        if let Some(n) = self.programmable_index() {
            ax_cpu::pmu::overflow::disable_irq(n);
            ax_cpu::pmu::counter::disable(n);
            sampling::unregister(n);
        }
    }

    fn disable_counter_on_owner(&self) {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::disable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::disable(n),
        }
    }

    fn release_counter_on_owner(&self) {
        self.disable_counter_on_owner();
        match self.counter {
            Counter::Cycle => percpu::free_cycle(),
            Counter::Programmable(n) => percpu::free_programmable(n),
        }
    }

    fn enable_on_owner(&mut self) -> StarryResult<()> {
        if self.enabled_since.is_none() {
            self.enabled_since = Some(ax_runtime::hal::time::monotonic_time_nanos());
        }
        if let Some(sampling) = &self.sampling {
            let Counter::Programmable(n) = self.counter else {
                return Err(StarryError::OperationNotSupported);
            };
            let (ring_vaddr, ring_len) = if let Some((vaddr, len, _)) = &sampling.redirect {
                (*vaddr, *len)
            } else {
                sampling
                    .ring
                    .as_ref()
                    .map_or((0, 0), |ring| (ring.ring_vaddr, ring.ring_len))
            };
            ax_cpu::pmu::counter::preload(n, sampling.period);
            sampling::register(
                n,
                SampleSlot {
                    ring_vaddr,
                    ring_len,
                    period: sampling.period,
                    sample_type: sampling.sample_type,
                    id: self.sample_id,
                    observer: crate::task::ROOT_PID_NS.id(),
                    notify: Arc::as_ptr(&sampling.notify).cast(),
                    freq: sampling.freq,
                    target_freq: sampling.target_freq,
                    last_time: 0,
                },
            );
            ax_cpu::pmu::overflow::enable_irq(n);
            ax_cpu::pmu::counter::enable(n);
        } else {
            match self.counter {
                Counter::Cycle => ax_cpu::pmu::cycles::enable(),
                Counter::Programmable(n) => ax_cpu::pmu::counter::enable(n),
            }
        }
        Ok(())
    }

    fn disable_on_owner(&mut self) {
        if self.sampling.is_some() {
            self.teardown_sampling_irq_on_owner();
        } else {
            self.disable_counter_on_owner();
        }
        if let Some(since) = self.enabled_since.take() {
            let elapsed = ax_runtime::hal::time::monotonic_time_nanos().saturating_sub(since);
            self.time_enabled += elapsed;
            self.time_running += elapsed;
        }
    }

    fn reset_on_owner(&self) {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::reset(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::reset(n),
        }
    }

    fn read_values_on_owner(&self) -> PerfReadValues {
        let (mut time_enabled, mut time_running) = (self.time_enabled, self.time_running);
        if let Some(since) = self.enabled_since {
            let elapsed = ax_runtime::hal::time::monotonic_time_nanos().saturating_sub(since);
            time_enabled += elapsed;
            time_running += elapsed;
        }
        PerfReadValues {
            eof: false,
            value: self.raw_value_on_owner(),
            time_enabled,
            time_running,
            read_format: self.read_format,
        }
    }

    /// `device_mmap` for a counting event: the single-page `perf_event_mmap_page`
    /// userspace maps for `rdpmc` self-monitoring.
    ///
    /// No ring buffer — the page only carries the metadata a userspace reader
    /// needs to read this event's hardware counter directly: `cap_user_rdpmc`,
    /// the 1-based `index` selecting the counter, its `pmc_width`, and the count
    /// already accumulated in `offset`. System-wide metadata is static;
    /// per-task metadata starts inactive and scheduler hooks update its
    /// seqlock/index/offset at every slice boundary.
    /// EL0 read access to the counters is enabled globally in
    /// [`ax_cpu::pmu::init_cpu`] via `PMUSERENR_EL0`.
    fn device_mmap_rdpmc(
        &self,
        len: usize,
    ) -> StarryResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        // Exactly one metadata page is allocated below. Accepting a larger VMA
        // would make the physical mapping continue into unrelated adjacent
        // memory after that page.
        if len != ax_memory_addr::PAGE_SIZE_4K {
            return Err(StarryError::InvalidInput);
        }
        if self
            .per_task
            .as_ref()
            .is_some_and(|ptc| ptc.rdpmc_page_mapped())
        {
            return Err(StarryError::ResourceBusy);
        }
        let mut pages = GlobalPage::alloc_contiguous(1, ax_memory_addr::PAGE_SIZE_4K)
            .map_err(|_| StarryError::NoMemory)?;
        pages.zero();
        let kvirt = pages.start_vaddr();
        let paddr = virt_to_phys(kvirt);

        // Encode which hardware counter backs this event. The mmap-page `index`
        // is 1-based (0 ⇒ rdpmc unusable); `index - 1` is the ARM counter the
        // reader accesses — `PMEVCNTR(index-1)_EL0`, or `PMCCNTR_EL0` for the
        // dedicated cycle counter (ARM index 31 ⇒ page index 32).
        let (index, pmc_width): (u32, u16) = match (&self.per_task, self.counter) {
            (Some(_), Counter::Programmable(_)) => (0, 32),
            (None, Counter::Cycle) => (32, 64),
            (None, Counter::Programmable(n)) => (n as u32 + 1, 32),
            (Some(_), Counter::Cycle) => return Err(StarryError::Unsupported),
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
            // `cap_bit0_is_deprecated` (bit 1) tells userspace the capability
            // bits have their post-3.12 meanings; `cap_user_rdpmc` is bit 2.
            core::ptr::addr_of_mut!((*header).__bindgen_anon_1.capabilities)
                .write((1u64 << 1) | (1u64 << 2));
        }

        let pages = Arc::new(pages);
        if let Some(ptc) = &self.per_task
            && !ptc.install_rdpmc_page(pages.clone())
        {
            return Err(StarryError::ResourceBusy);
        }
        let anchor: Arc<dyn Any + Send + Sync> = pages;
        Ok((paddr, anchor))
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
        let owner = self.owner_cpu;
        // SAFETY: the closure only touches PMU registers, the CPU-local
        // allocator, and the already-owned sampling registry. It allocates
        // nothing and cannot sleep in the remote IPI context.
        if let Err(error) = unsafe {
            percpu::run_on_cpu_sync(owner, || {
                self.teardown_sampling_irq_on_owner();
                self.release_counter_on_owner();
            })
        } {
            warn!("perf: failed to release counter on CPU {owner}: {error}");
        }
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
        // Per-task events: a sampling one is readable when its ring (on the
        // shared `PerTaskCounter`) has unread bytes; a counting one is always
        // readable (`read(perf_fd)` returns the current value without blocking).
        if let Some(ptc) = &self.per_task {
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
        // Per-task sampling events register a waker on the ptc's `PollSet` (the
        // one the per-task notify worker wakes). Counting events (per-task or
        // system-wide) never transition readiness, so they register nothing.
        if let Some(ptc) = &self.per_task {
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
    fn enable(&mut self) -> StarryResult<()> {
        // Per-task: just record userspace intent. The target task's next
        // `perf_sched_in` programs the counter onto HW (or an immediate one if
        // it is the running task at the next switch).
        if let Some(ptc) = &self.per_task {
            return ptc.set_enabled();
        }
        let owner = self.owner_cpu;
        // SAFETY: `enable_on_owner` performs only bounded PMU/register/registry
        // operations and is safe in the target CPU's IPI context.
        unsafe { percpu::run_on_cpu_sync(owner, || self.enable_on_owner()) }?
    }

    fn disable(&mut self) -> StarryResult<()> {
        // Per-task: clear userspace intent. The next `perf_sched_out` folds the
        // live slice and stops the HW counter; future slices skip it.
        if let Some(ptc) = &self.per_task {
            return ptc.set_disabled();
        }
        let owner = self.owner_cpu;
        // SAFETY: owner-side disable is bounded and allocation-free.
        unsafe { percpu::run_on_cpu_sync(owner, || self.disable_on_owner()) }?;
        Ok(())
    }

    fn reset(&mut self) -> StarryResult<()> {
        // Per-task: zero the accumulated count only (Linux `PERF_EVENT_IOC_RESET`
        // semantics); timing is preserved.
        if let Some(ptc) = &self.per_task {
            return ptc.reset();
        }
        let owner = self.owner_cpu;
        // SAFETY: owner-side reset is one PMU register operation.
        unsafe { percpu::run_on_cpu_sync(owner, || self.reset_on_owner()) }?;
        Ok(())
    }

    fn read_values(&mut self) -> StarryResult<PerfReadValues> {
        // Per-task: the accumulated count + live slice lives on the shared
        // `PerTaskCounter`; serialize it per this fd's `read_format`.
        if let Some(ptc) = &self.per_task {
            let (value, time_enabled, time_running) = super::task::read_values(ptc)?;
            return Ok(PerfReadValues {
                eof: ptc.scheduling_error(),
                value,
                time_enabled,
                time_running,
                read_format: ptc.read_format(),
            });
        }
        let owner = self.owner_cpu;
        // SAFETY: owner-side read performs bounded PMU and clock reads only.
        unsafe { percpu::run_on_cpu_sync(owner, || self.read_values_on_owner()) }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_sample_id(&mut self, id: u64) {
        self.sample_id = id;
        // Per-task: mirror onto the shared counter the scheduler hook reads.
        if let Some(ptc) = &self.per_task {
            ptc.set_sample_id(id);
        }
    }

    fn output_ring(&self) -> Option<(usize, usize, Arc<dyn Any + Send + Sync>)> {
        // Per-task: the ring lives on the shared `PerTaskCounter`.
        if let Some(ptc) = &self.per_task {
            return ptc.output_ring();
        }
        // System-wide sampling: hand out the mapped ring, upgrading the `Weak` to
        // a strong `Arc` so the redirecting event pins the pages even if this
        // event is later closed/munmap'd.
        let ring = self.sampling.as_ref()?.ring.as_ref()?;
        let pages = ring.pages.upgrade()?;
        let anchor: Arc<dyn Any + Send + Sync> = pages;
        Some((ring.ring_vaddr, ring.ring_len, anchor))
    }

    fn redirect_output(
        &mut self,
        ring_vaddr: usize,
        ring_len: usize,
        anchor: Arc<dyn Any + Send + Sync>,
    ) -> StarryResult<()> {
        // Per-task sampling source: stash the redirect on the shared counter so
        // the scheduler hook arms this counter to write into the target ring.
        if let Some(ptc) = &self.per_task {
            ptc.set_redirect_ring(ring_vaddr, ring_len, anchor);
            return Ok(());
        }
        // System-wide sampling source: record the redirect; `enable` builds the
        // `SampleSlot` against it. A non-sampling (counting) HW event produces no
        // records, so redirecting it is a harmless no-op.
        if let Some(sampling) = &mut self.sampling {
            sampling.redirect = Some((ring_vaddr, ring_len, anchor));
        }
        Ok(())
    }

    fn detach_output(&mut self) -> StarryResult<()> {
        if let Some(ptc) = &self.per_task {
            ptc.detach_redirect_ring();
            return Ok(());
        }
        if let Some(sampling) = &mut self.sampling {
            sampling.redirect = None;
        }
        Ok(())
    }

    fn device_mmap(&mut self, len: usize) -> StarryResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        // Per-task sampling owns a ring on `PerTaskCounter`; per-task counting
        // exposes the same one-page rdpmc metadata ABI as system-wide counting.
        // The reserved programmable slot remains stable for the event lifetime,
        // while scheduler hooks enable it only on the target task's slices.
        if let Some(ptc) = &self.per_task {
            if ptc.is_sampling() {
                return device_mmap_per_task(ptc, len);
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
        if sampling.ring.as_ref().is_some_and(RingState::is_mapped) {
            return Err(StarryError::ResourceBusy);
        }

        // Allocate + zero + header-init the ring (shared with the per-task path).
        let (pages, ring_vaddr, paddr) = alloc_sampling_ring(len)?;

        // Hand the sole strong ref to the caller (threaded into the VMA via
        // `DeviceMmap::Physical`'s retainer); keep only a `Weak`. See `bpf.rs`
        // for the ownership/UAF rationale.
        sampling.ring = Some(RingState {
            pages: Arc::downgrade(&pages),
            ring_vaddr,
            ring_len: len,
        });
        let anchor: Arc<dyn Any + Send + Sync> = pages;
        Ok((paddr, anchor))
    }
}

/// `device_mmap` for a per-task sampling event.
///
/// Allocates the ring (via [`alloc_sampling_ring`]), spawns the deferred notify
/// worker, and stores the ring vaddr/len + the page/notify/poll anchors onto the
/// shared [`super::task::PerTaskCounter`] via `set_ring`. The next
/// [`super::task::perf_sched_in`] for the target task will see a mapped ring and
/// arm the overflow IRQ. The returned anchor is the ring pages `Arc`, threaded
/// into the user VMA so the mapping outlives `close(perf_fd)`.
///
/// Rejecting a second mmap: a per-task event is opened once and mmap'd once by
/// `perf record`; a second attempt while the ring is still set is rejected.
#[cfg(target_arch = "aarch64")]
fn device_mmap_per_task(
    ptc: &Arc<super::task::PerTaskCounter>,
    len: usize,
) -> StarryResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
    // Counting events use `HwPerfEvent::device_mmap_rdpmc`; this helper owns
    // only the sampling ring path.
    if !ptc.is_sampling() {
        return Err(StarryError::Unsupported);
    }
    // One live ring per fd: refuse if a ring is already mapped.
    if ptc.ring_mapped() {
        return Err(StarryError::ResourceBusy);
    }

    let (pages, ring_vaddr, paddr) = alloc_sampling_ring(len)?;

    // Spawn the deferred worker (mirrors the M2 path): it turns IRQ-context
    // `notify_irq` pokes into `axpoll` `IoEvents::IN` wakeups.
    let poll_ready = Arc::new(PollSet::new());
    let notify = Arc::new(IrqNotify::new());
    let poll_alive = Arc::new(AtomicBool::new(true));
    start_sampling_notify_worker(poll_ready.clone(), notify.clone(), poll_alive.clone());

    // Publish the ring + anchors onto the ptc so `perf_sched_in` can arm it.
    ptc.set_ring(
        pages.clone(),
        ring_vaddr,
        len,
        notify,
        poll_ready,
        poll_alive,
    );

    let anchor: Arc<dyn Any + Send + Sync> = pages;
    Ok((paddr, anchor))
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
pub fn perf_event_open_hw(
    attr: &perf_event_attr,
    target: &ResolvedPerfTarget,
) -> StarryResult<HwPerfEvent> {
    match target {
        ResolvedPerfTarget::Task { task, cpu, .. } => {
            return perf_event_open_hw_per_task(attr, task.clone(), *cpu);
        }
        ResolvedPerfTarget::Cpu(_) => {}
    }

    let ResolvedPerfTarget::Cpu(owner) = target else {
        unreachable!();
    };
    let owner_cpu = owner.as_usize();
    let info = percpu::cpu_info(owner_cpu).ok_or(StarryError::OperationNotSupported)?;

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
            return Err(StarryError::Unsupported);
        }
        // A fixed period must fit the 32-bit programmable counter (the preload is
        // 32-bit). Frequency mode carries a (small) rate here, not a period.
        if !is_freq && raw > u32::MAX as u64 {
            warn!("perf_event_open: sample_period {raw} exceeds 32-bit counter");
            return Err(StarryError::InvalidInput);
        }
    }
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);

    // Select the ARM event and counter. Sampling events ALWAYS take a
    // programmable counter — even CPU_CYCLES maps to ARM event 0x11 — because
    // the dedicated cycle counter is not used by the M2 overflow path.
    enum CounterRequest {
        Cycle,
        Programmable(u16),
    }
    let spec = event_spec(attr)?;
    let request = if matches!(spec, PmuEventSpec::Hardware(config) if config == perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u32)
        && !is_sampling
    {
        CounterRequest::Cycle
    } else {
        CounterRequest::Programmable(spec.resolve(info)?)
    };

    // SAFETY: allocation and configuration touch only owner-local PMU state;
    // the closure is bounded, allocation-free, and safe in IPI context.
    let counter = unsafe {
        percpu::run_on_cpu_sync(owner_cpu, move || match request {
            CounterRequest::Cycle => {
                if !percpu::alloc_cycle() {
                    return Err(StarryError::ResourceBusy);
                }
                ax_cpu::pmu::cycles::configure(exclude_user, exclude_kernel);
                Ok(Counter::Cycle)
            }
            CounterRequest::Programmable(event) => {
                let Some(counter) = percpu::alloc_programmable() else {
                    return Err(StarryError::ResourceBusy);
                };
                ax_cpu::pmu::counter::configure(
                    counter,
                    event,
                    exclude_user,
                    exclude_kernel,
                );
                Ok(Counter::Programmable(counter))
            }
        })
    }??;

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
            ring: None,
            redirect: None,
        })
    } else {
        None
    };

    Ok(HwPerfEvent {
        owner_cpu,
        counter,
        // Assigned by `set_sample_id` once the `PerfEvent` wrapper is built.
        sample_id: 0,
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

/// Open a per-task hardware-PMU event (`perf_event_open` with `pid > 0`):
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
    task: ax_task::AxTaskRef,
    cpu_filter: Option<super::target::PerfCpuId>,
) -> StarryResult<HwPerfEvent> {
    use crate::task::AsThread;

    // Resolve the target task's `Thread` (kernel tasks have none).
    let thr = task.try_as_thread().ok_or(StarryError::NoSuchProcess)?;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;
    let validation_cpu = cpu_filter.map_or_else(
        ax_hal::percpu::this_cpu_id,
        super::target::PerfCpuId::as_usize,
    );
    let info = percpu::cpu_info(validation_cpu).ok_or(StarryError::OperationNotSupported)?;

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
            return Err(StarryError::Unsupported);
        }
        if !is_freq && raw > u32::MAX as u64 {
            warn!("perf_event_open: per-task sample_period {raw} exceeds 32-bit");
            return Err(StarryError::InvalidInput);
        }
    }
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);

    // Retain the generic request and resolve it again on every scheduling CPU.
    let spec = event_spec(attr)?;
    let _ = spec.resolve(info)?;

    // `disabled = 0` ⇒ count from the next sched-in; `disabled = 1` ⇒ wait for
    // `enable_on_exec` / `ioctl(ENABLE)`. `perf stat -- cmd` sets both
    // `disabled` and `enable_on_exec`, so it starts counting at the child's exec.
    let enabled = attr.disabled() == 0;
    let enable_on_exec = attr.enable_on_exec() != 0;

    let ptc = Arc::new(super::task::PerTaskCounter::new(
        super::task::PerTaskConfig {
            cpu_filter: cpu_filter.map(super::target::PerfCpuId::as_usize),
            event: spec,
            exclude_user,
            exclude_kernel,
            read_format: attr.read_format,
            enabled,
            enable_on_exec,
            pinned: attr.pinned() != 0,
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
            observer: ax_task::current().as_thread().active_pid_namespace().id(),
        },
    ));
    super::task::attach(thr, ptc.clone());

    Ok(HwPerfEvent {
        owner_cpu: cpu_filter.map_or_else(
            ax_hal::percpu::this_cpu_id,
            super::target::PerfCpuId::as_usize,
        ),
        // Inert placeholders: the per-task path drives `ptc`, not these fields.
        counter: Counter::Programmable(0),
        // Mirrors the wrapper id onto the ptc via `set_sample_id`; 0 until then.
        sample_id: 0,
        read_format: attr.read_format,
        enabled_since: None,
        time_enabled: 0,
        time_running: 0,
        sampling: None,
        per_task: Some(ptc),
    })
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
    fn enable(&mut self) -> StarryResult<()> {
        Err(StarryError::Unsupported)
    }

    fn disable(&mut self) -> StarryResult<()> {
        Err(StarryError::Unsupported)
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Non-aarch64 fallback: no hardware PMU support outside ARM PMUv3.
#[cfg(not(target_arch = "aarch64"))]
pub fn perf_event_open_hw(
    _attr: &perf_event_attr,
    _target: &ResolvedPerfTarget,
) -> StarryResult<HwPerfEvent> {
    Err(StarryError::Unsupported)
}

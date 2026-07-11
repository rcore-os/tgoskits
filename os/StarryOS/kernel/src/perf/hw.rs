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
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// Dynamic `perf_event_attr.type` for the Cortex-A55 ("LITTLE") cluster PMU,
/// exposed at `/sys/bus/event_source/devices/armv8_cortex_a55/type`. An event
/// opened against it is restricted to the A55 cluster (its `cpus` mask).
pub const ARMV8_CORTEX_A55_TYPE: u32 = 9;

/// Dynamic `perf_event_attr.type` for the Cortex-A76 ("big") cluster PMU,
/// exposed at `/sys/bus/event_source/devices/armv8_cortex_a76/type`. An event
/// opened against it is restricted to the A76 cluster.
pub const ARMV8_CORTEX_A76_TYPE: u32 = 10;

/// The [`ClusterMask`](super::percpu::ClusterMask) an event opened against
/// hardware `type_` is restricted to: the generic PMU (`PERF_TYPE_HARDWARE` /
/// `PERF_TYPE_RAW` / `armv8_pmuv3_0` = 8) runs on all clusters; the cluster PMUs
/// (9 / 10) are pinned to their cluster. `None` if `type_` is not a hardware PMU.
#[cfg(target_arch = "aarch64")]
fn cluster_mask_for_type(type_: u32) -> Option<super::percpu::ClusterMask> {
    use super::percpu::ClusterMask;
    if type_ == perf_type_id::PERF_TYPE_HARDWARE as u32
        || type_ == perf_type_id::PERF_TYPE_RAW as u32
        || type_ == ARMV8_PMUV3_PERF_TYPE
    {
        Some(ClusterMask::ALL)
    } else if type_ == ARMV8_CORTEX_A55_TYPE {
        Some(ClusterMask::LITTLE_ONLY)
    } else if type_ == ARMV8_CORTEX_A76_TYPE {
        Some(ClusterMask::BIG_ONLY)
    } else {
        None
    }
}

/// Resolve a Linux `perf_hw_id` to an ARM event number, with the per-cluster
/// `PERF_COUNT_HW_BRANCH_INSTRUCTIONS` (hw_id 4) special case.
///
/// Mirrors Linux's `__armv8_pmuv3_map_event_id`: BRANCH_INSTRUCTIONS prefers
/// `PC_WRITE_RETIRED` (0x0C) when the CURRENT core implements it (true on A55),
/// else `BR_RETIRED` (0x21, A76). The repo's static [`ax_cpu::pmu::hw_event_to_arm`]
/// hard-maps it to 0x21, which is wrong on A55, so this resolver is used at the
/// open/program sites where a per-core `PMCEID` (`event_supported`) is available.
/// Every other hw_id falls through to the architectural static map.
#[cfg(target_arch = "aarch64")]
fn resolve_hw_event(hw_id: u32) -> Option<u16> {
    const PERF_COUNT_HW_BRANCH_INSTRUCTIONS: u32 = 4;
    const PC_WRITE_RETIRED: u16 = 0x0C;
    const BR_RETIRED: u16 = 0x21;
    if hw_id == PERF_COUNT_HW_BRANCH_INSTRUCTIONS {
        if ax_cpu::pmu::event_supported(PC_WRITE_RETIRED) {
            return Some(PC_WRITE_RETIRED);
        }
        if ax_cpu::pmu::event_supported(BR_RETIRED) {
            return Some(BR_RETIRED);
        }
        return None;
    }
    ax_cpu::pmu::hw_event_to_arm(hw_id)
}

/// Decode the ARM PMUv3 event number from a hardware `perf_event_attr`
/// (`type_` + `config`): `PERF_TYPE_HARDWARE` via the per-cluster
/// [`resolve_hw_event`]; `PERF_TYPE_RAW` and the sysfs PMU types (generic
/// `armv8_pmuv3_0` = 8, cluster `armv8_cortex_a55`/`_a76` = 9/10) take the low 16
/// bits of `config`. `None` for an unsupported type/config.
#[cfg(target_arch = "aarch64")]
fn decode_arm_event(type_: u32, config: u64) -> Option<u16> {
    if type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        resolve_hw_event(config as u32)
    } else if type_ == perf_type_id::PERF_TYPE_RAW as u32
        || type_ == ARMV8_PMUV3_PERF_TYPE
        || type_ == ARMV8_CORTEX_A55_TYPE
        || type_ == ARMV8_CORTEX_A76_TYPE
    {
        Some((config & 0xFFFF) as u16)
    } else {
        None
    }
}

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

/// The counter allocator is per-CPU (`super::percpu::ALLOC`): `PMEVCNTRn_EL0` is
/// banked per-PE, so each core owns its own pool. Reservation/release go through
/// [`super::percpu::alloc_programmable_counter`] /
/// [`super::percpu::free_programmable_counter`] (and the cycle-counter pair),
/// which the per-task path drives per scheduling slice and the system-wide path
/// at open/close on the owning core.

/// The backing pages of a sampling event's mmap ring buffer, after the first
/// `mmap(perf_fd)`.
///
/// Ownership mirrors [`super::bpf::BpfPerfEventWrapper`]: the strong
/// `Arc<GlobalPage>` is handed to the user VMA via `DeviceMmap::PhysicalCached`'s
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
    /// Samples dropped because the ring was full. The overflow handler bumps it
    /// through the registered [`SampleSlot`]'s `lost` pointer at this `Arc`, and
    /// `read` returns it for `PERF_FORMAT_LOST`. The `Arc` keeps the counter alive
    /// for that raw pointer; it drops only after teardown unregisters the slot.
    lost: Arc<AtomicU64>,
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

/// A system-wide event pinned to a specific CPU (`perf_event_open` with
/// `pid <= 0 && cpu >= 0`, i.e. `perf stat -a`'s per-CPU fan-out).
///
/// Counting only: the event programs a programmable counter from the *target*
/// core's per-CPU pool and counts all activity on that core. Because the fd may
/// be opened, read, and closed from a different core, those operations run on
/// `cpu` via a synchronous IPI (mirroring Linux `smp_call_function_single` in
/// `__perf_event_read` / `perf_install_in_context`); they are infrequent (open /
/// end-of-run read / close), never a hot path. `slot` is the counter index on
/// `cpu`, `None` until the first `enable`.
#[cfg(target_arch = "aarch64")]
#[derive(Debug)]
struct SysCpuBinding {
    cpu: usize,
    event: u16,
    exclude_user: bool,
    exclude_kernel: bool,
    slot: Option<usize>,
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
    /// Cpu-bound system-wide counting state, `Some` iff opened with
    /// `pid <= 0 && cpu >= 0` (the `perf stat -a` fan-out). When set, the counter
    /// lives on `sys_cpu.cpu`'s per-CPU pool and `enable` / `read_values` /
    /// `disable` / `Drop` drive it there (locally or via IPI); `counter` is an
    /// inert placeholder. The timing fields (`enabled_since` / `time_*`) still
    /// apply (a cpu-bound event runs continuously while enabled).
    sys_cpu: Option<SysCpuBinding>,
    /// The core this event's HW counter lives on, for the self/system-wide
    /// (`pid <= 0 && cpu < 0`) path whose counter is allocated on the opening
    /// core and counts there. Because the monitoring thread is migratable, the
    /// HW lifecycle (`enable`/`disable`/`reset`/`read`/`Drop`) must run on
    /// `home_cpu` — via a synchronous IPI when the caller has migrated — so it
    /// never stomps another core's banked `PMEVCNTRn` or frees the slot in the
    /// wrong per-CPU pool. `usize::MAX` for the per-task / `sys_cpu` paths (which
    /// route their HW ops to the owning core by other means).
    home_cpu: usize,
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

    /// Samples the sampling ring dropped for this event (`0` for a non-sampling
    /// event), for `read`'s `PERF_FORMAT_LOST` field.
    fn sampling_lost(&self) -> u64 {
        self.sampling
            .as_ref()
            .map_or(0, |s| s.lost.load(Ordering::Relaxed))
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

    /// Program (alloc + configure + enable) this cpu-bound system event's counter
    /// on its target core. Idempotent: a no-op if already armed. Returns
    /// `NoMemory` if the target core's pool is full.
    fn sys_program(&mut self) -> AxResult<()> {
        let Some(sc) = &mut self.sys_cpu else {
            return Ok(());
        };
        if sc.slot.is_some() {
            return Ok(());
        }
        let mut op = SysCpuOp {
            op: SYS_OP_PROGRAM,
            event: sc.event,
            exclude_user: sc.exclude_user,
            exclude_kernel: sc.exclude_kernel,
            slot: 0,
            value: 0,
            ok: false,
        };
        run_sys_cpu_op(sc.cpu, &mut op);
        if !op.ok {
            return Err(AxError::NoMemory);
        }
        sc.slot = Some(op.slot);
        Ok(())
    }

    /// Build + run a slot-only [`SysCpuOp`] (`START` / `STOP` / `READ`) on the
    /// target core. No-op before the counter is allocated.
    fn sys_slot_op(&self, op_code: u8) -> u64 {
        let Some(sc) = &self.sys_cpu else {
            return 0;
        };
        let Some(n) = sc.slot else {
            return 0;
        };
        let mut op = SysCpuOp {
            op: op_code,
            event: sc.event,
            exclude_user: sc.exclude_user,
            exclude_kernel: sc.exclude_kernel,
            slot: n,
            value: 0,
            ok: false,
        };
        run_sys_cpu_op(sc.cpu, &mut op);
        op.value
    }

    /// Start (enable) the already-configured counter on its target core.
    fn sys_start(&self) {
        self.sys_slot_op(SYS_OP_START);
    }

    /// Stop (disable) the counter on its target core, keeping it allocated so its
    /// value stays readable until `Drop`.
    fn sys_stop(&self) {
        self.sys_slot_op(SYS_OP_STOP);
    }

    /// Reset the counter to 0 on its target core (re-configure), if allocated.
    fn sys_reset(&self) {
        self.sys_slot_op(SYS_OP_RESET);
    }

    /// Stop + free this cpu-bound system event's counter on its target core.
    fn sys_free(&mut self) {
        let Some(sc) = &mut self.sys_cpu else {
            return;
        };
        if let Some(n) = sc.slot.take() {
            let mut op = SysCpuOp {
                op: SYS_OP_FREE,
                event: 0,
                exclude_user: false,
                exclude_kernel: false,
                slot: n,
                value: 0,
                ok: false,
            };
            run_sys_cpu_op(sc.cpu, &mut op);
        }
    }

    /// Read this cpu-bound system event's counter value from its target core.
    fn sys_read(&self) -> u64 {
        self.sys_slot_op(SYS_OP_READ)
    }

    // --- self/system-wide (`pid<=0 && cpu<0`) HW lifecycle, pinned to `home_cpu`.
    // The `*_local` methods touch this core's banked PMU state directly; they run
    // either on `home_cpu` (when the caller is there) or via [`hw_home_thunk`]
    // over an IPI. They carry the exact logic the `enable`/`disable`/`reset`/
    // `read`/`Drop` paths used before, just routed to the owning core.

    /// Arm the HW counter (sampling overflow path, or plain counter enable).
    fn hw_enable_local(&mut self) {
        if let Some(sampling) = &self.sampling {
            let Counter::Programmable(n) = self.counter else {
                return; // unreachable: sampling always takes a programmable counter
            };
            let period = sampling.period;
            let sample_type = sampling.sample_type;
            let freq = sampling.freq;
            let target_freq = sampling.target_freq;
            let (ring_vaddr, ring_len) = if let Some((rv, rl, _anchor)) = &sampling.redirect {
                (*rv, *rl)
            } else {
                match sampling.ring.as_ref() {
                    Some(r) => (r.ring_vaddr, r.ring_len),
                    None => (0, 0),
                }
            };
            let notify_ptr = Arc::as_ptr(&sampling.notify) as *const ();
            let lost_ptr = Arc::as_ptr(&sampling.lost) as *const ();
            sampling::ensure_pmu_irq_registered();
            ax_cpu::pmu::counter::preload(n, period);
            sampling::register(
                n,
                SampleSlot {
                    ring_vaddr,
                    ring_len,
                    period,
                    sample_type,
                    id: self.sample_id,
                    notify: notify_ptr,
                    freq,
                    target_freq,
                    last_time: 0,
                    lost: lost_ptr,
                    // System-wide sampling: attribute to the interrupted
                    // `current()` in the handler (it matches the sampled IP).
                    owner_ids: None,
                },
            );
            ax_cpu::pmu::overflow::enable_irq(n);
            ax_cpu::pmu::counter::enable(n);
            return;
        }
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::enable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::enable(n),
        }
    }

    /// Stop the HW counter (sampling teardown, or plain counter disable). Keeps
    /// the counter allocated so a post-disable `read` returns the final value.
    fn hw_disable_local(&mut self) {
        if self.sampling.is_some() {
            self.teardown_sampling_irq();
        } else {
            match self.counter {
                Counter::Cycle => ax_cpu::pmu::cycles::disable(),
                Counter::Programmable(n) => ax_cpu::pmu::counter::disable(n),
            }
        }
    }

    /// Reset the HW counter to 0.
    fn hw_reset_local(&mut self) {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::reset(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::reset(n),
        }
    }

    /// Tear down + free the HW counter (sampling slot unregistered first so the
    /// overflow handler can no longer reach the ring/notify), releasing it to
    /// this core's per-CPU pool.
    fn hw_free_local(&mut self) {
        self.teardown_sampling_irq();
        match self.counter {
            Counter::Cycle => {
                ax_cpu::pmu::cycles::disable();
                super::percpu::free_cycle_counter();
            }
            Counter::Programmable(n) => {
                ax_cpu::pmu::counter::disable(n);
                super::percpu::free_programmable_counter(n);
            }
        }
    }

    /// Dispatch a [`HwOp`] to the matching `*_local` method on the current core.
    fn hw_op_local(&mut self, op: HwOp) -> u64 {
        match op {
            HwOp::Enable => {
                self.hw_enable_local();
                0
            }
            HwOp::Disable => {
                self.hw_disable_local();
                0
            }
            HwOp::Reset => {
                self.hw_reset_local();
                0
            }
            HwOp::Read => self.hw_read_local(),
            HwOp::Free => {
                self.hw_free_local();
                0
            }
        }
    }

    /// Read the HW counter value.
    fn hw_read_local(&self) -> u64 {
        self.raw_value()
    }

    /// Run a [`HwOp`] on this event's `home_cpu`: directly if the caller is on it
    /// (or it is unset), else over a synchronous IPI. Pins the self/system-wide
    /// counter's lifecycle to the core its slot lives on, so a migrated
    /// monitoring thread never touches another core's banked counter / pool.
    fn run_hw_on_home(&mut self, op: HwOp) -> u64 {
        if self.home_cpu == usize::MAX || self.home_cpu == ax_hal::percpu::this_cpu_id() {
            return self.hw_op_local(op);
        }
        let home = self.home_cpu;
        let mut ho = HwHomeOp {
            ev: self as *mut HwPerfEvent,
            op,
            value: 0,
        };
        let arg = &mut ho as *mut HwHomeOp as *mut ();
        if ax_ipi::wait_until_cpu_ready(home) {
            // SAFETY: `self` outlives the synchronous IPI (we block until the
            // thunk returns), so the remote `&mut` does not alias our paused one.
            let _ = unsafe { ax_ipi::run_on_cpu_sync_raw(home, hw_home_thunk, arg) };
            ho.value
        } else {
            // Home core not ready (should not happen for an online core);
            // best-effort local rather than skipping the op.
            self.hw_op_local(op)
        }
    }
}

/// A HW-counter lifecycle operation routed to [`HwPerfEvent::home_cpu`].
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
enum HwOp {
    Enable,
    Disable,
    Reset,
    Read,
    Free,
}

/// IPI argument for [`hw_home_thunk`]: the event + the op, with the read value
/// written back for the (blocked) caller.
#[cfg(target_arch = "aarch64")]
struct HwHomeOp {
    ev: *mut HwPerfEvent,
    op: HwOp,
    value: u64,
}

/// IPI thunk running a [`HwOp`] on the event's `home_cpu`.
///
/// # Safety
/// `arg` must point at a live [`HwHomeOp`] whose `ev` is a valid `HwPerfEvent`
/// kept alive for the call — guaranteed because the caller blocks on
/// `run_on_cpu_sync_raw` until this returns.
#[cfg(target_arch = "aarch64")]
unsafe fn hw_home_thunk(arg: *mut ()) {
    let ho = unsafe { &mut *(arg as *mut HwHomeOp) };
    super::percpu::ensure_core_inited();
    let ev = unsafe { &mut *ho.ev };
    ho.value = ev.hw_op_local(ho.op);
}

/// IPI opcode: alloc + configure a programmable counter (leaves it DISABLED).
#[cfg(target_arch = "aarch64")]
const SYS_OP_PROGRAM: u8 = 0;
/// IPI opcode: read a programmable counter.
#[cfg(target_arch = "aarch64")]
const SYS_OP_READ: u8 = 1;
/// IPI opcode: disable + free a programmable counter.
#[cfg(target_arch = "aarch64")]
const SYS_OP_FREE: u8 = 2;
/// IPI opcode: start (enable) an already-configured counter.
#[cfg(target_arch = "aarch64")]
const SYS_OP_START: u8 = 3;
/// IPI opcode: stop (disable) a counter, keeping it allocated + its value.
#[cfg(target_arch = "aarch64")]
const SYS_OP_STOP: u8 = 4;
/// IPI opcode: reset a counter to 0 (re-configure), keeping it allocated.
#[cfg(target_arch = "aarch64")]
const SYS_OP_RESET: u8 = 5;

/// Argument marshalled to [`sys_cpu_op_thunk`] when it runs on the target core
/// (in-place: outputs `slot`/`value`/`ok` are written back for the caller, which
/// blocks on the synchronous IPI until the thunk returns).
#[cfg(target_arch = "aarch64")]
struct SysCpuOp {
    op: u8,
    event: u16,
    exclude_user: bool,
    exclude_kernel: bool,
    slot: usize,
    value: u64,
    ok: bool,
}

/// Perform a [`SysCpuOp`] on the current core (the target core, reached locally
/// or via the IPI in [`run_sys_cpu_op`]).
///
/// # Safety
/// `arg` must point at a live [`SysCpuOp`] for the duration of the call (the
/// caller keeps it alive across the synchronous IPI).
#[cfg(target_arch = "aarch64")]
unsafe fn sys_cpu_op_thunk(arg: *mut ()) {
    let op = unsafe { &mut *(arg as *mut SysCpuOp) };
    super::percpu::ensure_core_inited();
    match op.op {
        // Allocate + configure (resets to 0); leaves the counter DISABLED so a
        // later STOP can pause it without freeing (perf reads the final value
        // after DISABLE). A separate START enables it.
        SYS_OP_PROGRAM => match super::percpu::alloc_programmable_counter() {
            Some(n) => {
                ax_cpu::pmu::counter::configure(n, op.event, op.exclude_user, op.exclude_kernel);
                op.slot = n;
                op.ok = true;
            }
            None => op.ok = false,
        },
        SYS_OP_START => {
            ax_cpu::pmu::counter::enable(op.slot);
            op.ok = true;
        }
        SYS_OP_STOP => {
            // Stop counting but KEEP the slot + value: `read(perf_fd)` after
            // DISABLE must still return the final count (Linux semantics).
            ax_cpu::pmu::counter::disable(op.slot);
            op.ok = true;
        }
        SYS_OP_RESET => {
            // Re-configure resets the counter to 0 (Linux `PERF_EVENT_IOC_RESET`).
            ax_cpu::pmu::counter::configure(op.slot, op.event, op.exclude_user, op.exclude_kernel);
            op.ok = true;
        }
        SYS_OP_READ => {
            op.value = ax_cpu::pmu::counter::read(op.slot);
            op.ok = true;
        }
        SYS_OP_FREE => {
            ax_cpu::pmu::counter::disable(op.slot);
            super::percpu::free_programmable_counter(op.slot);
            op.ok = true;
        }
        _ => {}
    }
}

/// Run a [`SysCpuOp`] on `cpu`: directly if it is the current core, else via a
/// synchronous IPI. Leaves `op.ok == false` if the target core is not ready.
#[cfg(target_arch = "aarch64")]
fn run_sys_cpu_op(cpu: usize, op: &mut SysCpuOp) {
    let arg = op as *mut SysCpuOp as *mut ();
    if cpu == ax_hal::percpu::this_cpu_id() {
        // SAFETY: `op` is live for this call.
        unsafe { sys_cpu_op_thunk(arg) };
    } else if ax_ipi::wait_until_cpu_ready(cpu) {
        // SAFETY: `op` outlives the synchronous IPI (we block until it returns),
        // and the thunk only touches `cpu`'s per-CPU PMU state.
        let _ = unsafe { ax_ipi::run_on_cpu_sync_raw(cpu, sys_cpu_op_thunk, arg) };
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
        // Cpu-bound system event: free its counter on the target core (IPI if the
        // closing core differs), then stop.
        if self.sys_cpu.is_some() {
            self.sys_free();
            return;
        }
        // Self/system-wide event: tear down + free the HW counter ON its
        // `home_cpu` (IPI if the closing thread migrated). For a sampling event
        // this unregisters the per-CPU `REGISTRY` slot on `home_cpu` BEFORE the
        // `Arc<IrqNotify>`/`Arc<GlobalPage>` drop below, so the overflow handler on
        // that core can never dereference a freed `notify` or write freed ring
        // pages, and the slot is returned to the correct core's pool.
        self.run_hw_on_home(HwOp::Free);
        // Stop the deferred worker (mirrors `BpfPerfEventWrapper::drop`). The
        // `Arc`s in `sampling` drop after this returns — safe, the slot was just
        // unregistered on `home_cpu`.
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
    fn enable(&mut self) -> AxResult<()> {
        // Per-task: just record userspace intent. The target task's next
        // `perf_sched_in` programs the counter onto HW (or an immediate one if
        // it is the running task at the next switch).
        if let Some(ptc) = &self.per_task {
            ptc.set_enabled();
            return Ok(());
        }
        // Cpu-bound system event (`perf stat -a`): allocate + configure the
        // counter on its target core (first enable), then start it. The counter
        // stays allocated across DISABLE so `read(perf_fd)` returns the final
        // count; it is freed only at `Drop`.
        if self.sys_cpu.is_some() {
            self.sys_program()?;
            self.sys_start();
            if self.enabled_since.is_none() {
                self.enabled_since = Some(ax_runtime::hal::time::monotonic_time_nanos());
            }
            return Ok(());
        }
        if self.enabled_since.is_none() {
            self.enabled_since = Some(ax_runtime::hal::time::monotonic_time_nanos());
        }
        // Self/system-wide event: arm the counter (sampling overflow path or plain
        // enable) ON its `home_cpu`, where its slot + (for sampling) `REGISTRY`
        // entry live — via IPI if this monitoring thread has migrated off it.
        self.run_hw_on_home(HwOp::Enable);
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        // Per-task: clear userspace intent. The next `perf_sched_out` folds the
        // live slice and stops the HW counter; future slices skip it.
        if let Some(ptc) = &self.per_task {
            ptc.set_disabled();
            return Ok(());
        }
        // Cpu-bound system event: STOP (not free) its counter on the target core
        // — the value must survive for a post-disable `read(perf_fd)` — then
        // accrue the enabled window. The counter is freed only at `Drop`.
        if self.sys_cpu.is_some() {
            self.sys_stop();
            if let Some(since) = self.enabled_since.take() {
                let now = ax_runtime::hal::time::monotonic_time_nanos();
                let elapsed = now.saturating_sub(since);
                self.time_enabled += elapsed;
                self.time_running += elapsed;
            }
            return Ok(());
        }
        // Self/system-wide event: stop the counter (sampling strict teardown, or
        // plain disable) ON its `home_cpu`, then accrue the enabled window. The
        // counter stays allocated (freed only at `Drop`) so a post-disable
        // `read(perf_fd)` returns the final value.
        self.run_hw_on_home(HwOp::Disable);
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
        // Cpu-bound system event: if armed, reset the counter to 0 on its target
        // core (re-configure); before enable there is nothing to reset, so
        // `perf stat`'s RESET-before-ENABLE is a no-op (the counter is configured
        // — and thus zeroed — at the first enable).
        if self.sys_cpu.is_some() {
            self.sys_reset();
            return Ok(());
        }
        // Self/system-wide event: reset the counter to 0 ON its `home_cpu`.
        self.run_hw_on_home(HwOp::Reset);
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
                read_format: ptc.read_format(),
                lost: super::task::read_lost(ptc),
            });
        }
        // Cpu-bound system event: read the counter from its target core (IPI if
        // the reader is elsewhere); timing is the enabled window (runs
        // continuously while enabled, so time_running == time_enabled).
        if self.sys_cpu.is_some() {
            let value = self.sys_read();
            let (mut time_enabled, mut time_running) = (self.time_enabled, self.time_running);
            if let Some(since) = self.enabled_since {
                let now = ax_runtime::hal::time::monotonic_time_nanos();
                let elapsed = now.saturating_sub(since);
                time_enabled += elapsed;
                time_running += elapsed;
            }
            return Ok(PerfReadValues {
                value,
                time_enabled,
                time_running,
                read_format: self.read_format,
                lost: self.sampling_lost(),
            });
        }
        // Self/system-wide event: read the counter from its `home_cpu` (IPI if the
        // reader migrated off it — `PMEVCNTRn` is per-PE banked). Current timing =
        // accumulated past windows + the live window, if any.
        let value = self.run_hw_on_home(HwOp::Read);
        let (mut time_enabled, mut time_running) = (self.time_enabled, self.time_running);
        if let Some(since) = self.enabled_since {
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            let elapsed = now.saturating_sub(since);
            time_enabled += elapsed;
            time_running += elapsed;
        }
        Ok(PerfReadValues {
            value,
            time_enabled,
            time_running,
            read_format: self.read_format,
            lost: self.sampling_lost(),
        })
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
    ) -> AxResult<()> {
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

    fn device_mmap(&mut self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        // Per-task sampling: the ring + notify/poll machinery live on the shared
        // `PerTaskCounter` (the scheduler hook builds the IRQ slot from there).
        // Allocate the ring, spawn the notify worker, and hand both to the ptc.
        if let Some(ptc) = &self.per_task {
            return device_mmap_per_task(ptc, len);
        }

        // Cpu-bound system event: no `rdpmc` page — its counter lives on another
        // core, so a userspace `mrs` on the mapping core would read the wrong PE's
        // counter. (`perf stat -a` does not mmap; only self-monitoring does.)
        if self.sys_cpu.is_some() {
            return Err(AxError::Unsupported);
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
            return Err(AxError::ResourceBusy);
        }

        // Allocate + zero + header-init the ring (shared with the per-task path).
        let (pages, ring_vaddr, paddr) = alloc_sampling_ring(len)?;

        // Hand the sole strong ref to the caller (threaded into the VMA via
        // `DeviceMmap::PhysicalCached`'s retainer); keep only a `Weak`. See `bpf.rs`
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
) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
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
pub fn perf_event_open_hw(attr: &perf_event_attr, pid: i32, cpu: i32) -> AxResult<HwPerfEvent> {
    // No PMUv3 → no hardware events.
    if ax_hal::pmu::info().is_none() {
        return Err(AxError::Unsupported);
    }

    // Per-CPU one-time clean-slate bring-up on the opening core (replaces the
    // bare per-open `init_cpu()`; the clears inside run exactly once per core,
    // so re-opens never disturb live counters of other events). The per-CPU
    // allocator caches this core's `PMCR.N` here, so no global counter count.
    super::percpu::ensure_core_inited();

    // `pid > 0`: attach a per-task counter to that task. `pid <= 0` (0 = self,
    // -1 = system-wide) keeps the existing M1/M2 behaviour untouched below.
    if pid > 0 {
        return perf_event_open_hw_per_task(attr, pid);
    }

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

    // `perf stat -a` per-CPU fan-out: a system-wide COUNTING event pinned to a
    // specific cpu (`cpu >= 0`) counts on THAT core via its per-CPU pool,
    // programmed / read / freed over a synchronous IPI. Sampling `-a`
    // (`perf record -a`) is not fanned out here — it stays on the current core.
    if cpu >= 0 && !is_sampling {
        // big.LITTLE: an event opened against a cluster's PMU but pinned to a CPU
        // of another cluster cannot run there — reject with ENOENT so `perf`
        // falls back / iterates PMUs (Linux `armpmu_event_init` cpumask gate).
        let valid_clusters =
            cluster_mask_for_type(attr.type_).unwrap_or(super::percpu::ClusterMask::ALL);
        if !valid_clusters.contains(super::percpu::cluster_of_cpu(cpu as usize)) {
            return Err(AxError::NotFound);
        }
        let Some(event) = decode_arm_event(attr.type_, attr.config) else {
            warn!(
                "perf_event_open: unsupported -a hardware type {:#x} config {:#x}",
                attr.type_, attr.config
            );
            return Err(AxError::Unsupported);
        };
        // Validated on the opening core; the target cluster's PMCEID would refine
        // this, but the common event set is architectural (all clusters).
        if !ax_cpu::pmu::event_supported(event) {
            warn!("perf_event_open: -a ARM event {event:#x} not implemented on this CPU");
            return Err(AxError::Unsupported);
        }
        return Ok(HwPerfEvent {
            counter: Counter::Programmable(usize::MAX),
            sample_id: 0,
            read_format: attr.read_format,
            enabled_since: None,
            time_enabled: 0,
            time_running: 0,
            sampling: None,
            per_task: None,
            sys_cpu: Some(SysCpuBinding {
                cpu: cpu as usize,
                event,
                exclude_user,
                exclude_kernel,
                slot: None,
            }),
            // Routed via `sys_cpu`'s own IPI ops, not `home_cpu`.
            home_cpu: usize::MAX,
        });
    }

    // System-wide / self counting+sampling on the opening core: an event opened
    // against a cluster PMU whose cluster differs from the opening core cannot run
    // here (it is pinned to this core) — ENOENT.
    {
        let valid_clusters =
            cluster_mask_for_type(attr.type_).unwrap_or(super::percpu::ClusterMask::ALL);
        if !valid_clusters.contains(super::percpu::current_cluster()) {
            return Err(AxError::NotFound);
        }
    }

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

    // Select the ARM event and counter. Sampling events ALWAYS take a
    // programmable counter — even CPU_CYCLES maps to ARM event 0x11 — because
    // the dedicated cycle counter is not used by the M2 overflow path.
    let counter = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        if attr.config == perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u64 && !is_sampling {
            // Counting CPU_CYCLES: the dedicated 64-bit cycle counter, from this
            // core's per-CPU pool.
            if !super::percpu::alloc_cycle_counter() {
                return Err(AxError::NoMemory);
            }
            // `exclude_*` map onto the cycle filter; `configure` also resets.
            ax_cpu::pmu::cycles::configure(exclude_user, exclude_kernel);
            Counter::Cycle
        } else {
            // Map the generic hardware event to an ARM PMUv3 event number
            // (per-cluster BRANCH_INSTRUCTIONS resolution; CPU_CYCLES → 0x11 for
            // the sampling case).
            let Some(event) = resolve_hw_event(attr.config as u32) else {
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
        || attr.type_ == ARMV8_CORTEX_A55_TYPE
        || attr.type_ == ARMV8_CORTEX_A76_TYPE
    {
        // Raw events (`PERF_TYPE_RAW`), the generic PMU (`armv8_pmuv3_0` = 8) and
        // the cluster PMUs (`armv8_cortex_a55`/`_a76` = 9/10) are decoded
        // identically: the low 16 bits of `config` are the ARM event number. The
        // real `perf` tool resolves a named event like `armv8_pmuv3_0/cpu_cycles/`
        // to (type, config = 0x11) via sysfs, so it lands here. (The cluster
        // restriction was already enforced as ENOENT above.)
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
            period: sample_period,
            freq: is_freq,
            target_freq,
            sample_type: attr.sample_type,
            poll_ready,
            notify,
            poll_alive,
            ring: None,
            redirect: None,
            lost: Arc::new(AtomicU64::new(0)),
        })
    } else {
        None
    };

    Ok(HwPerfEvent {
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
        // Counts on the opening core (`cpu < 0`); the cpu-bound `-a` fan-out
        // returned earlier.
        sys_cpu: None,
        // The counter is allocated on THIS (the opening) core; its HW lifecycle
        // is pinned here via IPI even if the monitoring thread migrates.
        home_cpu: ax_hal::percpu::this_cpu_id(),
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
fn perf_event_open_hw_per_task(attr: &perf_event_attr, pid: i32) -> AxResult<HwPerfEvent> {
    use crate::task::AsThread;

    // Resolve the target task and its `Thread` (kernel tasks have none).
    let task = crate::task::get_task(pid as u32)?;
    let thr = task.try_as_thread().ok_or(AxError::NoSuchProcess)?;

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

    // Decode the ARM event (per-cluster BRANCH_INSTRUCTIONS resolution; cluster
    // PMU types 9/10 accepted). Per-task always uses a programmable counter, so
    // even CPU_CYCLES maps to ARM event 0x11 (never the dedicated cycle counter).
    let Some(event) = decode_arm_event(attr.type_, attr.config) else {
        warn!(
            "perf_event_open: unsupported per-task hardware type {:#x} config {:#x}",
            attr.type_, attr.config
        );
        return Err(AxError::Unsupported);
    };
    // The cluster the event is restricted to (big.LITTLE). A per-task event is
    // NOT rejected for a cluster mismatch — it follows the task and simply skips
    // arming on non-matching cores (`perf_sched_in`), matching Linux's filter.
    let valid_clusters =
        cluster_mask_for_type(attr.type_).unwrap_or(super::percpu::ClusterMask::ALL);

    if !ax_cpu::pmu::event_supported(event) {
        warn!(
            "perf_event_open: per-task ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(AxError::Unsupported);
    }

    // No counter is reserved at open: the per-task path allocates a programmable
    // slot from the *running* core's per-CPU pool at each `perf_sched_in` and
    // releases it at `perf_sched_out`, so concurrent demand is bounded by the
    // events of the running task per core, not by live events system-wide.

    // `disabled = 0` ⇒ count from the next sched-in; `disabled = 1` ⇒ wait for
    // `enable_on_exec` / `ioctl(ENABLE)`. `perf stat -- cmd` sets both
    // `disabled` and `enable_on_exec`, so it starts counting at the child's exec.
    let enabled = attr.disabled() == 0;
    let enable_on_exec = attr.enable_on_exec() != 0;

    let ptc = Arc::new(super::task::PerTaskCounter::new(
        super::task::PerTaskConfig {
            event,
            exclude_user,
            exclude_kernel,
            read_format: attr.read_format,
            enabled,
            enable_on_exec,
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
            valid_clusters,
        },
    ));
    super::task::attach(thr, ptc.clone());

    Ok(HwPerfEvent {
        // Inert placeholders: the per-task path drives `ptc` (which holds the
        // per-slice slot), not these fields. `usize::MAX` is never used as a real
        // counter index — the per-task `Drop` delegates to `free_hw` and returns.
        counter: Counter::Programmable(usize::MAX),
        // Mirrors the wrapper id onto the ptc via `set_sample_id`; 0 until then.
        sample_id: 0,
        read_format: attr.read_format,
        enabled_since: None,
        time_enabled: 0,
        time_running: 0,
        sampling: None,
        per_task: Some(ptc),
        sys_cpu: None,
        // Per-task HW lives in the scheduler hooks (per slice), not `home_cpu`.
        home_cpu: usize::MAX,
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
    let Some(n) = super::percpu::alloc_programmable_counter() else {
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
pub fn perf_event_open_hw(_attr: &perf_event_attr, _pid: i32, _cpu: i32) -> AxResult<HwPerfEvent> {
    Err(AxError::Unsupported)
}

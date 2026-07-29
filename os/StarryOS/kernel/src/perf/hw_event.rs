//! Hardware-PMU event implementation (ARM PMUv3): counting (M1, `perf stat`) and
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
use core::sync::atomic::Ordering;

#[cfg(target_arch = "aarch64")]
use ax_alloc::GlobalPage;
use ax_errno::{AxError, AxResult};
#[cfg(target_arch = "aarch64")]
use ax_hal::mem::virt_to_phys;
#[cfg(target_arch = "aarch64")]
use ax_memory_addr::PhysAddr;
#[cfg(target_arch = "aarch64")]
use ax_sync::PiMutex;
use axpoll::{IoEvents, Pollable};
#[cfg(not(target_arch = "aarch64"))]
use kbpf_basic::linux_bpf::perf_event_attr;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use super::PerfEventOps;
#[cfg(target_arch = "aarch64")]
use super::PerfReadValues;
#[cfg(target_arch = "aarch64")]
use super::control::PerfControl;
#[cfg(target_arch = "aarch64")]
use super::target::PerfCpuId;
#[cfg(not(target_arch = "aarch64"))]
use super::{access::AuthorizedPerfTarget, hw::ValidatedHwOpen};
#[cfg(target_arch = "aarch64")]
use super::{
    cpu_worker,
    hw_allocation::free_counter,
    hw_owner::{Counter, SystemPmuDisable, SystemPmuEnable, SystemPmuRead, SystemPmuReset},
    hw_sampling::{SamplingState, alloc_sampling_ring, device_mmap_per_task, ring_has_data},
    inheritance::PerfInheritanceFamily,
    output::{PerfOutputScope, PerfRingOutput},
    sampling::{SampleOutput, SampleSlot, SampleSlotConfig},
    sampling_lifecycle::SampleRegistration,
};

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
pub(super) struct SystemEventInit {
    pub(super) counter: Counter,
    pub(super) owner: PerfCpuId,
    pub(super) read_format: u64,
    pub(super) sampling: Option<SamplingState>,
    pub(super) enable_at_open: bool,
}

#[cfg(target_arch = "aarch64")]
pub(super) struct TaskEventInit {
    pub(super) counter: Counter,
    pub(super) scheduler_id: u64,
    pub(super) read_format: u64,
    pub(super) family: Arc<PerfInheritanceFamily>,
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
        let (index, pmc_width) = self.counter.mmap_metadata();

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
        free_counter(self.counter);
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

    pub(super) fn new_system(init: SystemEventInit) -> Self {
        Self::new(
            HwPerfEventState {
                counter: init.counter,
                system_owner: Some(init.owner),
                output_scope: PerfOutputScope::Cpu(init.owner.as_usize()),
                sample_id: 0,
                read_format: init.read_format,
                enabled_since: None,
                time_enabled: 0,
                time_running: 0,
                sampling: init.sampling,
                sampling_registration: None,
                per_task: None,
            },
            init.enable_at_open,
        )
    }

    pub(super) fn new_task(init: TaskEventInit) -> Self {
        Self::new(
            HwPerfEventState {
                counter: init.counter,
                system_owner: None,
                output_scope: PerfOutputScope::Task(init.scheduler_id),
                sample_id: 0,
                read_format: init.read_format,
                enabled_since: None,
                time_enabled: 0,
                time_running: 0,
                sampling: None,
                sampling_registration: None,
                per_task: Some(init.family),
            },
            false,
        )
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
pub(super) fn perf_event_open_hw(
    _attr: &perf_event_attr,
    target: AuthorizedPerfTarget,
    validated: ValidatedHwOpen,
) -> AxResult<HwPerfEvent> {
    let _ = validated;
    match target {
        AuthorizedPerfTarget::Task { task, cpu } => {
            let _ = (task, cpu);
        }
        AuthorizedPerfTarget::Cpu(cpu) => {
            let _ = cpu;
        }
    }
    Err(AxError::Unsupported)
}

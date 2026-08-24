//! Allocation-free phase tracing for target-vCPU wakeups.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::vec::Vec;

use crate::host::{HostCpu, HostTime, default_host};

#[cfg(feature = "rt-trace-soak")]
const WAKE_TRACE_CAPACITY: usize = 262_144;
#[cfg(not(feature = "rt-trace-soak"))]
const WAKE_TRACE_CAPACITY: usize = 65_536;
const MAX_VCPUS: usize = 64;
const NO_VM: usize = usize::MAX;
const SOURCE_BITS: u32 = 2;
const SOURCE_MASK: u64 = (1 << SOURCE_BITS) - 1;

/// Origin of one traced target-vCPU wakeup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum VcpuWakeSource {
    /// A virtual interrupt controller published work to its deferred worker.
    DeferredIrq   = 1,
    /// A host timer callback reached an architectural guest deadline.
    TimerDeadline = 2,
}

impl VcpuWakeSource {
    /// Stable text used by the persisted trace schema.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeferredIrq => "deferred_irq",
            Self::TimerDeadline => "timer_deadline",
        }
    }
}

/// Observable stage in a target-vCPU wakeup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum VcpuWakePhase {
    /// Pending work became visible to the wake producer.
    Publish        = 0,
    /// The pre-created deferred IRQ worker began dispatching the wake.
    DeferredWorker = 1,
    /// Runtime notification correlation was published immediately before the
    /// target event generation and wait queue are notified.
    RuntimeNotify  = 2,
    /// The target pCPU IPI request was issued.
    IpiSent        = 3,
    /// The target vCPU task reached its next guest-run iteration.
    VcpuRun        = 4,
}

impl VcpuWakePhase {
    /// Stable text used by the persisted trace schema.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::DeferredWorker => "deferred_worker",
            Self::RuntimeNotify => "runtime_notify",
            Self::IpiSent => "ipi_sent",
            Self::VcpuRun => "vcpu_run",
        }
    }

    const fn mask(self) -> u8 {
        1 << self as u8
    }
}

/// One allocation-free host-counter observation in a vCPU wake pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VcpuWakePhaseRecord {
    /// Global reservation order within the wake-event buffer.
    pub sequence: u64,
    /// Capture-local wake identifier shared by all phases of one pipeline.
    pub wake_id: u64,
    /// AxVM VM identifier.
    pub vm_id: usize,
    /// AxVM vCPU identifier.
    pub vcpu_id: usize,
    /// Host pCPU that recorded this phase.
    pub pcpu_id: usize,
    /// Wake origin.
    pub source: VcpuWakeSource,
    /// Pipeline phase.
    pub phase: VcpuWakePhase,
    /// Raw host architectural counter at this phase.
    pub counter_ticks: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VcpuWakeTraceToken(u64);

impl VcpuWakeTraceToken {
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    const fn wake_id(self) -> u64 {
        self.0 >> SOURCE_BITS
    }

    const fn source(self) -> VcpuWakeSource {
        match self.0 & SOURCE_MASK {
            value if value == VcpuWakeSource::TimerDeadline as u64 => VcpuWakeSource::TimerDeadline,
            _ => VcpuWakeSource::DeferredIrq,
        }
    }
}

struct WakeTraceSlot {
    committed: AtomicUsize,
    record: UnsafeCell<MaybeUninit<VcpuWakePhaseRecord>>,
}

impl WakeTraceSlot {
    const fn new() -> Self {
        Self {
            committed: AtomicUsize::new(0),
            record: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: each buffer index has one writer, which publishes the initialized
// record with Release ordering before a quiesced snapshot acquires it.
unsafe impl Sync for WakeTraceSlot {}

struct WakeTraceBuffer<const N: usize> {
    enabled: AtomicBool,
    next: AtomicUsize,
    active_writers: AtomicUsize,
    dropped: AtomicUsize,
    slots: [WakeTraceSlot; N],
}

impl<const N: usize> WakeTraceBuffer<N> {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            active_writers: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            slots: [const { WakeTraceSlot::new() }; N],
        }
    }

    fn reset_and_start(&self) {
        self.stop();
        let previous_len = self.next.load(Ordering::Relaxed).min(N);
        for slot in &self.slots[..previous_len] {
            slot.committed.store(0, Ordering::Relaxed);
        }
        self.next.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
    }

    fn stop(&self) {
        self.enabled.store(false, Ordering::Release);
        while self.active_writers.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
    }

    fn record(&self, mut record: VcpuWakePhaseRecord) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        self.active_writers.fetch_add(1, Ordering::AcqRel);
        if !self.enabled.load(Ordering::Acquire) {
            self.active_writers.fetch_sub(1, Ordering::Release);
            return;
        }

        let index = self.next.fetch_add(1, Ordering::Relaxed);
        if index >= N {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            self.active_writers.fetch_sub(1, Ordering::Release);
            return;
        }
        record.sequence = index as u64;
        let slot = &self.slots[index];
        // SAFETY: `fetch_add` gives this writer exclusive ownership until the
        // matching commit token is published.
        unsafe { (*slot.record.get()).write(record) };
        slot.committed.store(index + 1, Ordering::Release);
        self.active_writers.fetch_sub(1, Ordering::Release);
    }

    fn record_at(&self, index: usize) -> Option<VcpuWakePhaseRecord> {
        let slot = self.slots.get(index)?;
        if slot.committed.load(Ordering::Acquire) != index + 1 {
            return None;
        }
        // SAFETY: capture stop drained every writer and this acquire observed
        // the matching commit token.
        Some(unsafe { *(*slot.record.get()).assume_init_ref() })
    }
}

pub(crate) struct WakeTraceSnapshot {
    pub(crate) dropped: usize,
    pub(crate) incomplete: usize,
    pub(crate) events: Vec<VcpuWakePhaseRecord>,
}

static WAKE_EVENTS: WakeTraceBuffer<WAKE_TRACE_CAPACITY> = WakeTraceBuffer::new();
static ACTIVE_VM: AtomicUsize = AtomicUsize::new(NO_VM);
static VCPU_COUNT: AtomicUsize = AtomicUsize::new(0);
static NEXT_WAKE_ID: AtomicU64 = AtomicU64::new(0);
static PENDING_VCPU_RUN: [AtomicU64; MAX_VCPUS] = [const { AtomicU64::new(0) }; MAX_VCPUS];

pub(crate) fn begin_vm(vm_id: usize, vcpu_count: usize) {
    ACTIVE_VM.store(vm_id, Ordering::Release);
    VCPU_COUNT.store(vcpu_count.min(MAX_VCPUS), Ordering::Release);
    NEXT_WAKE_ID.store(0, Ordering::Relaxed);
    for pending in &PENDING_VCPU_RUN {
        pending.store(0, Ordering::Relaxed);
    }
    WAKE_EVENTS.reset_and_start();
}

pub(crate) fn abort_vm(vm_id: usize) {
    if ACTIVE_VM
        .compare_exchange(vm_id, NO_VM, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        WAKE_EVENTS.stop();
    }
}

pub(crate) fn end_vm(vm_id: usize) {
    if ACTIVE_VM.load(Ordering::Acquire) != vm_id {
        return;
    }
    WAKE_EVENTS.stop();
    ACTIVE_VM.store(NO_VM, Ordering::Release);
}

pub(crate) fn begin_vcpu_wake(
    vm_id: usize,
    vcpu_id: usize,
    source: VcpuWakeSource,
) -> Option<VcpuWakeTraceToken> {
    if !capture_matches(vm_id, vcpu_id) {
        return None;
    }
    let wake_id = NEXT_WAKE_ID.fetch_add(1, Ordering::Relaxed) + 1;
    let token = VcpuWakeTraceToken((wake_id << SOURCE_BITS) | source as u64);
    record_phase(token, vm_id, vcpu_id, VcpuWakePhase::Publish);
    Some(token)
}

pub(crate) fn record_deferred_worker(token: VcpuWakeTraceToken, vm_id: usize, vcpu_id: usize) {
    record_phase(token, vm_id, vcpu_id, VcpuWakePhase::DeferredWorker);
}

pub(crate) fn record_runtime_notify(token: VcpuWakeTraceToken, vm_id: usize, vcpu_id: usize) {
    if !capture_matches(vm_id, vcpu_id) {
        return;
    }
    record_phase(token, vm_id, vcpu_id, VcpuWakePhase::RuntimeNotify);
    PENDING_VCPU_RUN[vcpu_id].store(token.raw(), Ordering::Release);
}

pub(crate) fn record_ipi_sent(token: VcpuWakeTraceToken, vm_id: usize, vcpu_id: usize) {
    record_phase(token, vm_id, vcpu_id, VcpuWakePhase::IpiSent);
}

pub(crate) fn record_vcpu_run(vm_id: usize, vcpu_id: usize) {
    if !capture_matches(vm_id, vcpu_id) {
        return;
    }
    let raw = PENDING_VCPU_RUN[vcpu_id].swap(0, Ordering::AcqRel);
    if let Some(token) = VcpuWakeTraceToken::from_raw(raw) {
        record_phase(token, vm_id, vcpu_id, VcpuWakePhase::VcpuRun);
    }
}

pub(crate) fn snapshot() -> WakeTraceSnapshot {
    let record_count = WAKE_EVENTS
        .next
        .load(Ordering::Acquire)
        .min(WAKE_TRACE_CAPACITY);
    let mut events = Vec::with_capacity(record_count);
    for index in 0..record_count {
        if let Some(record) = WAKE_EVENTS.record_at(index) {
            events.push(record);
        }
    }
    let missing_records = record_count.saturating_sub(events.len());
    WakeTraceSnapshot {
        dropped: WAKE_EVENTS.dropped.load(Ordering::Acquire),
        incomplete: missing_records + incomplete_pipeline_count(&events),
        events,
    }
}

fn capture_matches(vm_id: usize, vcpu_id: usize) -> bool {
    ACTIVE_VM.load(Ordering::Acquire) == vm_id && vcpu_id < VCPU_COUNT.load(Ordering::Acquire)
}

fn record_phase(token: VcpuWakeTraceToken, vm_id: usize, vcpu_id: usize, phase: VcpuWakePhase) {
    if !capture_matches(vm_id, vcpu_id) {
        return;
    }
    let host = default_host();
    WAKE_EVENTS.record(VcpuWakePhaseRecord {
        sequence: 0,
        wake_id: token.wake_id(),
        vm_id,
        vcpu_id,
        pcpu_id: host.this_cpu_id(),
        source: token.source(),
        phase,
        counter_ticks: host.current_ticks(),
    });
}

fn incomplete_pipeline_count(events: &[VcpuWakePhaseRecord]) -> usize {
    let wake_count = NEXT_WAKE_ID.load(Ordering::Acquire) as usize;
    let mut observed = std::vec![0_u8; wake_count.saturating_add(1)];
    let mut sources = std::vec![None; wake_count.saturating_add(1)];
    for event in events {
        let Some(mask) = observed.get_mut(event.wake_id as usize) else {
            continue;
        };
        *mask |= event.phase.mask();
        sources[event.wake_id as usize] = Some(event.source);
    }

    (1..=wake_count)
        .filter(|&wake_id| {
            let expected = match sources[wake_id] {
                Some(VcpuWakeSource::DeferredIrq) => {
                    VcpuWakePhase::Publish.mask()
                        | VcpuWakePhase::DeferredWorker.mask()
                        | VcpuWakePhase::RuntimeNotify.mask()
                        | VcpuWakePhase::IpiSent.mask()
                        | VcpuWakePhase::VcpuRun.mask()
                }
                Some(VcpuWakeSource::TimerDeadline) => {
                    VcpuWakePhase::Publish.mask()
                        | VcpuWakePhase::RuntimeNotify.mask()
                        | VcpuWakePhase::IpiSent.mask()
                        | VcpuWakePhase::VcpuRun.mask()
                }
                None => return true,
            };
            observed[wake_id] != expected
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(wake_id: u64, source: VcpuWakeSource, phase: VcpuWakePhase) -> VcpuWakePhaseRecord {
        VcpuWakePhaseRecord {
            sequence: 99,
            wake_id,
            vm_id: 1,
            vcpu_id: 0,
            pcpu_id: 2,
            source,
            phase,
            counter_ticks: 100,
        }
    }

    #[test]
    fn fixed_wake_buffer_assigns_sequence_and_reports_overflow() {
        let buffer = WakeTraceBuffer::<1>::new();
        buffer.reset_and_start();
        buffer.record(event(
            1,
            VcpuWakeSource::TimerDeadline,
            VcpuWakePhase::Publish,
        ));
        buffer.record(event(
            1,
            VcpuWakeSource::TimerDeadline,
            VcpuWakePhase::RuntimeNotify,
        ));
        buffer.stop();

        assert_eq!(buffer.record_at(0).unwrap().sequence, 0);
        assert_eq!(buffer.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn completeness_requires_every_source_specific_phase() {
        NEXT_WAKE_ID.store(2, Ordering::Relaxed);
        let complete_timer = [
            event(1, VcpuWakeSource::TimerDeadline, VcpuWakePhase::Publish),
            event(
                1,
                VcpuWakeSource::TimerDeadline,
                VcpuWakePhase::RuntimeNotify,
            ),
            event(1, VcpuWakeSource::TimerDeadline, VcpuWakePhase::IpiSent),
            event(1, VcpuWakeSource::TimerDeadline, VcpuWakePhase::VcpuRun),
            event(2, VcpuWakeSource::DeferredIrq, VcpuWakePhase::Publish),
        ];

        assert_eq!(incomplete_pipeline_count(&complete_timer), 1);
    }

    #[test]
    fn wake_token_round_trips_source_and_id() {
        let token = VcpuWakeTraceToken((17 << SOURCE_BITS) | VcpuWakeSource::TimerDeadline as u64);

        assert_eq!(token.wake_id(), 17);
        assert_eq!(token.source(), VcpuWakeSource::TimerDeadline);
        assert_eq!(VcpuWakeTraceToken::from_raw(token.raw()), Some(token));
        assert_eq!(VcpuWakeTraceToken::from_raw(0), None);
    }
}

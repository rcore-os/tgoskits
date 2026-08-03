//! AxVisor real-time trace and host CPU/vCPU accounting.
//!
//! Event recording is fixed-capacity, allocation-free, and lock-free. The
//! public snapshot allocates only after VM shutdown has quiesced the writers.

use alloc::vec::Vec;
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use crate::host::{HostCpu, HostTime, default_host};

const TRACE_CAPACITY: usize = 262_144;
const MAX_VCPUS: usize = 64;
const MAX_HOST_CPUS: usize = 128;
const NO_VM: usize = usize::MAX;
const NO_CPU: usize = usize::MAX;

/// One host virtual-timer forwarding observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualTimerInjectionRecord {
    /// Global reservation order within this capture.
    pub sequence: u64,
    /// AxVM VM identifier.
    pub vm_id: usize,
    /// AxVM vCPU identifier.
    pub vcpu_id: usize,
    /// Host pCPU that handled and injected the interrupt.
    pub pcpu_id: usize,
    /// Host architectural timer PPI.
    pub physical_irq: u32,
    /// Guest-visible architectural timer PPI.
    pub virtual_irq: u32,
    /// Host `CNTPCT_EL0` immediately before virtual interrupt injection.
    pub host_counter_ticks: u64,
    /// The same instant translated into the guest `CNTVCT_EL0` domain.
    pub guest_counter_ticks: u64,
    /// Ticks spent in the bounded AxVM forwarding/injection path.
    pub forwarding_ticks: u64,
    /// Whether a hardware list register accepted the interrupt.
    pub injected: bool,
}

struct TraceSlot {
    committed: AtomicUsize,
    record: UnsafeCell<MaybeUninit<VirtualTimerInjectionRecord>>,
}

impl TraceSlot {
    const fn new() -> Self {
        Self {
            committed: AtomicUsize::new(0),
            record: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: `TraceBuffer::record` assigns each slot to exactly one writer and
// publishes it with a Release store. Snapshot readers run only after `stop`
// drains active writers and acquire the matching commit token.
unsafe impl Sync for TraceSlot {}

struct TraceBuffer<const N: usize> {
    enabled: AtomicBool,
    next: AtomicUsize,
    active_writers: AtomicUsize,
    dropped: AtomicUsize,
    incomplete: AtomicUsize,
    slots: [TraceSlot; N],
}

impl<const N: usize> TraceBuffer<N> {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            active_writers: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            incomplete: AtomicUsize::new(0),
            slots: [const { TraceSlot::new() }; N],
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
        self.incomplete.store(0, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
    }

    fn stop(&self) {
        self.enabled.store(false, Ordering::Release);
        while self.active_writers.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
    }

    #[cfg(any(target_arch = "aarch64", test))]
    fn record(&self, mut record: VirtualTimerInjectionRecord) {
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
        // SAFETY: `fetch_add` gives this invocation exclusive ownership of the
        // slot until the commit token is published.
        unsafe { (*slot.record.get()).write(record) };
        slot.committed.store(index + 1, Ordering::Release);
        self.active_writers.fetch_sub(1, Ordering::Release);
    }

    fn record_at(&self, index: usize) -> Option<VirtualTimerInjectionRecord> {
        let slot = self.slots.get(index)?;
        if slot.committed.load(Ordering::Acquire) != index + 1 {
            return None;
        }
        // SAFETY: the matching commit token was acquired and capture stop
        // drained all writers before this method is used by snapshot export.
        Some(unsafe { *(*slot.record.get()).assume_init_ref() })
    }
}

struct VcpuCounters {
    run_count: AtomicU64,
    run_ticks: AtomicU64,
    max_run_ticks: AtomicU64,
    wait_count: AtomicU64,
    wait_ticks: AtomicU64,
    max_wait_ticks: AtomicU64,
    pcpu_mask: AtomicU64,
    migrations: AtomicU64,
    last_pcpu: AtomicUsize,
}

impl VcpuCounters {
    const fn new() -> Self {
        Self {
            run_count: AtomicU64::new(0),
            run_ticks: AtomicU64::new(0),
            max_run_ticks: AtomicU64::new(0),
            wait_count: AtomicU64::new(0),
            wait_ticks: AtomicU64::new(0),
            max_wait_ticks: AtomicU64::new(0),
            pcpu_mask: AtomicU64::new(0),
            migrations: AtomicU64::new(0),
            last_pcpu: AtomicUsize::new(NO_CPU),
        }
    }

    fn reset(&self) {
        self.run_count.store(0, Ordering::Relaxed);
        self.run_ticks.store(0, Ordering::Relaxed);
        self.max_run_ticks.store(0, Ordering::Relaxed);
        self.wait_count.store(0, Ordering::Relaxed);
        self.wait_ticks.store(0, Ordering::Relaxed);
        self.max_wait_ticks.store(0, Ordering::Relaxed);
        self.pcpu_mask.store(0, Ordering::Relaxed);
        self.migrations.store(0, Ordering::Relaxed);
        self.last_pcpu.store(NO_CPU, Ordering::Relaxed);
    }

    fn observe_pcpu(&self, pcpu_id: usize) {
        if let Some(bit) = 1_u64.checked_shl(pcpu_id as u32) {
            self.pcpu_mask.fetch_or(bit, Ordering::Relaxed);
        }
        let previous = self.last_pcpu.swap(pcpu_id, Ordering::Relaxed);
        if previous != NO_CPU && previous != pcpu_id {
            self.migrations.fetch_add(1, Ordering::Relaxed);
        }
    }
}

static TIMER_INJECTIONS: TraceBuffer<TRACE_CAPACITY> = TraceBuffer::new();
static ACTIVE_VM: AtomicUsize = AtomicUsize::new(NO_VM);
static COMPLETED_VM: AtomicUsize = AtomicUsize::new(NO_VM);
static VCPU_COUNT: AtomicUsize = AtomicUsize::new(0);
static HOST_CPU_COUNT: AtomicUsize = AtomicUsize::new(0);
static START_TICKS: AtomicU64 = AtomicU64::new(0);
static END_TICKS: AtomicU64 = AtomicU64::new(0);
static COUNTER_FREQUENCY_HZ: AtomicU64 = AtomicU64::new(0);
static COUNTER_FREQUENCY_MISMATCHES: AtomicU64 = AtomicU64::new(0);
static FAILED_INJECTIONS: AtomicU64 = AtomicU64::new(0);
static UNOWNED_VIRTUAL_TIMER_IRQS: AtomicU64 = AtomicU64::new(0);
static VCPU_COUNTERS: [VcpuCounters; MAX_VCPUS] = [const { VcpuCounters::new() }; MAX_VCPUS];
static PCPU_IDLE_START: [AtomicU64; MAX_HOST_CPUS] = [const { AtomicU64::new(0) }; MAX_HOST_CPUS];
static PCPU_IDLE_END: [AtomicU64; MAX_HOST_CPUS] = [const { AtomicU64::new(0) }; MAX_HOST_CPUS];

/// Starts a fresh RT capture for one VM.
///
/// RT trace builds intentionally capture one VM at a time. A concurrent second
/// VM remains functional but is not included in this evidence session.
pub(crate) fn begin_vm(vm_id: usize, vcpu_count: usize) {
    if ACTIVE_VM
        .compare_exchange(NO_VM, vm_id, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        warn!("RT trace already has an active VM; VM[{vm_id}] will not be captured");
        return;
    }

    let host = default_host();
    let start_ticks = host.current_ticks();
    let host_cpu_count = host.cpu_count().min(MAX_HOST_CPUS);
    let vcpu_count = vcpu_count.min(MAX_VCPUS);
    for counters in &VCPU_COUNTERS[..vcpu_count] {
        counters.reset();
    }
    for cpu_id in 0..host_cpu_count {
        let idle = host.idle_time_ticks(cpu_id, start_ticks).unwrap_or(0);
        PCPU_IDLE_START[cpu_id].store(idle, Ordering::Relaxed);
        PCPU_IDLE_END[cpu_id].store(idle, Ordering::Relaxed);
    }
    COMPLETED_VM.store(NO_VM, Ordering::Relaxed);
    VCPU_COUNT.store(vcpu_count, Ordering::Relaxed);
    HOST_CPU_COUNT.store(host_cpu_count, Ordering::Relaxed);
    START_TICKS.store(start_ticks, Ordering::Relaxed);
    END_TICKS.store(0, Ordering::Relaxed);
    COUNTER_FREQUENCY_HZ.store(0, Ordering::Relaxed);
    COUNTER_FREQUENCY_MISMATCHES.store(0, Ordering::Relaxed);
    FAILED_INJECTIONS.store(0, Ordering::Relaxed);
    UNOWNED_VIRTUAL_TIMER_IRQS.store(0, Ordering::Relaxed);
    TIMER_INJECTIONS.reset_and_start();
}

/// Aborts a capture when VM startup fails before vCPUs can run.
pub(crate) fn abort_vm(vm_id: usize) {
    if ACTIVE_VM
        .compare_exchange(vm_id, NO_VM, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        TIMER_INJECTIONS.stop();
        COMPLETED_VM.store(NO_VM, Ordering::Release);
    }
}

/// Stops the active capture after the final vCPU exits.
pub(crate) fn end_vm(vm_id: usize) {
    if ACTIVE_VM.load(Ordering::Acquire) != vm_id {
        return;
    }
    TIMER_INJECTIONS.stop();
    let host = default_host();
    let end_ticks = host.current_ticks();
    let host_cpu_count = HOST_CPU_COUNT.load(Ordering::Relaxed);
    for cpu_id in 0..host_cpu_count {
        let idle = host
            .idle_time_ticks(cpu_id, end_ticks)
            .unwrap_or_else(|| PCPU_IDLE_START[cpu_id].load(Ordering::Relaxed));
        PCPU_IDLE_END[cpu_id].store(idle, Ordering::Relaxed);
    }
    END_TICKS.store(end_ticks, Ordering::Release);
    COMPLETED_VM.store(vm_id, Ordering::Release);
    ACTIVE_VM.store(NO_VM, Ordering::Release);
}

pub(crate) fn current_ticks() -> u64 {
    default_host().current_ticks()
}

pub(crate) fn current_pcpu_id() -> usize {
    default_host().this_cpu_id()
}

fn capture_matches(vm_id: usize, vcpu_id: usize) -> bool {
    ACTIVE_VM.load(Ordering::Acquire) == vm_id && vcpu_id < VCPU_COUNT.load(Ordering::Relaxed)
}

/// Records one interval spent executing guest code until the next VM exit.
pub(crate) fn record_vcpu_run(
    vm_id: usize,
    vcpu_id: usize,
    pcpu_id: usize,
    started_ticks: u64,
    finished_ticks: u64,
) {
    if !capture_matches(vm_id, vcpu_id) {
        return;
    }
    let elapsed = finished_ticks.saturating_sub(started_ticks);
    let counters = &VCPU_COUNTERS[vcpu_id];
    counters.run_count.fetch_add(1, Ordering::Relaxed);
    counters.run_ticks.fetch_add(elapsed, Ordering::Relaxed);
    counters.max_run_ticks.fetch_max(elapsed, Ordering::Relaxed);
    counters.observe_pcpu(pcpu_id);
}

/// Records one interval for which a vCPU task was blocked waiting for work.
pub(crate) fn record_vcpu_wait(
    vm_id: usize,
    vcpu_id: usize,
    started_ticks: u64,
    finished_ticks: u64,
) {
    if !capture_matches(vm_id, vcpu_id) {
        return;
    }
    let elapsed = finished_ticks.saturating_sub(started_ticks);
    let counters = &VCPU_COUNTERS[vcpu_id];
    counters.wait_count.fetch_add(1, Ordering::Relaxed);
    counters.wait_ticks.fetch_add(elapsed, Ordering::Relaxed);
    counters
        .max_wait_ticks
        .fetch_max(elapsed, Ordering::Relaxed);
}

/// Records one direct host virtual-timer forwarding attempt.
#[cfg(target_arch = "aarch64")]
pub(crate) fn record_virtual_timer_injection(
    record: VirtualTimerInjectionRecord,
    counter_frequency_hz: u64,
) {
    if !capture_matches(record.vm_id, record.vcpu_id) {
        return;
    }
    let observed_frequency = COUNTER_FREQUENCY_HZ
        .compare_exchange(0, counter_frequency_hz, Ordering::AcqRel, Ordering::Acquire)
        .unwrap_or_else(|frequency| frequency);
    if observed_frequency != 0 && observed_frequency != counter_frequency_hz {
        COUNTER_FREQUENCY_MISMATCHES.fetch_add(1, Ordering::Relaxed);
    }
    if !record.injected {
        FAILED_INJECTIONS.fetch_add(1, Ordering::Relaxed);
    }
    TIMER_INJECTIONS.record(record);
}

/// Counts a guest virtual-timer PPI observed while no vCPU owns this pCPU.
#[cfg(target_arch = "aarch64")]
pub(crate) fn record_unowned_virtual_timer_irq() {
    if ACTIVE_VM.load(Ordering::Acquire) != NO_VM {
        UNOWNED_VIRTUAL_TIMER_IRQS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Per-host-CPU time split over the VM capture window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostPcpuAccounting {
    pub pcpu_id: usize,
    pub wall_ticks: u64,
    pub running_ticks: u64,
    pub idle_ticks: u64,
}

/// Per-vCPU run and blocked-wait accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostVcpuAccounting {
    pub vm_id: usize,
    pub vcpu_id: usize,
    pub run_count: u64,
    pub run_ticks: u64,
    pub max_run_ticks: u64,
    pub wait_count: u64,
    pub wait_ticks: u64,
    pub max_wait_ticks: u64,
    pub pcpu_mask: u64,
    pub migrations: u64,
}

/// Fully quiesced host RT evidence ready for file export.
#[derive(Debug)]
pub struct HostRtTraceSnapshot {
    pub vm_id: usize,
    pub counter_frequency_hz: u64,
    pub start_ticks: u64,
    pub end_ticks: u64,
    pub dropped: usize,
    pub incomplete: usize,
    pub failed_injections: u64,
    /// Guest virtual-timer PPIs observed without a current vCPU.
    pub unowned_virtual_timer_irqs: u64,
    /// Number of observations whose architectural counter frequency changed.
    pub counter_frequency_mismatches: u64,
    pub injections: Vec<VirtualTimerInjectionRecord>,
    pub pcpus: Vec<HostPcpuAccounting>,
    pub vcpus: Vec<HostVcpuAccounting>,
}

/// Returns the most recently completed VM capture.
pub fn snapshot() -> Option<HostRtTraceSnapshot> {
    let vm_id = COMPLETED_VM.load(Ordering::Acquire);
    if vm_id == NO_VM || ACTIVE_VM.load(Ordering::Acquire) != NO_VM {
        return None;
    }
    let start_ticks = START_TICKS.load(Ordering::Acquire);
    let end_ticks = END_TICKS.load(Ordering::Acquire);
    let record_count = TIMER_INJECTIONS
        .next
        .load(Ordering::Acquire)
        .min(TRACE_CAPACITY);
    let mut injections = Vec::with_capacity(record_count);
    for index in 0..record_count {
        if let Some(record) = TIMER_INJECTIONS.record_at(index) {
            injections.push(record);
        }
    }

    let wall_ticks = end_ticks.saturating_sub(start_ticks);
    let host_cpu_count = HOST_CPU_COUNT.load(Ordering::Acquire);
    let mut pcpus = Vec::with_capacity(host_cpu_count);
    for pcpu_id in 0..host_cpu_count {
        let idle_start = PCPU_IDLE_START[pcpu_id].load(Ordering::Acquire);
        let idle_end = PCPU_IDLE_END[pcpu_id].load(Ordering::Acquire);
        let idle_ticks = idle_end.saturating_sub(idle_start).min(wall_ticks);
        pcpus.push(HostPcpuAccounting {
            pcpu_id,
            wall_ticks,
            running_ticks: wall_ticks.saturating_sub(idle_ticks),
            idle_ticks,
        });
    }

    let vcpu_count = VCPU_COUNT.load(Ordering::Acquire);
    let mut vcpus = Vec::with_capacity(vcpu_count);
    for (vcpu_id, counters) in VCPU_COUNTERS[..vcpu_count].iter().enumerate() {
        vcpus.push(HostVcpuAccounting {
            vm_id,
            vcpu_id,
            run_count: counters.run_count.load(Ordering::Acquire),
            run_ticks: counters.run_ticks.load(Ordering::Acquire),
            max_run_ticks: counters.max_run_ticks.load(Ordering::Acquire),
            wait_count: counters.wait_count.load(Ordering::Acquire),
            wait_ticks: counters.wait_ticks.load(Ordering::Acquire),
            max_wait_ticks: counters.max_wait_ticks.load(Ordering::Acquire),
            pcpu_mask: counters.pcpu_mask.load(Ordering::Acquire),
            migrations: counters.migrations.load(Ordering::Acquire),
        });
    }

    Some(HostRtTraceSnapshot {
        vm_id,
        counter_frequency_hz: COUNTER_FREQUENCY_HZ.load(Ordering::Acquire),
        start_ticks,
        end_ticks,
        dropped: TIMER_INJECTIONS.dropped.load(Ordering::Acquire),
        incomplete: TIMER_INJECTIONS.incomplete.load(Ordering::Acquire)
            + record_count.saturating_sub(injections.len()),
        failed_injections: FAILED_INJECTIONS.load(Ordering::Acquire),
        unowned_virtual_timer_irqs: UNOWNED_VIRTUAL_TIMER_IRQS.load(Ordering::Acquire),
        counter_frequency_mismatches: COUNTER_FREQUENCY_MISMATCHES.load(Ordering::Acquire),
        injections,
        pcpus,
        vcpus,
    })
}

/// Converts a host counter delta to nanoseconds using the active platform.
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    default_host().ticks_to_nanos(ticks)
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    use super::{TraceBuffer, VirtualTimerInjectionRecord};

    fn record() -> VirtualTimerInjectionRecord {
        VirtualTimerInjectionRecord {
            sequence: 99,
            vm_id: 1,
            vcpu_id: 0,
            pcpu_id: 1,
            physical_irq: 27,
            virtual_irq: 27,
            host_counter_ticks: 200,
            guest_counter_ticks: 100,
            forwarding_ticks: 4,
            injected: true,
        }
    }

    #[test]
    fn fixed_trace_buffer_assigns_sequence_and_reports_overflow() {
        let buffer = TraceBuffer::<1>::new();
        buffer.reset_and_start();
        buffer.record(record());
        buffer.record(record());
        buffer.stop();

        let stored = buffer.record_at(0).unwrap();
        assert_eq!(stored.sequence, 0);
        assert_eq!(buffer.dropped.load(Ordering::Relaxed), 1);
    }
}

// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(feature = "timer-latency-stats")]
use std::sync::atomic::AtomicU64;
use std::{
    format,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    AsVCpuTask, AxVmResult, GuestPhysAddr, StopReason, VCpuTask, VmStatus, VmVcpuState,
    arch::current::CurrentArch,
    architecture::{ArchOps, Architecture, VcpuRunAction},
    ax_err_type,
    host::HostTime,
    irq::model::{PendingVcpuInterrupt, VirtualInterruptId},
    runtime::{VCpuRef, VIRQ_INJECTOR_TASK_PRIORITY, VMRef, sub_running_vm_count},
    vm::{PendingInterrupt, VmRuntimeHandle},
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB
const PERIODIC_VIRQ_STACK_SIZE: usize = 0x10000;
// `vm.running()` becomes true before the guest installs its ISR. Keep the
// warm-up identical for every A/B variant so startup is excluded from samples.
const PERIODIC_VIRQ_GUEST_WARMUP: Duration = Duration::from_secs(2);

static VCPU_PARK_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static POST_VMEXIT_YIELD_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VCPU_WAKE_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static NOTIFY_WOKE_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_ARM_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_IMMEDIATE_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_NO_DEADLINE_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_REGISTER_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_CALLBACK_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_STALE_CALLBACK_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_NOTIFICATION_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_INVALIDATION_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_DIRECT_ACK_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VTIMER_DIRECT_OVERLAP_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
const VTIMER_STAGE_LATENCY_BUCKET_NS: u64 = 1_000;
#[cfg(feature = "timer-latency-stats")]
const VTIMER_STAGE_LATENCY_BUCKETS: usize = 4_096;
#[cfg(all(feature = "timer-latency-stats", any(target_arch = "aarch64", test)))]
static VTIMER_CALLBACK_PENDING_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(all(feature = "timer-latency-stats", target_arch = "aarch64"))]
static VTIMER_CALLBACK_GUEST_ENTRY_PENDING_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_WAKE_HISTOGRAMS: [[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS]; 8] =
    [const { [const { AtomicUsize::new(0) }; VTIMER_STAGE_LATENCY_BUCKETS] }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_WAKE_OVERFLOWS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_WAKE_MAX_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_ENTRY_HISTOGRAMS: [[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS]; 8] =
    [const { [const { AtomicUsize::new(0) }; VTIMER_STAGE_LATENCY_BUCKETS] }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_ENTRY_OVERFLOWS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_ENTRY_MAX_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_GUEST_ENTRY_HISTOGRAMS: [[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS]; 8] =
    [const { [const { AtomicUsize::new(0) }; VTIMER_STAGE_LATENCY_BUCKETS] }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_GUEST_ENTRY_OVERFLOWS: [AtomicUsize; 8] =
    [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_CALLBACK_TO_GUEST_ENTRY_MAX_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(all(feature = "timer-latency-stats", target_arch = "aarch64"))]
static VTIMER_DIRECT_PENDING_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(all(feature = "timer-latency-stats", target_arch = "aarch64"))]
static VTIMER_DIRECT_GUEST_ENTRY_PENDING_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_DIRECT_TO_ENTRY_HISTOGRAMS: [[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS]; 8] =
    [const { [const { AtomicUsize::new(0) }; VTIMER_STAGE_LATENCY_BUCKETS] }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_DIRECT_TO_ENTRY_OVERFLOWS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_DIRECT_TO_ENTRY_MAX_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_DIRECT_TO_GUEST_ENTRY_HISTOGRAMS: [[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS]; 8] =
    [const { [const { AtomicUsize::new(0) }; VTIMER_STAGE_LATENCY_BUCKETS] }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_DIRECT_TO_GUEST_ENTRY_OVERFLOWS: [AtomicUsize; 8] =
    [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_DIRECT_TO_GUEST_ENTRY_MAX_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_ACTIVATION_HOLD_HISTOGRAMS: [[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS]; 8] =
    [const { [const { AtomicUsize::new(0) }; VTIMER_STAGE_LATENCY_BUCKETS] }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_ACTIVATION_HOLD_OVERFLOWS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
#[cfg(feature = "timer-latency-stats")]
static VTIMER_ACTIVATION_HOLD_MAX_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub(crate) static LR_SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Default)]
struct VtimerStageLatencySnapshot {
    samples: usize,
    overflow: usize,
    p50_ns: u64,
    p99_ns: u64,
    p99_9_ns: u64,
    max_ns: u64,
}

#[cfg(feature = "timer-latency-stats")]
fn vtimer_stage_latency_snapshot(
    histogram: &[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS],
    overflow: &AtomicUsize,
    max_ns: &AtomicU64,
) -> VtimerStageLatencySnapshot {
    let overflow = overflow.load(Ordering::Relaxed);
    let samples = histogram
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .sum::<usize>()
        .saturating_add(overflow);
    let max_ns = max_ns.load(Ordering::Relaxed);
    VtimerStageLatencySnapshot {
        samples,
        overflow,
        p50_ns: vtimer_stage_latency_percentile(histogram, samples, 50, 100, max_ns),
        p99_ns: vtimer_stage_latency_percentile(histogram, samples, 99, 100, max_ns),
        p99_9_ns: vtimer_stage_latency_percentile(histogram, samples, 999, 1_000, max_ns),
        max_ns,
    }
}

#[cfg(feature = "timer-latency-stats")]
fn vtimer_stage_latency_percentile(
    histogram: &[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS],
    samples: usize,
    numerator: usize,
    denominator: usize,
    max_ns: u64,
) -> u64 {
    if samples == 0 {
        return 0;
    }
    let rank = samples
        .saturating_mul(numerator)
        .saturating_add(denominator - 1)
        / denominator;
    let mut cumulative = 0usize;
    for (bucket, count) in histogram.iter().enumerate() {
        cumulative = cumulative.saturating_add(count.load(Ordering::Relaxed));
        if cumulative >= rank {
            return ((bucket as u64) + 1).saturating_mul(VTIMER_STAGE_LATENCY_BUCKET_NS);
        }
    }
    max_ns
}

#[cfg(all(feature = "timer-latency-stats", any(target_arch = "aarch64", test)))]
fn record_vtimer_stage_latency(
    histogram: &[AtomicUsize; VTIMER_STAGE_LATENCY_BUCKETS],
    overflow: &AtomicUsize,
    max_ns: &AtomicU64,
    latency_ns: u64,
) {
    max_ns.fetch_max(latency_ns, Ordering::Relaxed);
    let bucket = (latency_ns / VTIMER_STAGE_LATENCY_BUCKET_NS) as usize;
    if let Some(count) = histogram.get(bucket) {
        count.fetch_add(1, Ordering::Relaxed);
    } else {
        overflow.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(all(feature = "timer-latency-stats", any(target_arch = "aarch64", test)))]
fn vtimer_stage_now_ns() -> u64 {
    crate::host::default_host()
        .monotonic_time()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

pub(crate) fn notify_woke_count(vcpu_id: usize) -> Option<&'static AtomicUsize> {
    NOTIFY_WOKE_COUNTS.get(vcpu_id)
}

/// Records a vCPU entering its WFI/event wait (park) on the E1 counters.
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn note_vcpu_park(vcpu_id: usize) {
    VCPU_PARK_COUNTS
        .get(vcpu_id)
        .map(|count| count.fetch_add(1, Ordering::Relaxed));
}

/// Records a vCPU leaving its WFI/event wait (wake) on the E1 counters.
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn note_vcpu_wake(vcpu_id: usize) {
    VCPU_WAKE_COUNTS
        .get(vcpu_id)
        .map(|count| count.fetch_add(1, Ordering::Relaxed));
    #[cfg(feature = "timer-latency-stats")]
    if let Some(callback_ns) = VTIMER_CALLBACK_PENDING_NS
        .get(vcpu_id)
        .map(|timestamp| timestamp.load(Ordering::Acquire))
        .filter(|timestamp| *timestamp != 0)
    {
        record_vtimer_stage_latency(
            &VTIMER_CALLBACK_TO_WAKE_HISTOGRAMS[vcpu_id],
            &VTIMER_CALLBACK_TO_WAKE_OVERFLOWS[vcpu_id],
            &VTIMER_CALLBACK_TO_WAKE_MAX_NS[vcpu_id],
            vtimer_stage_now_ns().saturating_sub(callback_ns),
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn note_vtimer_counter(counters: &[AtomicUsize; 8], vcpu_id: usize) {
    if let Some(count) = counters.get(vcpu_id) {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_arm(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_ARM_COUNTS, vcpu_id);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_immediate(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_IMMEDIATE_COUNTS, vcpu_id);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_no_deadline(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_NO_DEADLINE_COUNTS, vcpu_id);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_registered(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_REGISTER_COUNTS, vcpu_id);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_callback(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_CALLBACK_COUNTS, vcpu_id);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_stale_callback(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_STALE_CALLBACK_COUNTS, vcpu_id);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_notification(vcpu_id: usize, _callback_ns: u64) {
    note_vtimer_counter(&VTIMER_NOTIFICATION_COUNTS, vcpu_id);
    #[cfg(feature = "timer-latency-stats")]
    if let Some(timestamp) = VTIMER_CALLBACK_PENDING_NS.get(vcpu_id) {
        timestamp.store(_callback_ns.max(1), Ordering::Release);
    }
    #[cfg(all(feature = "timer-latency-stats", target_arch = "aarch64"))]
    if let Some(timestamp) = VTIMER_CALLBACK_GUEST_ENTRY_PENDING_NS.get(vcpu_id) {
        timestamp.store(_callback_ns.max(1), Ordering::Release);
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_direct_ack(vcpu_id: usize, _accepted_ns: u64, overlaps_active: bool) {
    note_vtimer_counter(&VTIMER_DIRECT_ACK_COUNTS, vcpu_id);
    if overlaps_active {
        note_vtimer_counter(&VTIMER_DIRECT_OVERLAP_COUNTS, vcpu_id);
    }
    #[cfg(feature = "timer-latency-stats")]
    if let Some(timestamp) = VTIMER_DIRECT_PENDING_NS.get(vcpu_id) {
        timestamp.store(_accepted_ns.max(1), Ordering::Release);
    }
    #[cfg(feature = "timer-latency-stats")]
    if let Some(timestamp) = VTIMER_DIRECT_GUEST_ENTRY_PENDING_NS.get(vcpu_id) {
        timestamp.store(_accepted_ns.max(1), Ordering::Release);
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_activation_hold(_vcpu_id: usize, _accepted_ns: u64) {
    #[cfg(feature = "timer-latency-stats")]
    if let Some(histogram) = VTIMER_ACTIVATION_HOLD_HISTOGRAMS.get(_vcpu_id) {
        record_vtimer_stage_latency(
            histogram,
            &VTIMER_ACTIVATION_HOLD_OVERFLOWS[_vcpu_id],
            &VTIMER_ACTIVATION_HOLD_MAX_NS[_vcpu_id],
            vtimer_stage_now_ns().saturating_sub(_accepted_ns),
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn note_vtimer_run_dispatch(_vcpu_id: usize) {
    #[cfg(feature = "timer-latency-stats")]
    if let Some(callback_ns) = VTIMER_CALLBACK_PENDING_NS
        .get(_vcpu_id)
        .map(|timestamp| timestamp.swap(0, Ordering::AcqRel))
        .filter(|timestamp| *timestamp != 0)
    {
        record_vtimer_stage_latency(
            &VTIMER_CALLBACK_TO_ENTRY_HISTOGRAMS[_vcpu_id],
            &VTIMER_CALLBACK_TO_ENTRY_OVERFLOWS[_vcpu_id],
            &VTIMER_CALLBACK_TO_ENTRY_MAX_NS[_vcpu_id],
            vtimer_stage_now_ns().saturating_sub(callback_ns),
        );
    }
    #[cfg(feature = "timer-latency-stats")]
    if let Some(accepted_ns) = VTIMER_DIRECT_PENDING_NS
        .get(_vcpu_id)
        .map(|timestamp| timestamp.swap(0, Ordering::AcqRel))
        .filter(|timestamp| *timestamp != 0)
    {
        record_vtimer_stage_latency(
            &VTIMER_DIRECT_TO_ENTRY_HISTOGRAMS[_vcpu_id],
            &VTIMER_DIRECT_TO_ENTRY_OVERFLOWS[_vcpu_id],
            &VTIMER_DIRECT_TO_ENTRY_MAX_NS[_vcpu_id],
            vtimer_stage_now_ns().saturating_sub(accepted_ns),
        );
    }
}

/// Records the first architecture backend entry after a virtual-timer event.
///
/// This boundary is after pending-vIRQ drain, timer preparation, vCPU state
/// transition, and VGIC load, immediately before entering the Guest backend.
#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_guest_entry(_vcpu_id: usize) {
    #[cfg(feature = "timer-latency-stats")]
    if let Some(callback_ns) = VTIMER_CALLBACK_GUEST_ENTRY_PENDING_NS
        .get(_vcpu_id)
        .map(|timestamp| timestamp.swap(0, Ordering::AcqRel))
        .filter(|timestamp| *timestamp != 0)
    {
        record_vtimer_stage_latency(
            &VTIMER_CALLBACK_TO_GUEST_ENTRY_HISTOGRAMS[_vcpu_id],
            &VTIMER_CALLBACK_TO_GUEST_ENTRY_OVERFLOWS[_vcpu_id],
            &VTIMER_CALLBACK_TO_GUEST_ENTRY_MAX_NS[_vcpu_id],
            vtimer_stage_now_ns().saturating_sub(callback_ns),
        );
    }
    #[cfg(feature = "timer-latency-stats")]
    if let Some(accepted_ns) = VTIMER_DIRECT_GUEST_ENTRY_PENDING_NS
        .get(_vcpu_id)
        .map(|timestamp| timestamp.swap(0, Ordering::AcqRel))
        .filter(|timestamp| *timestamp != 0)
    {
        record_vtimer_stage_latency(
            &VTIMER_DIRECT_TO_GUEST_ENTRY_HISTOGRAMS[_vcpu_id],
            &VTIMER_DIRECT_TO_GUEST_ENTRY_OVERFLOWS[_vcpu_id],
            &VTIMER_DIRECT_TO_GUEST_ENTRY_MAX_NS[_vcpu_id],
            vtimer_stage_now_ns().saturating_sub(accepted_ns),
        );
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn note_vtimer_invalidation(vcpu_id: usize) {
    note_vtimer_counter(&VTIMER_INVALIDATION_COUNTS, vcpu_id);
}

pub(crate) fn rt_vcpu_stats_snapshot() -> Vec<crate::VcpuRuntimeCounts> {
    (0..VCPU_PARK_COUNTS.len())
        .map(|vcpu_id| {
            #[cfg(feature = "timer-latency-stats")]
            let callback_to_wake = vtimer_stage_latency_snapshot(
                &VTIMER_CALLBACK_TO_WAKE_HISTOGRAMS[vcpu_id],
                &VTIMER_CALLBACK_TO_WAKE_OVERFLOWS[vcpu_id],
                &VTIMER_CALLBACK_TO_WAKE_MAX_NS[vcpu_id],
            );
            #[cfg(not(feature = "timer-latency-stats"))]
            let callback_to_wake = VtimerStageLatencySnapshot::default();
            #[cfg(feature = "timer-latency-stats")]
            let callback_to_entry = vtimer_stage_latency_snapshot(
                &VTIMER_CALLBACK_TO_ENTRY_HISTOGRAMS[vcpu_id],
                &VTIMER_CALLBACK_TO_ENTRY_OVERFLOWS[vcpu_id],
                &VTIMER_CALLBACK_TO_ENTRY_MAX_NS[vcpu_id],
            );
            #[cfg(not(feature = "timer-latency-stats"))]
            let callback_to_entry = VtimerStageLatencySnapshot::default();
            #[cfg(feature = "timer-latency-stats")]
            let callback_to_guest_entry = vtimer_stage_latency_snapshot(
                &VTIMER_CALLBACK_TO_GUEST_ENTRY_HISTOGRAMS[vcpu_id],
                &VTIMER_CALLBACK_TO_GUEST_ENTRY_OVERFLOWS[vcpu_id],
                &VTIMER_CALLBACK_TO_GUEST_ENTRY_MAX_NS[vcpu_id],
            );
            #[cfg(not(feature = "timer-latency-stats"))]
            let callback_to_guest_entry = VtimerStageLatencySnapshot::default();
            #[cfg(feature = "timer-latency-stats")]
            let direct_to_entry = vtimer_stage_latency_snapshot(
                &VTIMER_DIRECT_TO_ENTRY_HISTOGRAMS[vcpu_id],
                &VTIMER_DIRECT_TO_ENTRY_OVERFLOWS[vcpu_id],
                &VTIMER_DIRECT_TO_ENTRY_MAX_NS[vcpu_id],
            );
            #[cfg(not(feature = "timer-latency-stats"))]
            let direct_to_entry = VtimerStageLatencySnapshot::default();
            #[cfg(feature = "timer-latency-stats")]
            let direct_to_guest_entry = vtimer_stage_latency_snapshot(
                &VTIMER_DIRECT_TO_GUEST_ENTRY_HISTOGRAMS[vcpu_id],
                &VTIMER_DIRECT_TO_GUEST_ENTRY_OVERFLOWS[vcpu_id],
                &VTIMER_DIRECT_TO_GUEST_ENTRY_MAX_NS[vcpu_id],
            );
            #[cfg(not(feature = "timer-latency-stats"))]
            let direct_to_guest_entry = VtimerStageLatencySnapshot::default();
            #[cfg(feature = "timer-latency-stats")]
            let activation_hold = vtimer_stage_latency_snapshot(
                &VTIMER_ACTIVATION_HOLD_HISTOGRAMS[vcpu_id],
                &VTIMER_ACTIVATION_HOLD_OVERFLOWS[vcpu_id],
                &VTIMER_ACTIVATION_HOLD_MAX_NS[vcpu_id],
            );
            #[cfg(not(feature = "timer-latency-stats"))]
            let activation_hold = VtimerStageLatencySnapshot::default();
            crate::VcpuRuntimeCounts {
                vcpu_id,
                post_vmexit_yields: POST_VMEXIT_YIELD_COUNTS[vcpu_id].load(Ordering::Relaxed),
                parks: VCPU_PARK_COUNTS[vcpu_id].load(Ordering::Relaxed),
                wakes: VCPU_WAKE_COUNTS[vcpu_id].load(Ordering::Relaxed),
                notify_woke: NOTIFY_WOKE_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_arms: VTIMER_ARM_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_immediate: VTIMER_IMMEDIATE_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_no_deadline: VTIMER_NO_DEADLINE_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_registered: VTIMER_REGISTER_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_callbacks: VTIMER_CALLBACK_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_stale_callbacks: VTIMER_STALE_CALLBACK_COUNTS[vcpu_id]
                    .load(Ordering::Relaxed),
                vtimer_notifications: VTIMER_NOTIFICATION_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_invalidations: VTIMER_INVALIDATION_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_direct_acks: VTIMER_DIRECT_ACK_COUNTS[vcpu_id].load(Ordering::Relaxed),
                vtimer_direct_overlaps: VTIMER_DIRECT_OVERLAP_COUNTS[vcpu_id]
                    .load(Ordering::Relaxed),
                vtimer_callback_to_wake_samples: callback_to_wake.samples,
                vtimer_callback_to_wake_overflow: callback_to_wake.overflow,
                vtimer_callback_to_wake_p50_ns: callback_to_wake.p50_ns,
                vtimer_callback_to_wake_p99_ns: callback_to_wake.p99_ns,
                vtimer_callback_to_wake_p99_9_ns: callback_to_wake.p99_9_ns,
                vtimer_callback_to_wake_max_ns: callback_to_wake.max_ns,
                vtimer_callback_to_entry_samples: callback_to_entry.samples,
                vtimer_callback_to_entry_overflow: callback_to_entry.overflow,
                vtimer_callback_to_entry_p50_ns: callback_to_entry.p50_ns,
                vtimer_callback_to_entry_p99_ns: callback_to_entry.p99_ns,
                vtimer_callback_to_entry_p99_9_ns: callback_to_entry.p99_9_ns,
                vtimer_callback_to_entry_max_ns: callback_to_entry.max_ns,
                vtimer_callback_to_guest_entry_samples: callback_to_guest_entry.samples,
                vtimer_callback_to_guest_entry_overflow: callback_to_guest_entry.overflow,
                vtimer_callback_to_guest_entry_p50_ns: callback_to_guest_entry.p50_ns,
                vtimer_callback_to_guest_entry_p99_ns: callback_to_guest_entry.p99_ns,
                vtimer_callback_to_guest_entry_p99_9_ns: callback_to_guest_entry.p99_9_ns,
                vtimer_callback_to_guest_entry_max_ns: callback_to_guest_entry.max_ns,
                vtimer_direct_to_entry_samples: direct_to_entry.samples,
                vtimer_direct_to_entry_overflow: direct_to_entry.overflow,
                vtimer_direct_to_entry_p50_ns: direct_to_entry.p50_ns,
                vtimer_direct_to_entry_p99_ns: direct_to_entry.p99_ns,
                vtimer_direct_to_entry_p99_9_ns: direct_to_entry.p99_9_ns,
                vtimer_direct_to_entry_max_ns: direct_to_entry.max_ns,
                vtimer_direct_to_guest_entry_samples: direct_to_guest_entry.samples,
                vtimer_direct_to_guest_entry_overflow: direct_to_guest_entry.overflow,
                vtimer_direct_to_guest_entry_p50_ns: direct_to_guest_entry.p50_ns,
                vtimer_direct_to_guest_entry_p99_ns: direct_to_guest_entry.p99_ns,
                vtimer_direct_to_guest_entry_p99_9_ns: direct_to_guest_entry.p99_9_ns,
                vtimer_direct_to_guest_entry_max_ns: direct_to_guest_entry.max_ns,
                vtimer_activation_hold_samples: activation_hold.samples,
                vtimer_activation_hold_overflow: activation_hold.overflow,
                vtimer_activation_hold_p50_ns: activation_hold.p50_ns,
                vtimer_activation_hold_p99_ns: activation_hold.p99_ns,
                vtimer_activation_hold_p99_9_ns: activation_hold.p99_9_ns,
                vtimer_activation_hold_max_ns: activation_hold.max_ns,
            }
        })
        .collect()
}

#[cfg(test)]
mod rt_stats_tests {
    use super::*;

    #[test]
    fn vcpu_runtime_snapshot_observes_park_and_wake_edges() {
        let before = crate::rt_runtime_stats_snapshot();
        note_vcpu_park(3);
        note_vcpu_wake(3);
        let after = crate::rt_runtime_stats_snapshot();

        assert_eq!(after.vcpus[3].parks, before.vcpus[3].parks + 1);
        assert_eq!(after.vcpus[3].wakes, before.vcpus[3].wakes + 1);
    }

    #[test]
    fn vcpu_runtime_snapshot_observes_post_vmexit_yields() {
        let vcpu_id = 6;
        let before = crate::rt_runtime_stats_snapshot();
        POST_VMEXIT_YIELD_COUNTS[vcpu_id].fetch_add(1, Ordering::Relaxed);
        let after = crate::rt_runtime_stats_snapshot();

        assert_eq!(
            after.vcpus[vcpu_id].post_vmexit_yields,
            before.vcpus[vcpu_id].post_vmexit_yields + 1
        );
    }

    #[cfg(feature = "timer-latency-stats")]
    #[test]
    fn vtimer_stage_histogram_reports_percentiles_and_overflow() {
        let vcpu_id = 7;
        let before = rt_vcpu_stats_snapshot()[vcpu_id];
        record_vtimer_stage_latency(
            &VTIMER_CALLBACK_TO_ENTRY_HISTOGRAMS[vcpu_id],
            &VTIMER_CALLBACK_TO_ENTRY_OVERFLOWS[vcpu_id],
            &VTIMER_CALLBACK_TO_ENTRY_MAX_NS[vcpu_id],
            25_500,
        );
        record_vtimer_stage_latency(
            &VTIMER_CALLBACK_TO_ENTRY_HISTOGRAMS[vcpu_id],
            &VTIMER_CALLBACK_TO_ENTRY_OVERFLOWS[vcpu_id],
            &VTIMER_CALLBACK_TO_ENTRY_MAX_NS[vcpu_id],
            5_000_000,
        );
        let after = rt_vcpu_stats_snapshot()[vcpu_id];

        assert_eq!(
            after.vtimer_callback_to_entry_samples,
            before.vtimer_callback_to_entry_samples + 2
        );
        assert_eq!(
            after.vtimer_callback_to_entry_overflow,
            before.vtimer_callback_to_entry_overflow + 1
        );
        assert!(after.vtimer_callback_to_entry_p50_ns >= 26_000);
        assert!(after.vtimer_callback_to_entry_p99_ns >= 5_000_000);
        assert!(after.vtimer_callback_to_entry_max_ns >= 5_000_000);
    }
}

/// Spawn the common host-side periodic injector used by both A and B.
pub(crate) fn spawn_periodic_virq_injector(
    vm: VMRef,
    config: crate::PeriodicVirqConfig,
) -> AxVmResult {
    validate_periodic_virq_config(&config)?;
    if vm.vcpu(config.vcpu_id).is_none() {
        return Err(ax_err_type!(
            NotFound,
            format!("vCPU {} not found", config.vcpu_id)
        ));
    }

    let task = crate::TaskInner::new(
        move || run_periodic_virq_injector(vm, config),
        format!("openrace-virq-injector-vcpu-{}", config.vcpu_id),
        PERIODIC_VIRQ_STACK_SIZE,
    );
    task.set_sched_priority(VIRQ_INJECTOR_TASK_PRIORITY);
    if let Some(cpu_id) = config.injector_cpu_id {
        let bits = 1usize.checked_shl(cpu_id as u32).ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                format!("injector CPU {cpu_id} is not representable")
            )
        })?;
        task.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(bits));
    }
    crate::host::task::spawn_task(task);
    Ok(())
}

fn validate_periodic_virq_config(config: &crate::PeriodicVirqConfig) -> AxVmResult {
    if config.samples == 0 {
        return Err(ax_err_type!(
            BadState,
            "periodic vIRQ samples must be non-zero"
        ));
    }
    if config.period.is_zero() {
        return Err(ax_err_type!(
            BadState,
            "periodic vIRQ period must be non-zero"
        ));
    }
    // The injector targets a vCPU with a fixed 64-bit host mask; reject larger
    // IDs explicitly instead of panicking inside CpuMask::one_shot.
    if config.vcpu_id >= 64 {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "vCPU {} exceeds the 64-bit injector target mask",
                config.vcpu_id
            )
        ));
    }
    Ok(())
}

fn run_periodic_virq_injector(vm: VMRef, config: crate::PeriodicVirqConfig) {
    let wait_started = ax_std::time::Instant::now();
    while !vm.running() {
        if vm.stopped() || wait_started.elapsed() >= Duration::from_secs(5) {
            warn!(
                "OpenRace vIRQ injector did not observe VM[{}] running before timeout",
                vm.id()
            );
            return;
        }
        ax_std::thread::sleep(Duration::from_millis(1));
    }

    let targets = crate::CpuMask::<64>::one_shot(config.vcpu_id);
    let mut deadline = ax_std::time::Instant::now() + PERIODIC_VIRQ_GUEST_WARMUP + config.period;
    let mut failed = 0usize;
    for sequence in 0..config.samples {
        let now = ax_std::time::Instant::now();
        let remaining = deadline.duration_since(now);
        if !remaining.is_zero() {
            ax_std::thread::sleep(remaining);
        }
        let requested_ns = crate::host::default_host().monotonic_time().as_nanos() as u64;
        let result = vm.inject_interrupt_to_vcpu(targets, config.vector);
        let completed_ns = crate::host::default_host().monotonic_time().as_nanos() as u64;
        if result.is_err() {
            failed += 1;
        }
        info!(
            "VIRQ_INJECT sequence={} vm={} vcpu={} vector={} requested_ns={} completed_ns={} \
             status={}",
            sequence,
            vm.id(),
            config.vcpu_id,
            config.vector,
            requested_ns,
            completed_ns,
            if result.is_ok() { "ok" } else { "error" },
        );
        deadline += config.period;
        if !vm.running() {
            break;
        }
    }
    info!(
        "VIRQ_INJECT_COMPLETE vm={} vcpu={} vector={} samples={} errors={}",
        vm.id(),
        config.vcpu_id,
        config.vector,
        config.samples,
        failed,
    );
    info!(
        "E1_COUNTERS vcpu0_park={} vcpu0_wake={} vcpu1_park={} vcpu1_wake={} notify_woke0={} \
         notify_woke1={} lr_skip={}",
        VCPU_PARK_COUNTS[0].load(Ordering::Relaxed),
        VCPU_WAKE_COUNTS[0].load(Ordering::Relaxed),
        VCPU_PARK_COUNTS[1].load(Ordering::Relaxed),
        VCPU_WAKE_COUNTS[1].load(Ordering::Relaxed),
        NOTIFY_WOKE_COUNTS[0].load(Ordering::Relaxed),
        NOTIFY_WOKE_COUNTS[1].load(Ordering::Relaxed),
        LR_SKIP_COUNT.load(Ordering::Relaxed),
    );
    // Let the final interrupt reach and be timestamped by the guest before
    // the injector task exits. The AxVM timer wheel is registered as a
    // deadline source in axtask, so a plain host-task sleep wakes on time.
    ax_std::thread::sleep(config.period);
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu wait queue is used to block the current thread.
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
fn wait_for<F>(vm_vcpus: &VmRuntimeHandle, condition: F)
where
    F: Fn() -> bool,
{
    vm_vcpus.wait_until(condition);
}

fn vcpu_start_is_ready(vm_running: bool, task_registered: bool) -> bool {
    vm_running && task_registered
}

/// Notifies the primary VCpu task associated with the specified VM to wake up and resume execution.
/// This function is used to notify the primary VCpu of a VM to start running after the VM has been booted.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus are to be notified.
pub(crate) fn notify_primary_vcpu(vm_id: usize) {
    // Generally, the primary VCpu is the first and **only** VCpu in the list.
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found while notifying primary vCPU");
        return;
    };
    if let Err(err) = vm.runtime_snapshot().map(|runtime| {
        runtime.notify_vcpu_startup(0);
    }) {
        warn!("VM[{vm_id}] vCPU runtime not found: {err:?}");
    }
}

/// Notifies all VCpu tasks associated with the specified VM to wake up.
/// This is useful when shutting down a VM to ensure all waiting vCPUs can check the shutdown flag.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus should be notified.
pub(crate) fn notify_all_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id) {
        let _ = vm.runtime_snapshot().map(|runtime| {
            runtime.notify_all();
        });
    }
}

pub(crate) fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    // Take the runtime handle without holding the VM machine lock across the
    // wake: a parked vCPU evaluates its wait condition while holding its wait
    // queue lock and takes the machine lock inside `vm.running()`/`stopping()`,
    // so notifying under the machine lock is an ABBA deadlock (observed once
    // vCPUs actually park via PSCI CPU_SUSPEND standby).
    let runtime = vm.with_runtime(|runtime| Ok(runtime.clone()))?;
    runtime.dispatch_vcpu_interrupt(
        vcpu_id,
        PendingVcpuInterrupt {
            id: VirtualInterruptId(vector as u32),
            trigger: crate::InterruptTriggerMode::EdgeTriggered,
        },
    )
}

#[cfg_attr(
    not(target_arch = "loongarch64"),
    expect(
        dead_code,
        reason = "only the LoongArch IRQ backend queues physical interrupts"
    )
)]
pub(crate) fn queue_pending_interrupt(
    vm_id: usize,
    vcpu_id: usize,
    interrupt: PendingInterrupt,
) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let cpu_id = vm.with_runtime(|runtime| runtime.queue_pending_interrupt(vcpu_id, interrupt))?;
    vm.runtime_snapshot()?.notify_all();
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

/// Wake and kick a target vCPU after an architecture IRQ backend has
/// published pending state outside the generic runtime queue.
pub(crate) fn notify_vcpu(vm_id: usize, vcpu_id: usize) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let runtime = vm.with_runtime(|runtime| Ok(runtime.clone()))?;
    let cpu_id = runtime.vcpu_cpu_id(vcpu_id)?;
    // Architecture controllers already own the pending interrupt state. Wake
    // only its target vCPU; waking every guest CPU adds cross-CPU scheduler
    // traffic and obscures whether the intended waiter was actually released.
    runtime.notify_vcpu_unconditional(vcpu_id);
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

pub(crate) fn inject_pending_interrupts<A: Architecture>(
    vm_id: usize,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) {
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };
    let Ok(interrupts) = vm.with_runtime(|runtime| Ok(runtime.drain_pending_interrupts(vcpu_id)))
    else {
        warn!("VM[{vm_id}] vCPU runtime not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };

    for interrupt in interrupts {
        A::inject_pending_interrupt(&vm, vcpu, interrupt);
    }
}

/// Cleans up VCpu resources for a VM that is being deleted.
/// This removes the VM's entry from the global VCpu wait queue.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu resources should be cleaned up.
///
/// # Note
///
/// This should be called after all VCpu threads have exited to avoid resource leaks.
/// It will join all VCpu tasks to ensure they are fully cleaned up.
pub(crate) fn cleanup_vm_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id)
        && let Err(err) = vm.with_runtime(|runtime| runtime.join_all_vcpu_tasks(vm_id))
    {
        warn!("VM[{vm_id}] vCPU runtime cleanup skipped: {err:?}");
    }
}

/// Marks the VCpu of the specified VM as running.
fn mark_vcpu_running(vm: &VMRef) {
    let _ = vm.with_runtime(|runtime| {
        runtime.mark_vcpu_running();
        Ok(())
    });
}

type CpuOnStartAckLock<T> = std::sync::Mutex<T>;

#[allow(dead_code)]
pub(crate) struct CpuOnStartAck {
    inner: CpuOnStartAckLock<CpuOnStartAckInner>,
}

struct CpuOnStartAckInner {
    started: bool,
    cancelled: bool,
    result: Option<crate::AxVmResult>,
}

#[allow(dead_code)]
impl CpuOnStartAck {
    pub(crate) fn new() -> Self {
        Self {
            inner: CpuOnStartAckLock::new(CpuOnStartAckInner {
                started: false,
                cancelled: false,
                result: None,
            }),
        }
    }

    pub(crate) fn begin_startup(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.cancelled {
            false
        } else {
            inner.started = true;
            true
        }
    }

    pub(crate) fn cancel_before_startup(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.started || inner.result.is_some() {
            false
        } else {
            inner.cancelled = true;
            true
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.lock_inner().cancelled
    }

    pub(crate) fn complete(&self, result: crate::AxVmResult) {
        self.lock_inner().result = Some(result);
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.lock_inner().result.is_some()
    }

    pub(crate) fn take_result(&self) -> Option<crate::AxVmResult> {
        self.lock_inner().result.take()
    }

    fn lock_inner(&self) -> impl std::ops::DerefMut<Target = CpuOnStartAckInner> + '_ {
        use crate::sync::MutexExt;
        self.inner.lock_unpoisoned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum VcpuOnError {
    AlreadyOn,
    OnPending,
    StartFailed,
}

/// Boot target VCpu on the specified VM.
/// This function is used to boot a secondary VCpu on a VM, setting the entry point and argument for the VCpu.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM on which the VCpu is to be booted.
/// * `vcpu_id` - The ID of the VCpu to be booted.
/// * `entry_point` - The entry point of the VCpu.
/// * `arg` - The argument to be passed to the VCpu.
#[allow(dead_code)]
pub(crate) fn vcpu_on(
    vm: VMRef,
    vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
) -> Result<(), VcpuOnError> {
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or(VcpuOnError::StartFailed)?;

    match vcpu.state() {
        VmVcpuState::Free => {}
        VmVcpuState::Starting => return Err(VcpuOnError::OnPending),
        VmVcpuState::Ready | VmVcpuState::Running => return Err(VcpuOnError::AlreadyOn),
        _ => return Err(VcpuOnError::StartFailed),
    }

    vcpu.reserve_for_cpu_on()
        .map_err(|_| VcpuOnError::OnPending)?;

    let start_result = (|| {
        let runtime = vm
            .with_runtime(|runtime| Ok(runtime.clone()))
            .map_err(|_| VcpuOnError::StartFailed)?;
        if runtime.has_vcpu_task(vcpu_id) {
            return Err(VcpuOnError::StartFailed);
        }

        vcpu.set_entry(entry_point)
            .map_err(|_| VcpuOnError::StartFailed)?;
        CurrentArch::set_vcpu_on_args(&vcpu, vcpu_id, arg);

        let ack = Arc::new(CpuOnStartAck::new());
        runtime
            .insert_cpu_on_start_ack(vcpu_id, ack.clone())
            .map_err(|_| VcpuOnError::StartFailed)?;

        let vcpu_task = build_vcpu_task(&vm, vcpu.clone());
        spawn_registered_vcpu_task(vm.id(), vcpu_id, runtime.clone(), vcpu_task, None);
        runtime.notify_all();

        runtime.wait_until(|| ack.is_complete() || !vm.running());

        if !ack.is_complete() && !vm.running() {
            if ack.cancel_before_startup() {
                runtime.notify_all();

                if let Some(task) = runtime.remove_vcpu_task(vcpu_id) {
                    let _ = task.join();
                }

                runtime.remove_cpu_on_start_ack(vcpu_id);
                return Err(VcpuOnError::StartFailed);
            }

            runtime.wait_until(|| ack.is_complete());
        }

        let result = ack.take_result().unwrap_or_else(|| {
            Err(ax_err_type!(
                BadState,
                format!("vCPU {vcpu_id} CPU_ON startup did not complete")
            ))
        });
        runtime.remove_cpu_on_start_ack(vcpu_id);

        if result.is_err() {
            runtime.remove_vcpu_task(vcpu_id);
            return Err(VcpuOnError::StartFailed);
        }

        Ok(())
    })();

    if start_result.is_err() && vcpu.state() == VmVcpuState::Starting {
        vcpu.rollback_cpu_on();
    }
    start_result
}
pub(crate) fn spawn_registered_vcpu_task(
    vm_id: usize,
    vcpu_id: usize,
    runtime: std::sync::Arc<VmRuntimeHandle>,
    task: crate::TaskInner,
    cpu_id: Option<usize>,
) -> crate::AxTaskRef {
    crate::host::task::spawn_task_with(task, |task_ref| {
        let cpu_id = cpu_id.unwrap_or_else(|| task_ref.cpu_id() as usize);
        runtime
            .add_vcpu_task(vcpu_id, task_ref.clone(), cpu_id)
            .unwrap_or_else(|error| {
                panic!("VM[{vm_id}] vCPU[{vcpu_id}] task registration failed: {error}")
            });
    })
}

fn spawn_deferred_reset_task(vm_id: usize) {
    let reset_task = crate::TaskInner::new(
        move || {
            if let Err(err) = crate::runtime::reset_vm(vm_id) {
                warn!("VM[{vm_id}] deferred reset failed: {err:?}");
                crate::host::task::wait_queue_wake(&super::VMM, 1);
            }
        },
        format!("VM[{vm_id}]-reset"),
        KERNEL_STACK_SIZE,
    );
    crate::host::task::spawn_task(reset_task);
}

pub(crate) fn build_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> crate::TaskInner {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = crate::TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );
    let host_priority = vm.host_sched_priority();
    vcpu_task.set_sched_priority(host_priority);

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(
            vcpu_task_cpu_mask(vm.id(), vcpu.id(), phys_cpu_set),
        ));
    }

    // Use Weak reference in TaskExt to avoid keeping VM alive
    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(crate::AxTaskExt::from_impl(inner));

    info!(
        "VCpu task {} created priority={} {:?}",
        vcpu_task.id_name(),
        host_priority,
        vcpu_task.cpumask()
    );
    vcpu_task
}

fn vcpu_task_cpu_mask(vm_id: usize, vcpu_id: usize, requested_mask: usize) -> usize {
    let enabled_mask = crate::percpu::enabled_cpu_mask();
    if enabled_mask == 0 {
        warn!(
            "VM[{vm_id}] VCpu[{vcpu_id}] has no initialized host CPU mask; using requested mask \
             {requested_mask:#x}"
        );
        return requested_mask;
    }

    let initialized_requested_mask = requested_mask & enabled_mask;
    if initialized_requested_mask != 0 {
        if initialized_requested_mask != requested_mask {
            warn!(
                "VM[{vm_id}] VCpu[{vcpu_id}] requested host CPU mask {requested_mask:#x}, but \
                 only {initialized_requested_mask:#x} is initialized for AxVM"
            );
        }
        return initialized_requested_mask;
    }

    let fallback_mask = enabled_mask.isolate_lowest_one();
    warn!(
        "VM[{vm_id}] VCpu[{vcpu_id}] requested host CPU mask {requested_mask:#x}, but none of \
         those CPUs initialized AxVM; using initialized host CPU mask {fallback_mask:#x}"
    );
    fallback_mask
}

/// The main routine for VCpu task.
/// This function is the entry point for the VCpu tasks, which are spawned for each VCpu of a VM.
///
/// When the VCpu first starts running, it waits for the VM to be in the running state.
/// It then enters a loop where it runs the VCpu and handles the various exit reasons.
fn vcpu_run() {
    let curr = crate::host::task::current_task();

    let vm = curr.as_vcpu_task().vm();
    let vcpu = curr.as_vcpu_task().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();
    let Ok(runtime) = vm.with_runtime(|runtime| Ok(runtime.clone())) else {
        warn!("VM[{vm_id}] vCPU runtime not found, VCpu[{vcpu_id}] exiting");
        return;
    };

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    let cpu_on_start_ack = runtime.cpu_on_start_ack(vcpu_id);
    wait_for(&runtime, || {
        vcpu_start_is_ready(vm.running(), runtime.has_vcpu_task(vcpu_id))
            || cpu_on_start_ack
                .as_ref()
                .is_some_and(|ack| ack.is_cancelled())
    });

    if let Some(ack) = &cpu_on_start_ack {
        if !ack.begin_startup() {
            ack.complete(Err(ax_err_type!(
                BadState,
                format!("vCPU {vcpu_id} CPU_ON startup was cancelled")
            )));
            runtime.notify_all();
            return;
        }

        match vcpu.bind_after_cpu_on_or_rollback() {
            Ok(()) => {
                CurrentArch::before_first_run(&vm, &vcpu);
                runtime.publish_cpu_on_start_success(ack);
                runtime.notify_all();
            }
            Err(err) => {
                ack.complete(Err(err));
                runtime.notify_all();
                runtime.remove_cpu_on_start_ack(vcpu_id);
                runtime.remove_vcpu_task(vcpu_id);
                return;
            }
        }
    } else {
        CurrentArch::before_first_run(&vm, &vcpu);
        mark_vcpu_running(&vm);
    }

    info!(
        "VM[{}] VCpu[{}] running on CPU{}...",
        vm.id(),
        vcpu.id(),
        crate::host::cpu::current_id()
    );

    loop {
        if vcpu_id == 0 {
            // Host services only publish a request and wake this task. Polling
            // here avoids running virtual-device and VGIC callbacks in host
            // console context, where an idle guest may otherwise stall input.
            let _ = poll_primary_vcpu_devices_with(&runtime, || poll_vm_devices(&vm));
        }

        #[cfg(target_arch = "aarch64")]
        note_vtimer_run_dispatch(vcpu_id);
        match CurrentArch::run_vcpu(&vm, &vcpu) {
            Ok(VcpuRunAction {
                exits_vcpu: true, ..
            }) => {
                if let Err(err) = vcpu.power_off_after_cpu_off() {
                    warn!("VM[{vm_id}] VCpu[{vcpu_id}] CPU_OFF cleanup failed: {err:?}");
                }
                runtime.remove_vcpu_task(vcpu_id);
                let remaining = if runtime.consume_cpu_off_reservation(vcpu_id) {
                    // A pending CPU_ON holds this slot open, so the VM keeps a
                    // vCPU even though this task is gone.
                    RemainingVcpus::Present
                } else if runtime.mark_vcpu_exiting() {
                    RemainingVcpus::None
                } else {
                    RemainingVcpus::Present
                };
                if vcpu_exit_duty(VcpuExitDoor::CpuOff, remaining) == VcpuExitDuty::FinishVmStop {
                    finish_vm_stop_from_last_vcpu(&vm, &runtime, vcpu_id, VcpuExitDoor::CpuOff);
                }
                break;
            }
            Ok(VcpuRunAction {
                resets_vm: true, ..
            }) => {
                if runtime.request_deferred_reset()
                    && let Err(err) = vm.stop(StopReason::Forced)
                {
                    if vm.stopping() {
                        warn!("VM[{vm_id}] reset requested while VM is already stopping: {err:?}");
                    } else {
                        let _ = runtime.take_deferred_reset_request();
                        warn!("VM[{vm_id}] failed to request deferred reset stop: {err:?}");
                        if let Err(stop_err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                            warn!(
                                "VM[{vm_id}] shutdown after reset request failure failed: \
                                 {stop_err:?}"
                            );
                        }
                    }
                }
                notify_all_vcpus(vm_id);
            }
            Ok(VcpuRunAction {
                stop_reason: Some(reason),
                ..
            }) => {
                if let Err(err) = vm.stop(reason) {
                    warn!("VM[{vm_id}] shutdown failed: {err:?}");
                }
                notify_all_vcpus(vm_id);
            }
            Ok(VcpuRunAction {
                waits_for_event: true,
                ..
            }) => CurrentArch::wait_for_vcpu_event(&vm, &vcpu, &runtime),
            Ok(VcpuRunAction { .. }) => {}
            Err(err) => {
                error!("VM[{vm_id}] run VCpu[{vcpu_id}] get error {err:?}");
                if let Err(err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                    warn!("VM[{vm_id}] shutdown failed after vCPU error: {err:?}");
                }
                // Notify all vCPUs to wake up to check the shutdown flag
                notify_all_vcpus(vm_id);
            }
        }

        // Check if the VM is suspended
        if vm.suspending() {
            debug!(
                "VM[{}] VCpu[{}] is suspended, waiting for resume...",
                vm_id, vcpu_id
            );
            wait_for(&runtime, || !vm.suspending());
            info!("VM[{}] VCpu[{}] resumed from suspend", vm_id, vcpu_id);
            continue;
        }

        // Check if the VM is stopping.
        if vm.stopping() {
            warn!(
                "VM[{}] VCpu[{}] stopping because of VM stopping",
                vm_id, vcpu_id
            );

            let remaining = if runtime.mark_vcpu_exiting() {
                RemainingVcpus::None
            } else {
                RemainingVcpus::Present
            };
            if vcpu_exit_duty(VcpuExitDoor::VmStopping, remaining) == VcpuExitDuty::FinishVmStop {
                finish_vm_stop_from_last_vcpu(&vm, &runtime, vcpu_id, VcpuExitDoor::VmStopping);
            }

            break;
        }

        // Compatibility path for cooperative schedulers. Fixed-priority
        // builds can disable this unconditional run-queue round trip and rely
        // on explicit blocking/preemption at the stable VM-exit boundary.
        #[cfg(not(feature = "no-vcpu-exit-yield"))]
        {
            if let Some(count) = POST_VMEXIT_YIELD_COUNTS.get(vcpu_id) {
                count.fetch_add(1, Ordering::Relaxed);
            }
            crate::host::task::yield_now();
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}

/// Releases the VM-wide state that only the last vCPU out can release.
///
/// Runs architecture device cleanup, drives the machine to `Stopped`, drops the
/// host running-VM count, and then either hands the VM to a deferred reset or
/// wakes the VMM. Lifecycle failures are recorded rather than propagated: the
/// caller is a vCPU task on its way out and has no one left to report to.
fn finish_vm_stop_from_last_vcpu(
    vm: &VMRef,
    runtime: &VmRuntimeHandle,
    vcpu_id: usize,
    door: VcpuExitDoor,
) {
    let vm_id = vm.id();
    let reset_after_stop = runtime.take_deferred_reset_request();
    info!("VM[{vm_id}] VCpu[{vcpu_id}] last VCpu exiting, decreasing running VM count");

    if let Err(err) = CurrentArch::on_last_vcpu_exit(vm) {
        warn!("VM[{vm_id}] architecture device cleanup failed: {err:?}");
        runtime.record_lifecycle_error(err);
    }
    if let Err(err) = vm.finish_stop_from_last_vcpu(unrecorded_stop_reason(door)) {
        warn!("VM[{vm_id}] finish stop failed: {err:?}");
        runtime.record_lifecycle_error(err);
    } else {
        info!("VM[{vm_id}] state changed to Stopped");
    }

    sub_running_vm_count(1);
    if reset_after_stop {
        spawn_deferred_reset_task(vm_id);
    } else {
        crate::host::task::wait_queue_wake(&super::VMM, 1);
    }
}

/// The reason to record when the machine has not been asked to stop yet.
///
/// Only the `CPU_OFF` door reaches a still-`Running` machine: the guest brought
/// its own last CPU down, which is a guest-initiated system shutdown. The
/// `Stopping` door always finds a reason already recorded by whoever requested
/// the stop, so its value here is an unreachable fallback.
fn unrecorded_stop_reason(door: VcpuExitDoor) -> StopReason {
    match door {
        VcpuExitDoor::CpuOff => StopReason::SystemDown,
        VcpuExitDoor::VmStopping => StopReason::Forced,
    }
}

/// The way a vCPU task leaves [`vcpu_run`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VcpuExitDoor {
    /// The guest turned this vCPU off through PSCI `CPU_OFF`.
    CpuOff,
    /// The vCPU observed the VM-wide `Stopping` state.
    VmStopping,
}

/// Whether any vCPU task of the same VM can still reach a lifecycle check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemainingVcpus {
    /// At least one sibling vCPU is still in its run loop.
    Present,
    /// This task is the last one leaving, and no `CPU_ON` reservation holds a
    /// slot open for a later restart.
    None,
}

/// Lifecycle work a leaving vCPU still owes its VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VcpuExitDuty {
    /// A sibling vCPU will observe the lifecycle state later.
    LeaveToSiblings,
    /// Complete the stop so the VM reaches `Stopped`.
    FinishVmStop,
}

/// Decides what a leaving vCPU still owes its VM.
///
/// Both doors converge once no sibling remains: after the last vCPU task is
/// gone, nothing is left that could observe `Stopping` and complete the
/// transition, so whichever task leaves last has to do it. Listing the doors
/// explicitly keeps this match non-exhaustive if a third door is added, which
/// forces that author to make the same decision deliberately.
fn vcpu_exit_duty(door: VcpuExitDoor, remaining: RemainingVcpus) -> VcpuExitDuty {
    match (door, remaining) {
        (_, RemainingVcpus::Present) => VcpuExitDuty::LeaveToSiblings,
        (VcpuExitDoor::CpuOff | VcpuExitDoor::VmStopping, RemainingVcpus::None) => {
            VcpuExitDuty::FinishVmStop
        }
    }
}

fn poll_primary_vcpu_devices_with(runtime: &VmRuntimeHandle, poll_devices: impl FnOnce()) -> bool {
    let consumed_request = runtime.take_device_poll_request();
    poll_devices();
    consumed_request
}

pub(super) fn poll_vm_devices(vm: &VMRef) {
    poll_vm_input_devices(vm);
    poll_vm_dma_devices(vm);
}

pub(super) fn poll_vm_input_devices(vm: &VMRef) {
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    for device in devices.iter_pollable_dev() {
        if let Err(error) = device.poll(now_ns) {
            warn!("VM[{}] failed to poll virtual device: {error}", vm.id());
        }
    }
}

fn poll_vm_dma_devices(vm: &VMRef) {
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    let now_ns = ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos();
    let mut memory = crate::vm::VmGuestMemoryAccess::new(vm);
    devices.poll_dma_devices(now_ns, &mut memory, |result| {
        if let Err(error) = result {
            warn!("VM[{}] failed to poll DMA virtual device: {error}", vm.id());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_vcpu_leaving_through_cpu_off_finishes_the_vm_stop() {
        // A guest that powers its final CPU down (StarryOS does this when init
        // exits) leaves `vcpu_run` through the `CPU_OFF` door instead of the
        // `Stopping` door. No task remains afterwards to observe `Stopping`,
        // so this door has to complete the stop as well; otherwise the VM is
        // wedged in `Stopping`, where `vm start` is refused and `vm reset`
        // times out, and only a whole-board reset recovers it.
        assert_eq!(
            vcpu_exit_duty(VcpuExitDoor::CpuOff, RemainingVcpus::None),
            VcpuExitDuty::FinishVmStop
        );
    }

    #[test]
    fn last_vcpu_observing_the_stopping_state_finishes_the_vm_stop() {
        assert_eq!(
            vcpu_exit_duty(VcpuExitDoor::VmStopping, RemainingVcpus::None),
            VcpuExitDuty::FinishVmStop
        );
    }

    #[test]
    fn vcpu_leaving_while_siblings_run_does_not_touch_the_vm_lifecycle() {
        for door in [VcpuExitDoor::CpuOff, VcpuExitDoor::VmStopping] {
            assert_eq!(
                vcpu_exit_duty(door, RemainingVcpus::Present),
                VcpuExitDuty::LeaveToSiblings
            );
        }
    }

    #[test]
    fn vcpu_waits_for_runtime_registration_before_entering_guest() {
        assert!(!vcpu_start_is_ready(true, false));
        assert!(vcpu_start_is_ready(true, true));
        assert!(!vcpu_start_is_ready(false, true));
    }

    #[test]
    fn request_published_before_wfi_snapshot_prevents_sleep_and_is_consumed_once() {
        let runtime = Arc::new(VmRuntimeHandle::new());
        let request_published = Arc::new(std::sync::Barrier::new(2));
        let notifier_runtime = runtime.clone();
        let notifier_published = request_published.clone();
        let notifier = std::thread::spawn(move || {
            notifier_runtime.notify_device_poll();
            notifier_published.wait();
        });

        request_published.wait();
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        let wait_count = std::cell::Cell::new(0);
        crate::vm::wait_for_vcpu_event_if_idle(
            &runtime,
            &wait_snapshot,
            || true,
            |_| wait_count.set(wait_count.get() + 1),
        );

        assert_eq!(wait_count.get(), 0);
        let poll_count = std::cell::Cell::new(0);
        let consumed = poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        });

        assert!(consumed);
        assert_eq!(poll_count.get(), 1);
        assert!(!poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        }));
        assert_eq!(poll_count.get(), 2);
        notifier.join().unwrap();
    }

    #[test]
    fn request_published_at_wait_boundary_prevents_sleep_and_is_consumed_once() {
        let runtime = Arc::new(VmRuntimeHandle::new());
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        let wait_boundary_reached = Arc::new(std::sync::Barrier::new(2));
        let request_published = Arc::new(std::sync::Barrier::new(2));
        let notifier_runtime = runtime.clone();
        let notifier_wait_boundary = wait_boundary_reached.clone();
        let notifier_published = request_published.clone();
        let notifier = std::thread::spawn(move || {
            notifier_wait_boundary.wait();
            notifier_runtime.notify_device_poll();
            notifier_published.wait();
        });

        let sleep_count = std::cell::Cell::new(0);
        crate::vm::wait_for_vcpu_event_if_idle(
            &runtime,
            &wait_snapshot,
            || true,
            |wake_condition| {
                wait_boundary_reached.wait();
                request_published.wait();
                if !wake_condition() {
                    sleep_count.set(sleep_count.get() + 1);
                }
            },
        );

        assert_eq!(sleep_count.get(), 0);
        let poll_count = std::cell::Cell::new(0);
        let consumed = poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        });

        assert!(consumed);
        assert_eq!(poll_count.get(), 1);
        assert!(!poll_primary_vcpu_devices_with(&runtime, || {
            poll_count.set(poll_count.get() + 1);
        }));
        assert_eq!(poll_count.get(), 2);
        notifier.join().unwrap();
    }

    #[test]
    fn cpu_on_start_ack_cancel_before_startup_blocks_late_startup() {
        let ack = CpuOnStartAck::new();

        assert!(ack.cancel_before_startup());
        assert!(ack.is_cancelled());
        assert!(!ack.begin_startup());

        ack.complete(Err(ax_err_type!(
            BadState,
            "vCPU 1 CPU_ON startup was cancelled"
        )));

        assert!(ack.is_complete());
        assert!(ack.take_result().unwrap().is_err());
    }
}

//! Temporary qperf-only futex wake stage counters.

use alloc::string::String;
use core::{fmt::Write, sync::atomic::{AtomicU64, Ordering}};

use ax_runtime::hal::time::monotonic_time;

const STAGES: [&str; 5] = ["key_and_hint", "bucket_lock", "collect", "unlock", "wake_batch"];

struct FutexWakeProbe {
    skipped: AtomicU64,
    selected_zero: AtomicU64,
    selected_many: AtomicU64,
    selected_one_coalesced: AtomicU64,
    selected_one_enqueued: AtomicU64,
    stage_total_ns: [AtomicU64; 5],
}

impl FutexWakeProbe {
    const fn new() -> Self {
        Self {
            skipped: AtomicU64::new(0),
            selected_zero: AtomicU64::new(0),
            selected_many: AtomicU64::new(0),
            selected_one_coalesced: AtomicU64::new(0),
            selected_one_enqueued: AtomicU64::new(0),
            stage_total_ns: [const { AtomicU64::new(0) }; 5],
        }
    }
}

static PROBE: FutexWakeProbe = FutexWakeProbe::new();

pub(super) fn now_ns() -> u64 {
    u64::try_from(monotonic_time().as_nanos())
        .expect("platform monotonic clock exceeds the nanosecond representation")
}

pub(super) fn record_skipped() {
    PROBE.skipped.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn record_wake(selected: usize, enqueued: usize, times_ns: [u64; 6]) {
    if selected == 0 {
        PROBE.selected_zero.fetch_add(1, Ordering::Relaxed);
    } else if selected != 1 {
        PROBE.selected_many.fetch_add(1, Ordering::Relaxed);
    } else if enqueued != 1 {
        PROBE.selected_one_coalesced.fetch_add(1, Ordering::Relaxed);
    } else {
        PROBE.selected_one_enqueued.fetch_add(1, Ordering::Relaxed);
        for (total, pair) in PROBE.stage_total_ns.iter().zip(times_ns.windows(2)) {
            total.fetch_add(pair[1].saturating_sub(pair[0]), Ordering::Relaxed);
        }
    }
}

pub(crate) fn render() -> String {
    let mut output = String::new();
    for (name, counter) in [
        ("skipped", &PROBE.skipped),
        ("selected_zero", &PROBE.selected_zero),
        ("selected_many", &PROBE.selected_many),
        ("selected_one_coalesced", &PROBE.selected_one_coalesced),
        ("selected_one_enqueued", &PROBE.selected_one_enqueued),
    ] {
        writeln!(output, "futex_wake_{name} {}", counter.load(Ordering::Relaxed)).unwrap();
    }
    for (name, total) in STAGES.iter().zip(PROBE.stage_total_ns.iter()) {
        writeln!(output, "futex_wake_{name}_total_ns {}", total.load(Ordering::Relaxed))
            .unwrap();
    }
    output
}

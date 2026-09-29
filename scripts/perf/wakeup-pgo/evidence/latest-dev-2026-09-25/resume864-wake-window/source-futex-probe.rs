//! Temporary qperf-only classification of futex wake windows.

use alloc::string::String;
use core::{fmt::Write, sync::atomic::{AtomicU64, Ordering}};

use super::FutexKey;

const KEY_NAMES: [&str; 3] = ["gate", "done", "other"];
const GATE_OFFSET: usize = 16;
const DONE_OFFSET: usize = 20;

struct WindowCounters {
    selected_one_enqueued: AtomicU64,
    any_switch: AtomicU64,
    preempted_switch: AtomicU64,
    multiple_switches: AtomicU64,
}

impl WindowCounters {
    const fn new() -> Self {
        Self {
            selected_one_enqueued: AtomicU64::new(0),
            any_switch: AtomicU64::new(0),
            preempted_switch: AtomicU64::new(0),
            multiple_switches: AtomicU64::new(0),
        }
    }
}

static WINDOWS: [WindowCounters; 3] = [const { WindowCounters::new() }; 3];

pub(super) fn key_slot(key: &FutexKey) -> usize {
    let FutexKey::Private { address, .. } = key else {
        return 2;
    };
    // The frozen handoff_state is mmap-aligned; gate/done are its 16/20-byte words.
    match address & 0xfff {
        GATE_OFFSET => 0,
        DONE_OFFSET => 1,
        _ => 2,
    }
}

pub(super) fn record_window(slot: usize, before: (u64, u64), after: (u64, u64)) {
    let counters = &WINDOWS[slot];
    counters.selected_one_enqueued.fetch_add(1, Ordering::Relaxed);
    let total = after.0.saturating_sub(before.0);
    let preempted = after.1.saturating_sub(before.1);
    if total != 0 {
        counters.any_switch.fetch_add(1, Ordering::Relaxed);
    }
    if preempted != 0 {
        counters.preempted_switch.fetch_add(1, Ordering::Relaxed);
    }
    if total > 1 {
        counters.multiple_switches.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn render() -> String {
    let mut output = String::new();
    for (name, counters) in KEY_NAMES.iter().zip(WINDOWS.iter()) {
        for (field, counter) in [
            ("selected_one_enqueued", &counters.selected_one_enqueued),
            ("any_switch", &counters.any_switch),
            ("preempted_switch", &counters.preempted_switch),
            ("multiple_switches", &counters.multiple_switches),
        ] {
            writeln!(output, "futex_window_{name}_{field} {}", counter.load(Ordering::Relaxed))
                .unwrap();
        }
    }
    output
}

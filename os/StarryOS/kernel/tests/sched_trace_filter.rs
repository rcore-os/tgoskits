#[path = "../src/tracepoint/sched_filter.rs"]
mod sched_filter;

use sched_filter::should_defer_sched_switch;

#[test]
fn disabled_trace_and_worker_transitions_cannot_wake_the_deferred_worker() {
    const SCHED_WORKER: u64 = 7;
    const PIPE_WORKER: u64 = 8;
    const WORKERS: [u64; 2] = [SCHED_WORKER, PIPE_WORKER];

    assert!(!should_defer_sched_switch(false, WORKERS, 1, 2));
    assert!(!should_defer_sched_switch(true, WORKERS, SCHED_WORKER, 2));
    assert!(!should_defer_sched_switch(true, WORKERS, 1, SCHED_WORKER));
    assert!(!should_defer_sched_switch(true, WORKERS, PIPE_WORKER, 2));
    assert!(!should_defer_sched_switch(true, WORKERS, 1, PIPE_WORKER));
    assert!(should_defer_sched_switch(true, WORKERS, 1, 2));
    assert!(should_defer_sched_switch(true, [0, 0], 1, 2));
}

#![no_std]
#![no_main]

use ax_hal as _;
use ax_std as _;
use axtest::prelude::*;

#[axtest]
fn fifo_switch_does_not_derive_a_task_local_rt_quota_deadline() {
    let (has_quota_clock_event, period_before, period_after) =
        ax_task::axtest_fifo_switch_rt_deadline();

    ax_assert!(!has_quota_clock_event);
    ax_assert_eq!(period_before, 100);
    ax_assert_eq!(period_after, period_before);
}

#[axtest]
fn active_rt_bandwidth_reactivation_does_not_sample_the_clock() {
    let (samples, restarted, deadline_preserved) =
        ax_task::axtest_active_rt_bandwidth_reactivation_clock_samples();

    ax_assert_eq!(samples, 1);
    ax_assert!(!restarted);
    ax_assert!(deadline_preserved);
}

#[axtest]
fn unchanged_fifo_effective_key_is_not_republished() {
    let (initial_generation, final_generation) =
        ax_task::axtest_unchanged_fifo_effective_key_generations();

    ax_assert_eq!(final_generation, initial_generation);
}

#[axtest]
fn running_interval_is_committed_only_at_switch_out() {
    let (initial, while_running, after_switch_out) =
        ax_task::axtest_runtime_interval_commit_samples();

    ax_assert_eq!(initial, 0);
    ax_assert_eq!(while_running, 0);
    ax_assert_eq!(after_switch_out, 10);
}

#[axtest]
fn ordinary_runtime_update_does_not_run_the_rr_tick_hook() {
    let (current, peer, update_next, update_request, tick_next, tick_request) =
        ax_task::axtest_rr_runtime_update_outcome();

    ax_assert_eq!(update_next, current);
    ax_assert!(!update_request);
    ax_assert_eq!(tick_next, peer);
    ax_assert!(tick_request);
}

#[axtest]
fn realtime_migration_does_not_maintain_fair_virtual_time() {
    let (before, after) = ax_task::axtest_realtime_migration_fair_virtual_time();

    ax_assert_eq!(after, before);
}

#[axtest]
fn no_switch_ignores_persistent_rt_overload() {
    ax_assert!(ax_task::axtest_no_switch_ignores_persistent_rt_overload());
}

#[axtest]
fn schedule_selection_ignores_persistent_rt_overload() {
    ax_assert!(ax_task::axtest_schedule_selection_ignores_persistent_rt_overload());
}

#[axtest]
fn priority_drop_without_overload_does_not_publish_push_work() {
    let (before, after) = ax_task::axtest_priority_drop_without_overload_push_generations();

    ax_assert_eq!(after, before);
}

#[axtest]
fn clean_push_target_query_does_not_take_root_domain_state_locks() {
    let (pending, acquisitions) = ax_task::axtest_clean_push_target_query_lock_acquisitions();

    ax_assert!(!pending);
    ax_assert_eq!(acquisitions, 0);
}

#[axtest]
fn empty_idle_entry_does_not_run_a_balance_pass() {
    ax_assert!(!ax_task::axtest_empty_idle_entry_balance_pending());
}

#[axtest]
fn lone_yield_reuses_scheduler_deadline() {
    ax_assert!(ax_task::axtest_lone_yield_reuses_scheduler_deadline());
}

#[axtest]
fn balance_callback_preserves_owner_rq_baton() {
    ax_assert!(ax_task::axtest_balance_callback_preserves_owner_rq_baton());
}

#[axtest]
fn pinned_realtime_membership_updates_do_not_scan_the_active_fifo() {
    let (enqueue_active, enqueue_pushable, dequeue_active, dequeue_pushable) =
        ax_task::axtest_pinned_realtime_membership_visits();

    ax_assert_eq!(enqueue_active, 0);
    ax_assert_eq!(enqueue_pushable, 0);
    ax_assert_eq!(dequeue_active, 0);
    ax_assert_eq!(dequeue_pushable, 0);
}

#[axtest]
fn delayed_wake_preserves_linux_lag_after_requeue_placement() {
    let (virtual_lag, nr_running, queued, total_weight) =
        ax_task::axtest_delayed_wake_linux_lag_after_requeue_placement();

    ax_assert_eq!(virtual_lag, -100);
    ax_assert_eq!(nr_running, 3);
    ax_assert_eq!(queued, 3);
    ax_assert_eq!(total_weight, 3 * u64::from(ax_task::Nice::ZERO.weight()));
}

#[axtest]
fn on_cpu_switch_publications_are_linux_style_stores() {
    let (set_next_rmw, set_next_store, finish_rmw, finish_store) =
        ax_task::axtest_on_cpu_publication_kinds();

    ax_assert_eq!(set_next_rmw, 0);
    ax_assert_eq!(set_next_store, 1);
    ax_assert_eq!(finish_rmw, 0);
    ax_assert_eq!(finish_store, 1);
}

#[unsafe(no_mangle)]
fn main() {
    fn print(args: core::fmt::Arguments<'_>) {
        ax_std::print!("{}", args);
    }

    fn wait_for_coverage_extraction() {
        const WAIT_NANOS: u64 = 30_000_000_000;
        let start = ax_hal::time::wall_time_nanos();
        while ax_hal::time::wall_time_nanos().saturating_sub(start) < WAIT_NANOS {
            core::hint::spin_loop();
        }
    }

    axtest::set_coverage_wait_fn(wait_for_coverage_extraction);
    let summary = axtest::init()
        .with_filter(&[
            "effective_schedule_axtest",
            "pinned_realtime_membership_updates_do_not_scan_the_active_fifo",
        ])
        .set_printer(print)
        .run_tests();
    if summary.failed == 0 {
        axtest::dump_coverage();
        axtest_println!("AXTEST_SUITE_OK");
        ax_std::os::arceos::api::sys::ax_terminate();
    }

    panic!("AXTEST_SUITE_FAIL failed={}", summary.failed);
}

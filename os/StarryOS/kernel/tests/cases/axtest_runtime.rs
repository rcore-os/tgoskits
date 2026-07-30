extern crate alloc;

use alloc::{string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn user_stack_layout_is_inside_user_space() {
    ax_assert!(axtest_exports::user_space_base() < axtest_exports::user_stack_top());
    ax_assert!(axtest_exports::user_stack_size() > 0);
    ax_assert!(
        axtest_exports::user_stack_top()
            <= axtest_exports::user_space_base() + axtest_exports::user_space_size()
    );
}

#[axtest]
fn signal_trampoline_is_page_aligned() {
    ax_assert_eq!(axtest_exports::signal_trampoline() & 0xfff, 0);
}

#[axtest]
fn timespec_rejects_invalid_nsec() {
    ax_assert!(axtest_exports::invalid_timespec_is_rejected());
}

#[axtest]
fn random_write_mixes_entropy() {
    ax_assert!(axtest_exports::random_write_mixes_entropy());
}

#[axtest]
fn time_value_conversion_rules_hold() {
    ax_assert!(axtest_exports::time_value_conversion_rules_hold());
}

#[axtest]
fn inactive_task_and_posix_timers_keep_the_fast_gate_closed() {
    ax_assert!(axtest_exports::timer_active_gate_rules_hold());
}

#[axtest]
fn posix_timer_clock_sampling_stays_outside_metadata_lock() {
    ax_assert!(axtest_exports::posix_timer_clock_sampling_rules_hold());
}

#[axtest]
fn posix_timer_timespec_conversion_saturates() {
    ax_assert!(axtest_exports::posix_timer_saturating_timespec_rules_hold());
}

#[axtest]
fn posix_timer_expiry_scans_use_bounded_batches() {
    ax_assert!(axtest_exports::posix_timer_expiry_batch_rules_hold());
}

#[axtest]
fn posix_timer_disarm_suppresses_collected_stale_expiry() {
    ax_assert!(axtest_exports::posix_timer_stale_expiry_signal_is_suppressed());
}

#[axtest]
fn stale_alarm_cancellation_preserves_new_generation() {
    ax_assert!(axtest_exports::alarm_generation_rules_hold());
}

#[axtest]
fn interval_timer_arm_starts_from_current_clock_snapshot() {
    ax_assert!(axtest_exports::interval_timer_arm_uses_current_snapshot());
}

#[axtest]
fn cpu_interval_timers_are_scheduler_tick_driven() {
    ax_assert!(axtest_exports::cpu_interval_timers_avoid_wall_alarms());
}

#[axtest]
fn scheduler_ticks_publish_process_cpu_time_without_sibling_scans() {
    ax_assert!(axtest_exports::scheduler_tick_group_accounting_is_aggregate());
}

#[axtest]
fn scheduler_tick_accounting_excludes_an_active_state_writer() {
    ax_assert!(axtest_exports::scheduler_tick_accounting_excludes_state_writer());
}

#[axtest]
fn futex_empty_wake_op_avoids_entry_allocation() {
    ax_assert!(axtest_exports::futex_empty_wake_op_avoids_entry_allocation());
}

#[axtest]
fn nofault_user_access_rejects_unmapped_word() {
    ax_assert!(axtest_exports::nofault_user_access_rejects_unmapped_word());
}

#[axtest]
fn dummy_stat_fs_fields_match_expected_defaults() {
    ax_assert!(axtest_exports::dummy_stat_fs_fields_match_expected_defaults());
}

#[axtest]
fn perf_control_callback_runs_preemptible() {
    ax_assert!(axtest_exports::perf_control_callback_runs_preemptible());
}

#[axtest]
fn staged_thread_entry_waits_for_activation() {
    let entered = Arc::new(AtomicBool::new(false));
    let entered_by_thread = Arc::clone(&entered);
    let prepared = ax_std::os::arceos::task::prepare_raw(
        move || entered_by_thread.store(true, Ordering::Release),
        String::from("staged-start-gate"),
        64 * 1024,
    )
    .expect("failed to prepare staged test thread");
    let staged = prepared.stage().expect("failed to stage test thread");

    for _ in 0..4 {
        ax_std::os::arceos::task::yield_current_cpu().expect("failed to yield to staged thread");
    }
    ax_assert!(!entered.load(Ordering::Acquire));

    let thread = staged.activate();
    ax_std::os::arceos::task::join_thread(thread).expect("failed to join activated test thread");
    ax_assert!(entered.load(Ordering::Acquire));
}

#[axtest]
fn dropping_staged_thread_aborts_its_entry() {
    let entered = Arc::new(AtomicBool::new(false));
    let entered_by_thread = Arc::clone(&entered);
    let prepared = ax_std::os::arceos::task::prepare_raw(
        move || entered_by_thread.store(true, Ordering::Release),
        String::from("staged-start-abort"),
        64 * 1024,
    )
    .expect("failed to prepare abortable test thread");
    let observer = prepared.thread_handle();
    let staged = prepared.stage().expect("failed to stage abortable thread");

    drop(staged);
    ax_std::os::arceos::task::wait_thread(&observer).expect("aborted staged thread did not exit");
    ax_assert!(!entered.load(Ordering::Acquire));
    ax_std::os::arceos::task::join_thread(observer).expect("failed to reap aborted staged thread");
}

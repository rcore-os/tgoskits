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

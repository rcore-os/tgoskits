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
fn boot_id_formats_firmware_entropy() {
    ax_assert!(axtest_exports::boot_id_formats_firmware_entropy());
}

#[axtest]
fn boot_id_is_omitted_without_trusted_entropy() {
    ax_assert!(axtest_exports::boot_id_is_omitted_without_trusted_entropy());
}

#[axtest]
fn kmsg_reports_no_readiness_without_read_side() {
    ax_assert!(axtest_exports::kmsg_reports_no_readiness_without_read_side());
}

#[axtest]
fn time_value_conversion_rules_hold() {
    ax_assert!(axtest_exports::time_value_conversion_rules_hold());
}

#[axtest]
fn dummy_stat_fs_fields_match_expected_defaults() {
    ax_assert!(axtest_exports::dummy_stat_fs_fields_match_expected_defaults());
}

#[axtest]
fn perf_control_callback_runs_preemptible() {
    ax_assert!(axtest_exports::perf_control_callback_runs_preemptible());
}

#[cfg(target_arch = "aarch64")]
#[axtest]
fn perf_kernel_task_sample_ids_are_empty() {
    ax_assert!(axtest_exports::perf_kernel_task_sample_ids_are_empty());
}

#[axtest]
fn stop_machine_runs_action_and_sync_on_each_cpu() {
    ax_assert!(axtest_exports::stop_machine_runs_action_and_sync_on_each_cpu());
}

#[axtest]
fn smp_log_mailbox_preserves_local_fifo_and_tty_capacity() {
    ax_assert!(axtest_exports::smp_log_mailbox_contract_holds());
}

use axtest::prelude::*;
use starry_kernel::axtest_exports;

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
fn pty_preserves_mouse_escape_reports() {
    ax_assert!(axtest_exports::pty_preserves_mouse_escape_reports());
}

#[axtest]
fn canonical_long_line_drain_continues_past_buf_size() {
    axtest_exports::canonical_long_line_drain_continues_past_buf_size();
}

#[axtest]
fn canonical_echo_is_batched_after_input_progress() {
    axtest_exports::canonical_echo_is_batched_after_input_progress();
}

#[axtest]
fn canonical_echo_can_be_flushed_before_input_is_returned() {
    axtest_exports::canonical_echo_can_be_flushed_before_input_is_returned();
}

#[axtest]
fn canonical_small_echo_respects_sync_limit() {
    axtest_exports::canonical_small_echo_respects_sync_limit();
}

#[axtest]
fn canonical_large_echo_exceeding_sync_limit_is_queued() {
    axtest_exports::canonical_large_echo_exceeding_sync_limit_is_queued();
}

#[axtest]
fn canonical_input_progress_does_not_wait_for_echo_writer() {
    axtest_exports::canonical_input_progress_does_not_wait_for_echo_writer();
}

#[axtest]
fn synchronous_echo_backpressure_queues_unsent_suffix() {
    axtest_exports::synchronous_echo_backpressure_queues_unsent_suffix();
}

#[axtest]
fn injected_input_is_readable_immediately() {
    axtest_exports::injected_input_is_readable_immediately();
}

#[axtest]
fn passive_read_drains_source_before_reporting_peer_eof() {
    axtest_exports::passive_read_drains_source_before_reporting_peer_eof();
}

#[axtest]
fn passive_read_preserves_input_across_partially_full_ring_buffer() {
    axtest_exports::passive_read_preserves_input_across_partially_full_ring_buffer();
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

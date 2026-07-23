use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn bpf_unknown_command_is_invalid() {
    ax_assert!(axtest_exports::bpf_unknown_command_is_invalid());
}

#[axtest]
fn bpf_error_adapter_rules_hold() {
    ax_assert!(axtest_exports::bpf_error_adapter_rules_hold());
}

#[axtest]
fn posix_timer_clock_validation_rules_hold() {
    ax_assert!(axtest_exports::posix_timer_clock_validation_rules_hold());
}

#[axtest]
fn itimer_type_signo_and_time_conversion_rules_hold() {
    ax_assert!(axtest_exports::itimer_type_signo_and_time_conversion_rules_hold());
}

#[axtest]
fn signal_sigset_size_and_signo_validation_rules_hold() {
    ax_assert!(axtest_exports::signal_sigset_size_and_signo_validation_rules_hold());
}

#[axtest]
fn signal_sigset_and_signo_validation_rules_hold() {
    ax_assert!(axtest_exports::signal_sigset_and_signo_validation_rules_hold());
}

#[axtest]
fn io_rwf_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::io_rwf_flags_validation_rules_hold());
}

#[axtest]
fn uid_valid_and_syslog_validation_rules_hold() {
    ax_assert!(axtest_exports::uid_valid_and_syslog_validation_rules_hold());
}

#[axtest]
fn credential_capability_rules_hold() {
    ax_assert!(axtest_exports::credential_capability_rules_hold());
}

#[axtest]
fn resource_limit_defaults_hold() {
    ax_assert!(axtest_exports::resource_limit_defaults_hold());
}

#[axtest]
fn seccomp_filter_rules_hold() {
    ax_assert!(axtest_exports::seccomp_filter_rules_hold());
}

#[axtest]
fn seccomp_filter_construction_rules_hold() {
    ax_assert!(axtest_exports::seccomp_filter_construction_rules_hold());
}

#[axtest]
fn rseq_validation_rejects_invalid_arguments() {
    ax_assert!(axtest_exports::rseq_validation_rejects_invalid_arguments());
}

#[axtest]
fn rseq_validation_rules_hold() {
    ax_assert!(axtest_exports::rseq_validation_rules_hold());
}

#[axtest]
fn membarrier_validation_rules_hold() {
    ax_assert!(axtest_exports::membarrier_validation_rules_hold());
}

#[axtest]
fn mempolicy_validation_rules_hold() {
    ax_assert!(axtest_exports::mempolicy_validation_rules_hold());
}

#[axtest]
fn task_clone_validation_rules_hold() {
    ax_assert!(axtest_exports::task_clone_validation_rules_hold());
}

#[axtest]
fn capability_data_conversion_rules_hold() {
    ax_assert!(axtest_exports::capability_data_conversion_rules_hold());
}

#[axtest]
fn cmsg_alignment_and_space_rules_hold() {
    ax_assert!(axtest_exports::cmsg_alignment_and_space_rules_hold());
}

#[axtest]
fn seccomp_action_and_precedence_rules_hold() {
    ax_assert!(axtest_exports::seccomp_action_and_precedence_rules_hold());
}

#[axtest]
fn syscall_signal_restart_rules_hold() {
    ax_assert!(axtest_exports::syscall_signal_restart_rules_hold());
}

#[axtest]
fn futex_op_and_compare_rules_hold() {
    ax_assert!(axtest_exports::futex_op_and_compare_rules_hold());
}

#[axtest]
fn mmap_capped_device_map_len_rules_hold() {
    ax_assert!(axtest_exports::mmap_capped_device_map_len_rules_hold());
}

#[axtest]
fn aio_iocb_validation_rules_hold() {
    ax_assert!(axtest_exports::aio_iocb_validation_rules_hold());
}

#[axtest]
fn decode_wait_status_rules_hold() {
    ax_assert!(axtest_exports::decode_wait_status_rules_hold());
}

#[axtest]
fn xattr_name_and_value_validation_rules_hold() {
    ax_assert!(axtest_exports::xattr_name_and_value_validation_rules_hold());
}

#[axtest]
fn eventfd_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::eventfd_flags_validation_rules_hold());
}

#[axtest]
fn signalfd_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::signalfd_flags_validation_rules_hold());
}

#[axtest]
fn pidfd_flags_and_signal_validation_rules_hold() {
    ax_assert!(axtest_exports::pidfd_flags_and_signal_validation_rules_hold());
}

#[axtest]
fn timerfd_timespec_conversion_rules_hold() {
    ax_assert!(axtest_exports::timerfd_timespec_conversion_rules_hold());
}

#[axtest]
fn inotify_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::inotify_flags_validation_rules_hold());
}

#[axtest]
fn pipe_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::pipe_flags_validation_rules_hold());
}

#[axtest]
fn stat_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::stat_flags_validation_rules_hold());
}

#[axtest]
fn memfd_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::memfd_flags_validation_rules_hold());
}

#[axtest]
fn mount_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::mount_flags_validation_rules_hold());
}

#[axtest]
fn io_offset_from_hilo_rules_hold() {
    ax_assert!(axtest_exports::io_offset_from_hilo_rules_hold());
}

#[axtest]
fn io_uring_round_ring_entries_rules_hold() {
    ax_assert!(axtest_exports::io_uring_round_ring_entries_rules_hold());
}

#[axtest]
fn fd_ops_flags_to_options_rules_hold() {
    ax_assert!(axtest_exports::fd_ops_flags_to_options_rules_hold());
}

#[axtest]
fn mincore_validation_rules_hold() {
    ax_assert!(axtest_exports::mincore_validation_rules_hold());
}

#[axtest]
fn time_clock_id_validation_rules_hold() {
    ax_assert!(axtest_exports::time_clock_id_validation_rules_hold());
}

#[axtest]
fn exit_code_encoding_rules_hold() {
    ax_assert!(axtest_exports::exit_code_encoding_rules_hold());
}

#[axtest]
fn job_setpgid_validation_rules_hold() {
    ax_assert!(axtest_exports::job_setpgid_validation_rules_hold());
}

#[axtest]
fn schedule_clock_and_sched_validation_rules_hold() {
    ax_assert!(axtest_exports::schedule_clock_and_sched_validation_rules_hold());
}

#[axtest]
fn thread_arch_prctl_code_rules_hold() {
    ax_assert!(axtest_exports::thread_arch_prctl_code_rules_hold());
}

#[axtest]
fn resources_rlimit_validation_rules_hold() {
    ax_assert!(axtest_exports::resources_rlimit_validation_rules_hold());
}

#[axtest]
fn kmod_flags_validation_rules_hold() {
    ax_assert!(axtest_exports::kmod_flags_validation_rules_hold());
}

#[axtest]
fn sys_constants_and_validation_rules_hold() {
    ax_assert!(axtest_exports::sys_constants_and_validation_rules_hold());
}

#[axtest]
fn select_fd_set_and_validation_rules_hold() {
    ax_assert!(axtest_exports::select_fd_set_and_validation_rules_hold());
}

#[axtest]
fn poll_nfds_validation_rules_hold() {
    ax_assert!(axtest_exports::poll_nfds_validation_rules_hold());
}

#[axtest]
fn epoll_validation_rules_hold() {
    ax_assert!(axtest_exports::epoll_validation_rules_hold());
}

#[axtest]
fn ipc_permission_and_constants_rules_hold() {
    ax_assert!(axtest_exports::ipc_permission_and_constants_rules_hold());
}

#[axtest]
fn net_addr_conversion_rules_hold() {
    ax_assert!(axtest_exports::net_addr_conversion_rules_hold());
}

#[axtest]
fn ctl_ioctl_constants_hold() {
    ax_assert!(axtest_exports::ctl_ioctl_constants_hold());
}

#[axtest]
fn net_optNormalization_rules_hold() {
    ax_assert!(axtest_exports::net_optNormalization_rules_hold());
}
#[axtest]
fn net_io_constants_hold() {
    ax_assert!(axtest_exports::net_io_constants_hold());
}

#[axtest]
fn net_socket_constants_hold() {
    ax_assert!(axtest_exports::net_socket_constants_hold());
}

#[axtest]
fn seccomp_bpf_constants_hold() {
    ax_assert!(axtest_exports::seccomp_bpf_constants_hold());
}

#[axtest]
fn rss_kind_and_accounting_rules_hold() {
    ax_assert!(axtest_exports::rss_kind_and_accounting_rules_hold());
}
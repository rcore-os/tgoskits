//! Narrow test-only exports for kernel axtest targets.

pub fn user_space_base() -> usize {
    super::config::USER_SPACE_BASE
}

pub fn user_space_size() -> usize {
    super::config::USER_SPACE_SIZE
}

pub fn user_stack_top() -> usize {
    super::config::USER_STACK_TOP
}

pub fn user_stack_size() -> usize {
    super::config::USER_STACK_SIZE
}

pub fn signal_trampoline() -> usize {
    super::config::SIGNAL_TRAMPOLINE
}

pub fn invalid_timespec_is_rejected() -> bool {
    use super::time::TimeValueLike;

    let invalid = linux_raw_sys::general::__kernel_timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000_000,
    };
    invalid.try_into_time_value().is_err()
}

pub fn random_write_mixes_entropy() -> bool {
    super::pseudofs::dev::random_write_mixes_entropy_for_test()
}

pub fn pipe_peer_close_with_multiple_readers_is_visible() -> bool {
    super::file::peer_close_with_multiple_readers_is_visible_for_test()
}

pub fn pipe_resize_rejects_oversized_pipe() -> bool {
    super::file::resize_rejects_oversized_pipe_for_test()
}

pub fn fcntl_setpipe_size_returns_capacity() -> bool {
    super::syscall::fcntl_setpipe_size_returns_capacity_for_test()
}

pub fn private_mmap_rejects_fault_at_file_eof() -> bool {
    super::mm::private_mmap_eof_check_for_test()
}

pub fn cow_file_max_read_len_boundary_rules_hold() -> bool {
    super::mm::cow_file_max_read_len_boundary_rules_hold_for_test()
}

pub fn concurrent_epoll_reverse_add_is_serialized() -> bool {
    super::file::concurrent_reverse_add_is_serialized_for_test()
}

pub fn process_mem_stats_formats_linux_fields() -> bool {
    use super::mm::ProcessMemStats;

    let stats = ProcessMemStats {
        vss_pages: 256,
        text_pages: 10,
        data_pages: 64,
        stack_pages: 32,
        exe_pages: 16,
        resident_pages: 48,
        rss_anon_pages: 40,
        rss_file_pages: 4,
        rss_shmem_pages: 4,
        hiwater_rss_pages: 48,
        peak_pages: 512,
        ..Default::default()
    };

    stats.format_statm() == "256 48 8 10 0 64 0\n"
        && stats.vsize_bytes() == 256 * 4096
        && stats.rss_pages() == 48
        && {
            let status = stats.format_status_vm_lines();
            status.contains("VmPeak:\t2048 kB\n")
                && status.contains("VmSize:\t1024 kB\n")
                && status.contains("VmHWM:\t192 kB\n")
                && status.contains("VmRSS:\t192 kB\n")
                && status.contains("RssAnon:\t160 kB\n")
                && status.contains("RssFile:\t16 kB\n")
                && status.contains("RssShmem:\t16 kB\n")
                && status.contains("VmData:\t256 kB\n")
                && status.contains("VmStk:\t128 kB\n")
                && status.contains("VmExe:\t64 kB\n")
        }
}

pub fn memory_accounting_tracks_cow_charge_transitions() -> bool {
    use ax_memory_addr::VirtAddr;

    use super::mm::{MemoryAccounting, RssKind};

    let acct = MemoryAccounting::new();
    let file_page = VirtAddr::from(0x1000usize);
    let moved_page = VirtAddr::from(0x2000usize);

    acct.record_charge(file_page, RssKind::File).is_ok()
        && acct.rss_file_pages() == 1
        && acct.cow_file_write_to_anon(file_page)
        && acct.rss_file_pages() == 0
        && acct.rss_anon_pages() == 1
        && acct.move_charge(file_page, moved_page).is_ok()
        && acct.charge_entries() == alloc::vec![(moved_page, RssKind::Anon)]
        && acct.remove_charge(moved_page) == Some(RssKind::Anon)
        && acct.rss_total_pages() == 0
}

pub fn memory_accounting_rejects_duplicate_and_conflicting_charges() -> bool {
    use ax_memory_addr::VirtAddr;

    use super::mm::{MemoryAccounting, RssKind};

    let acct = MemoryAccounting::new();
    let src = VirtAddr::from(0x3000usize);
    let dst = VirtAddr::from(0x4000usize);
    let orphan = VirtAddr::from(0x5000usize);

    acct.record_charge(src, RssKind::File).is_ok()
        && acct.record_charge(src, RssKind::Anon).is_err()
        && acct.record_charge(dst, RssKind::Shmem).is_ok()
        && acct.move_charge(src, dst).is_err()
        && acct.charge_entries().contains(&(src, RssKind::File))
        && acct.charge_entries().contains(&(dst, RssKind::Shmem))
        && acct.adopt_cow_write_as_anon(orphan).is_ok()
        && acct.charge_entries().contains(&(orphan, RssKind::Anon))
        && acct.rss_anon_pages() == 1
}

pub fn accounting_edge_cases_and_snapshot_rules_hold() -> bool {
    super::mm::accounting_edge_cases_and_snapshot_rules_hold_for_test()
}

pub fn rss_kind_and_accounting_rules_hold() -> bool {
    super::mm::rss_kind_and_accounting_rules_hold_for_test()
}

pub fn accounting_rss_kind_debug_and_default_hold() -> bool {
    super::mm::accounting_rss_kind_debug_and_default_hold_for_test()
}

pub fn process_vm_stat_watermarks_hold() -> bool {
    super::mm::process_vm_stat_watermarks_hold_for_test()
}

pub fn process_vm_stat_edge_cases_hold() -> bool {
    super::mm::process_vm_stat_edge_cases_hold_for_test()
}

pub fn user_pointer_metadata_rules_hold() -> bool {
    super::mm::user_pointer_metadata_rules_hold_for_test()
}

pub fn time_value_conversion_rules_hold() -> bool {
    super::time::time_value_conversion_rules_hold_for_test()
}

pub fn credential_capability_rules_hold() -> bool {
    super::task::credential_capability_rules_hold_for_test()
}

pub fn resource_limit_defaults_hold() -> bool {
    super::task::resource_limit_defaults_hold_for_test()
}

pub fn posix_timer_clock_validation_rules_hold() -> bool {
    super::task::posix_timer_clock_validation_rules_hold_for_test()
}

pub fn itimer_type_signo_and_time_conversion_rules_hold() -> bool {
    super::task::itimer_type_signo_and_time_conversion_rules_hold_for_test()
}

pub fn seccomp_filter_rules_hold() -> bool {
    super::task::seccomp_filter_rules_hold_for_test()
}

pub fn rseq_validation_rejects_invalid_arguments() -> bool {
    super::syscall::rseq_validation_rejects_invalid_arguments_for_test()
}

pub fn membarrier_validation_rules_hold() -> bool {
    super::syscall::membarrier_validation_rules_hold_for_test()
}

pub fn signal_sigset_size_and_signo_validation_rules_hold() -> bool {
    super::syscall::signal_sigset_size_and_signo_validation_rules_hold_for_test()
}

pub fn signal_sigset_and_signo_validation_rules_hold() -> bool {
    super::syscall::signal_sigset_and_signo_validation_rules_hold_for_test()
}

pub fn io_rwf_flags_validation_rules_hold() -> bool {
    super::syscall::io_rwf_flags_validation_rules_hold_for_test()
}

pub fn metadata_to_kstat_conversion_rules_hold() -> bool {
    super::file::metadata_to_kstat_conversion_rules_hold_for_test()
}

pub fn uid_valid_and_syslog_validation_rules_hold() -> bool {
    super::syscall::uid_valid_and_syslog_validation_rules_hold_for_test()
}

pub fn mempolicy_validation_rules_hold() -> bool {
    super::syscall::mempolicy_validation_rules_hold_for_test()
}

pub fn task_clone_validation_rules_hold() -> bool {
    super::syscall::task_clone_validation_rules_hold_for_test()
}

pub fn proc_formatting_contracts_hold() -> bool {
    super::pseudofs::proc::formatting_contracts_hold_for_test()
}

pub fn proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout() -> bool {
    super::pseudofs::proc::proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout_for_test()
}

pub fn bpf_unknown_command_is_invalid() -> bool {
    super::ebpf::bpf_unknown_command_is_invalid_for_test()
}

pub fn bpf_error_adapter_rules_hold() -> bool {
    super::ebpf::bpf_error_adapter_rules_hold_for_test()
}

pub fn bpf_error_more_variants_and_edge_cases_hold() -> bool {
    super::ebpf::bpf_error_more_variants_and_edge_cases_hold_for_test()
}

pub fn pipe_resize_rounding_and_state_rules_hold() -> bool {
    super::file::pipe_resize_rounding_and_state_rules_hold_for_test()
}

pub fn epoll_event_matching_rules_hold() -> bool {
    super::file::epoll_event_matching_rules_hold_for_test()
}

pub fn stats_classify_and_accumulate_rules_hold() -> bool {
    super::mm::stats_classify_and_accumulate_rules_hold_for_test()
}

pub fn capability_data_conversion_rules_hold() -> bool {
    super::syscall::capability_data_conversion_rules_hold_for_test()
}

pub fn pipe_size_rounding_and_rejection_rules_hold() -> bool {
    super::syscall::pipe_size_rounding_and_rejection_rules_hold_for_test()
}

pub fn seccomp_filter_construction_rules_hold() -> bool {
    super::task::seccomp_filter_construction_rules_hold_for_test()
}

pub fn push_topology_item_preserves_order_and_grows_capacity() -> bool {
    super::file::push_topology_item_preserves_order_and_grows_capacity()
}

pub fn epoll_edge_id_and_constants_hold() -> bool {
    super::file::epoll_edge_id_and_constants_hold_for_test()
}

pub fn epoll_topology_struct_and_methods_hold() -> bool {
    super::file::epoll_topology_struct_and_methods_hold_for_test()
}

pub fn epoll_topology_direction_and_scan_hold() -> bool {
    super::file::epoll_topology_direction_and_scan_hold_for_test()
}

pub fn epoll_edge_id_clone_copy_partial_eq_hold() -> bool {
    super::file::epoll_edge_id_clone_copy_partial_eq_hold_for_test()
}

pub fn epoll_topology_static_constants_hold() -> bool {
    super::file::epoll_topology_static_constants_hold_for_test()
}

pub fn epoll_topology_link_clone_hold() -> bool {
    super::file::epoll_topology_link_clone_hold_for_test()
}

pub fn epoll_topology_vec_and_reserve_hold() -> bool {
    super::file::epoll_topology_vec_and_reserve_hold_for_test()
}

pub fn epoll_arc_operations_hold() -> bool {
    super::file::epoll_arc_operations_hold_for_test()
}

pub fn dummy_stat_fs_fields_match_expected_defaults() -> bool {
    super::pseudofs::dummy_stat_fs_fields_match_expected_defaults_for_test()
}

pub fn is_wext_ioctl_validation_rules_hold() -> bool {
    super::file::is_wext_ioctl_validation_rules_hold_for_test()
}

pub fn cmsg_alignment_and_space_rules_hold() -> bool {
    super::syscall::cmsg_alignment_and_space_rules_hold_for_test()
}

pub fn seccomp_action_and_precedence_rules_hold() -> bool {
    super::task::seccomp_action_and_precedence_rules_hold_for_test()
}

pub fn seccomp_bpf_constants_hold() -> bool {
    super::task::seccomp_bpf_constants_hold_for_test()
}

pub fn syscall_signal_restart_rules_hold() -> bool {
    super::syscall::syscall_signal_restart_rules_hold_for_test()
}

pub fn futex_op_and_compare_rules_hold() -> bool {
    super::syscall::futex_op_and_compare_rules_hold_for_test()
}

pub fn mmap_capped_device_map_len_rules_hold() -> bool {
    super::syscall::mmap_capped_device_map_len_rules_hold_for_test()
}

pub fn aio_iocb_validation_rules_hold() -> bool {
    super::syscall::aio_iocb_validation_rules_hold_for_test()
}

pub fn decode_wait_status_rules_hold() -> bool {
    super::task::decode_wait_status_rules_hold_for_test()
}

pub fn xattr_name_and_value_validation_rules_hold() -> bool {
    super::syscall::xattr_name_and_value_validation_rules_hold_for_test()
}

pub fn eventfd_flags_validation_rules_hold() -> bool {
    super::syscall::eventfd_flags_validation_rules_hold_for_test()
}

pub fn signalfd_flags_validation_rules_hold() -> bool {
    super::syscall::signalfd_flags_validation_rules_hold_for_test()
}

pub fn pidfd_flags_and_signal_validation_rules_hold() -> bool {
    super::syscall::pidfd_flags_and_signal_validation_rules_hold_for_test()
}

pub fn timerfd_timespec_conversion_rules_hold() -> bool {
    super::syscall::timerfd_timespec_conversion_rules_hold_for_test()
}

pub fn inotify_flags_validation_rules_hold() -> bool {
    super::syscall::inotify_flags_validation_rules_hold_for_test()
}

pub fn pipe_flags_validation_rules_hold() -> bool {
    super::syscall::pipe_flags_validation_rules_hold_for_test()
}

pub fn stat_flags_validation_rules_hold() -> bool {
    super::syscall::stat_flags_validation_rules_hold_for_test()
}

pub fn memfd_flags_validation_rules_hold() -> bool {
    super::syscall::memfd_flags_validation_rules_hold_for_test()
}

pub fn mount_flags_validation_rules_hold() -> bool {
    super::syscall::mount_flags_validation_rules_hold_for_test()
}

pub fn io_offset_from_hilo_rules_hold() -> bool {
    super::syscall::io_offset_from_hilo_rules_hold_for_test()
}

pub fn io_uring_round_ring_entries_rules_hold() -> bool {
    super::syscall::io_uring_round_ring_entries_rules_hold_for_test()
}

pub fn fd_ops_flags_to_options_rules_hold() -> bool {
    super::syscall::fd_ops_flags_to_options_rules_hold_for_test()
}

pub fn rseq_validation_rules_hold() -> bool {
    super::syscall::rseq_validation_rules_hold_for_test()
}

pub fn mincore_validation_rules_hold() -> bool {
    super::syscall::mincore_validation_rules_hold_for_test()
}

pub fn time_clock_id_validation_rules_hold() -> bool {
    super::syscall::time_clock_id_validation_rules_hold_for_test()
}

pub fn exit_code_encoding_rules_hold() -> bool {
    super::syscall::exit_code_encoding_rules_hold_for_test()
}

pub fn job_setpgid_validation_rules_hold() -> bool {
    super::syscall::job_setpgid_validation_rules_hold_for_test()
}

pub fn schedule_clock_and_sched_validation_rules_hold() -> bool {
    super::syscall::schedule_clock_and_sched_validation_rules_hold_for_test()
}

pub fn thread_arch_prctl_code_rules_hold() -> bool {
    super::syscall::thread_arch_prctl_code_rules_hold_for_test()
}

pub fn resources_rlimit_validation_rules_hold() -> bool {
    super::syscall::resources_rlimit_validation_rules_hold_for_test()
}

pub fn kmod_flags_validation_rules_hold() -> bool {
    super::syscall::kmod_flags_validation_rules_hold_for_test()
}

pub fn sys_constants_and_validation_rules_hold() -> bool {
    super::syscall::sys_constants_and_validation_rules_hold_for_test()
}

pub fn select_fd_set_and_validation_rules_hold() -> bool {
    super::syscall::select_fd_set_and_validation_rules_hold_for_test()
}

pub fn poll_nfds_validation_rules_hold() -> bool {
    super::syscall::poll_nfds_validation_rules_hold_for_test()
}

pub fn epoll_validation_rules_hold() -> bool {
    super::syscall::epoll_validation_rules_hold_for_test()
}

pub fn ipc_permission_and_constants_rules_hold() -> bool {
    super::syscall::ipc_permission_and_constants_rules_hold_for_test()
}

pub fn net_addr_conversion_rules_hold() -> bool {
    super::syscall::net_addr_conversion_rules_hold_for_test()
}

pub fn ctl_ioctl_constants_hold() -> bool {
    super::syscall::ctl_ioctl_constants_hold_for_test()
}

pub fn net_optNormalization_rules_hold() -> bool {
    super::syscall::net_optNormalization_rules_hold_for_test()
}

pub fn net_io_constants_hold() -> bool {
    super::syscall::net_io_constants_hold_for_test()
}

pub fn net_socket_constants_hold() -> bool {
    super::syscall::net_socket_constants_hold_for_test()
}

pub fn buddy_slab_page_constants_and_header_helpers_hold() -> bool {
    super::buddy_slab_page_constants_and_header_helpers_hold_for_test()
}

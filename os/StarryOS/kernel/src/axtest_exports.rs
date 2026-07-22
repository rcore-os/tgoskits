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

pub fn process_vm_stat_watermarks_hold() -> bool {
    super::mm::process_vm_stat_watermarks_hold_for_test()
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

pub fn seccomp_filter_rules_hold() -> bool {
    super::task::seccomp_filter_rules_hold_for_test()
}

pub fn rseq_validation_rejects_invalid_arguments() -> bool {
    super::syscall::rseq_validation_rejects_invalid_arguments_for_test()
}

pub fn membarrier_validation_rules_hold() -> bool {
    super::syscall::membarrier_validation_rules_hold_for_test()
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

pub fn dummy_stat_fs_fields_match_expected_defaults() -> bool {
    super::pseudofs::dummy_stat_fs_fields_match_expected_defaults_for_test()
}

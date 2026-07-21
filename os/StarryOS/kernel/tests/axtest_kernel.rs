#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use ax_std as _;
use starry_kernel::axtest_exports;

#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    use super::axtest_exports;

    #[test]
    fn user_stack_layout_is_inside_user_space() {
        ax_assert!(axtest_exports::user_space_base() < axtest_exports::user_stack_top());
        ax_assert!(axtest_exports::user_stack_size() > 0);
        ax_assert!(
            axtest_exports::user_stack_top()
                <= axtest_exports::user_space_base() + axtest_exports::user_space_size()
        );
    }

    #[test]
    fn signal_trampoline_is_page_aligned() {
        ax_assert_eq!(axtest_exports::signal_trampoline() & 0xfff, 0);
    }

    #[test]
    fn timespec_rejects_invalid_nsec() {
        ax_assert!(axtest_exports::invalid_timespec_is_rejected());
    }

    #[test]
    fn random_write_mixes_entropy() {
        ax_assert!(axtest_exports::random_write_mixes_entropy());
    }

    #[test]
    fn pipe_peer_close_with_multiple_readers_is_visible() {
        ax_assert!(axtest_exports::pipe_peer_close_with_multiple_readers_is_visible());
    }

    #[test]
    fn pipe_resize_rejects_oversized_pipe() {
        ax_assert!(axtest_exports::pipe_resize_rejects_oversized_pipe());
    }

    #[test]
    fn fcntl_setpipe_size_returns_capacity() {
        ax_assert!(axtest_exports::fcntl_setpipe_size_returns_capacity());
    }

    #[test]
    fn private_mmap_rejects_fault_at_file_eof() {
        ax_assert!(axtest_exports::private_mmap_rejects_fault_at_file_eof());
    }

    #[test]
    fn concurrent_epoll_reverse_add_is_serialized() {
        ax_assert!(axtest_exports::concurrent_epoll_reverse_add_is_serialized());
    }

    #[test]
    fn process_mem_stats_formats_linux_fields() {
        ax_assert!(axtest_exports::process_mem_stats_formats_linux_fields());
    }

    #[test]
    fn memory_accounting_tracks_cow_charge_transitions() {
        ax_assert!(axtest_exports::memory_accounting_tracks_cow_charge_transitions());
    }

    #[test]
    fn memory_accounting_rejects_duplicate_and_conflicting_charges() {
        ax_assert!(axtest_exports::memory_accounting_rejects_duplicate_and_conflicting_charges());
    }

    #[test]
    fn process_vm_stat_watermarks_hold() {
        ax_assert!(axtest_exports::process_vm_stat_watermarks_hold());
    }

    #[test]
    fn user_pointer_metadata_rules_hold() {
        ax_assert!(axtest_exports::user_pointer_metadata_rules_hold());
    }

    #[test]
    fn bpf_unknown_command_is_invalid() {
        ax_assert!(axtest_exports::bpf_unknown_command_is_invalid());
    }

    #[test]
    fn credential_capability_rules_hold() {
        ax_assert!(axtest_exports::credential_capability_rules_hold());
    }

    #[test]
    fn resource_limit_defaults_hold() {
        ax_assert!(axtest_exports::resource_limit_defaults_hold());
    }

    #[test]
    fn seccomp_filter_rules_hold() {
        ax_assert!(axtest_exports::seccomp_filter_rules_hold());
    }

    #[test]
    fn time_value_conversion_rules_hold() {
        ax_assert!(axtest_exports::time_value_conversion_rules_hold());
    }

    #[test]
    fn rseq_validation_rejects_invalid_arguments() {
        ax_assert!(axtest_exports::rseq_validation_rejects_invalid_arguments());
    }

    #[test]
    fn membarrier_validation_rules_hold() {
        ax_assert!(axtest_exports::membarrier_validation_rules_hold());
    }

    #[test]
    fn mempolicy_validation_rules_hold() {
        ax_assert!(axtest_exports::mempolicy_validation_rules_hold());
    }

    #[test]
    fn task_clone_validation_rules_hold() {
        ax_assert!(axtest_exports::task_clone_validation_rules_hold());
    }

    #[test]
    fn proc_formatting_contracts_hold() {
        ax_assert!(axtest_exports::proc_formatting_contracts_hold());
    }

    #[test]
    fn proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout() {
        ax_assert!(axtest_exports::proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout());
    }
}

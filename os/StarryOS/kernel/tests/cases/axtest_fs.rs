use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn pipe_peer_close_with_multiple_readers_is_visible() {
    ax_assert!(axtest_exports::pipe_peer_close_with_multiple_readers_is_visible());
}

#[axtest]
fn pipe_resize_rejects_oversized_pipe() {
    ax_assert!(axtest_exports::pipe_resize_rejects_oversized_pipe());
}

#[axtest]
fn fcntl_setpipe_size_returns_capacity() {
    ax_assert!(axtest_exports::fcntl_setpipe_size_returns_capacity());
}

#[axtest]
fn private_mmap_rejects_fault_at_file_eof() {
    ax_assert!(axtest_exports::private_mmap_rejects_fault_at_file_eof());
}

#[axtest]
fn concurrent_epoll_reverse_add_is_serialized() {
    ax_assert!(axtest_exports::concurrent_epoll_reverse_add_is_serialized());
}

#[axtest]
fn pipe_resize_rounding_and_state_rules_hold() {
    ax_assert!(axtest_exports::pipe_resize_rounding_and_state_rules_hold());
}

#[axtest]
fn epoll_event_matching_rules_hold() {
    ax_assert!(axtest_exports::epoll_event_matching_rules_hold());
}

#[axtest]
fn push_topology_item_preserves_order_and_grows_capacity() {
    ax_assert!(axtest_exports::push_topology_item_preserves_order_and_grows_capacity());
}

#[axtest]
fn epoll_edge_id_and_constants_hold() {
    ax_assert!(axtest_exports::epoll_edge_id_and_constants_hold());
}

#[axtest]
fn epoll_topology_struct_and_methods_hold() {
    ax_assert!(axtest_exports::epoll_topology_struct_and_methods_hold());
}

#[axtest]
fn epoll_topology_direction_and_scan_hold() {
    ax_assert!(axtest_exports::epoll_topology_direction_and_scan_hold());
}

#[axtest]
fn epoll_edge_id_clone_copy_partial_eq_hold() {
    ax_assert!(axtest_exports::epoll_edge_id_clone_copy_partial_eq_hold());
}

#[axtest]
fn epoll_topology_static_constants_hold() {
    ax_assert!(axtest_exports::epoll_topology_static_constants_hold());
}

#[axtest]
fn epoll_topology_link_clone_hold() {
    ax_assert!(axtest_exports::epoll_topology_link_clone_hold());
}

#[axtest]
fn epoll_topology_vec_and_reserve_hold() {
    ax_assert!(axtest_exports::epoll_topology_vec_and_reserve_hold());
}

#[axtest]
fn epoll_arc_operations_hold() {
    ax_assert!(axtest_exports::epoll_arc_operations_hold());
}

#[axtest]
fn proc_formatting_contracts_hold() {
    ax_assert!(axtest_exports::proc_formatting_contracts_hold());
}

#[axtest]
fn proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout() {
    ax_assert!(axtest_exports::proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout());
}

#[axtest]
fn capability_data_conversion_rules_hold() {
    ax_assert!(axtest_exports::capability_data_conversion_rules_hold());
}

#[axtest]
fn pipe_size_rounding_and_rejection_rules_hold() {
    ax_assert!(axtest_exports::pipe_size_rounding_and_rejection_rules_hold());
}

#[axtest]
fn metadata_to_kstat_conversion_rules_hold() {
    ax_assert!(axtest_exports::metadata_to_kstat_conversion_rules_hold());
}

#[axtest]
fn is_wext_ioctl_validation_rules_hold() {
    ax_assert!(axtest_exports::is_wext_ioctl_validation_rules_hold());
}

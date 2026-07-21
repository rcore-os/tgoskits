use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest::def_test]
fn pipe_peer_close_with_multiple_readers_is_visible() {
    ax_assert!(axtest_exports::pipe_peer_close_with_multiple_readers_is_visible());
}

#[axtest::def_test]
fn pipe_resize_rejects_oversized_pipe() {
    ax_assert!(axtest_exports::pipe_resize_rejects_oversized_pipe());
}

#[axtest::def_test]
fn fcntl_setpipe_size_returns_capacity() {
    ax_assert!(axtest_exports::fcntl_setpipe_size_returns_capacity());
}

#[axtest::def_test]
fn private_mmap_rejects_fault_at_file_eof() {
    ax_assert!(axtest_exports::private_mmap_rejects_fault_at_file_eof());
}

#[axtest::def_test]
fn concurrent_epoll_reverse_add_is_serialized() {
    ax_assert!(axtest_exports::concurrent_epoll_reverse_add_is_serialized());
}

#[axtest::def_test]
fn proc_formatting_contracts_hold() {
    ax_assert!(axtest_exports::proc_formatting_contracts_hold());
}

#[axtest::def_test]
fn proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout() {
    ax_assert!(axtest_exports::proc_bus_usb_devices_snapshot_matches_busybox_lsusb_layout());
}

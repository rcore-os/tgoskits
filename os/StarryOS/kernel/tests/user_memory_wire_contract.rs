//! User-memory copy APIs must express both input-validity and output-byte invariants.

const ACCESS: &str = include_str!("../src/mm/access.rs");
const FS_IO: &str = include_str!("../src/syscall/fs/io.rs");
const RGA: &str = include_str!("../src/pseudofs/dev/rga.rs");
const STAT: &str = include_str!("../src/syscall/fs/stat.rs");
const TIME: &str = include_str!("../src/syscall/time.rs");

#[test]
fn bidirectional_user_buffer_splits_copy_in_and_copy_out_capabilities() {
    let user_ptr = section(
        ACCESS,
        "impl<T> UserPtr<T> {",
        "pub fn atomic_update_user_u32",
    );
    let user_const_ptr = section(
        ACCESS,
        "impl<T> UserConstPtr<T> {",
        "/// Cumulative count of user page faults",
    );

    assert!(!user_ptr.contains("pub fn read_slice(self, len: usize)"));
    assert!(user_const_ptr.contains("pub fn read_slice(self, task: &UserTaskRef, len: usize)"));
}

#[test]
fn scalar_stream_write_preserves_the_user_buffer_as_an_io_cursor() {
    let syscall = section(FS_IO, "pub fn sys_write(", "\n}\n\npub fn sys_writev");

    assert!(
        syscall.find("check_access(").unwrap()
            < syscall.find("memfd_checks_before_stream_write(").unwrap(),
        "write must validate pointer geometry before seals without faulting the payload"
    );
    assert!(
        syscall.contains("VmBytes::new(current, buf.cast_const(), len)"),
        "write must stream from the user buffer through the existing IoSrc boundary"
    );
    assert!(
        !syscall.contains("copy_user_read_buf"),
        "write must not allocate and copy the complete payload before the file consumes it"
    );
}

#[test]
fn rga_release_copies_user_handles_before_locking_the_table() {
    let release = section(
        RGA,
        "fn handle_release_buffer",
        "/// `RGA_IOC_GET_DRVIER_VERSION`",
    );
    let table_lock = release
        .find("self.handle_table.lock()")
        .expect("RGA release must serialize handle removal");

    assert!(
        !release[table_lock..].contains(".vm_read_uninit()"),
        "faultable RGA user-memory reads must finish before the non-sleeping handle-table lock"
    );
}

#[test]
fn stat_abi_fields_share_one_faultable_user_memory_transfer() {
    let write_stat = section(STAT, "fn write_stat(", "\n}\n\nfn write_statx_timestamp");

    assert!(write_stat.contains("write_abi_fields"));
    assert!(
        !write_stat.contains("write_field("),
        "stat must not repeat address-space preparation for every ABI field"
    );
}

#[test]
fn clock_gettime_timespec_uses_one_faultable_user_transfer() {
    let write_timespec = section(
        TIME,
        "pub(crate) fn write_timespec(",
        "\n}\n\nfn write_timeval",
    );

    assert!(write_timespec.contains("write_abi_fields"));
    assert!(
        !write_timespec.contains("write_field("),
        "clock_gettime must not prepare user memory separately for tv_sec and tv_nsec"
    );
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start: {start}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end: {end}"))
        .0
}

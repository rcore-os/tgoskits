//! User-memory copy APIs must express both input-validity and output-byte invariants.

const ACCESS: &str = include_str!("../src/mm/access.rs");
const NET_FILE: &str = include_str!("../src/file/net.rs");
const NET_IO: &str = include_str!("../src/syscall/net/io.rs");
const RGA: &str = include_str!("../src/pseudofs/dev/rga.rs");
const SYS: &str = include_str!("../src/syscall/sys.rs");

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
    let syscall = section(SYS, "pub fn sys_riscv_hwprobe", "Ok(0)\n}");

    assert!(!user_ptr.contains("pub fn read_slice(self, len: usize)"));
    assert!(user_const_ptr.contains("pub fn read_slice(self, len: usize)"));
    assert!(syscall.contains("crate::mm::UserConstPtr::<RiscvHwprobe>"));
    assert!(syscall.contains("input_pairs.read_slice(pair_count)?"));
    assert!(syscall.contains("output_pairs.write_slice(&pairs)?"));
}

#[test]
fn riscv_hwprobe_is_a_bidirectional_wire_type() {
    let hwprobe = attributed_item(SYS, "struct RiscvHwprobe", "pub fn sys_riscv_hwprobe");
    let syscall = section(SYS, "pub fn sys_riscv_hwprobe", "Ok(0)\n}");

    assert!(hwprobe.contains("bytemuck::AnyBitPattern"));
    assert!(hwprobe.contains("bytemuck::NoUninit"));
    assert!(syscall.contains("input_pairs.read_slice(pair_count)?"));
    assert!(!syscall.contains("read_abi_slice"));
}

#[test]
fn socket_payloads_cross_transport_locks_through_kernel_staging_buffers() {
    let receive = section(
        NET_FILE,
        "pub(crate) fn recv_to_user<",
        "\n    pub fn ip_domain(",
    );
    let send = section(
        NET_FILE,
        "pub(crate) fn send_from_user<",
        "\n    pub(crate) fn recv_to_user<",
    );
    let file_like = section(NET_FILE, "impl FileLike for Socket {", "\n    fn stat(");
    let send_impl = section(NET_IO, "fn send_impl(", "\n}\n\npub fn sys_sendto");
    let recv_impl = section(NET_IO, "fn recv_impl(", "\n}\n\npub fn sys_recvfrom");

    assert!(send.contains("src.read_exact(&mut staging)"));
    assert!(send.contains("self.inner.send(staging.as_slice(), options)"));
    assert!(receive.contains("self.inner.recv(&mut staging, options)"));
    assert!(receive.contains("dst.write_all(&buffer[..copied])"));
    assert!(file_like.contains("self.recv_to_user(dst, RecvOptions::default())"));
    assert!(file_like.contains("self.send_from_user(src, SendOptions::default())"));
    assert!(send_impl.contains("socket.send_from_user("));
    assert!(recv_impl.contains("socket.recv_to_user("));
    assert!(!send_impl.contains("socket.send(\n"));
    assert!(!recv_impl.contains("socket.recv(\n"));
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

fn attributed_item<'a>(source: &'a str, item: &str, end: &str) -> &'a str {
    let item_offset = source
        .find(item)
        .unwrap_or_else(|| panic!("missing item: {item}"));
    let attribute_offset = source[..item_offset]
        .rfind("#[repr(C)]")
        .unwrap_or_else(|| panic!("missing repr(C) for item: {item}"));
    let end_offset = source[item_offset..]
        .find(end)
        .unwrap_or_else(|| panic!("missing item end: {end}"));
    &source[attribute_offset..item_offset + end_offset]
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

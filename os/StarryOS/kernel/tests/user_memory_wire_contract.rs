//! User-memory copy APIs must express both input-validity and output-byte invariants.

const ACCESS: &str = include_str!("../src/mm/access.rs");
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

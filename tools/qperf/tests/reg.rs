#[path = "../src/target.rs"]
mod target;

#[path = "../src/qemu/ffi.rs"]
pub mod ffi;

use target::{Frame, Reg, Target};

#[test]
fn qemu_v7_register_descriptor_includes_readonly_flag() {
    use std::mem::{offset_of, size_of};

    // C ABI: three pointers followed by a bool and trailing pointer alignment.
    let pointer = size_of::<*mut ()>();
    assert_eq!(offset_of!(ffi::RegisterDescriptor, name), pointer);
    assert_eq!(offset_of!(ffi::RegisterDescriptor, feature), 2 * pointer);
    assert_eq!(
        offset_of!(ffi::RegisterDescriptor, is_readonly),
        3 * pointer
    );
    assert_eq!(size_of::<ffi::RegisterDescriptor>(), 4 * pointer);
}

#[test]
fn parses_qemu_x86_64_target() {
    let x86_64 = "x86_64".parse::<Target>().unwrap();

    assert_eq!(x86_64.reg(Reg::Sp), "rsp");
    assert_eq!(x86_64.reg(Reg::Fp), "rbp");
    assert_eq!(x86_64.frame_address(0x1000), Some(0x1000));
    assert_eq!(
        "riscv64".parse::<Target>().unwrap().frame_address(0x1000),
        Some(0xff0)
    );
    assert_eq!(
        "loongarch64"
            .parse::<Target>()
            .unwrap()
            .frame_address(0x1000),
        Some(0xff0)
    );
    assert_eq!(Frame::default().fp, 0);
}

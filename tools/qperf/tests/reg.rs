#[path = "../src/target.rs"]
mod target;

use target::{Frame, Reg, Target};

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

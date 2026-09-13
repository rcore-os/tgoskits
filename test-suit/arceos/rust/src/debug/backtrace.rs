use std::println;

pub fn run() -> crate::TestResult {
    test_adjust_ip();
    println!("debug_backtrace: normal capture");
    emit_nested_backtrace();

    println!("debug_backtrace: raw trap capture with invalid frame pointer");
    let anchor = 0usize;
    let invalid_fp = (&anchor as *const usize as usize).wrapping_add(1);
    println!(
        "{}",
        axbacktrace::Backtrace::capture_trap(invalid_fp, 0, 0).kind("arceos-test-suit-raw-badfp")
    );
    Ok(())
}

fn test_adjust_ip() {
    let frame = axbacktrace::Frame { fp: 0, ip: 0x1000 };
    #[cfg(target_arch = "x86_64")]
    let expected = 0x0fff;
    #[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
    let expected = 0x0ffc;
    #[cfg(target_arch = "riscv64")]
    let expected = 0x0ffe;

    assert_eq!(frame.adjust_ip(), expected);
}

#[inline(never)]
fn emit_nested_backtrace() {
    nested_a();
    core::hint::black_box(());
}

#[inline(never)]
fn nested_a() {
    nested_b();
    core::hint::black_box(());
}

#[inline(never)]
fn nested_b() {
    nested_c();
    core::hint::black_box(());
}

#[inline(never)]
fn nested_c() {
    let backtrace = axbacktrace::Backtrace::capture();
    println!("{}", backtrace.kind("arceos-test-suit-raw-normal"));
    core::hint::black_box(());
}

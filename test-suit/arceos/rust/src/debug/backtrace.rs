use std::println;

pub fn run() -> crate::TestResult {
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

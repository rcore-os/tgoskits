use std::println;

pub fn run() -> crate::TestResult {
    println!("debug_panic_path: triggering panic to exercise panic backtrace path");
    nested_a();
    panic!("backtrace panic-path smoke test");
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
    core::hint::black_box(());
}

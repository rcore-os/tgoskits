use core::num::NonZeroU32;
use std::{any::Any, panic};

use scope_local::scope_local;

struct KernelGuardIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn enable_preempt() {}

    fn disable_preempt() {}
}

scope_local! {
    static RECURSION_TARGET: usize = 23;
    static RECURSIVE_VALUE: usize = {
        RECURSION_TARGET.with(|value| *value)
    };
}

fn bind_test_cpu() {
    let area_count = NonZeroU32::new(1).unwrap();
    ax_percpu::host_test::initialize(area_count).unwrap();
    let area = ax_percpu::area(ax_percpu::CpuIndex::try_from(0).unwrap()).unwrap();
    // SAFETY: this test models one CPU and never replaces its area.
    unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload
            .downcast::<&'static str>()
            .map(|message| (*message).to_owned())
            .unwrap_or_else(|_| "non-string panic payload".to_owned()),
    }
}

#[test]
fn recursive_global_initialization_fails_fast_and_can_retry() {
    bind_test_cpu();

    for _ in 0..2 {
        let panic = panic::catch_unwind(|| RECURSIVE_VALUE.with(|value| *value))
            .expect_err("recursive initialization must fail");
        assert_eq!(
            panic_message(panic),
            "scope-local global scope initialization is already in progress"
        );
    }
}

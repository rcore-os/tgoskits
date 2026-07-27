use core::num::NonZeroU32;
use std::{
    panic,
    sync::{
        Barrier, OnceLock,
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use scope_local::scope_local;

struct KernelGuardIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn enable_preempt() {}

    fn disable_preempt() {}
}

static INITIALIZER_ENTERED: OnceLock<Barrier> = OnceLock::new();
static RELEASE_INITIALIZER: OnceLock<Barrier> = OnceLock::new();
static WAITER_READY: OnceLock<Barrier> = OnceLock::new();

scope_local! {
    static BLOCKING_VALUE: usize = {
        INITIALIZER_ENTERED.get().unwrap().wait();
        RELEASE_INITIALIZER.get().unwrap().wait();
        41
    };
    static OBSERVED_VALUE: usize = 42;
}

fn bind_test_cpu(cpu_id: usize) {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id).unwrap();
    let area = ax_percpu::area(cpu_index).unwrap();
    // SAFETY: each host thread models one initialized CPU for its lifetime.
    unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();
}

#[test]
fn concurrent_global_access_waits_for_the_initializing_cpu() {
    ax_percpu::host_test::initialize(NonZeroU32::new(2).unwrap()).unwrap();
    assert!(INITIALIZER_ENTERED.set(Barrier::new(2)).is_ok());
    assert!(RELEASE_INITIALIZER.set(Barrier::new(2)).is_ok());
    assert!(WAITER_READY.set(Barrier::new(2)).is_ok());

    let initializer = thread::spawn(|| {
        bind_test_cpu(0);
        assert_eq!(BLOCKING_VALUE.with(|value| *value), 41);
    });
    INITIALIZER_ENTERED.get().unwrap().wait();

    let (result_sender, result_receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        bind_test_cpu(1);
        WAITER_READY.get().unwrap().wait();
        let result = panic::catch_unwind(|| OBSERVED_VALUE.with(|value| *value));
        result_sender.send(result).unwrap();
    });
    WAITER_READY.get().unwrap().wait();

    let early_result = result_receiver.recv_timeout(Duration::from_millis(200));
    RELEASE_INITIALIZER.get().unwrap().wait();

    let (completed_before_publication, result) = match early_result {
        Ok(result) => (true, result),
        Err(RecvTimeoutError::Timeout) => (
            false,
            result_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("waiting CPU must finish after global scope publication"),
        ),
        Err(RecvTimeoutError::Disconnected) => panic!("waiting CPU exited without a result"),
    };

    initializer.join().unwrap();
    waiter.join().unwrap();
    assert!(
        !completed_before_publication,
        "a competing CPU must wait for the global scope to be published"
    );
    assert_eq!(
        result.expect("a competing CPU must not observe recursive initialization"),
        42
    );
}

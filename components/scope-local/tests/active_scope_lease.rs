use core::num::NonZeroU32;
use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use scope_local::{ActiveScope, ScopeCell, scope_local};

struct KernelGuardIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn enable_preempt() {}

    fn disable_preempt() {}
}

scope_local! {
    static VALUE: usize = 7;
}

fn bind_test_cpu(cpu_id: usize) {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id).unwrap();
    let area = ax_percpu::area(cpu_index).unwrap();
    // SAFETY: each host thread models one initialized CPU for its lifetime.
    unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();
}

#[test]
fn active_scope_holds_one_shared_lease_until_switch_out() {
    ax_percpu::host_test::initialize(NonZeroU32::new(2).unwrap()).unwrap();
    bind_test_cpu(0);

    let scope = Arc::new(ScopeCell::new());
    // SAFETY: CPU 0 owns this activation until the matching deactivation.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| scope.activate_pinned(pin))
            .expect("CPU 0 must have an installed CPU area")
    };
    assert_eq!(VALUE.with(|value| *value), 7);

    let (attempted_tx, attempted_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let writer_scope = Arc::clone(&scope);
    let writer = thread::spawn(move || {
        bind_test_cpu(1);
        attempted_tx.send(()).unwrap();
        let mut guard = writer_scope.write();
        *VALUE.scope_cell_mut(&mut guard) = 11;
        drop(guard);
        acquired_tx.send(()).unwrap();
    });

    attempted_rx.recv().unwrap();
    let acquired_while_active = match acquired_rx.recv_timeout(Duration::from_millis(200)) {
        Ok(()) => true,
        Err(RecvTimeoutError::Timeout) => false,
        Err(RecvTimeoutError::Disconnected) => {
            panic!("the writer exited without reporting acquisition")
        }
    };

    // SAFETY: this releases the activation established above on CPU 0.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| scope.deactivate_pinned(pin))
            .expect("CPU 0 must retain its installed CPU area")
    };
    if !acquired_while_active {
        acquired_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the writer must acquire the scope after switch-out");
    }
    writer.join().unwrap();
    assert!(
        !acquired_while_active,
        "a remote writer must wait until the active task switches out"
    );

    // SAFETY: CPU 0 owns this second activation until the matching release.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| scope.activate_pinned(pin))
            .expect("CPU 0 must retain its installed CPU area")
    };
    assert_eq!(VALUE.with(|value| *value), 11);

    // SAFETY: CPU 0 owns the sole activation and remains pinned for the
    // complete mutation.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            scope.with_active_mut_pinned(pin, |guard| {
                *VALUE.scope_cell_mut(guard) = 13;
            })
        })
        .expect("CPU 0 must retain its installed CPU area")
    };
    assert_eq!(VALUE.with(|value| *value), 13);

    let panic = panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the same sole-activation and CPU-pin invariant applies. The
        // mutation guard must restore the shared activation during unwinding.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| {
                scope.with_active_mut_pinned(pin, |_guard| {
                    panic!("scope mutation unwind");
                })
            })
            .expect("CPU 0 must retain its installed CPU area")
        };
    }));
    assert!(panic.is_err());
    assert_eq!(VALUE.with(|value| *value), 13);

    // SAFETY: this releases the second CPU 0 activation.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| scope.deactivate_pinned(pin))
            .expect("CPU 0 must retain its installed CPU area")
    };

    assert!(ActiveScope::is_global());
}

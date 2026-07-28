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

use scope_local::{ActiveScope, ScopeCell, ScopeCellBusy, scope_local};

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
        ax_percpu::with_cpu_pin(|pin| scope.try_activate_pinned(pin))
            .expect("CPU 0 must have an installed CPU area")
            .expect("an idle scope must admit its scheduler activation")
    };
    assert_eq!(VALUE.with(|value| *value), 7);

    let (second_activation_tx, second_activation_rx) = mpsc::channel();
    let second_scope = Arc::clone(&scope);
    let second_activation = thread::spawn(move || {
        bind_test_cpu(1);
        // SAFETY: the test immediately tears down any activation admitted by
        // the old implementation before publishing the observed result.
        let result = unsafe {
            ax_percpu::with_cpu_pin(|pin| second_scope.try_activate_pinned(pin))
                .expect("CPU 1 must have an installed CPU area")
        };
        let published = !ActiveScope::is_global();
        if result.is_ok() {
            // SAFETY: this releases the activation admitted above so the
            // regression can report the old behavior without leaking it.
            unsafe {
                ax_percpu::with_cpu_pin(|pin| second_scope.deactivate_pinned(pin))
                    .expect("CPU 1 must retain its installed CPU area")
            };
        }
        second_activation_tx.send((result, published)).unwrap();
    });
    let second_activation_result = second_activation_rx.recv().unwrap();
    second_activation.join().unwrap();

    let (attempted_tx, attempted_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let writer_scope = Arc::clone(&scope);
    let writer = thread::spawn(move || {
        bind_test_cpu(1);
        attempted_tx.send(()).unwrap();
        let mut guard = loop {
            match writer_scope.try_write() {
                Ok(guard) => break guard,
                Err(ScopeCellBusy) => thread::yield_now(),
            }
        };
        *VALUE.scope_cell_mut(&mut guard) = 11;
        drop(guard);
        acquired_tx.send(()).unwrap();
    });

    // A sole scheduler activation upgrades without polling or waiting.
    let sole_activation_result = unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            scope.try_with_active_mut_pinned(pin, |guard| {
                *VALUE.scope_cell_mut(guard) = 9;
            })
        })
        .expect("CPU 0 must retain its installed CPU area")
    };
    assert_eq!(sole_activation_result, Ok(()));
    assert_eq!(VALUE.with(|value| *value), 9);

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
        ax_percpu::with_cpu_pin(|pin| scope.try_activate_pinned(pin))
            .expect("CPU 0 must retain its installed CPU area")
            .expect("the scheduler activation must be immediately available")
    };
    assert_eq!(VALUE.with(|value| *value), 11);

    let remote_read = scope
        .try_read()
        .expect("an active scope must admit an ordinary shared reader");
    let recursive_result = unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            scope.try_with_active_mut_pinned(pin, |_guard| {
                panic!("a retained read lease must not enter mutation")
            })
        })
        .expect("CPU 0 must retain its installed CPU area")
    };
    assert_eq!(recursive_result, Err(ScopeCellBusy));
    drop(remote_read);

    // SAFETY: CPU 0 owns the sole activation and remains pinned for the
    // complete mutation.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            scope.try_with_active_mut_pinned(pin, |guard| {
                *VALUE.scope_cell_mut(guard) = 13;
            })
        })
        .expect("CPU 0 must retain its installed CPU area")
        .expect("the sole active lease must upgrade immediately")
    };
    assert_eq!(VALUE.with(|value| *value), 13);

    let panic = panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the same sole-activation and CPU-pin invariant applies. The
        // mutation guard must restore the shared activation during unwinding.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| {
                scope.try_with_active_mut_pinned(pin, |_guard| {
                    panic!("scope mutation unwind");
                })
            })
            .expect("CPU 0 must retain its installed CPU area")
            .expect("the sole active lease must upgrade immediately")
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
    assert_eq!(
        second_activation_result,
        (Err(ScopeCellBusy), false),
        "a task-owned scope must reject a second scheduler activation before publishing it"
    );
}

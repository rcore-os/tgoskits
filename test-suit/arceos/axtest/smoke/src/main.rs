#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "runtime")]
use ax_net as _;

#[cfg(all(feature = "ax-std", axtest))]
#[axtest::tests]
mod smoke {
    use core::{
        hint::spin_loop,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::{
        os::arceos::{
            api::task::{AxCpuMask, ax_set_current_affinity},
            guard::PreemptGuard,
            modules::ax_hal::percpu::{scheduler_preempt_guard_depth, this_cpu_id},
            percpu,
        },
        sync::Arc,
        thread,
    };

    use axtest::prelude::*;
    use scope_local::{Scope, ScopeActivationError, ScopeCell, ScopeCellBusy, scope_local};

    const WAIT_STEPS: usize = 100_000;

    static INITIALIZER_ENTERED: AtomicBool = AtomicBool::new(false);
    static RELEASE_INITIALIZER: AtomicBool = AtomicBool::new(false);
    static WAITER_STARTED: AtomicBool = AtomicBool::new(false);
    static WAITER_DONE: AtomicBool = AtomicBool::new(false);
    static PINNED_INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

    scope_local! {
        static INIT_PREEMPT_DEPTH: u32 =
            scheduler_preempt_guard_depth().unwrap_or(u32::MAX);
        static BLOCKING_VALUE: usize = {
            INITIALIZER_ENTERED.store(true, Ordering::Release);
            while !RELEASE_INITIALIZER.load(Ordering::Acquire) {
                thread::yield_now();
            }
            41
        };
        static OBSERVED_VALUE: usize = 42;
        static CELL_VALUE: usize = 7;
        static TASK_VALUE: usize = 5;
        static TASK_SHARED: Arc<()> = Arc::new(());
        static PINNED_VALUE: usize = {
            PINNED_INIT_COUNT.fetch_add(1, Ordering::AcqRel);
            11
        };
    }

    struct ActivationSync {
        abort: AtomicBool,
        worker_ready: AtomicBool,
        worker_bound: AtomicBool,
        activation_ready: AtomicBool,
        activation_checked: AtomicBool,
        second_activation: AtomicUsize,
        start_reader: AtomicBool,
        reader_held: AtomicBool,
        release_reader: AtomicBool,
        reader_released: AtomicBool,
        deactivated: AtomicBool,
    }

    impl ActivationSync {
        fn new() -> Self {
            Self {
                abort: AtomicBool::new(false),
                worker_ready: AtomicBool::new(false),
                worker_bound: AtomicBool::new(false),
                activation_ready: AtomicBool::new(false),
                activation_checked: AtomicBool::new(false),
                second_activation: AtomicUsize::new(0),
                start_reader: AtomicBool::new(false),
                reader_held: AtomicBool::new(false),
                release_reader: AtomicBool::new(false),
                reader_released: AtomicBool::new(false),
                deactivated: AtomicBool::new(false),
            }
        }
    }

    #[derive(Default)]
    struct ActiveOutcome {
        activated: bool,
        second_activation_observed: bool,
        mutation_rejected_with_reader: bool,
        mutation_succeeded: bool,
        pinned_value_after_mutation: usize,
    }

    struct WorkerOutcome {
        bound: bool,
        reader_acquired: bool,
        writer_busy_while_active: bool,
        writer_acquired: bool,
    }

    fn bind_current_cpu(cpu_id: usize) -> bool {
        if ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_err() {
            return false;
        }
        for _ in 0..WAIT_STEPS {
            if this_cpu_id() == cpu_id {
                return true;
            }
            thread::yield_now();
        }
        false
    }

    fn wait_yield(flag: &AtomicBool) -> bool {
        for _ in 0..WAIT_STEPS {
            if flag.load(Ordering::Acquire) {
                return true;
            }
            thread::yield_now();
        }
        false
    }

    fn wait_yield_or_abort(flag: &AtomicBool, abort: &AtomicBool) -> bool {
        for _ in 0..WAIT_STEPS {
            if flag.load(Ordering::Acquire) {
                return true;
            }
            if abort.load(Ordering::Acquire) {
                return false;
            }
            thread::yield_now();
        }
        false
    }

    fn wait_spin_or_abort(flag: &AtomicBool, abort: &AtomicBool) -> bool {
        for _ in 0..WAIT_STEPS * 100 {
            if flag.load(Ordering::Acquire) {
                return true;
            }
            if abort.load(Ordering::Acquire) {
                return false;
            }
            spin_loop();
        }
        false
    }

    fn exercise_concurrent_global_initialization() -> Option<(usize, usize, bool)> {
        INITIALIZER_ENTERED.store(false, Ordering::Release);
        RELEASE_INITIALIZER.store(false, Ordering::Release);
        WAITER_STARTED.store(false, Ordering::Release);
        WAITER_DONE.store(false, Ordering::Release);

        let initializer = thread::spawn(|| {
            let bound = bind_current_cpu(0);
            let value = BLOCKING_VALUE.with(|value| *value);
            (bound, value)
        });
        if !wait_yield(&INITIALIZER_ENTERED) {
            RELEASE_INITIALIZER.store(true, Ordering::Release);
            let _ = initializer.join();
            return None;
        }

        let waiter = thread::spawn(|| {
            let bound = bind_current_cpu(1);
            WAITER_STARTED.store(true, Ordering::Release);
            let value = OBSERVED_VALUE.with(|value| *value);
            WAITER_DONE.store(true, Ordering::Release);
            (bound, value)
        });
        let waiter_started = wait_yield(&WAITER_STARTED);
        for _ in 0..256 {
            thread::yield_now();
        }
        let completed_before_publication = WAITER_DONE.load(Ordering::Acquire);
        RELEASE_INITIALIZER.store(true, Ordering::Release);

        let (initializer_bound, initialized) = initializer.join().ok()?;
        let (waiter_bound, observed) = waiter.join().ok()?;
        Some((
            initialized,
            observed,
            initializer_bound && waiter_bound && waiter_started && !completed_before_publication,
        ))
    }

    fn scope_cell_worker(cell: Arc<ScopeCell>, sync: Arc<ActivationSync>) -> WorkerOutcome {
        let bound = bind_current_cpu(1);
        sync.worker_bound.store(bound, Ordering::Release);
        sync.worker_ready.store(true, Ordering::Release);
        if !wait_yield_or_abort(&sync.activation_ready, &sync.abort) {
            return WorkerOutcome {
                bound,
                reader_acquired: false,
                writer_busy_while_active: false,
                writer_acquired: false,
            };
        }

        let second_activation = {
            let _preempt = PreemptGuard::new();
            // SAFETY: the preemption guard prevents migration for the complete
            // callback. Any unexpected successful activation is retired before
            // the guard and CPU pin leave scope.
            unsafe {
                percpu::with_cpu_pin(|pin| {
                    let result = cell.try_activate_pinned(pin);
                    if result.is_ok() {
                        cell.deactivate_pinned(pin);
                    }
                    result
                })
            }
        };
        let second_activation = match second_activation {
            Ok(Err(ScopeActivationError::AlreadyActive)) => 1,
            _ => 2,
        };
        sync.second_activation
            .store(second_activation, Ordering::Release);
        sync.activation_checked.store(true, Ordering::Release);

        if !wait_yield_or_abort(&sync.start_reader, &sync.abort) {
            return WorkerOutcome {
                bound,
                reader_acquired: false,
                writer_busy_while_active: false,
                writer_acquired: false,
            };
        }
        let reader = cell.try_read();
        let reader_acquired = reader.is_ok();
        if reader_acquired {
            sync.reader_held.store(true, Ordering::Release);
            let _ = wait_spin_or_abort(&sync.release_reader, &sync.abort);
        } else {
            sync.abort.store(true, Ordering::Release);
        }
        drop(reader);
        sync.reader_released.store(true, Ordering::Release);

        let writer_busy_while_active = cell.try_write().is_err();

        if !wait_yield_or_abort(&sync.deactivated, &sync.abort) {
            return WorkerOutcome {
                bound,
                reader_acquired,
                writer_busy_while_active,
                writer_acquired: false,
            };
        }
        let mut writer_acquired = false;
        for _ in 0..WAIT_STEPS {
            match cell.try_write() {
                Ok(mut writer) => {
                    *CELL_VALUE.scope_cell_mut(&mut writer) = 13;
                    writer_acquired = true;
                    break;
                }
                Err(ScopeCellBusy) => thread::yield_now(),
            }
        }
        WorkerOutcome {
            bound,
            reader_acquired,
            writer_busy_while_active,
            writer_acquired,
        }
    }

    fn exercise_active_scope(cell: &ScopeCell, sync: &ActivationSync) -> ActiveOutcome {
        let _preempt = PreemptGuard::new();
        // SAFETY: the preemption guard prevents migration and context switches
        // for the complete pinned activation. The worker runs on CPU 1, so it
        // can make progress while this CPU retains the scheduler-style baton.
        unsafe {
            percpu::with_cpu_pin(|pin| {
                let mut outcome = ActiveOutcome::default();
                if cell.try_activate_pinned(pin).is_err() {
                    sync.abort.store(true, Ordering::Release);
                    sync.activation_ready.store(true, Ordering::Release);
                    return outcome;
                }
                outcome.activated = true;
                sync.activation_ready.store(true, Ordering::Release);

                outcome.second_activation_observed =
                    wait_spin_or_abort(&sync.activation_checked, &sync.abort)
                        && sync.second_activation.load(Ordering::Acquire) == 1;
                sync.start_reader.store(true, Ordering::Release);
                let reader_held = wait_spin_or_abort(&sync.reader_held, &sync.abort);
                outcome.mutation_rejected_with_reader =
                    reader_held && cell.try_with_active_mut_pinned(pin, |_writer| ()).is_err();

                sync.release_reader.store(true, Ordering::Release);
                let reader_released = wait_spin_or_abort(&sync.reader_released, &sync.abort);
                if reader_released && !sync.abort.load(Ordering::Acquire) {
                    outcome.mutation_succeeded = cell
                        .try_with_active_mut_pinned(pin, |writer| {
                            *CELL_VALUE.scope_cell_mut(writer) = 11;
                        })
                        .is_ok();
                    outcome.pinned_value_after_mutation =
                        CELL_VALUE.with_pinned(pin, |value| *value);
                }

                cell.deactivate_pinned(pin);
                sync.deactivated.store(true, Ordering::Release);
                outcome
            })
        }
        .unwrap_or_default()
    }

    fn observe_reactivated_value(cell: &ScopeCell) -> Option<usize> {
        let _preempt = PreemptGuard::new();
        // SAFETY: the preemption guard covers the full activation/deactivation
        // pair, and the cell is idle after the worker released its writer.
        unsafe {
            percpu::with_cpu_pin(|pin| {
                cell.try_activate_pinned(pin).ok()?;
                let value = CELL_VALUE.with_pinned(pin, |value| *value);
                cell.deactivate_pinned(pin);
                Some(value)
            })
        }
        .ok()
        .flatten()
    }

    fn observe_shared_scope_on_cpu(
        scope: Arc<ScopeCell>,
        expected_shared: Arc<()>,
        cpu_id: usize,
    ) -> Option<(usize, bool)> {
        if !bind_current_cpu(cpu_id) {
            return None;
        }
        let _preempt = PreemptGuard::new();
        // SAFETY: the worker owns the cell's only activation and retires it
        // before the preemption guard and CPU pin leave scope.
        unsafe {
            percpu::with_cpu_pin(|pin| {
                scope.try_activate_pinned(pin).ok()?;
                let value = TASK_VALUE.with_pinned(pin, |value| *value);
                let shared =
                    TASK_SHARED.with_pinned(pin, |shared| Arc::ptr_eq(shared, &expected_shared));
                scope.deactivate_pinned(pin);
                Some((value, shared))
            })
        }
        .ok()
        .flatten()
    }

    fn observe_global_value_on_cpu(cpu_id: usize) -> Option<usize> {
        bind_current_cpu(cpu_id).then(|| TASK_VALUE.with(|value| *value))
    }

    #[test]
    fn arithmetic_smoke() {
        ax_assert_eq!(2 + 2, 4);
    }

    #[test]
    fn explicit_result_smoke() -> axtest::AxTestResult {
        ax_assert!(true);
        axtest::AxTestResult::Ok
    }

    #[test]
    fn scope_local_uses_real_smp_runtime() -> axtest::AxTestResult {
        let available_cpus = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(0);
        ax_assert!(available_cpus >= 4);
        ax_assert!(bind_current_cpu(2));
        PINNED_INIT_COUNT.store(0, Ordering::Release);

        let initialization = exercise_concurrent_global_initialization();
        ax_assert!(initialization.is_some());
        let (initialized, observed, waited_for_publication) = initialization.unwrap();
        ax_assert_eq!(initialized, 41);
        ax_assert_eq!(observed, 42);
        ax_assert!(waited_for_publication);
        ax_assert_eq!(INIT_PREEMPT_DEPTH.with(|depth| *depth), 0);
        ax_assert_eq!(PINNED_INIT_COUNT.load(Ordering::Acquire), 1);

        ax_assert!(bind_current_cpu(0));
        let pinned_value = {
            let _preempt = PreemptGuard::new();
            unsafe { percpu::with_cpu_pin(|pin| PINNED_VALUE.with_pinned(pin, |value| *value)) }
                .ok()
        };
        ax_assert_eq!(pinned_value, Some(11));
        ax_assert_eq!(PINNED_INIT_COUNT.load(Ordering::Acquire), 1);

        let cell = Arc::new(ScopeCell::new());
        let sync = Arc::new(ActivationSync::new());
        let worker = {
            let cell = Arc::clone(&cell);
            let sync = Arc::clone(&sync);
            thread::spawn(move || scope_cell_worker(cell, sync))
        };
        let worker_ready = wait_yield(&sync.worker_ready);
        let active = if worker_ready {
            exercise_active_scope(&cell, &sync)
        } else {
            sync.abort.store(true, Ordering::Release);
            sync.activation_ready.store(true, Ordering::Release);
            ActiveOutcome::default()
        };
        if !active.activated {
            sync.abort.store(true, Ordering::Release);
        }
        sync.release_reader.store(true, Ordering::Release);
        sync.deactivated.store(true, Ordering::Release);
        let worker = worker.join();
        ax_assert!(worker.is_ok());
        let worker = worker.unwrap();

        ax_assert!(worker_ready);
        ax_assert!(sync.worker_bound.load(Ordering::Acquire));
        ax_assert!(worker.bound);
        ax_assert!(active.activated);
        ax_assert!(active.second_activation_observed);
        ax_assert!(worker.reader_acquired);
        ax_assert!(active.mutation_rejected_with_reader);
        ax_assert!(worker.writer_busy_while_active);
        ax_assert!(active.mutation_succeeded);
        ax_assert_eq!(active.pinned_value_after_mutation, 11);
        ax_assert!(worker.writer_acquired);
        ax_assert_eq!(observe_reactivated_value(&cell), Some(13));

        let global_shared = TASK_SHARED.clone_current();
        let mut shared_scope = Scope::new();
        *TASK_VALUE.scope_mut(&mut shared_scope) = 29;
        *TASK_SHARED.scope_mut(&mut shared_scope) = Arc::clone(&global_shared);
        let shared_scope = Arc::new(ScopeCell::from_scope(shared_scope));
        let shared_observation = {
            let scope = Arc::clone(&shared_scope);
            let expected_shared = Arc::clone(&global_shared);
            thread::spawn(move || observe_shared_scope_on_cpu(scope, expected_shared, 3)).join()
        };
        ax_assert!(shared_observation.is_ok());
        ax_assert_eq!(shared_observation.unwrap(), Some((29, true)));
        ax_assert!(TASK_SHARED.with(|shared| Arc::ptr_eq(shared, &global_shared)));
        ax_assert_eq!(TASK_VALUE.with(|value| *value), 5);

        let remote_global = thread::spawn(|| observe_global_value_on_cpu(2)).join();
        ax_assert!(remote_global.is_ok());
        ax_assert_eq!(remote_global.unwrap(), Some(5));

        axtest::AxTestResult::Ok
    }
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("arceos-axtest-suit requires the ax-std feature for kernel runs");
}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}

//! Serialized VM startup transaction mechanics.

use crate::AxVmResult;

pub(crate) trait StartLock {
    fn with_lock<T>(&self, transaction: impl FnOnce() -> T) -> T;
}

pub(crate) struct SleepableStartLock {
    gate: ax_sync::Mutex<()>,
}

impl SleepableStartLock {
    const fn new() -> Self {
        Self {
            gate: ax_sync::Mutex::new(()),
        }
    }
}

impl StartLock for SleepableStartLock {
    fn with_lock<T>(&self, transaction: impl FnOnce() -> T) -> T {
        let _guard = self.gate.lock();
        transaction()
    }
}

pub(crate) struct StartCoordinator<L = SleepableStartLock> {
    gate: L,
}

impl StartCoordinator {
    pub(crate) const fn new() -> Self {
        Self {
            gate: SleepableStartLock::new(),
        }
    }
}

impl<L: StartLock> StartCoordinator<L> {
    pub(crate) fn run<T>(&self, transaction: impl FnOnce() -> AxVmResult<T>) -> AxVmResult<T> {
        self.gate.with_lock(transaction)
    }
}

pub(crate) fn execute_start_transaction(
    prepare: impl FnOnce() -> AxVmResult,
    commit: impl FnOnce() -> AxVmResult,
    cancel: impl FnOnce(),
    spawn: impl FnOnce(),
) -> AxVmResult {
    prepare()?;
    if let Err(error) = commit() {
        cancel();
        return Err(error);
    }
    spawn();
    Ok(())
}

pub(crate) fn execute_stopped_restart(
    join: impl FnOnce(),
    restore: impl FnOnce() -> AxVmResult,
    prepare: impl FnOnce() -> AxVmResult,
) -> AxVmResult {
    join();
    restore()?;
    prepare()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use core::{
        cell::{Cell, RefCell},
        sync::atomic::{AtomicUsize, Ordering},
    };
    use std::{
        sync::{Barrier, Mutex as StdMutex},
        thread,
    };

    use super::{StartCoordinator, StartLock, execute_start_transaction, execute_stopped_restart};
    use crate::{AxVmError, lifecycle::Machine};

    struct HostStartLock(StdMutex<()>);

    impl StartLock for HostStartLock {
        fn with_lock<T>(&self, transaction: impl FnOnce() -> T) -> T {
            let _guard = self.0.lock().expect("host start lock poisoned");
            transaction()
        }
    }

    fn test_error(operation: &'static str) -> AxVmError {
        AxVmError::unsupported(operation, "deterministic startup test failure")
    }

    #[test]
    fn start_transaction_prepare_failure_skips_commit_cancel_and_spawn() {
        let events = RefCell::new(alloc::vec::Vec::new());
        let machine = RefCell::new(Machine::<(), ()>::Ready(()));
        let spawned = Cell::new(false);
        let expected = test_error("prepare start");

        let result = execute_start_transaction(
            || {
                events.borrow_mut().push("prepare");
                Err(expected.clone())
            },
            || {
                events.borrow_mut().push("commit");
                machine.borrow_mut().start_with(|_| Ok(()))
            },
            || events.borrow_mut().push("cancel"),
            || {
                events.borrow_mut().push("spawn");
                spawned.set(true);
            },
        );

        assert_eq!(result, Err(expected));
        assert_eq!(&*events.borrow(), &["prepare"]);
        assert!(!spawned.get());
        assert_ne!(machine.borrow().status(), crate::VmStatus::Running);
    }

    #[test]
    fn start_transaction_commit_failure_cancels_without_spawning() {
        let events = RefCell::new(alloc::vec::Vec::new());
        let machine = RefCell::new(Machine::<(), ()>::Destroyed);
        let spawned = Cell::new(false);

        let result = execute_start_transaction(
            || {
                events.borrow_mut().push("prepare");
                Ok(())
            },
            || {
                events.borrow_mut().push("commit");
                machine.borrow_mut().start_with(|_| Ok(()))
            },
            || events.borrow_mut().push("cancel"),
            || {
                events.borrow_mut().push("spawn");
                spawned.set(true);
            },
        );

        assert!(result.is_err());
        assert_eq!(&*events.borrow(), &["prepare", "commit", "cancel"]);
        assert!(!spawned.get());
        assert_ne!(machine.borrow().status(), crate::VmStatus::Running);
    }

    #[test]
    fn start_transaction_success_spawns_only_after_commit() {
        let events = RefCell::new(alloc::vec::Vec::new());
        let machine = RefCell::new(Machine::<(), ()>::Ready(()));

        execute_start_transaction(
            || {
                events.borrow_mut().push("prepare");
                Ok(())
            },
            || {
                events.borrow_mut().push("commit");
                machine.borrow_mut().start_with(|_| Ok(()))
            },
            || events.borrow_mut().push("cancel"),
            || events.borrow_mut().push("spawn"),
        )
        .unwrap();

        assert_eq!(&*events.borrow(), &["prepare", "commit", "spawn"]);
        assert_eq!(machine.borrow().status(), crate::VmStatus::Running);
    }

    #[test]
    fn start_transaction_coordinator_serializes_callers() {
        let coordinator = Arc::new(StartCoordinator {
            gate: HostStartLock(StdMutex::new(())),
        });
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(3));
        let callers = (0..2)
            .map(|_| {
                let coordinator = coordinator.clone();
                let in_flight = in_flight.clone();
                let max_in_flight = max_in_flight.clone();
                let start = start.clone();
                thread::spawn(move || {
                    start.wait();
                    coordinator
                        .run(|| {
                            let current = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
                            max_in_flight.fetch_max(current, Ordering::AcqRel);
                            for _ in 0..32 {
                                thread::yield_now();
                            }
                            in_flight.fetch_sub(1, Ordering::AcqRel);
                            Ok(())
                        })
                        .unwrap();
                })
            })
            .collect::<alloc::vec::Vec<_>>();

        start.wait();
        for caller in callers {
            caller.join().unwrap();
        }
        assert_eq!(max_in_flight.load(Ordering::Acquire), 1);
    }

    #[test]
    fn stopped_restart_joins_then_restores_before_prepare() {
        let events = RefCell::new(alloc::vec::Vec::new());

        execute_stopped_restart(
            || events.borrow_mut().push("join"),
            || {
                events.borrow_mut().push("restore");
                Ok(())
            },
            || {
                events.borrow_mut().push("prepare");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(&*events.borrow(), &["join", "restore", "prepare"]);
    }

    #[test]
    fn stopped_restart_restore_failure_skips_prepare() {
        let events = RefCell::new(alloc::vec::Vec::new());
        let expected = test_error("restore boot memory");

        let result = execute_stopped_restart(
            || events.borrow_mut().push("join"),
            || {
                events.borrow_mut().push("restore");
                Err(expected.clone())
            },
            || {
                events.borrow_mut().push("prepare");
                Ok(())
            },
        );

        assert_eq!(result, Err(expected));
        assert_eq!(&*events.borrow(), &["join", "restore"]);
    }
}

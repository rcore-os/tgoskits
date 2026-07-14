use std::{sync::Arc, thread};

use ax_kspin::{
    LockRuntime, LockdepEvent, RawSpinIrqSave, RawSpinIrqSaveRwLock, RawSpinLock, RawSpinNoIrq,
    RawSpinNoIrqRwLock, RawSpinNoPreempt, RawSpinNoPreemptIrqSave, RawSpinNoPreemptIrqSaveRwLock,
    RawSpinNoPreemptRwLock, RawSpinRwLock, SpinIrqSave, SpinIrqSaveGuard, SpinIrqSaveRwLock,
    SpinIrqSaveRwLockReadGuard, SpinIrqSaveRwLockWriteGuard, SpinNoIrq, SpinNoIrqGuard,
    SpinNoIrqRwLock, SpinNoIrqRwLockReadGuard, SpinNoIrqRwLockWriteGuard, SpinNoPreempt,
    SpinNoPreemptGuard, SpinNoPreemptIrqSave, SpinNoPreemptIrqSaveGuard,
    SpinNoPreemptIrqSaveRwLock, SpinNoPreemptIrqSaveRwLockReadGuard,
    SpinNoPreemptIrqSaveRwLockWriteGuard, SpinNoPreemptRwLock, SpinNoPreemptRwLockReadGuard,
    SpinNoPreemptRwLockWriteGuard, SpinRaw, SpinRawGuard, SpinRawRwLock, SpinRawRwLockReadGuard,
    SpinRawRwLockWriteGuard, impl_trait,
};
use lock_api::{GuardNoSend, RawMutex, RawRwLock};

struct TestRuntime;

impl_trait! {
    impl LockRuntime for TestRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn irqs_enabled() -> bool { true }
        fn preempt_enter() {}
        fn preempt_exit() -> bool { true }
        fn in_hard_irq() -> bool { false }
        fn need_resched() -> bool { false }
        fn schedule() {}
        fn current_thread_id() -> u64 { 1 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}

#[test]
fn raw_mutex_is_always_smp_safe() {
    const THREADS: usize = 4;
    const ITERATIONS: usize = 2_000;

    let value = Arc::new(SpinRaw::new(0usize));
    let workers = (0..THREADS)
        .map(|_| {
            let value = Arc::clone(&value);
            thread::spawn(move || {
                for _ in 0..ITERATIONS {
                    *value.lock() += 1;
                }
            })
        })
        .collect::<Vec<_>>();

    for worker in workers {
        worker.join().expect("spin-lock worker should complete");
    }

    assert_eq!(*value.lock(), THREADS * ITERATIONS);
}

#[test]
fn context_aware_mutex_has_a_non_send_guard() {
    assert_mutex_guard_is_not_send::<RawSpinLock>();
    assert_mutex_guard_is_not_send::<RawSpinNoPreempt>();
    assert_mutex_guard_is_not_send::<RawSpinIrqSave>();
    assert_mutex_guard_is_not_send::<RawSpinNoPreemptIrqSave>();
    assert_mutex_guard_is_not_send::<RawSpinNoIrq>();
}

#[test]
fn context_aware_rwlock_has_non_send_guards() {
    assert_rwlock_guards_are_not_send::<RawSpinRwLock>();
    assert_rwlock_guards_are_not_send::<RawSpinNoPreemptRwLock>();
    assert_rwlock_guards_are_not_send::<RawSpinIrqSaveRwLock>();
    assert_rwlock_guards_are_not_send::<RawSpinNoPreemptIrqSaveRwLock>();
    assert_rwlock_guards_are_not_send::<RawSpinNoIrqRwLock>();
}

#[test]
fn public_mutex_aliases_match_their_guard_types() {
    let raw = SpinRaw::new(0usize);
    accept_spin_raw_guard(raw.lock());

    let no_preempt = SpinNoPreempt::new(0usize);
    accept_spin_no_preempt_guard(no_preempt.lock());

    let irq_save = SpinIrqSave::new(0usize);
    accept_spin_irq_save_guard(irq_save.lock());

    let combined = SpinNoPreemptIrqSave::new(0usize);
    accept_spin_combined_guard(combined.lock());

    let compatible = SpinNoIrq::new(0usize);
    accept_spin_no_irq_guard(compatible.lock());
}

#[test]
fn public_rwlock_aliases_match_their_guard_types() {
    let raw = SpinRawRwLock::new(0usize);
    accept_raw_rw_read_guard(raw.read());
    accept_raw_rw_write_guard(raw.write());

    let no_preempt = SpinNoPreemptRwLock::new(0usize);
    accept_no_preempt_rw_read_guard(no_preempt.read());
    accept_no_preempt_rw_write_guard(no_preempt.write());

    let irq_save = SpinIrqSaveRwLock::new(0usize);
    accept_irq_save_rw_read_guard(irq_save.read());
    accept_irq_save_rw_write_guard(irq_save.write());

    let combined = SpinNoPreemptIrqSaveRwLock::new(0usize);
    accept_combined_rw_read_guard(combined.read());
    accept_combined_rw_write_guard(combined.write());

    let compatible = SpinNoIrqRwLock::new(0usize);
    accept_no_irq_rw_read_guard(compatible.read());
    accept_no_irq_rw_write_guard(compatible.write());
}

fn assert_mutex_guard_is_not_send<R>()
where
    R: RawMutex<GuardMarker = GuardNoSend>,
{
}

fn assert_rwlock_guards_are_not_send<R>()
where
    R: RawRwLock<GuardMarker = GuardNoSend>,
{
}

fn accept_spin_raw_guard(_guard: SpinRawGuard<'_, usize>) {}

fn accept_spin_no_preempt_guard(_guard: SpinNoPreemptGuard<'_, usize>) {}

fn accept_spin_irq_save_guard(_guard: SpinIrqSaveGuard<'_, usize>) {}

fn accept_spin_combined_guard(_guard: SpinNoPreemptIrqSaveGuard<'_, usize>) {}

fn accept_spin_no_irq_guard(_guard: SpinNoIrqGuard<'_, usize>) {}

fn accept_raw_rw_read_guard(_guard: SpinRawRwLockReadGuard<'_, usize>) {}

fn accept_raw_rw_write_guard(_guard: SpinRawRwLockWriteGuard<'_, usize>) {}

fn accept_no_preempt_rw_read_guard(_guard: SpinNoPreemptRwLockReadGuard<'_, usize>) {}

fn accept_no_preempt_rw_write_guard(_guard: SpinNoPreemptRwLockWriteGuard<'_, usize>) {}

fn accept_irq_save_rw_read_guard(_guard: SpinIrqSaveRwLockReadGuard<'_, usize>) {}

fn accept_irq_save_rw_write_guard(_guard: SpinIrqSaveRwLockWriteGuard<'_, usize>) {}

fn accept_combined_rw_read_guard(_guard: SpinNoPreemptIrqSaveRwLockReadGuard<'_, usize>) {}

fn accept_combined_rw_write_guard(_guard: SpinNoPreemptIrqSaveRwLockWriteGuard<'_, usize>) {}

fn accept_no_irq_rw_read_guard(_guard: SpinNoIrqRwLockReadGuard<'_, usize>) {}

fn accept_no_irq_rw_write_guard(_guard: SpinNoIrqRwLockWriteGuard<'_, usize>) {}

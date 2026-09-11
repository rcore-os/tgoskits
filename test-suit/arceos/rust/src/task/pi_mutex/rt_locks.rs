use super::*;

pub(super) fn run() {
    pi_boost_checks_current_priority();
    rt_spin_lock_remains_preemptible();
    rt_lock_preserves_outer_timeout();
    reader_drain_uses_lock_wake();
    nested_migration_defers_remote_affinity();
    local_lock_serializes_preempting_tasks();
    semaphore_grants_and_cancellation();
    semaphore_hard_irq_release();
}

fn pi_boost_checks_current_priority() {
    for (priority, expected) in [(80, false), (60, true)] {
        assert_eq!(
            queued_pi_boost_requests_preemption(priority),
            expected,
            "PI boost to FIFO 80 must preempt only a lower-priority current task (FIFO {priority})"
        );
    }
}

fn queued_pi_boost_requests_preemption(current_priority: u8) -> bool {
    use ax_std::os::arceos::task::{
        runtime::cpu::current_immediate_preemption_requested,
        sync::RawSpinLock,
        thread::{ThreadState, current},
    };

    let owner = current::current_thread_handle().unwrap();
    let original_affinity = owner.affinity().unwrap();
    pin_current_to_cpu(0);
    let mutex = Arc::new(Mutex::new(()));
    let held = mutex.lock();
    let waiter = {
        let mutex = Arc::clone(&mutex);
        ax_std::os::arceos::thread::builder("pi-equal-waiter".into())
            .affinity(cpu_mask(0))
            .policy(SchedulePolicy::fifo(RtPriority::new(40).unwrap()))
            .spawn(move || drop(mutex.lock()))
            .unwrap()
    };
    wait_until(
        || waiter.state() == ThreadState::Blocked,
        "PI equal-priority waiter must first donate priority 40",
    );
    assert_eq!(
        owner.effective_policy(),
        SchedulePolicy::fifo(RtPriority::new(40).unwrap())
    );

    let update = Arc::new(AtomicBool::new(false));
    let updated = Arc::new(AtomicBool::new(false));
    let observations = Arc::new(AtomicUsize::new(0));
    let controller = {
        let waiter = waiter.clone();
        let update = Arc::clone(&update);
        let updated = Arc::clone(&updated);
        ax_std::os::arceos::thread::builder("pi-equal-controller".into())
            .affinity(cpu_mask(1))
            .spawn(move || {
                wait_until(
                    || update.load(Ordering::Acquire),
                    "PI observer must be running",
                );
                waiter
                    .set_policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
                    .unwrap();
                updated.store(true, Ordering::Release);
            })
            .unwrap()
    };
    let observer = {
        let observations = Arc::clone(&observations);
        ax_std::os::arceos::thread::builder("pi-equal-current".into())
            .affinity(cpu_mask(0))
            .policy(SchedulePolicy::fifo(
                RtPriority::new(current_priority).unwrap(),
            ))
            .spawn(move || {
                // Freeze this CPU's execution while the other CPU performs
                // the real policy/PI transaction. No timer or IPI handler can
                // consume the sticky reschedule publication before we read it.
                let exclusion = RawSpinLock::new(());
                let guard = exclusion.lock_irqsave();
                let before = current_immediate_preemption_requested().unwrap();
                update.store(true, Ordering::Release);
                let started = Instant::now();
                while !updated.load(Ordering::Acquire) {
                    assert!(
                        started.elapsed() < PROGRESS_TIMEOUT,
                        "PI update must finish remotely"
                    );
                    core::hint::spin_loop();
                }
                let after = current_immediate_preemption_requested().unwrap();
                observations.store(
                    4 | usize::from(before) | (usize::from(after) << 1),
                    Ordering::Release,
                );
                drop(guard);
            })
            .unwrap()
    };
    // Keep the owner runnable until the remote PI update has finished. A
    // join here before the observer runs would test an inactive owner instead.
    wait_until(
        || observations.load(Ordering::Acquire) & 4 != 0,
        "PI observer must record the queued-owner update",
    );
    observer.join().unwrap();
    controller.join().unwrap();
    assert_eq!(
        owner.effective_policy(),
        SchedulePolicy::fifo(RtPriority::new(80).unwrap())
    );
    drop(held);
    waiter.join().unwrap();
    current::set_current_thread_affinity(original_affinity).unwrap();
    let observations = observations.load(Ordering::Acquire);
    assert_eq!(
        observations & 1,
        0,
        "observer must start without pending preemption"
    );
    observations & 2 != 0
}

fn rt_spin_lock_remains_preemptible() {
    use ax_std::os::arceos::task::{
        sched::{CpuId, CpuSet},
        sync::{SpinLock, WaitQueue},
        thread::ThreadState,
    };
    let current = ax_std::os::arceos::task::thread::current::current_thread_handle().unwrap();
    let original_affinity = current.affinity().unwrap();
    pin_current_to_cpu(0);
    let mut affinity = CpuSet::empty(ax_hal::cpu_num());
    affinity.insert(CpuId::new(0));
    let gate = Arc::new(WaitQueue::new());
    let released = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(AtomicBool::new(false));
    let child_gate = Arc::clone(&gate);
    let child_released = Arc::clone(&released);
    let child_ran = Arc::clone(&ran);
    let worker = ax_std::os::arceos::thread::builder("rt-lock-preempt".into())
        .affinity(affinity)
        .policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
        .spawn(move || {
            child_gate.wait_until(|| child_released.load(Ordering::Acquire));
            child_ran.store(true, Ordering::Release);
        })
        .unwrap();
    wait_until(
        || worker.state() == ThreadState::Blocked,
        "RT lock probe must park first",
    );
    let lock = SpinLock::new(());
    {
        let _guard = lock.lock();
        assert!(matches!(
            ax_std::os::arceos::task::thread::current::validate_blocking_context(),
            Err(ax_std::os::arceos::task::thread::TaskError::UnsafeContext)
        ));
        let nested = SpinLock::new(());
        drop(nested.lock());
        released.store(true, Ordering::Release);
        gate.notify_one();
        assert!(
            ran.load(Ordering::Acquire),
            "Linux RT spinlock must allow higher-priority preemption while held"
        );
    }
    worker.join().unwrap();
    ax_std::os::arceos::task::thread::current::set_current_thread_affinity(original_affinity)
        .unwrap();
}

fn rt_lock_preserves_outer_timeout() {
    use ax_std::os::arceos::{
        api::time::ax_monotonic_time,
        task::{
            sched::{CpuId, CpuSet},
            sync::SpinLock,
            thread::{
                ThreadState,
                current::{self, CurrentParkStart},
            },
            time::MonotonicDeadline,
        },
    };
    let parent = current::current_thread_handle().unwrap();
    let original = parent.affinity().unwrap();
    pin_current_to_cpu(0);
    let lock = Arc::new(SpinLock::new(()));
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let restored = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let on_cpu = |cpu| {
        let mut mask = CpuSet::empty(ax_hal::cpu_num());
        mask.insert(CpuId::new(cpu));
        mask
    };
    let owner = {
        let lock = Arc::clone(&lock);
        let held = Arc::clone(&held);
        let release = Arc::clone(&release);
        ax_std::os::arceos::thread::builder("rt-timeout-owner".into())
            .affinity(on_cpu(1))
            .spawn(move || {
                let _guard = lock.lock();
                held.store(true, Ordering::Release);
                while !release.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
            })
            .unwrap()
    };
    wait_until(
        || held.load(Ordering::Acquire),
        "RT timeout owner must hold lock",
    );
    let helper = {
        let parent = parent.clone();
        let release = Arc::clone(&release);
        let restored = Arc::clone(&restored);
        let finished = Arc::clone(&finished);
        ax_std::os::arceos::thread::builder("rt-timeout-release".into())
            .affinity(on_cpu(1))
            .policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
            .spawn(move || {
                // Preempt the owner so rtmutex owner spinning must stop.
                wait_until(
                    || parent.state() == ThreadState::Blocked,
                    "RT waiter must block",
                );
                thread::sleep(Duration::from_millis(80));
                release.store(true, Ordering::Release);
                pin_current_to_cpu(2);
                wait_until(
                    || restored.load(Ordering::Acquire),
                    "RT waiter must acquire lock",
                );
                // A lost timeout is recovered only to expose the wrong park
                // disposition, rather than letting the regression hang QEMU.
                wait_until(
                    || finished.load(Ordering::Acquire) || parent.state() == ThreadState::Blocked,
                    "outer park must complete or block",
                );
                if !finished.load(Ordering::Acquire) {
                    parent.wake_handle().wake();
                }
            })
            .unwrap()
    };
    let mut park = loop {
        if let CurrentParkStart::Prepared(park) = current::begin_current_park().unwrap() {
            break park;
        }
    };
    park.arm_deadline(MonotonicDeadline::from_duration(
        ax_monotonic_time() + Duration::from_millis(40),
    ))
    .unwrap();
    drop(lock.lock());
    restored.store(true, Ordering::Release);
    let resumed = park.commit().unwrap();
    finished.store(true, Ordering::Release);
    assert!(
        resumed.was_notified_before_block(),
        "outer timeout must survive the RT-lock inner park"
    );
    owner.join().unwrap();
    helper.join().unwrap();
    current::set_current_thread_affinity(original).unwrap();
}

fn cpu_mask(cpu: u32) -> ax_std::os::arceos::task::sched::CpuSet {
    use ax_std::os::arceos::task::sched::{CpuId, CpuSet};
    let mut mask = CpuSet::empty(ax_hal::cpu_num());
    mask.insert(CpuId::new(cpu));
    mask
}

fn reader_drain_uses_lock_wake() {
    use ax_std::os::arceos::task::{
        sync::{RwSemaphore, SpinRwLock},
        thread::ThreadState,
    };
    pin_current_to_cpu(2);
    let lock = Arc::new(SpinRwLock::new(0));
    let reader = lock.read();
    let writer_lock = Arc::clone(&lock);
    let writer = ax_std::os::arceos::thread::builder("rt-rw-drain".into())
        .affinity(cpu_mask(0))
        .spawn(move || {
            *writer_lock.write() = 1;
        })
        .unwrap();
    wait_until(
        || writer.state() == ThreadState::Blocked,
        "RT writer must drain existing readers",
    );
    writer.wake_handle().wake();
    assert_eq!(
        writer.state(),
        ThreadState::Blocked,
        "ordinary wake must not release RT reader drain"
    );
    assert!(lock.try_write().is_none());
    drop(reader);
    writer.join().unwrap();
    assert_eq!(*lock.read(), 1);

    let semaphore = Arc::new(RwSemaphore::new(0));
    let reader = semaphore.read();
    let writer_lock = Arc::clone(&semaphore);
    let writer = ax_std::os::arceos::thread::builder("rwsem-drain".into())
        .affinity(cpu_mask(0))
        .spawn(move || {
            *writer_lock.write() = 2;
        })
        .unwrap();
    wait_until(
        || writer.state() == ThreadState::Blocked,
        "rwsem writer must drain existing readers",
    );
    drop(reader);
    writer.join().unwrap();
    assert_eq!(*semaphore.read(), 2);
}

fn nested_migration_defers_remote_affinity() {
    use ax_std::os::arceos::task::{sync::MigrationGuard, thread::current};
    pin_current_to_cpu(0);
    let parent = current::current_thread_handle().unwrap();
    let requested = Arc::new(AtomicBool::new(false));
    let first = MigrationGuard::new().unwrap();
    let second = MigrationGuard::new().unwrap();
    let setter = {
        let requested = Arc::clone(&requested);
        let parent = parent.clone();
        ax_std::os::arceos::thread::builder("pinned-affinity-setter".into())
            .affinity(cpu_mask(2))
            .spawn(move || {
                let completion = parent.request_affinity(cpu_mask(1)).unwrap();
                requested.store(true, Ordering::Release);
                completion.wait().unwrap();
            })
            .unwrap()
    };
    wait_until(
        || requested.load(Ordering::Acquire),
        "remote affinity must publish while pinned",
    );
    assert_eq!(parent.affinity().unwrap(), cpu_mask(1));
    assert_eq!(this_cpu_id(), 0);
    drop(first);
    thread::yield_now();
    assert_eq!(
        this_cpu_id(),
        0,
        "one surviving migration pin must retain CPU"
    );
    drop(second);
    wait_until(
        || this_cpu_id() == 1,
        "outermost migration enable must honor remote affinity",
    );
    setter.join().unwrap();
}

fn local_lock_serializes_preempting_tasks() {
    use ax_std::os::arceos::task::{
        sync::{LocalLock, WaitQueue},
        thread::ThreadState,
    };
    pin_current_to_cpu(0);
    let lock = Arc::new(LocalLock::new(|_| 0usize).unwrap());
    let gate = Arc::new(WaitQueue::new());
    let start = Arc::new(AtomicBool::new(false));
    let child = {
        let lock = Arc::clone(&lock);
        let gate = Arc::clone(&gate);
        let start = Arc::clone(&start);
        ax_std::os::arceos::thread::builder("local-lock-preempt".into())
            .affinity(cpu_mask(0))
            .policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
            .spawn(move || {
                gate.wait_until(|| start.load(Ordering::Acquire));
                *lock.lock() += 1;
            })
            .unwrap()
    };
    wait_until(
        || child.state() == ThreadState::Blocked,
        "local lock contender must wait for start",
    );
    {
        let mut owner = lock.lock();
        *owner = 10;
        start.store(true, Ordering::Release);
        gate.notify_one();
        assert_eq!(
            child.state(),
            ThreadState::Blocked,
            "preempting local lock contender must sleep"
        );
        assert_eq!(*owner, 10);
    }
    child.join().unwrap();
    assert_eq!(*lock.lock(), 11);
}

fn semaphore_grants_and_cancellation() {
    use ax_std::os::arceos::{
        api::time::ax_monotonic_time,
        task::{
            sync::{Semaphore, SemaphoreError},
            thread::ThreadState,
            time::MonotonicDeadline,
        },
    };
    let semaphore = Arc::new(Semaphore::new(0));
    ax_std::os::arceos::task::sync::fail_next_semaphore_timer_registration();
    let failure_deadline =
        MonotonicDeadline::from_duration(ax_monotonic_time() + Duration::from_secs(1));
    assert!(matches!(
        semaphore.down_until(failure_deadline),
        Err(SemaphoreError::Task(
            ax_std::os::arceos::task::thread::TaskError::TimerCapacity
        ))
    ));
    assert_eq!(
        ax_std::os::arceos::task::thread::current::current_thread_handle()
            .unwrap()
            .state(),
        ThreadState::Running
    );

    let completed = Arc::new(AtomicUsize::new(0));
    let first = {
        let semaphore = Arc::clone(&semaphore);
        let completed = Arc::clone(&completed);
        ax_std::os::arceos::thread::builder("semaphore-fifo-first".into())
            .affinity(cpu_mask(0))
            .spawn(move || {
                semaphore.down().unwrap();
                assert_eq!(completed.fetch_add(1, Ordering::AcqRel), 0);
            })
            .unwrap()
    };
    wait_until(
        || first.state() == ThreadState::Blocked,
        "first semaphore waiter must queue",
    );
    let second = {
        let semaphore = Arc::clone(&semaphore);
        let completed = Arc::clone(&completed);
        ax_std::os::arceos::thread::builder("semaphore-fifo-second".into())
            .affinity(cpu_mask(0))
            .spawn(move || {
                semaphore.down().unwrap();
                assert_eq!(completed.fetch_add(1, Ordering::AcqRel), 1);
            })
            .unwrap()
    };
    wait_until(
        || second.state() == ThreadState::Blocked,
        "second semaphore waiter must queue",
    );
    semaphore.up().unwrap();
    assert!(
        !semaphore.try_down(),
        "a selected FIFO grant cannot be stolen"
    );
    first.join().unwrap();
    assert_eq!(second.state(), ThreadState::Blocked);
    semaphore.up().unwrap();
    second.join().unwrap();
    assert!(matches!(
        semaphore.down_interruptible(|| true),
        Err(SemaphoreError::Interrupted)
    ));
    let deadline = MonotonicDeadline::from_duration(ax_monotonic_time() + Duration::from_millis(5));
    assert!(matches!(
        semaphore.down_until(deadline),
        Err(SemaphoreError::TimedOut)
    ));
    semaphore.up().unwrap();
    assert!(
        semaphore.try_down(),
        "cancelled waiters must not absorb a later permit"
    );
    assert!(!semaphore.try_down());
}

fn semaphore_hard_irq_release() {
    use ax_std::os::arceos::{
        api::time::ax_monotonic_time,
        task::{
            sync::{Semaphore, SpinLock},
            time::{
                MonotonicDeadline,
                hard_timer::{
                    HardKernelTimerAction, HardKernelTimerCallback,
                    register_hard_restartable_kernel_timer,
                },
            },
        },
    };
    let semaphore = Arc::new(Semaphore::new(0));
    let target = Arc::clone(&semaphore);
    let callback = unsafe {
        // SAFETY: Semaphore::up uses raw IRQ-safe state and a borrowed wake
        // capability. The callback only performs nonblocking context probes;
        // callback allocation reclamation belongs to the task timer service.
        HardKernelTimerCallback::new(std::boxed::Box::new(move |_| {
            assert!(SpinLock::new(()).try_lock().is_none());
            assert!(
                Mutex::new(()).try_lock().is_none(),
                "sleeping mutex trylock must reject hard IRQ"
            );
            target.up().unwrap();
            HardKernelTimerAction::Complete
        }))
    };
    register_hard_restartable_kernel_timer(
        MonotonicDeadline::from_duration(ax_monotonic_time() + Duration::from_millis(5)),
        callback,
    )
    .unwrap();
    semaphore.down().unwrap();
    assert!(!semaphore.try_down());
}

use core::{
    future::poll_fn,
    task::{Poll, Waker},
};
use std::{
    os::arceos::{
        modules::ax_hal,
        task::{
            CpuId, CpuSet, LocalExecutor, WaitQueue, current_thread_handle, current_thread_id,
            set_current_thread_affinity, set_thread_affinity,
            spawn_raw_with_extension_and_affinity,
        },
    },
    string::String,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use ax_kspin::{PreemptGuard, SpinNoIrq};

const NUM_TASKS: usize = 16;
const NUM_TIMES: usize = 32;

fn assert_irq_enabled() {
    assert!(
        ax_hal::asm::irqs_enabled(),
        "Task id = {:?} IRQs should be enabled",
        thread::current().id()
    );
}

fn assert_irq_disabled() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "Task id = {:?} IRQs should be disabled",
        thread::current().id()
    );
}

fn assert_irq_enabled_and_disabled() {
    assert_irq_enabled();
    ax_hal::asm::disable_irqs();
    assert_irq_disabled();
    ax_hal::asm::enable_irqs();
}

fn test_yielding() {
    static FINISHED: AtomicUsize = AtomicUsize::new(0);
    FINISHED.store(0, Ordering::Release);
    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            assert_irq_enabled();
            for _ in 0..NUM_TIMES {
                assert_irq_enabled();
                thread::yield_now();
                assert_irq_enabled_and_disabled();
            }
            FINISHED.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED.load(Ordering::Acquire) < NUM_TASKS {
        thread::yield_now();
        assert_irq_enabled_and_disabled();
    }
}

fn test_sleep() {
    static FINISHED: AtomicUsize = AtomicUsize::new(0);
    FINISHED.store(0, Ordering::Release);

    assert_irq_enabled();
    thread::sleep(Duration::from_millis(100));
    assert_irq_enabled_and_disabled();

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            for _ in 0..2 {
                assert_irq_enabled();
                thread::sleep(Duration::from_millis(100));
                assert_irq_enabled_and_disabled();
            }
            FINISHED.fetch_add(1, Ordering::Release);
        });
    }

    while FINISHED.load(Ordering::Acquire) < NUM_TASKS {
        thread::sleep(Duration::from_millis(10));
    }
}

fn test_wait_queue() {
    static WQ1: WaitQueue = WaitQueue::new();
    static WQ2: WaitQueue = WaitQueue::new();
    static WQ3: WaitQueue = WaitQueue::new();
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    static GO: AtomicBool = AtomicBool::new(false);

    COUNTER.store(0, Ordering::Release);
    GO.store(false, Ordering::Release);

    for _ in 0..NUM_TASKS {
        thread::spawn(move || {
            assert_irq_enabled();
            WQ3.wait_timeout_until(Duration::from_millis(50), || false);
            assert_irq_enabled_and_disabled();
            COUNTER.fetch_add(1, Ordering::Release);
            WQ1.notify_one();
            assert_irq_enabled();
            WQ2.wait_until(|| GO.load(Ordering::Acquire));
            assert_irq_enabled_and_disabled();
            COUNTER.fetch_sub(1, Ordering::Release);
            WQ1.notify_one();
        });
    }

    assert_irq_enabled();
    WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == NUM_TASKS);
    assert_irq_enabled_and_disabled();
    GO.store(true, Ordering::Release);
    WQ2.notify_all();
    assert_irq_enabled();
    WQ1.wait_until(|| COUNTER.load(Ordering::Acquire) == 0);
    assert_irq_enabled_and_disabled();
}

fn test_irq_return_preemption() {
    static RAN: AtomicBool = AtomicBool::new(false);
    RAN.store(false, Ordering::Release);
    thread::spawn(|| RAN.store(true, Ordering::Release));

    // Do not yield or block here: only timer-IRQ return preemption can let the
    // newly ready peer run. The monotonic timer continues to advance on the
    // broken implementation, making this a deterministic finite failure.
    let start = std::time::Instant::now();
    while !RAN.load(Ordering::Acquire) {
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "ready task was not scheduled from the timer IRQ return path"
        );
        core::hint::spin_loop();
    }
    assert_irq_enabled_and_disabled();
}

fn test_executor_remote_wake_during_scheduler_park_entry() {
    static OWNER_ENTERED_PARK: AtomicBool = AtomicBool::new(false);
    static FUTURE_READY: AtomicBool = AtomicBool::new(false);
    static REMOTE_WAKE_SETTLED: AtomicBool = AtomicBool::new(false);

    let cpu_count = ax_hal::cpu_num();
    assert!(
        cpu_count >= 2,
        "executor remote-wake regression requires SMP >= 2, got {cpu_count}"
    );

    OWNER_ENTERED_PARK.store(false, Ordering::Release);
    FUTURE_READY.store(false, Ordering::Release);
    REMOTE_WAKE_SETTLED.store(false, Ordering::Release);

    let owner_cpu = CpuId::new(0);
    let remote_cpu = CpuId::new(1);
    let mut owner_only = CpuSet::empty(cpu_count);
    assert!(owner_only.insert(owner_cpu));
    let mut remote_only = CpuSet::empty(cpu_count);
    assert!(remote_only.insert(remote_cpu));
    set_current_thread_affinity(owner_only).expect("failed to pin executor owner to CPU0");

    let saved_waker = Arc::new(SpinNoIrq::new(None::<Waker>));
    let remote_saved_waker = Arc::clone(&saved_waker);
    let remote = thread::spawn(move || {
        set_current_thread_affinity(remote_only).expect("failed to pin executor waker to CPU1");
        while !OWNER_ENTERED_PARK.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        FUTURE_READY.store(true, Ordering::Release);
        remote_saved_waker
            .lock()
            .as_ref()
            .expect("executor future did not publish its waker")
            .wake_by_ref();

        // Keep the owner spinning long enough for the scheduler IPI to drain
        // the direct wake while its lifecycle is still Running. The executor
        // ready bit must independently abort the following WaitQueue park.
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_millis(5) {
            core::hint::spin_loop();
        }
        REMOTE_WAKE_SETTLED.store(true, Ordering::Release);
    });

    let owner = current_thread_handle().expect("executor owner has no scheduler handle");
    let executor = LocalExecutor::new(owner.wake_handle())
        .expect("executor owner identity must match current thread");
    let wait = WaitQueue::new();
    executor.run(
        {
            let saved_waker = Arc::clone(&saved_waker);
            poll_fn(move |context| {
                if FUTURE_READY.load(Ordering::Acquire) {
                    return Poll::Ready(());
                }
                *saved_waker.lock() = Some(context.waker().clone());
                if FUTURE_READY.load(Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
        },
        |condition| {
            OWNER_ENTERED_PARK.store(true, Ordering::Release);
            while !REMOTE_WAKE_SETTLED.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            wait.wait_until(|| condition.should_abort());
        },
    );
    remote.join().unwrap();

    assert!(FUTURE_READY.load(Ordering::Acquire));
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("failed to restore executor owner affinity");
}

fn test_irq_return_migration_keeps_percpu_base() {
    static EXPECTED_THREAD: AtomicU64 = AtomicU64::new(0);
    static EXPECTED_READY: AtomicBool = AtomicBool::new(false);
    static VICTIM_STARTED: AtomicBool = AtomicBool::new(false);
    static VICTIM_FINISHED: AtomicBool = AtomicBool::new(false);

    let cpu_count = ax_hal::cpu_num();
    assert!(
        cpu_count >= 2,
        "IRQ-return migration regression requires SMP >= 2, got {cpu_count}"
    );

    EXPECTED_READY.store(false, Ordering::Release);
    VICTIM_STARTED.store(false, Ordering::Release);
    VICTIM_FINISHED.store(false, Ordering::Release);

    let source_cpu = CpuId::new(0);
    let destination_cpu = CpuId::new(1);
    let mut source_only = CpuSet::empty(cpu_count);
    assert!(source_only.insert(source_cpu));
    let mut destination_only = CpuSet::empty(cpu_count);
    assert!(destination_only.insert(destination_cpu));

    set_current_thread_affinity(destination_only.clone())
        .expect("failed to pin migration coordinator");
    assert_eq!(ax_hal::percpu::this_cpu_id(), destination_cpu.as_usize());
    let destination_guard = PreemptGuard::new();
    let destination_pin = ax_percpu::bound_current(destination_guard.cpu_pin())
        .expect("CPU1 area must remain bound while migration is disabled");
    let destination_cookie = destination_pin.cookie();
    assert_ne!(destination_cookie, 0, "CPU-area cookie was not published");
    assert_eq!(
        destination_pin.cpu_index(),
        ax_percpu::CpuIndex::try_from(destination_cpu.as_usize()).unwrap(),
        "coordinator did not observe the CPU1 area header"
    );
    drop(destination_guard);

    let victim = unsafe {
        // SAFETY: the test transfers no extension ownership and retains the
        // returned scheduler handle until the thread reports completion.
        spawn_raw_with_extension_and_affinity(
            move || {
                while !EXPECTED_READY.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
                let expected = EXPECTED_THREAD.load(Ordering::Acquire);

                assert_eq!(ax_hal::percpu::this_cpu_id(), source_cpu.as_usize());
                thread::sleep(Duration::from_millis(20));
                assert_eq!(
                    ax_hal::percpu::this_cpu_id(),
                    source_cpu.as_usize(),
                    "timer IRQ wake did not return to the pinned source CPU"
                );
                VICTIM_STARTED.store(true, Ordering::Release);

                loop {
                    let current = current_thread_id()
                        .expect("migrating thread lost its current-thread identity")
                        .as_u64();
                    assert_eq!(
                        current, expected,
                        "IRQ-return migration restored a stale per-CPU base"
                    );
                    if ax_hal::percpu::this_cpu_id() == destination_cpu.as_usize() {
                        let guard = PreemptGuard::new();
                        let bound_pin = ax_percpu::bound_current(guard.cpu_pin())
                            .expect("migrated thread must observe a bound CPU area");
                        assert_eq!(
                            bound_pin.cpu_index(),
                            ax_percpu::CpuIndex::try_from(destination_cpu.as_usize()).unwrap(),
                            "safe CpuPin access observed a stale CPU-area index"
                        );
                        assert_eq!(
                            bound_pin.cookie(),
                            destination_cookie,
                            "safe CpuPin access observed a stale CPU-area cookie"
                        );
                        break;
                    }
                    core::hint::spin_loop();
                }
                VICTIM_FINISHED.store(true, Ordering::Release);
            },
            String::from("irq-return-migration"),
            64 * 1024,
            None,
            Some(source_only),
        )
        .expect("failed to create migration victim")
    };
    EXPECTED_THREAD.store(victim.id().as_u64(), Ordering::Release);
    EXPECTED_READY.store(true, Ordering::Release);

    while !VICTIM_STARTED.load(Ordering::Acquire) {
        thread::yield_now();
    }
    set_thread_affinity(victim.id(), destination_only)
        .expect("failed to request IRQ-return migration");

    let start = std::time::Instant::now();
    while !VICTIM_FINISHED.load(Ordering::Acquire) {
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "IRQ-return migration did not reach the destination CPU"
        );
        thread::yield_now();
    }
    while victim.state() != std::os::arceos::task::ThreadState::Exited {
        thread::yield_now();
    }
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("failed to restore coordinator affinity");
}

pub fn run() -> crate::TestResult {
    test_irq_return_preemption();
    test_executor_remote_wake_during_scheduler_park_entry();
    test_irq_return_migration_keeps_percpu_base();
    test_yielding();
    test_sleep();
    test_wait_queue();
    Ok(())
}

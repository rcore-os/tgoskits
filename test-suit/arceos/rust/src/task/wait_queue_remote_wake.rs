use core::{hint, sync::atomic::AtomicUsize};
use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        guard::PreemptGuard,
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{
                CpuId, CpuSet, CurrentParkStart, FairMode, Nice, RtPriority, SchedulePolicy,
                ThreadId, ThreadWakeHandle, WakeResult, begin_current_park, current_thread_handle,
                current_thread_id, schedule_current_cpu, scheduler_wait_test_hooks,
                set_thread_affinity, set_thread_policy, task_test_hooks, thread_runtime,
            },
        },
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static SLEEP_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static DONE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static TIMEOUT_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static OCCUPIER_READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static SWITCH_WAKE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static MAY_SLEEP: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static SLEEPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static EXPECTED_SLEEPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

const REMOTE_WAKE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(1);
// The semantic regression is the delayed rq-membership assertion below.  Give
// a TCG-scheduled peer enough host time to complete the already-committed
// context-switch tail before using it as the non-preemptible current entity.
const OCCUPIER_CURRENT_TIMEOUT: Duration = Duration::from_secs(5);
const AFFINITY_MIGRATION_TIMEOUT: Duration = Duration::from_secs(5);
const WORKER_READY_TIMEOUT: Duration = Duration::from_secs(1);
const WAITER_BLOCK_TIMEOUT: Duration = Duration::from_secs(1);

fn exercise_notified_park_runtime(waker_cpu: usize, sleeper_cpu: usize) {
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let ready = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let worker_ready = Arc::clone(&ready);
    let worker = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle().expect("park-race worker must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("park-race worker must accept its FIFO policy");
        let park = match begin_current_park().expect("park-race worker must prepare its park") {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("park-race worker consumed an unexpected notification")
            }
        };
        task_test_hooks::arm_park_after_final_wake_check(current.id())
            .expect("park-race worker must arm the real scheduler hook");
        *worker_wake_handle.lock() = Some(current.wake_handle());
        worker_ready.store(true, Ordering::Release);
        let resume = park
            .commit()
            .expect("wake after the final park check must cancel blocking");
        assert!(
            resume.was_notified_before_block(),
            "a wake after the final park check must report that schedule-out was cancelled"
        );
        assert!(
            thread_runtime(current.id())
                .expect("resumed park-race runtime must be readable")
                .is_running(),
            "a notified park that keeps rq->curr must retain its running interval"
        );
    });

    let ready_started = Instant::now();
    while !ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "park-race worker did not publish its wake handle"
        );
        thread::yield_now();
    }
    let hook_started = Instant::now();
    while !task_test_hooks::park_after_final_wake_check_entered() {
        assert!(
            hook_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "park-race worker did not reach the post-accounting wake window"
        );
        thread::yield_now();
    }
    assert_eq!(this_cpu_id(), waker_cpu);
    let wake_handle = wake_handle
        .lock()
        .clone()
        .expect("park-race worker must publish a wake handle");
    assert_eq!(wake_handle.wake_from_task(), WakeResult::Notified);
    task_test_hooks::complete_park_after_final_wake_check();
    worker.join().expect("park-race worker must exit normally");
}

fn exercise_rt_park_releases_task_lock_while_publication_reader_waits(
    observer_cpu: usize,
    sleeper_cpu: usize,
    waker_cpu: usize,
) {
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let sleeper_id = Arc::new(Mutex::new(None::<ThreadId>));
    let ready = Arc::new(AtomicBool::new(false));
    let may_park = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let worker_sleeper_id = Arc::clone(&sleeper_id);
    let worker_ready = Arc::clone(&ready);
    let worker_may_park = Arc::clone(&may_park);
    let worker = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current =
            current_thread_handle().expect("publication-wait worker must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("publication-wait worker must accept its FIFO policy");
        let park = match begin_current_park().expect("publication-wait worker must prepare park") {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("publication-wait worker consumed an unexpected notification")
            }
        };
        task_test_hooks::arm_park_after_blocked_publication(current.id())
            .expect("publication-wait worker must arm the blocked-publication hook");
        *worker_wake_handle.lock() = Some(current.wake_handle());
        *worker_sleeper_id.lock() = Some(current.id());
        worker_ready.store(true, Ordering::Release);
        while !worker_may_park.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        park.commit()
            .expect("publication-wait worker must resume after its direct wake");
    });

    let ready_started = Instant::now();
    while !ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "publication-wait worker did not publish its handle"
        );
        thread::yield_now();
    }
    assert_eq!(this_cpu_id(), observer_cpu);
    let sleeper_id = sleeper_id
        .lock()
        .as_ref()
        .copied()
        .expect("publication-wait worker must publish its identity");
    let wake_handle = wake_handle
        .lock()
        .clone()
        .expect("publication-wait worker must publish its wake handle");
    let waker_ready = Arc::new(AtomicBool::new(false));
    let waker_go = Arc::new(AtomicBool::new(false));
    let wake_returned = Arc::new(AtomicBool::new(false));
    let thread_waker_ready = Arc::clone(&waker_ready);
    let thread_waker_go = Arc::clone(&waker_go);
    let thread_wake_returned = Arc::clone(&wake_returned);
    let waker = thread::spawn(move || {
        pin_current_to_cpu(waker_cpu);
        thread_waker_ready.store(true, Ordering::Release);
        while !thread_waker_go.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        let result = wake_handle.wake_from_task();
        thread_wake_returned.store(true, Ordering::Release);
        result
    });
    let waker_ready_started = Instant::now();
    while !waker_ready.load(Ordering::Acquire) {
        assert!(
            waker_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "publication waker did not become runnable on its target CPU"
        );
        thread::yield_now();
    }
    may_park.store(true, Ordering::Release);
    let blocked_started = Instant::now();
    while !task_test_hooks::park_after_blocked_publication_entered() {
        assert!(
            blocked_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "publication-wait worker did not reach its blocked publication"
        );
        thread::yield_now();
    }
    let _ = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    task_test_hooks::arm_thread_sched_publication_wait(sleeper_id);
    waker_go.store(true, Ordering::Release);
    let publication_wait_started = Instant::now();
    while !task_test_hooks::thread_sched_publication_wait_entered() {
        assert!(
            publication_wait_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the direct waker did not wait for detached-owner publication"
        );
        thread::yield_now();
    }
    assert!(
        task_test_hooks::thread_sched_lock_available(sleeper_id)
            .expect("RT park task-lock availability must be observable"),
        "the rq-only FIFO/RR parker must not retain the task scheduler lock while publishing"
    );
    assert!(
        !wake_returned.load(Ordering::Acquire),
        "a direct waker must not pass the task lock before rq removal and detached-owner \
         installation"
    );
    task_test_hooks::complete_park_after_blocked_publication();
    assert_eq!(
        waker.join().expect("publication waker must exit normally"),
        WakeResult::Notified
    );
    worker
        .join()
        .expect("publication-wait worker must exit normally");
    let waits = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    assert!(
        waits.detached_publication_waits > 0,
        "a task-lock reader must observe the detached-owner publication marker"
    );
    assert!(
        waits.detached_publication_wait_iterations > 0,
        "a task-lock reader must remain outside the task lock until publication completes"
    );
}

fn exercise_rt_park_does_not_wait_for_task_lock(
    observer_cpu: usize,
    sleeper_cpu: usize,
    lock_holder_cpu: usize,
) {
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let sleeper_id = Arc::new(Mutex::new(None::<ThreadId>));
    let ready = Arc::new(AtomicBool::new(false));
    let may_park = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let worker_sleeper_id = Arc::clone(&sleeper_id);
    let worker_ready = Arc::clone(&ready);
    let worker_may_park = Arc::clone(&may_park);
    let worker = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle()
            .expect("RT publication serialization worker must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("RT publication serialization worker must accept FIFO policy");
        let park = match begin_current_park()
            .expect("RT publication serialization worker must prepare park")
        {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("RT publication serialization worker consumed an unexpected notification")
            }
        };
        task_test_hooks::arm_park_before_active_publication(current.id())
            .expect("RT publication worker must arm its pre-publication hook");
        task_test_hooks::arm_park_publication_serialization(current.id());
        *worker_wake_handle.lock() = Some(current.wake_handle());
        *worker_sleeper_id.lock() = Some(current.id());
        worker_ready.store(true, Ordering::Release);
        while !worker_may_park.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        park.commit()
            .expect("RT publication serialization worker must resume after wake");
    });

    let ready_started = Instant::now();
    while !ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "RT publication worker did not publish its identity"
        );
        thread::yield_now();
    }
    assert_eq!(this_cpu_id(), observer_cpu);
    let sleeper_id = sleeper_id
        .lock()
        .as_ref()
        .copied()
        .expect("RT publication worker must publish its identity");
    let wake_handle = wake_handle
        .lock()
        .clone()
        .expect("RT publication worker must publish its wake handle");

    task_test_hooks::arm_thread_sched_lock_hold(sleeper_id);
    let holder = thread::spawn(move || {
        pin_current_to_cpu(lock_holder_cpu);
        task_test_hooks::hold_thread_sched_lock(sleeper_id)
            .expect("the RT publication task-lock holder must complete");
    });
    let holder_started = Instant::now();
    while !task_test_hooks::thread_sched_lock_hold_entered() {
        assert!(
            holder_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the RT publication task-lock holder did not acquire the lock"
        );
        thread::yield_now();
    }

    may_park.store(true, Ordering::Release);
    let prepublication_started = Instant::now();
    while !task_test_hooks::park_before_active_publication_entered() {
        assert!(
            prepublication_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "RT publication worker did not reach its owner-rq publication window"
        );
        thread::yield_now();
    }

    task_test_hooks::complete_park_before_active_publication();
    let observation_started = Instant::now();
    while !task_test_hooks::park_publication_serialization_observed() {
        assert!(
            observation_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the rq-only RT park did not record its task-lock publication decision"
        );
        thread::yield_now();
    }
    let observation = task_test_hooks::take_park_publication_serialization()
        .expect("the rq-only RT publication decision must be available");

    let blocked_started = Instant::now();
    while !task_test_hooks::thread_is_blocked(sleeper_id.as_u64()) {
        assert!(
            blocked_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the rq-only RT park waited for an unrelated task-lock owner"
        );
        thread::yield_now();
    }
    assert_eq!(
        observation,
        task_test_hooks::ParkPublicationSerialization {
            task_lock_busy: false,
            publication_started: true,
        },
        "an rq-only RT park must reserve detached ownership without probing the task lock"
    );
    task_test_hooks::complete_thread_sched_lock_hold();
    holder
        .join()
        .expect("the RT publication task-lock holder must exit normally");
    assert_eq!(wake_handle.wake_from_task(), WakeResult::Notified);
    worker
        .join()
        .expect("RT publication serialization worker must exit normally");
}

fn exercise_rt_park_uses_detached_publication_without_task_lock(
    observer_cpu: usize,
    sleeper_cpu: usize,
) {
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let sleeper_id = Arc::new(Mutex::new(None::<ThreadId>));
    let ready = Arc::new(AtomicBool::new(false));
    let may_park = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let worker_sleeper_id = Arc::clone(&sleeper_id);
    let worker_ready = Arc::clone(&ready);
    let worker_may_park = Arc::clone(&may_park);
    let worker = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle()
            .expect("RT detached-publication worker must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("RT detached-publication worker must accept FIFO policy");
        let park =
            match begin_current_park().expect("RT detached-publication worker must prepare park") {
                CurrentParkStart::Prepared(park) => park,
                CurrentParkStart::Notified => {
                    panic!("RT detached-publication worker consumed an unexpected notification")
                }
            };
        task_test_hooks::arm_park_before_active_publication(current.id())
            .expect("RT detached-publication worker must arm its owner-rq hook");
        task_test_hooks::arm_park_publication_serialization(current.id());
        *worker_wake_handle.lock() = Some(current.wake_handle());
        *worker_sleeper_id.lock() = Some(current.id());
        worker_ready.store(true, Ordering::Release);
        while !worker_may_park.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        park.commit()
            .expect("RT detached-publication worker must resume after wake");
    });

    let ready_started = Instant::now();
    while !ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "RT detached-publication worker did not publish its identity"
        );
        thread::yield_now();
    }
    assert_eq!(this_cpu_id(), observer_cpu);
    let sleeper_id = sleeper_id
        .lock()
        .as_ref()
        .copied()
        .expect("RT detached-publication worker must publish its identity");
    let wake_handle = wake_handle
        .lock()
        .clone()
        .expect("RT detached-publication worker must publish its wake handle");

    task_test_hooks::arm_park_irq_owner_probe(sleeper_id.as_u64());

    may_park.store(true, Ordering::Release);
    let park_started = Instant::now();
    while !task_test_hooks::park_before_active_publication_entered() {
        assert!(
            park_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "RT detached-publication worker did not reach its owner-rq window"
        );
        thread::yield_now();
    }
    task_test_hooks::complete_park_before_active_publication();

    let observation_started = Instant::now();
    while !task_test_hooks::park_publication_serialization_observed() {
        assert!(
            observation_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "RT detached-publication park did not record its publication decision"
        );
        thread::yield_now();
    }
    let observation = task_test_hooks::take_park_publication_serialization()
        .expect("RT detached-publication park decision must be available");
    let blocked_started = Instant::now();
    while !task_test_hooks::thread_is_blocked(sleeper_id.as_u64()) {
        assert!(
            blocked_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "RT detached-publication park did not reach Blocked"
        );
        thread::yield_now();
    }
    assert_eq!(wake_handle.wake_from_task(), WakeResult::Notified);
    worker
        .join()
        .expect("RT detached-publication worker must exit normally");

    assert_eq!(
        observation,
        task_test_hooks::ParkPublicationSerialization {
            task_lock_busy: false,
            publication_started: true,
        },
        "an uncontended FIFO/RR park must reserve detached ownership without taking the task lock"
    );
    assert_eq!(
        task_test_hooks::take_park_irq_owner_entries(),
        Some(task_test_hooks::ParkIrqOwnerEntries {
            thread_sched_acquired: 0,
            thread_sched: 0,
            run_queue: 0,
        }),
        "Linux-style FIFO/RR current blocking must remain an rq-only transaction"
    );
}

fn exercise_wait_claim_does_not_block_rt_publication(
    observer_cpu: usize,
    sleeper_cpu: usize,
    waker_cpu: usize,
) {
    static PUBLICATION_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();

    let sleeper_id = Arc::new(Mutex::new(None::<ThreadId>));
    let ready = Arc::new(AtomicBool::new(false));
    let may_wait = Arc::new(AtomicBool::new(false));
    let predicate = Arc::new(AtomicBool::new(false));
    let worker_sleeper_id = Arc::clone(&sleeper_id);
    let worker_ready = Arc::clone(&ready);
    let worker_may_wait = Arc::clone(&may_wait);
    let worker_predicate = Arc::clone(&predicate);
    let worker = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current =
            current_thread_handle().expect("wait-claim publication worker must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("wait-claim publication worker must accept its FIFO policy");
        task_test_hooks::arm_park_before_active_publication(current.id())
            .expect("wait-claim worker must arm its pre-publication hook");
        task_test_hooks::arm_park_after_blocked_publication(current.id())
            .expect("wait-claim worker must arm its blocked-publication hook");
        task_test_hooks::arm_park_publication_serialization(current.id());
        *worker_sleeper_id.lock() = Some(current.id());
        worker_ready.store(true, Ordering::Release);
        while !worker_may_wait.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        api::ax_wait_queue_wait_until(
            &PUBLICATION_WQ,
            || worker_predicate.load(Ordering::Acquire),
            None,
        );
    });

    let ready_started = Instant::now();
    while !ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "wait-claim publication worker did not publish its identity"
        );
        thread::yield_now();
    }
    assert_eq!(this_cpu_id(), observer_cpu);
    let sleeper_id = sleeper_id
        .lock()
        .as_ref()
        .copied()
        .expect("wait-claim publication worker must publish its identity");
    let waker_ready = Arc::new(AtomicBool::new(false));
    let waker_go = Arc::new(AtomicBool::new(false));
    let wake_returned = Arc::new(AtomicBool::new(false));
    let thread_waker_ready = Arc::clone(&waker_ready);
    let thread_waker_go = Arc::clone(&waker_go);
    let thread_wake_returned = Arc::clone(&wake_returned);
    let waker = thread::spawn(move || {
        pin_current_to_cpu(waker_cpu);
        thread_waker_ready.store(true, Ordering::Release);
        while !thread_waker_go.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        let woken = api::ax_wait_queue_wake(&PUBLICATION_WQ, 1);
        thread_wake_returned.store(true, Ordering::Release);
        woken
    });
    let waker_ready_started = Instant::now();
    while !waker_ready.load(Ordering::Acquire) {
        assert!(
            waker_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "wait-claim publication waker did not become runnable"
        );
        thread::yield_now();
    }

    may_wait.store(true, Ordering::Release);
    let prepublication_started = Instant::now();
    while !task_test_hooks::park_before_active_publication_entered() {
        assert!(
            prepublication_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "wait-claim worker did not reach its pre-publication window"
        );
        thread::yield_now();
    }
    task_test_hooks::arm_wait_claim_before_wake(sleeper_id);
    predicate.store(true, Ordering::Release);
    waker_go.store(true, Ordering::Release);
    let claim_started = Instant::now();
    while !task_test_hooks::wait_claim_before_wake_entered() {
        assert!(
            claim_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "wait-claim waker did not acquire its selected claim"
        );
        thread::yield_now();
    }

    task_test_hooks::arm_thread_sched_publication_wait(sleeper_id);
    task_test_hooks::complete_park_before_active_publication();
    let observation_started = Instant::now();
    while !task_test_hooks::park_publication_serialization_observed() {
        assert!(
            observation_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the wait-claim task owner did not force the rq-only RT park fallback"
        );
        thread::yield_now();
    }
    let observation = task_test_hooks::take_park_publication_serialization()
        .expect("the wait-claim RT publication decision must be available");
    let blocked_started = Instant::now();
    while !task_test_hooks::park_after_blocked_publication_entered() {
        assert!(
            blocked_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the rq-only RT park did not pause after publishing Blocked"
        );
        thread::yield_now();
    }
    task_test_hooks::complete_wait_claim_before_wake();
    let publication_wait_started = Instant::now();
    while !task_test_hooks::thread_sched_publication_wait_entered() {
        assert!(
            publication_wait_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "a wait claim that observed Blocked did not revalidate detached publication"
        );
        thread::yield_now();
    }
    assert!(
        task_test_hooks::thread_sched_lock_available(sleeper_id)
            .expect("wait-claim task-lock availability must be observable"),
        "the wait claim must release stale task state while rq publication finishes"
    );
    assert!(
        !wake_returned.load(Ordering::Acquire),
        "the wait claim must not return before Blocked, on_rq, and detached ownership agree"
    );
    task_test_hooks::complete_park_after_blocked_publication();
    assert_eq!(
        waker.join().expect("wait-claim waker must exit normally"),
        1
    );
    worker
        .join()
        .expect("wait-claim publication worker must exit normally");
    assert_eq!(
        observation,
        task_test_hooks::ParkPublicationSerialization {
            task_lock_busy: false,
            publication_started: true,
        },
        "a wait claim holding the task lock must not make current blocking reacquire that lock"
    );
}

fn exercise_direct_wake_retries_failed_delivery(sleeper_cpu: usize) {
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let ready = Arc::new(AtomicBool::new(false));
    let resumed = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let worker_ready = Arc::clone(&ready);
    let worker_resumed = Arc::clone(&resumed);
    let worker = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle().expect("direct-wake worker must have a task handle");
        let park = match begin_current_park().expect("direct-wake worker must prepare its park") {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("direct-wake worker consumed an unexpected notification")
            }
        };
        *worker_wake_handle.lock() = Some(current.wake_handle());
        worker_ready.store(true, Ordering::Release);
        park.commit()
            .expect("one direct wake must survive a transient delivery failure");
        worker_resumed.store(true, Ordering::Release);
    });

    let ready_started = Instant::now();
    while !ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "direct-wake worker did not publish its handle"
        );
        thread::yield_now();
    }
    let wake_handle = wake_handle
        .lock()
        .clone()
        .expect("direct-wake worker must publish a wake handle");
    let thread = wake_handle.thread_id().as_u64();
    let blocked_started = Instant::now();
    while !task_test_hooks::thread_is_blocked(thread) {
        assert!(
            blocked_started.elapsed() < WAITER_BLOCK_TIMEOUT,
            "direct-wake worker did not become blocked"
        );
        thread::yield_now();
    }

    task_test_hooks::arm_direct_wake_delivery_failure(thread);
    let waker_handle = wake_handle.clone();
    let waker = thread::spawn(move || waker_handle.wake_from_task());
    let paused_started = Instant::now();
    while !task_test_hooks::direct_wake_delivery_failure_paused() {
        assert!(
            paused_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "direct wake did not pause in its delivery transaction"
        );
        thread::yield_now();
    }
    task_test_hooks::release_direct_wake_delivery_failure();

    assert_eq!(
        waker.join().expect("direct waker must exit"),
        WakeResult::Notified,
        "one published wake must retry a transient target-publication failure"
    );
    let resumed_started = Instant::now();
    while !resumed.load(Ordering::Acquire) {
        assert!(
            resumed_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "the retried direct wake left its target blocked"
        );
        thread::yield_now();
    }
    worker
        .join()
        .expect("direct-wake worker must exit normally");
}

fn exercise_rt_direct_waker_waits_for_switch_tail(sleeper_cpu: usize, waker_cpu: usize) {
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let may_wake = Arc::new(AtomicBool::new(false));
    let waker_ready = Arc::new(AtomicBool::new(false));
    let wake_returned = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let waker_may_wake = Arc::clone(&may_wake);
    let worker_waker_ready = Arc::clone(&waker_ready);
    let worker_wake_returned = Arc::clone(&wake_returned);
    let waker = thread::spawn(move || {
        pin_current_to_cpu(waker_cpu);
        worker_waker_ready.store(true, Ordering::Release);
        while !waker_may_wake.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        let wake_handle = worker_wake_handle
            .lock()
            .clone()
            .expect("switch-tail sleeper must publish a wake handle");
        let result = wake_handle.wake_from_task();
        worker_wake_returned.store(true, Ordering::Release);
        result
    });
    let waker_ready_started = Instant::now();
    while !waker_ready.load(Ordering::Acquire) {
        assert!(
            waker_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "switch-tail waker did not become ready"
        );
        thread::yield_now();
    }

    let sleeper_ready = Arc::new(AtomicBool::new(false));
    let sleeper_resumed = Arc::new(AtomicBool::new(false));
    let sleeper_resumed_cpu = Arc::new(AtomicUsize::new(usize::MAX));
    let sleeper_wake_handle = Arc::clone(&wake_handle);
    let worker_ready = Arc::clone(&sleeper_ready);
    let worker_resumed = Arc::clone(&sleeper_resumed);
    let worker_resumed_cpu = Arc::clone(&sleeper_resumed_cpu);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle().expect("switch-tail sleeper must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("switch-tail sleeper must accept its FIFO policy");
        let park = match begin_current_park().expect("switch-tail sleeper must prepare its park") {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("switch-tail sleeper consumed an unexpected notification")
            }
        };
        task_test_hooks::arm_policy_switch_handoff_probe(current.id().as_u64());
        *sleeper_wake_handle.lock() = Some(current.wake_handle());
        worker_ready.store(true, Ordering::Release);
        park.commit()
            .expect("the direct waker must resume the switch-tail sleeper");
        worker_resumed_cpu.store(this_cpu_id(), Ordering::Release);
        worker_resumed.store(true, Ordering::Release);
    });

    let sleeper_ready_started = Instant::now();
    while !sleeper_ready.load(Ordering::Acquire) {
        assert!(
            sleeper_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "switch-tail sleeper did not publish its wake handle"
        );
        thread::yield_now();
    }
    let handoff_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_paused() {
        assert!(
            handoff_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "switch-tail sleeper did not reach the committed handoff window"
        );
        thread::yield_now();
    }

    let _ = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    may_wake.store(true, Ordering::Release);
    let wake_started = Instant::now();
    let observed_on_cpu_wait = loop {
        let waits = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
        if waits.on_cpu_waits != 0 {
            break true;
        }
        if wake_returned.load(Ordering::Acquire) {
            break false;
        }
        assert!(
            wake_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "Linux PREEMPT_RT direct wake did not reach its bounded on_cpu wait"
        );
        thread::yield_now();
    };
    let returned_before_tail = wake_returned.load(Ordering::Acquire);
    task_test_hooks::release_policy_switch_handoff_after_observation();

    assert_eq!(
        waker.join().expect("switch-tail waker must exit normally"),
        WakeResult::Notified,
        "the direct waker must activate the sleeper after switch tail"
    );
    sleeper
        .join()
        .expect("switch-tail sleeper must resume and exit normally");
    let _ = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    assert!(
        observed_on_cpu_wait,
        "Linux PREEMPT_RT disables TTWU_QUEUE, so the direct waker must wait for on_cpu"
    );
    assert!(
        !returned_before_tail,
        "Linux PREEMPT_RT keeps activation in the waker until switch tail clears on_cpu"
    );
    assert!(
        sleeper_resumed.load(Ordering::Acquire),
        "the directly activated sleeper must resume after switch tail"
    );
    assert_eq!(
        sleeper_resumed_cpu.load(Ordering::Acquire),
        sleeper_cpu,
        "the direct wake must preserve the sleeper's fixed CPU placement"
    );
}

fn exercise_rt_migratable_waker_waits_for_switch_tail(sleeper_cpu: usize, waker_cpu: usize) {
    let occupier = TargetOccupier::spawn(sleeper_cpu);
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let may_wake = Arc::new(AtomicBool::new(false));
    let waker_ready = Arc::new(AtomicBool::new(false));
    let wake_returned = Arc::new(AtomicBool::new(false));
    let worker_wake_handle = Arc::clone(&wake_handle);
    let waker_may_wake = Arc::clone(&may_wake);
    let worker_waker_ready = Arc::clone(&waker_ready);
    let worker_wake_returned = Arc::clone(&wake_returned);
    let waker = thread::spawn(move || {
        pin_current_to_cpu(waker_cpu);
        worker_waker_ready.store(true, Ordering::Release);
        while !waker_may_wake.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        let wake_handle = worker_wake_handle
            .lock()
            .clone()
            .expect("migratable sleeper must publish a wake handle");
        let result = wake_handle.wake_from_task();
        worker_wake_returned.store(true, Ordering::Release);
        result
    });
    let waker_ready_started = Instant::now();
    while !waker_ready.load(Ordering::Acquire) {
        assert!(
            waker_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "migratable waker did not become ready"
        );
        thread::yield_now();
    }

    let sleeper_ready = Arc::new(AtomicBool::new(false));
    let sleeper_resumed = Arc::new(AtomicBool::new(false));
    let sleeper_wake_handle = Arc::clone(&wake_handle);
    let worker_ready = Arc::clone(&sleeper_ready);
    let worker_resumed = Arc::clone(&sleeper_resumed);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle().expect("migratable sleeper must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("migratable sleeper must accept its FIFO policy");
        let mut affinity = AxCpuMask::new();
        affinity.set(sleeper_cpu, true);
        affinity.set(waker_cpu, true);
        assert!(
            ax_set_current_affinity(affinity).is_ok(),
            "busy-owner sleeper must accept a migratable affinity"
        );
        assert_eq!(
            this_cpu_id(),
            sleeper_cpu,
            "expanding affinity must retain the allowed current CPU"
        );
        let park = match begin_current_park().expect("migratable sleeper must prepare its park") {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("migratable sleeper consumed an unexpected notification")
            }
        };
        task_test_hooks::arm_policy_switch_handoff_probe(current.id().as_u64());
        *sleeper_wake_handle.lock() = Some(current.wake_handle());
        worker_ready.store(true, Ordering::Release);
        park.commit()
            .expect("the migratable wake must resume its sleeper");
        worker_resumed.store(true, Ordering::Release);
    });

    let sleeper_ready_started = Instant::now();
    while !sleeper_ready.load(Ordering::Acquire) {
        assert!(
            sleeper_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "migratable sleeper did not publish its wake handle"
        );
        thread::yield_now();
    }
    let handoff_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_paused() {
        assert!(
            handoff_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "migratable sleeper did not reach the committed handoff window"
        );
        thread::yield_now();
    }
    assert!(
        task_test_hooks::cpu_nr_running(sleeper_cpu as u32)
            .expect("the old owner's rq summary must be readable")
            > 0,
        "the regression requires another runnable task on the old owner"
    );

    let _ = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    may_wake.store(true, Ordering::Release);
    let wake_started = Instant::now();
    let observed_on_cpu_wait = loop {
        let waits = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
        if waits.on_cpu_waits != 0 {
            break true;
        }
        if wake_returned.load(Ordering::Acquire) {
            break false;
        }
        assert!(
            wake_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "migratable PREEMPT_RT wake did not reach its bounded on_cpu wait"
        );
        thread::yield_now();
    };
    let returned_before_tail = wake_returned.load(Ordering::Acquire);
    task_test_hooks::release_policy_switch_handoff_after_observation();

    assert_eq!(
        waker.join().expect("migratable waker must exit normally"),
        WakeResult::Notified
    );
    sleeper
        .join()
        .expect("migratable sleeper must resume and exit normally");
    occupier.stop();
    assert!(
        observed_on_cpu_wait,
        "Linux PREEMPT_RT direct activation must wait for a migratable wakee's on_cpu release"
    );
    assert!(
        !returned_before_tail,
        "a PREEMPT_RT waker must retain activation until switch tail completes"
    );
    assert!(
        sleeper_resumed.load(Ordering::Acquire),
        "the ordinary post-tail wake path must resume the sleeper"
    );
}

fn exercise_rt_wait_claim_waker_waits_for_switch_tail(sleeper_cpu: usize, waker_cpu: usize) {
    let idle_started = Instant::now();
    while task_test_hooks::cpu_nr_running(sleeper_cpu as u32)
        .expect("the wait-claim target rq summary must be readable")
        != 0
    {
        assert!(
            idle_started.elapsed() < OCCUPIER_CURRENT_TIMEOUT,
            "the wait-claim target CPU must be idle before installing its last runnable task"
        );
        thread::yield_now();
    }

    let may_wake = Arc::new(AtomicBool::new(false));
    let waker_ready = Arc::new(AtomicBool::new(false));
    let wake_returned = Arc::new(AtomicBool::new(false));
    let waker_may_wake = Arc::clone(&may_wake);
    let worker_waker_ready = Arc::clone(&waker_ready);
    let worker_wake_returned = Arc::clone(&wake_returned);
    let waker = thread::spawn(move || {
        pin_current_to_cpu(waker_cpu);
        worker_waker_ready.store(true, Ordering::Release);
        while !waker_may_wake.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        let count = api::ax_wait_queue_wake(&SWITCH_WAKE_WQ, 1);
        worker_wake_returned.store(true, Ordering::Release);
        count
    });
    let waker_ready_started = Instant::now();
    while !waker_ready.load(Ordering::Acquire) {
        assert!(
            waker_ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "wait-claim switch-tail waker did not become ready"
        );
        thread::yield_now();
    }

    let resumed = Arc::new(AtomicBool::new(false));
    let worker_resumed = Arc::clone(&resumed);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        let current = current_thread_handle()
            .expect("wait-claim switch-tail sleeper must have a task handle");
        set_thread_policy(
            current.id(),
            SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 must be valid")),
        )
        .expect("wait-claim switch-tail sleeper must accept FIFO policy");
        task_test_hooks::arm_policy_switch_handoff_probe(current.id().as_u64());
        assert!(
            !api::ax_wait_queue_wait(&SWITCH_WAKE_WQ, None),
            "an untimed wait-claim park must not report a timeout"
        );
        worker_resumed.store(true, Ordering::Release);
    });

    let handoff_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_paused() {
        assert!(
            handoff_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "wait-claim sleeper did not reach the committed handoff window"
        );
        thread::yield_now();
    }
    assert_eq!(
        task_test_hooks::cpu_nr_running(sleeper_cpu as u32)
            .expect("the old owner's committed rq summary must be readable"),
        0,
        "the regression requires the outgoing wakee to be the owner's last runnable task"
    );

    let _ = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
    may_wake.store(true, Ordering::Release);
    let wake_started = Instant::now();
    let observed_on_cpu_wait = loop {
        let waits = scheduler_wait_test_hooks::take_scheduler_wait_snapshot();
        if waits.on_cpu_waits != 0 {
            break true;
        }
        if wake_returned.load(Ordering::Acquire) {
            break false;
        }
        assert!(
            wake_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "wait-claim PREEMPT_RT wake did not reach its bounded on_cpu wait"
        );
        thread::yield_now();
    };
    let returned_before_tail = wake_returned.load(Ordering::Acquire);
    task_test_hooks::release_policy_switch_handoff_after_observation();

    assert_eq!(
        waker
            .join()
            .expect("wait-claim switch-tail waker must exit"),
        1,
        "the direct waker must retain the selected wait claim"
    );
    sleeper
        .join()
        .expect("wait-claim switch-tail sleeper must resume and exit");
    assert!(
        observed_on_cpu_wait,
        "Linux PREEMPT_RT wait-claim wake must wait for on_cpu"
    );
    assert!(
        !returned_before_tail,
        "the wait-claim wake API must not return before direct activation is possible"
    );
    assert!(
        resumed.load(Ordering::Acquire),
        "the waker must activate the selected wait claim after switch tail"
    );
}

fn exercise_delayed_fair_wake_before_switch_tail(sleeper_cpu: usize, waker_cpu: usize) {
    let occupier = TargetOccupier::spawn(sleeper_cpu);
    let wake_handle = Arc::new(Mutex::new(None::<ThreadWakeHandle>));
    let sleeper_ready = Arc::new(AtomicBool::new(false));
    let sleeper_resumed = Arc::new(AtomicBool::new(false));
    let waker_ready = Arc::new(AtomicBool::new(false));
    let wake_allowed = Arc::new(AtomicBool::new(false));
    let wake_started = Arc::new(AtomicBool::new(false));
    let wake_returned = Arc::new(AtomicBool::new(false));
    let waker_wake_handle = Arc::clone(&wake_handle);
    let worker_waker_ready = Arc::clone(&waker_ready);
    let worker_wake_allowed = Arc::clone(&wake_allowed);
    let worker_wake_started = Arc::clone(&wake_started);
    let worker_wake_returned = Arc::clone(&wake_returned);
    let waker = thread::spawn(move || {
        pin_current_to_cpu(waker_cpu);
        worker_waker_ready.store(true, Ordering::Release);
        while !worker_wake_allowed.load(Ordering::Acquire) {
            hint::spin_loop();
        }
        let wake_handle = waker_wake_handle
            .lock()
            .clone()
            .expect("delayed switch-tail sleeper must publish a wake handle");
        worker_wake_started.store(true, Ordering::Release);
        let result = wake_handle.wake_from_task();
        worker_wake_returned.store(true, Ordering::Release);
        result
    });
    let waker_ready_at = Instant::now();
    while !waker_ready.load(Ordering::Acquire) {
        assert!(
            waker_ready_at.elapsed() < WORKER_READY_TIMEOUT,
            "delayed-rq waker did not become ready"
        );
        thread::yield_now();
    }

    let sleeper_wake_handle = Arc::clone(&wake_handle);
    let worker_ready = Arc::clone(&sleeper_ready);
    let worker_resumed = Arc::clone(&sleeper_resumed);
    let sleeper = thread::spawn(move || {
        pin_current_to_cpu(sleeper_cpu);
        set_current_fair_idle_policy();
        let current =
            current_thread_handle().expect("delayed switch-tail sleeper must have a task handle");
        let park = match begin_current_park()
            .expect("delayed switch-tail sleeper must prepare its park")
        {
            CurrentParkStart::Prepared(park) => park,
            CurrentParkStart::Notified => {
                panic!("delayed switch-tail sleeper consumed an unexpected notification")
            }
        };
        task_test_hooks::arm_fair_delay_dequeue(current.id().as_u64());
        task_test_hooks::arm_policy_switch_handoff_probe(current.id().as_u64());
        *sleeper_wake_handle.lock() = Some(current.wake_handle());
        worker_ready.store(true, Ordering::Release);
        park.commit()
            .expect("a delayed-rq wake must resume the switch-tail sleeper");
        worker_resumed.store(true, Ordering::Release);
    });

    let ready_started = Instant::now();
    while !sleeper_ready.load(Ordering::Acquire) {
        assert!(
            ready_started.elapsed() < WORKER_READY_TIMEOUT,
            "delayed switch-tail sleeper did not publish its wake handle"
        );
        thread::yield_now();
    }
    let handoff_started = Instant::now();
    while !task_test_hooks::policy_switch_handoff_paused() {
        assert!(
            handoff_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT,
            "delayed switch-tail sleeper did not reach the committed handoff window"
        );
        thread::yield_now();
    }

    let wake_handle = wake_handle
        .lock()
        .clone()
        .expect("delayed switch-tail sleeper must publish a wake handle");
    assert!(
        task_test_hooks::thread_is_delayed_fair(wake_handle.thread_id().as_u64()),
        "the regression requires Blocked plus delayed rq membership before wake"
    );
    let sleeper_id = wake_handle.thread_id().as_u64();
    task_test_hooks::arm_direct_wake_on_rq_probe(sleeper_id);
    wake_allowed.store(true, Ordering::Release);
    let wake_started_at = Instant::now();
    while !wake_started.load(Ordering::Acquire) {
        assert!(
            wake_started_at.elapsed() < WORKER_READY_TIMEOUT,
            "delayed-rq waker did not start"
        );
        hint::spin_loop();
    }
    let observation_started = Instant::now();
    while !task_test_hooks::direct_wake_on_rq_paused()
        && !wake_returned.load(Ordering::Acquire)
        && observation_started.elapsed() < REMOTE_WAKE_PROGRESS_TIMEOUT
    {
        hint::spin_loop();
    }
    let observed_existing_rq = task_test_hooks::direct_wake_on_rq_paused();
    task_test_hooks::release_policy_switch_handoff_after_observation();
    if observed_existing_rq {
        task_test_hooks::release_direct_wake_on_rq_probe();
    }
    let wake_result = waker.join().expect("delayed-rq waker must exit normally");

    assert!(
        observed_existing_rq,
        "Linux handles on_rq before waiting for the outgoing on_cpu claim"
    );
    assert!(
        task_test_hooks::take_direct_wake_on_rq_observation(),
        "the existing-rq wake observation must complete exactly once"
    );
    assert_eq!(
        wake_result,
        WakeResult::Notified,
        "a delayed-rq task must be reactivated on its existing rq"
    );
    occupier.stop();
    sleeper
        .join()
        .expect("delayed switch-tail sleeper must resume and exit normally");
    assert!(
        sleeper_resumed.load(Ordering::Acquire),
        "the delayed-rq wake must survive the outgoing switch tail"
    );
}

struct RemoteSleeper {
    worker: Option<thread::JoinHandle<()>>,
    thread_id: u64,
}

impl RemoteSleeper {
    fn spawn(cpu: usize) -> Self {
        let worker = thread::spawn(move || {
            pin_current_to_cpu(cpu);
            set_current_fair_idle_policy();
            SLEEPER_CPU.store(this_cpu_id(), Ordering::Release);
            READY.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&READY_WQ, 1);

            while !MAY_SLEEP.load(Ordering::Acquire) {
                thread::yield_now();
            }
            api::ax_wait_queue_wait_until(&SLEEP_WQ, || GO.load(Ordering::Acquire), None);
            assert_eq!(
                this_cpu_id(),
                EXPECTED_SLEEPER_CPU.load(Ordering::Acquire),
                "remote wakeup resumed on the wrong CPU"
            );
            DONE.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&DONE_WQ, 1);
        });
        let thread_id = worker.thread().id().as_u64().get();
        let sleeper = Self {
            worker: Some(worker),
            thread_id,
        };
        assert!(
            !api::ax_wait_queue_wait_until(
                &READY_WQ,
                || READY.load(Ordering::Acquire),
                Some(WORKER_READY_TIMEOUT),
            ),
            "remote sleeper did not become ready"
        );
        sleeper
    }

    const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    fn finish(mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

impl Drop for RemoteSleeper {
    fn drop(&mut self) {
        MAY_SLEEP.store(true, Ordering::Release);
        GO.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&SLEEP_WQ, 1);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct TargetOccupier {
    worker: Option<thread::JoinHandle<()>>,
    hold_current: Arc<AtomicBool>,
    current_held: Arc<AtomicBool>,
    release_current: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl TargetOccupier {
    fn spawn(cpu: usize) -> Self {
        let ready = Arc::new(AtomicBool::new(false));
        let hold_current = Arc::new(AtomicBool::new(false));
        let current_held = Arc::new(AtomicBool::new(false));
        let release_current = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_ready = Arc::clone(&ready);
        let worker_hold_current = Arc::clone(&hold_current);
        let worker_current_held = Arc::clone(&current_held);
        let worker_release_current = Arc::clone(&release_current);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            pin_current_to_cpu(cpu);
            set_current_fair_idle_policy();
            worker_ready.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&OCCUPIER_READY_WQ, 1);
            while !worker_stop.load(Ordering::Acquire) {
                if worker_hold_current.load(Ordering::Acquire) {
                    let _preempt = PreemptGuard::new();
                    // Publish the barrier only after this task is the
                    // non-preemptible current entity on the target CPU.
                    worker_current_held.store(true, Ordering::Release);
                    while !worker_release_current.load(Ordering::Acquire)
                        && !worker_stop.load(Ordering::Acquire)
                    {
                        hint::spin_loop();
                    }
                    break;
                }
                hint::spin_loop();
            }
        });
        let occupier = Self {
            worker: Some(worker),
            hold_current,
            current_held,
            release_current,
            stop,
        };
        assert!(
            !api::ax_wait_queue_wait_until(
                &OCCUPIER_READY_WQ,
                || ready.load(Ordering::Acquire),
                Some(WORKER_READY_TIMEOUT),
            ),
            "target occupier did not become ready"
        );
        occupier
    }

    fn hold_current(&self, cpu: usize) {
        self.hold_current.store(true, Ordering::Release);
        let started = Instant::now();
        while !self.current_held.load(Ordering::Acquire)
            && started.elapsed() < OCCUPIER_CURRENT_TIMEOUT
        {
            thread::yield_now();
        }
        assert!(
            self.current_held.load(Ordering::Acquire),
            "target occupier did not retain CPU {cpu}",
        );
    }

    fn release_current(&self) {
        self.release_current.store(true, Ordering::Release);
    }

    fn stop(mut self) {
        self.release_current.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

impl Drop for TargetOccupier {
    fn drop(&mut self) {
        self.release_current.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin current task to CPU {cpu_id}"
    );
    for _ in 0..256 {
        if this_cpu_id() == cpu_id {
            return;
        }
        thread::yield_now();
    }
    assert_eq!(
        this_cpu_id(),
        cpu_id,
        "current task did not migrate to CPU {cpu_id}"
    );
}

fn set_current_fair_idle_policy() {
    let current = current_thread_id().expect("test worker must have a task identity");
    set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
        .expect("test worker must accept the isolated Fair policy");
}

fn move_delayed_sleeper(
    sleeper: u64,
    cpu_num: usize,
    target_cpu: usize,
    source_occupier: &TargetOccupier,
    target_occupier: &TargetOccupier,
) {
    let sleeper_id = ThreadId::from_parts(sleeper as u32, (sleeper >> 32) as u32);
    let mut affinity = CpuSet::empty(cpu_num);
    assert!(affinity.insert(CpuId::new(target_cpu as u32)));
    set_thread_affinity(sleeper_id, affinity)
        .expect("a blocked delayed Fair task must accept a remote affinity update");
    source_occupier.release_current();

    let started = Instant::now();
    while started.elapsed() < AFFINITY_MIGRATION_TIMEOUT {
        if task_test_hooks::thread_has_committed_migration_to_cpu(sleeper, target_cpu as u32) {
            EXPECTED_SLEEPER_CPU.store(target_cpu, Ordering::Release);
            GO.store(true, Ordering::Release);
            assert_eq!(api::ax_wait_queue_wake(&SLEEP_WQ, 1), 1);
            target_occupier.release_current();
            break;
        }
        thread::yield_now();
    }
    assert!(
        GO.load(Ordering::Acquire),
        "source owner did not commit the delayed Fair migration: delayed={}, target_committed={}",
        task_test_hooks::thread_is_delayed_fair(sleeper),
        task_test_hooks::thread_has_committed_migration_to_cpu(sleeper, target_cpu as u32),
    );

    let started = Instant::now();
    while started.elapsed() < AFFINITY_MIGRATION_TIMEOUT {
        if task_test_hooks::thread_affinity_is_settled_on_cpu(sleeper, target_cpu as u32) {
            return;
        }
        thread::yield_now();
    }
    panic!(
        "wake-overtaken delayed Fair migration did not complete: delayed={}, target_committed={}, \
         target_settled={}",
        task_test_hooks::thread_is_delayed_fair(sleeper),
        task_test_hooks::thread_has_committed_migration_to_cpu(sleeper, target_cpu as u32),
        task_test_hooks::thread_affinity_is_settled_on_cpu(sleeper, target_cpu as u32),
    );
}

fn wake_sleep_queue_after_waiter_enqueued(sleeper: u64, occupier: &TargetOccupier) {
    let sleeper_id = ThreadId::from_parts(sleeper as u32, (sleeper >> 32) as u32);
    let started = Instant::now();
    while started.elapsed() < WAITER_BLOCK_TIMEOUT {
        if task_test_hooks::thread_is_blocked(sleeper) {
            assert!(
                task_test_hooks::thread_is_delayed_fair(sleeper),
                "an ineligible Fair sleeper must retain Linux delayed-dequeue rq membership"
            );
            occupier.hold_current(1);
            set_thread_policy(
                sleeper_id,
                SchedulePolicy::fair(Nice::ZERO, FairMode::Batch),
            )
            .expect("a delayed Fair task must accept a same-class policy update");
            assert!(
                task_test_hooks::thread_is_delayed_fair(sleeper),
                "same-class policy update must preserve delayed Fair rq ownership"
            );
            set_thread_policy(sleeper_id, SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .expect("a delayed Fair task must restore its original policy");
            assert!(
                task_test_hooks::thread_is_delayed_fair(sleeper),
                "restoring a Fair policy must preserve delayed Fair rq ownership"
            );
            task_test_hooks::arm_wake_irq_owner_probe(sleeper);
            task_test_hooks::arm_wake_entity_read_copy_probe(sleeper);
            task_test_hooks::arm_wake_owner_deadline_refresh_probe(sleeper);
            GO.store(true, Ordering::Release);
            let woken = api::ax_wait_queue_wake(&SLEEP_WQ, 1);
            let deadline_refresh = task_test_hooks::take_wake_owner_deadline_refresh_required();
            occupier.release_current();
            assert_eq!(woken, 1);
            assert_eq!(
                deadline_refresh,
                Some(false),
                "ENQUEUE_DELAYED must not republish a contender already covered by the owner \
                 deadline",
            );
            return;
        }
        thread::yield_now();
    }
    panic!("sleeper did not enter wait queue");
}

fn migrate_sleep_queue_after_waiter_enqueued(
    sleeper: u64,
    cpu_num: usize,
    source_cpu: usize,
    target_cpu: usize,
    source_occupier: &TargetOccupier,
    target_occupier: &TargetOccupier,
) {
    let started = Instant::now();
    while started.elapsed() < WAITER_BLOCK_TIMEOUT {
        if task_test_hooks::thread_is_blocked(sleeper) {
            assert!(
                task_test_hooks::thread_is_delayed_fair(sleeper),
                "the affinity regression requires a real delayed Fair source entity"
            );
            source_occupier.hold_current(source_cpu);
            target_occupier.hold_current(target_cpu);
            move_delayed_sleeper(
                sleeper,
                cpu_num,
                target_cpu,
                source_occupier,
                target_occupier,
            );
            return;
        }
        thread::yield_now();
    }
    panic!("affinity sleeper did not enter wait queue");
}

fn reset_sleeper_state(expected_cpu: usize) {
    READY.store(false, Ordering::Release);
    MAY_SLEEP.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    SLEEPER_CPU.store(usize::MAX, Ordering::Release);
    EXPECTED_SLEEPER_CPU.store(expected_cpu, Ordering::Release);
}

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    assert!(
        cpu_num >= 4,
        "task_wait_queue_remote_wake requires at least four CPUs"
    );

    let waker_cpu = 0;
    let sleeper_cpu = 1;
    let migrated_cpu = 2;
    let isolated_cpu = cpu_num - 1;
    reset_sleeper_state(sleeper_cpu);

    pin_current_to_cpu(waker_cpu);
    exercise_notified_park_runtime(waker_cpu, sleeper_cpu);
    exercise_rt_park_uses_detached_publication_without_task_lock(waker_cpu, sleeper_cpu);
    exercise_rt_park_does_not_wait_for_task_lock(waker_cpu, sleeper_cpu, migrated_cpu);
    exercise_rt_park_releases_task_lock_while_publication_reader_waits(
        waker_cpu,
        sleeper_cpu,
        migrated_cpu,
    );
    exercise_wait_claim_does_not_block_rt_publication(waker_cpu, sleeper_cpu, migrated_cpu);
    exercise_direct_wake_retries_failed_delivery(sleeper_cpu);
    exercise_rt_direct_waker_waits_for_switch_tail(sleeper_cpu, migrated_cpu);
    exercise_rt_migratable_waker_waits_for_switch_tail(sleeper_cpu, migrated_cpu);
    // The preceding migratable-wake case deliberately leaves another task on
    // `sleeper_cpu` until its exit switch tail completes. Use the otherwise
    // untouched final CPU for the last-runnable wait-claim case rather than
    // racing task-exit cleanup from the previous scenario.
    exercise_rt_wait_claim_waker_waits_for_switch_tail(isolated_cpu, migrated_cpu);
    exercise_delayed_fair_wake_before_switch_tail(sleeper_cpu, migrated_cpu);
    let sleeper = RemoteSleeper::spawn(sleeper_cpu);
    let sleeper_id = sleeper.thread_id();
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);
    assert_eq!(this_cpu_id(), waker_cpu);
    // Keep a user SCHED_IDLE-equivalent task runnable on the target rq so
    // wakeup preemption must inspect the authoritative current entity instead
    // of taking the dedicated-idle shortcut. Using the separate Fair mode
    // prevents unrelated normal background work in the all-features image
    // from becoming an earlier same-mode contender.
    let occupier = TargetOccupier::spawn(sleeper_cpu);
    task_test_hooks::arm_park_irq_owner_probe(sleeper_id);
    task_test_hooks::arm_switch_tail_irq_owner_probe(sleeper_id);
    task_test_hooks::arm_fair_delay_dequeue(sleeper_id);
    MAY_SLEEP.store(true, Ordering::Release);
    wake_sleep_queue_after_waiter_enqueued(sleeper_id, &occupier);

    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(REMOTE_WAKE_PROGRESS_TIMEOUT),
        ),
        "remote wait-queue wakeup did not make bounded progress"
    );
    sleeper.finish();
    occupier.stop();

    reset_sleeper_state(sleeper_cpu);
    let affinity_sleeper = RemoteSleeper::spawn(sleeper_cpu);
    let affinity_sleeper_id = affinity_sleeper.thread_id();
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);
    let source_occupier = TargetOccupier::spawn(sleeper_cpu);
    let target_occupier = TargetOccupier::spawn(migrated_cpu);
    task_test_hooks::arm_fair_delay_dequeue(affinity_sleeper_id);
    MAY_SLEEP.store(true, Ordering::Release);
    migrate_sleep_queue_after_waiter_enqueued(
        affinity_sleeper_id,
        cpu_num,
        sleeper_cpu,
        migrated_cpu,
        &source_occupier,
        &target_occupier,
    );
    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(REMOTE_WAKE_PROGRESS_TIMEOUT),
        ),
        "affinity-migrated wait-queue wakeup did not make bounded progress"
    );
    affinity_sleeper.finish();
    source_occupier.stop();
    target_occupier.stop();
    assert_eq!(
        task_test_hooks::take_park_irq_owner_entries(),
        Some(task_test_hooks::ParkIrqOwnerEntries {
            thread_sched_acquired: 1,
            thread_sched: 0,
            run_queue: 0,
        }),
        "one scheduler-frame park transaction must reuse the runtime IRQ baton"
    );
    assert_eq!(
        task_test_hooks::take_switch_tail_irq_owner_entries(),
        Some(task_test_hooks::SwitchTailIrqOwnerEntries {
            thread_sched_acquired: 0,
            thread_sched: 0,
            run_queue: 0,
            rq_reacquired: 0,
            rq_baton_consumed: 1,
        }),
        "an ordinary switch tail must consume the selection rq baton without reopening the \
         previous task lock or reacquiring the rq"
    );
    assert_eq!(
        task_test_hooks::take_wake_irq_owner_entries(),
        Some(task_test_hooks::WakeIrqOwnerEntries {
            thread_sched: 1,
            run_queue: 0,
        }),
        "one Linux-style task-sched/rq wake transaction must own one runtime IRQ guard"
    );
    assert_eq!(
        task_test_hooks::take_wake_entity_read_events(),
        Some(task_test_hooks::WakeEntityReadEvents {
            reads: 4,
            copies: 0,
        }),
        "delayed requeue, wake placement, and preemption must borrow rq-owned scheduling entities"
    );
    task_test_hooks::arm_park_deadline_publication_probe(this_cpu_id());
    task_test_hooks::request_current_owner_work()
        .expect("the deadline probe must tolerate unrelated pending owner work");
    schedule_current_cpu().expect("the unrelated owner pass must complete before timed park");
    task_test_hooks::arm_deadline_soft_expiry_probe(this_cpu_id());
    assert!(api::ax_wait_queue_wait_until(
        &TIMEOUT_WQ,
        || false,
        Some(Duration::from_millis(1)),
    ));
    assert_eq!(
        task_test_hooks::take_deadline_publication_entries(),
        Some(task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 1,
            publication: 0,
        }),
        "one timed park must register and publish through one deadline-base transaction"
    );
    assert_eq!(
        task_test_hooks::take_deadline_soft_expiry_entries(),
        Some(1),
        "one clockevent must expire task and kernel timers under one deadline-base guard"
    );
    Ok(())
}

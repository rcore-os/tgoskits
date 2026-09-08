use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::ax_hal::percpu::this_cpu_id,
        task::{
            FairMode, Nice, RtPriority, SchedulePolicy, ThreadId, current_thread_id,
            set_thread_policy, thread_handle,
        },
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);

fn pin_current_to_cpu(cpu: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu)).is_ok(),
        "failed to pin PI mutex test task to CPU {cpu}"
    );
    wait_until(
        || this_cpu_id() == cpu,
        "PI mutex test task did not migrate",
    );
}

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

fn ownerless_lock_rekey_wakes_new_top() {
    let lock_l = Arc::new(Mutex::new(()));
    let lock_m = Arc::new(Mutex::new(()));
    let owner_gate = Arc::new(AxWaitQueueHandle::new());
    let selected_gate = Arc::new(AxWaitQueueHandle::new());
    let boosted_gate = Arc::new(AxWaitQueueHandle::new());
    let probe_gate = Arc::new(AxWaitQueueHandle::new());
    let release_owner = Arc::new(AtomicBool::new(false));
    let run_selected = Arc::new(AtomicBool::new(false));
    let run_boosted = Arc::new(AtomicBool::new(false));
    let lock_l_ready = Arc::new(AtomicBool::new(false));
    let lock_m_ready = Arc::new(AtomicBool::new(false));
    let selected_gate_ready = Arc::new(AtomicBool::new(false));
    let boosted_gate_ready = Arc::new(AtomicBool::new(false));
    let probe_gate_ready = Arc::new(AtomicBool::new(false));
    let probe_phase = Arc::new(AtomicUsize::new(0));
    let probe_ack = Arc::new(AtomicUsize::new(0));
    let selected_done = Arc::new(AtomicBool::new(false));
    let boosted_done = Arc::new(AtomicBool::new(false));

    let probe = {
        let gate = Arc::clone(&probe_gate);
        let ready = Arc::clone(&probe_gate_ready);
        let phase = Arc::clone(&probe_phase);
        let ack = Arc::clone(&probe_ack);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("the PI probe must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(1).expect("priority 1 is valid")),
            )
            .expect("the PI probe must accept its policy");
            ready.store(true, Ordering::Release);
            for expected in 1..=2 {
                api::ax_wait_queue_wait_until(
                    gate.as_ref(),
                    || phase.load(Ordering::Acquire) >= expected,
                    None,
                );
                ack.store(expected, Ordering::Release);
            }
        })
    };

    let owner = {
        let lock_l = Arc::clone(&lock_l);
        let lock_m = Arc::clone(&lock_m);
        let gate = Arc::clone(&owner_gate);
        let release = Arc::clone(&release_owner);
        let ready = Arc::clone(&lock_l_ready);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("the PI owner must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(90).expect("priority 90 is valid")),
            )
            .expect("the PI owner must accept its policy");
            let lock_l = lock_l.lock();
            ready.store(true, Ordering::Release);
            api::ax_wait_queue_wait_until(gate.as_ref(), || release.load(Ordering::Acquire), None);
            drop(lock_l);
            drop(lock_m.lock());
        })
    };

    let selected = {
        let lock_l = Arc::clone(&lock_l);
        let gate = Arc::clone(&selected_gate);
        let run = Arc::clone(&run_selected);
        let ready = Arc::clone(&selected_gate_ready);
        let done = Arc::clone(&selected_done);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("the selected PI waiter must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(30).expect("priority 30 is valid")),
            )
            .expect("the selected PI waiter must accept its policy");
            ready.store(true, Ordering::Release);
            api::ax_wait_queue_wait_until(gate.as_ref(), || run.load(Ordering::Acquire), None);
            drop(lock_l.lock());
            done.store(true, Ordering::Release);
        })
    };

    let boosted = {
        let lock_l = Arc::clone(&lock_l);
        let lock_m = Arc::clone(&lock_m);
        let gate = Arc::clone(&boosted_gate);
        let run = Arc::clone(&run_boosted);
        let ready = Arc::clone(&boosted_gate_ready);
        let lock_ready = Arc::clone(&lock_m_ready);
        let done = Arc::clone(&boosted_done);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            set_thread_policy(
                current_thread_id().expect("the boosted PI waiter must have an identity"),
                SchedulePolicy::fifo(RtPriority::new(10).expect("priority 10 is valid")),
            )
            .expect("the boosted PI waiter must accept its policy");
            let lock_m = lock_m.lock();
            lock_ready.store(true, Ordering::Release);
            ready.store(true, Ordering::Release);
            api::ax_wait_queue_wait_until(gate.as_ref(), || run.load(Ordering::Acquire), None);
            drop(lock_l.lock());
            done.store(true, Ordering::Release);
            drop(lock_m);
        })
    };

    wait_until(
        || lock_l_ready.load(Ordering::Acquire),
        "the owner must hold L",
    );
    wait_until(
        || lock_m_ready.load(Ordering::Acquire),
        "the boosted waiter must hold M",
    );
    wait_until(
        || selected_gate_ready.load(Ordering::Acquire),
        "the selected waiter must reach its gate",
    );
    wait_until(
        || boosted_gate_ready.load(Ordering::Acquire),
        "the boosted waiter must reach its gate",
    );
    wait_until(
        || probe_gate_ready.load(Ordering::Acquire),
        "the PI probe must reach its gate",
    );

    // Prove every actor has entered its public wait queue while its predicate
    // remains false. The wake is intentionally spurious; wait_until rechecks
    // the predicate and parks again without any scheduler-private observation.
    for (gate, message) in [
        (selected_gate.as_ref(), "the selected waiter must park"),
        (boosted_gate.as_ref(), "the boosted waiter must park"),
        (probe_gate.as_ref(), "the PI probe must park"),
    ] {
        wait_until(|| api::ax_wait_queue_wake(gate, 1) == 1, message);
    }

    run_selected.store(true, Ordering::Release);
    api::ax_wait_queue_wake(selected_gate.as_ref(), 1);
    probe_phase.store(1, Ordering::Release);
    api::ax_wait_queue_wake(probe_gate.as_ref(), 1);
    wait_until(
        || probe_ack.load(Ordering::Acquire) >= 1,
        "the selected waiter must block on L before the probe runs",
    );

    // The probe loops back to the same public wait queue. Wake it once while
    // phase 2 is false to prove it has parked before the next ordering check.
    wait_until(
        || api::ax_wait_queue_wake(probe_gate.as_ref(), 1) == 1,
        "the PI probe must repark for phase 2",
    );
    run_boosted.store(true, Ordering::Release);
    api::ax_wait_queue_wake(boosted_gate.as_ref(), 1);
    probe_phase.store(2, Ordering::Release);
    api::ax_wait_queue_wake(probe_gate.as_ref(), 1);
    wait_until(
        || probe_ack.load(Ordering::Acquire) >= 2,
        "the boosted waiter must block on L before the probe runs",
    );

    release_owner.store(true, Ordering::Release);
    api::ax_wait_queue_wake(owner_gate.as_ref(), 1);
    wait_until(
        || boosted_done.load(Ordering::Acquire),
        "the rekeyed top waiter must be woken on the ownerless lock",
    );
    wait_until(
        || selected_done.load(Ordering::Acquire),
        "the original handoff waiter must acquire L after the new top",
    );

    boosted.join().expect("the boosted PI waiter must exit");
    selected.join().expect("the selected PI waiter must exit");
    owner.join().expect("the PI owner must exit");
    probe.join().expect("the PI probe must exit");
}

pub fn run() -> crate::TestResult {
    assert!(
        thread::available_parallelism().unwrap().get() >= 3,
        "task-pi-mutex requires at least three CPUs"
    );
    pin_current_to_cpu(2);

    ownerless_lock_rekey_wakes_new_top();

    let mutex = Arc::new(Mutex::new(()));
    let owner_locked = Arc::new(AtomicBool::new(false));
    let release_owner = Arc::new(AtomicBool::new(false));
    let owner = {
        let mutex = Arc::clone(&mutex);
        let owner_locked = Arc::clone(&owner_locked);
        let release_owner = Arc::clone(&release_owner);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            let current = current_thread_id().expect("PI owner must have a thread identity");
            set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .expect("PI owner must enter the idle Fair class");
            let guard = mutex.lock();
            owner_locked.store(true, Ordering::Release);
            while !release_owner.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            drop(guard);
        })
    };
    let owner_id = owner.thread().id().as_u64().get();
    let owner_id = ThreadId::from_parts(owner_id as u32, (owner_id >> 32) as u32);
    wait_until(
        || owner_locked.load(Ordering::Acquire),
        "PI mutex owner did not acquire the lock",
    );

    // Keep normal Fair work runnable on the owner's CPU. Without PI donation,
    // the SCHED_IDLE owner cannot run again to release the mutex.
    let stop_competitor = Arc::new(AtomicBool::new(false));
    let competitor_ready = Arc::new(AtomicBool::new(false));
    let competitor = {
        let stop = Arc::clone(&stop_competitor);
        let ready = Arc::clone(&competitor_ready);
        thread::spawn(move || {
            pin_current_to_cpu(0);
            let current = current_thread_id().expect("PI competitor must have a thread identity");
            set_thread_policy(current, SchedulePolicy::fair(Nice::ZERO, FairMode::Normal))
                .expect("PI competitor must enter the normal Fair class");
            ready.store(true, Ordering::Release);
            while !stop.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        })
    };
    wait_until(
        || competitor_ready.load(Ordering::Acquire),
        "PI competitor did not become runnable",
    );

    let waiter_started = Arc::new(AtomicBool::new(false));
    let waiter_acquired = Arc::new(AtomicBool::new(false));
    let waiter_completion = Arc::new(AxWaitQueueHandle::new());
    let waiter = {
        let mutex = Arc::clone(&mutex);
        let started = Arc::clone(&waiter_started);
        let acquired = Arc::clone(&waiter_acquired);
        let completion = Arc::clone(&waiter_completion);
        thread::spawn(move || {
            pin_current_to_cpu(1);
            let current = current_thread_id().expect("PI waiter must have a thread identity");
            set_thread_policy(
                current,
                SchedulePolicy::fifo(RtPriority::new(80).expect("priority 80 must be valid")),
            )
            .expect("PI waiter must enter FIFO policy");
            started.store(true, Ordering::Release);
            drop(mutex.lock());
            acquired.store(true, Ordering::Release);
            api::ax_wait_queue_wake(completion.as_ref(), 1);
        })
    };
    wait_until(
        || waiter_started.load(Ordering::Acquire),
        "PI waiter did not start",
    );
    let donated_policy = SchedulePolicy::fifo(RtPriority::new(80).expect("priority 80 is valid"));
    wait_until(
        || thread_handle(owner_id).is_ok_and(|owner| owner.effective_policy() == donated_policy),
        "PI waiter did not donate FIFO priority to the owner",
    );

    release_owner.store(true, Ordering::Release);
    let timed_out = api::ax_wait_queue_wait_until(
        waiter_completion.as_ref(),
        || waiter_acquired.load(Ordering::Acquire),
        Some(PROGRESS_TIMEOUT),
    );
    assert!(
        !timed_out,
        "RT waiter did not donate priority and acquire the mutex",
    );

    stop_competitor.store(true, Ordering::Release);
    waiter.join().expect("PI waiter must exit normally");
    owner.join().expect("PI owner must exit normally");
    competitor.join().expect("PI competitor must exit normally");

    // The handoff must leave the public mutex usable by a later owner.
    drop(mutex.lock());
    Ok(())
}

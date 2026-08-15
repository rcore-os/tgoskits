macro_rules! def_test_sched {
    ($name:ident, $scheduler:ty, $task:ty) => {
        mod $name {
            use alloc::sync::Arc;

            use crate::*;

            #[test]
            fn test_sched() {
                const NUM_TASKS: usize = 11;

                let mut scheduler = <$scheduler>::new();
                for i in 0..NUM_TASKS {
                    scheduler.add_task(Arc::new(<$task>::new(i)));
                }

                for i in 0..NUM_TASKS * 10 - 1 {
                    let next = scheduler.pick_next_task().unwrap();
                    assert_eq!(*next.inner(), i % NUM_TASKS);
                    // pass a tick to ensure the order of tasks
                    scheduler.task_tick(&next);
                    scheduler.put_prev_task(next, false);
                }

                let mut n = 0;
                while scheduler.pick_next_task().is_some() {
                    n += 1;
                }
                assert_eq!(n, NUM_TASKS);
            }

            #[test]
            fn test_len() {
                const NUM_TASKS: usize = 7;
                let mut scheduler = <$scheduler>::new();
                assert_eq!(scheduler.len(), 0);
                assert!(scheduler.is_empty());
                let mut tasks = Vec::new();
                for i in 0..NUM_TASKS {
                    let t = Arc::new(<$task>::new(i));
                    tasks.push(t.clone());
                    scheduler.add_task(t);
                    assert_eq!(scheduler.len(), i + 1);
                }
                assert!(!scheduler.is_empty());
                for k in 0..NUM_TASKS {
                    assert_eq!(scheduler.len(), NUM_TASKS - k);
                    let _ = scheduler.pick_next_task().unwrap();
                }
                assert_eq!(scheduler.len(), 0);
                assert!(scheduler.pick_next_task().is_none());
                assert_eq!(scheduler.len(), 0);
                let mut scheduler = <$scheduler>::new();
                for t in &tasks {
                    scheduler.add_task(t.clone());
                }
                assert_eq!(scheduler.len(), NUM_TASKS);
                assert!(scheduler.remove_task(&tasks[3]).is_some());
                assert_eq!(scheduler.len(), NUM_TASKS - 1);
            }

            #[test]
            fn test_steal() {
                let mut scheduler = <$scheduler>::new();
                assert!(scheduler.pick_stealable_task(|_| true).is_none());
                for i in 0..5 {
                    scheduler.add_task(Arc::new(<$task>::new(i)));
                }
                assert!(scheduler.pick_stealable_task(|_| false).is_none());
                assert_eq!(scheduler.len(), 5);
                let stolen = scheduler.pick_stealable_task(|v| *v == 3).unwrap();
                assert_eq!(*stolen.inner(), 3);
                assert_eq!(scheduler.len(), 4);
                assert!(scheduler.pick_stealable_task(|v| *v == 3).is_none());
            }

            #[test]
            fn bench_yield() {
                const NUM_TASKS: usize = 1_000_000;
                const COUNT: usize = NUM_TASKS * 3;

                let mut scheduler = <$scheduler>::new();
                for i in 0..NUM_TASKS {
                    scheduler.add_task(Arc::new(<$task>::new(i)));
                }

                let t0 = std::time::Instant::now();
                for _ in 0..COUNT {
                    let next = scheduler.pick_next_task().unwrap();
                    scheduler.put_prev_task(next, false);
                }
                let t1 = std::time::Instant::now();
                println!(
                    "  {}: task yield speed: {:?}/task",
                    stringify!($scheduler),
                    (t1 - t0) / (COUNT as u32)
                );
            }

            #[test]
            fn bench_remove() {
                const NUM_TASKS: usize = 10_000;

                let mut scheduler = <$scheduler>::new();
                let mut tasks = Vec::new();
                for i in 0..NUM_TASKS {
                    let t = Arc::new(<$task>::new(i));
                    tasks.push(t.clone());
                    scheduler.add_task(t);
                }

                let t0 = std::time::Instant::now();
                for i in (0..NUM_TASKS).rev() {
                    let t = scheduler.remove_task(&tasks[i]).unwrap();
                    assert_eq!(*t.inner(), i);
                }
                let t1 = std::time::Instant::now();
                println!(
                    "  {}: task remove speed: {:?}/task",
                    stringify!($scheduler),
                    (t1 - t0) / (NUM_TASKS as u32)
                );
            }
        }
    };
}

def_test_sched!(fifo, FifoScheduler::<usize>, FifoTask::<usize>);
def_test_sched!(rr, RRScheduler::<usize, 5>, RRTask::<usize, 5>);
def_test_sched!(cfs, CFScheduler::<usize>, CFSTask::<usize>);
def_test_sched!(rt, RTScheduler::<usize>, RTTask::<usize>);

#[test]
fn rt_priority_preemption() {
    use alloc::sync::Arc;

    use crate::{BaseScheduler, RTScheduler, RTTask};

    // Low-priority task (priority 0) is running.
    let mut scheduler = RTScheduler::<usize>::new();
    let low = Arc::new(RTTask::<usize>::new_with_priority(0, 0));
    scheduler.add_task(low.clone());

    // Pick the low-priority task (it's the only one).
    let current = scheduler.pick_next_task().unwrap();
    assert_eq!(*current.inner(), 0);

    // A high-priority task (priority 10) becomes ready.
    let high = Arc::new(RTTask::<usize>::new_with_priority(1, 10));
    scheduler.add_task(high);

    // On the next tick, the scheduler should detect the higher-priority task
    // and signal preemption.
    let need_resched = scheduler.task_tick(&current);
    assert!(
        need_resched,
        "higher-priority task should trigger preemption"
    );

    // Put the low-priority task back (preempted).
    scheduler.put_prev_task(current, true);

    // The next task picked should be the high-priority one.
    let next = scheduler.pick_next_task().unwrap();
    assert_eq!(*next.inner(), 1, "highest-priority task should be picked");
}

#[test]
fn rt_same_priority_round_robin() {
    use alloc::sync::Arc;

    use crate::{BaseScheduler, RTScheduler, RTTask};

    let mut scheduler = RTScheduler::<usize>::new();

    // Add 3 tasks at the same priority (default = 0).
    for i in 0..3 {
        scheduler.add_task(Arc::new(RTTask::<usize>::new(i)));
    }

    // They should be picked in round-robin order: 0, 1, 2, 0, 1, 2, ...
    for i in 0..3 * 5 - 1 {
        let next = scheduler.pick_next_task().unwrap();
        assert_eq!(*next.inner(), i % 3, "round-robin order at i={}", i);

        // Tick until time slice expires, then yield.
        loop {
            let need_resched = scheduler.task_tick(&next);
            if need_resched {
                break;
            }
        }
        scheduler.put_prev_task(next, false);
    }
}

#[test]
fn rt_set_priority_changes_order() {
    use alloc::sync::Arc;

    use crate::{BaseScheduler, RTScheduler, RTTask};

    let mut scheduler = RTScheduler::<usize>::new();

    let task_a = Arc::new(RTTask::<usize>::new(0)); // priority 0
    let task_b = Arc::new(RTTask::<usize>::new(1)); // priority 0
    scheduler.add_task(task_a);
    scheduler.add_task(task_b);

    // Initially both at priority 0: pick A first.
    let first = scheduler.pick_next_task().unwrap();
    assert_eq!(*first.inner(), 0);
    scheduler.put_prev_task(first, false);

    // Raise B's priority to 10.
    let second = scheduler.pick_next_task().unwrap();
    assert_eq!(*second.inner(), 1);
    assert!(scheduler.set_priority(&second, 10));
    scheduler.put_prev_task(second, false);

    // Now B (priority 10) should always be picked before A (priority 0).
    let next = scheduler.pick_next_task().unwrap();
    assert_eq!(*next.inner(), 1, "higher-priority task should be picked");
}

/// Simulate a mixed-priority scenario that exercises the full RT scheduler
/// behavior: same-priority round-robin, cross-priority preemption, time-slice
/// expiry, and dynamic priority elevation.
///
/// Scenario:
/// - Task A (prio 0, low)    — background worker
/// - Task B (prio 0, low)    — another background worker (round-robins with A)
/// - Task C (prio 10, high)  — real-time task that arrives mid-run
/// - Task D (prio 5, medium) — medium-priority task
///
/// Timeline:
/// 1. A and B round-robin at priority 0.
/// 2. C (prio 10) arrives → preempts A/B.
/// 3. C exhausts its time slice → D (prio 5) arrives → D runs (preempts A/B).
/// 4. D yields → A/B resume round-robin.
/// 5. A's priority is elevated to 8 → A preempts B.
#[test]
fn rt_mixed_priority_simulation() {
    use alloc::{format, string::String, vec::Vec};

    use crate::{BaseScheduler, RTScheduler, RTTask};

    let mut scheduler = RTScheduler::<usize>::new();
    let mut trace: Vec<String> = Vec::new();

    // Helper: run `ticks` ticks for the current task, then put it back.
    // Returns the task value that was running.
    let run_ticks = |scheduler: &mut RTScheduler<usize>,
                     task: &alloc::sync::Arc<RTTask<usize>>,
                     trace: &mut Vec<String>,
                     label: &str| {
        loop {
            let resched = scheduler.task_tick(task);
            if resched {
                trace.push(format!(
                    "  [tick] {} (prio={}) -> resched",
                    label,
                    task.priority()
                ));
                break;
            }
        }
    };

    // ── Phase 1: Two low-priority tasks round-robin ──────────────────
    let task_a = alloc::sync::Arc::new(RTTask::<usize>::new_with_priority(0, 0));
    let task_b = alloc::sync::Arc::new(RTTask::<usize>::new_with_priority(1, 0));
    scheduler.add_task(task_a.clone());
    scheduler.add_task(task_b.clone());

    trace.push(String::from("Phase 1: A(0) and B(0) round-robin"));

    // Pick A first (earlier task_id).
    let running = scheduler.pick_next_task().unwrap();
    assert_eq!(*running.inner(), 0, "Phase 1: A should run first");
    trace.push(String::from("  run: A (prio=0)"));
    run_ticks(&mut scheduler, &running, &mut trace, "A");
    scheduler.put_prev_task(running, false); // time slice expired

    // Pick B next (round-robin).
    let running = scheduler.pick_next_task().unwrap();
    assert_eq!(
        *running.inner(),
        1,
        "Phase 1: B should run next (round-robin)"
    );
    trace.push(String::from("  run: B (prio=0)"));
    run_ticks(&mut scheduler, &running, &mut trace, "B");
    scheduler.put_prev_task(running, false); // time slice expired

    // Pick A again.
    let running = scheduler.pick_next_task().unwrap();
    assert_eq!(*running.inner(), 0, "Phase 1: A round-robins again");
    trace.push(String::from("  run: A (prio=0)"));

    // ── Phase 2: High-priority C arrives, preempts A ─────────────────
    trace.push(String::from("Phase 2: C(prio=10) arrives — preemption!"));

    let task_c = alloc::sync::Arc::new(RTTask::<usize>::new_with_priority(2, 10));
    scheduler.add_task(task_c.clone());

    // Next tick should detect C's higher priority and trigger reschedule.
    let resched = scheduler.task_tick(&running);
    assert!(
        resched,
        "Phase 2: tick should detect higher-priority C and trigger preemption"
    );
    trace.push(String::from(
        "  [tick] A detects C(prio=10) ready -> preempt!",
    ));

    // Put A back (preempted, keeps remaining slice).
    scheduler.put_prev_task(running, true);

    // C should be picked next (highest priority).
    let running = scheduler.pick_next_task().unwrap();
    assert_eq!(
        *running.inner(),
        2,
        "Phase 2: C should be picked (highest prio)"
    );
    trace.push(String::from("  run: C (prio=10)"));

    // ── Phase 3: C runs, D (prio 5) arrives, C's slice expires ───────
    trace.push(String::from("Phase 3: C runs, D(prio=5) arrives"));

    let task_d = alloc::sync::Arc::new(RTTask::<usize>::new_with_priority(3, 5));
    // D arrives while C is running. C is higher priority, so no preemption yet.
    scheduler.add_task(task_d.clone());
    trace.push(String::from("  D(prio=5) added to ready queue"));

    // C keeps running (prio 10 > 5, no preemption from tick).
    let resched = scheduler.task_tick(&running);
    assert!(
        !resched,
        "Phase 3: C(prio=10) should NOT be preempted by D(prio=5)"
    );
    trace.push(String::from("  [tick] C continues (prio 10 > 5)"));

    // C exhausts its time slice.
    loop {
        if scheduler.task_tick(&running) {
            break;
        }
    }
    trace.push(String::from("  [tick] C time slice expired -> resched"));
    scheduler.put_prev_task(running, false); // C's slice expired

    // C is still highest priority, so it gets picked again (correct RT behavior).
    let c_again = scheduler.pick_next_task().unwrap();
    assert_eq!(
        *c_again.inner(),
        2,
        "Phase 3: C should run again (still highest prio)"
    );
    trace.push(String::from(
        "  run: C (prio=10) — still highest, runs again",
    ));

    // C finishes its real-time work and blocks (removed from ready queue).
    trace.push(String::from("  C blocks — removed from ready queue"));

    // ── Phase 4: D (prio 5) runs before A/B (prio 0) ─────────────────
    trace.push(String::from("Phase 4: D(prio=5) runs before A/B(prio=0)"));

    let running = scheduler.pick_next_task().unwrap();
    assert_eq!(
        *running.inner(),
        3,
        "Phase 4: D(prio=5) should be picked before A/B(prio=0)"
    );
    trace.push(String::from("  run: D (prio=5)"));
    run_ticks(&mut scheduler, &running, &mut trace, "D");
    // D finishes and blocks (not put back = removed from scheduler).
    trace.push(String::from("  D blocks — removed from ready queue"));

    // ── Phase 5: A/B resume, then A's priority is elevated ───────────
    trace.push(String::from(
        "Phase 5: A/B resume, then A priority elevated to 8",
    ));

    // After D, the highest remaining is A and B (both prio 0).
    let running = scheduler.pick_next_task().unwrap();
    assert!(
        *running.inner() == 0 || *running.inner() == 1,
        "Phase 5: A or B should resume (prio 0)"
    );
    trace.push(format!("  run: task {} (prio=0)", *running.inner()));

    // Elevate A's priority to 8 while it's in the ready queue.
    // Put current task back first.
    scheduler.put_prev_task(running, false);

    // Find and elevate A.
    let a_in_queue = scheduler.pick_next_task().unwrap();
    if *a_in_queue.inner() == 0 {
        assert!(scheduler.set_priority(&a_in_queue, 8));
        scheduler.put_prev_task(a_in_queue, false);
        trace.push(String::from("  A priority elevated 0 -> 8"));

        // A (prio 8) should now be picked before B (prio 0).
        let next = scheduler.pick_next_task().unwrap();
        assert_eq!(
            *next.inner(),
            0,
            "Phase 5: A(prio=8) should preempt B(prio=0)"
        );
        trace.push(String::from("  run: A (prio=8) — elevated!"));
    } else {
        // B was picked, put it back and pick again — A should come next.
        scheduler.put_prev_task(a_in_queue, false);
        let next = scheduler.pick_next_task().unwrap();
        assert_eq!(
            *next.inner(),
            0,
            "Phase 5: A(prio=8) should be picked before B(prio=0)"
        );
        trace.push(String::from("  run: A (prio=8) — elevated!"));
    }

    // Print the trace for visual verification.
    println!("RT mixed-priority simulation trace:");
    for line in &trace {
        println!("{}", line);
    }
    println!("  Total events: {}", trace.len());
}

#[test]
fn rr_preempt_preserves_slice_but_forced_reschedule_rotates() {
    use alloc::sync::Arc;

    use crate::{BaseScheduler, RRScheduler, RRTask};

    let mut scheduler = RRScheduler::<usize, 5>::new();
    let current = Arc::new(RRTask::<usize, 5>::new(0));
    let remote = Arc::new(RRTask::<usize, 5>::new(1));
    scheduler.add_task(remote.clone());
    scheduler.put_prev_task(current.clone(), true);
    assert_eq!(
        *scheduler.pick_next_task().unwrap().inner(),
        0,
        "ordinary RR preemption preserves the current task's remaining slice",
    );

    let mut scheduler = RRScheduler::<usize, 5>::new();
    let current = Arc::new(RRTask::<usize, 5>::new(0));
    let remote = Arc::new(RRTask::<usize, 5>::new(1));
    scheduler.add_task(remote);
    scheduler.put_prev_task(current, false);
    assert_eq!(
        *scheduler.pick_next_task().unwrap().inner(),
        1,
        "forced remote reschedule must rotate the current task behind queued work",
    );
}

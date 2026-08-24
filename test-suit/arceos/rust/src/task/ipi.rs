use core::{
    cmp::min,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::{
    os::arceos::{
        api::{
            task::{AxCpuMask, ax_set_current_affinity},
            time::ax_monotonic_time,
        },
        modules::{
            ax_hal::{self, irq::CpuId, percpu::this_cpu_id},
            ax_ipi::{self, IpiNotification},
            ax_task::{current_thread_id, runtime::MonotonicDeadline, task_test_hooks},
        },
        task::WaitQueue,
    },
    println,
    sync::Arc,
    thread,
    time::{Duration, Instant},
    vec::Vec,
};

const MAX_SENDER_CPUS: usize = 3;
const IDLE_WAKE_POLLS: usize = 100_000;
const POST_IPI_WAITER_COUNT: usize = 16;
const POST_IPI_SHARED_DEADLINE: Duration = Duration::from_millis(500);
const POST_IPI_PROGRESS_TIMEOUT: Duration = Duration::from_secs(3);
const STALL_POLLS: usize = 200;
const POLL_INTERVAL_MS: u64 = 1;

static TARGET_CPU: AtomicUsize = AtomicUsize::new(0);
static EXECUTED_HARD_CALLS: AtomicUsize = AtomicUsize::new(0);
static IDLE_TARGET_MASKED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_PUBLISHED: AtomicBool = AtomicBool::new(false);
static IDLE_IPI_ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);
static POST_IPI_WAITERS: WaitQueue = WaitQueue::new();
static POST_IPI_COMPLETED: WaitQueue = WaitQueue::new();
static POST_IPI_STARTED_COUNT: AtomicUsize = AtomicUsize::new(0);
static POST_IPI_COMPLETED_COUNT: AtomicUsize = AtomicUsize::new(0);
static POST_IPI_START: AtomicBool = AtomicBool::new(false);

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
        "task did not migrate to CPU {cpu_id}"
    );
}

unsafe fn counting_hard_call(argument: *mut ()) {
    let expected_cpu = unsafe { *(argument as *const usize) };
    assert_eq!(
        this_cpu_id(),
        expected_cpu,
        "IPI hard call ran on the wrong CPU"
    );
    EXECUTED_HARD_CALLS.fetch_add(1, Ordering::Release);
}

unsafe fn idle_wake_callback(_argument: *mut ()) {
    let target_cpu = TARGET_CPU.load(Ordering::Relaxed);
    assert_eq!(
        this_cpu_id(),
        target_cpu,
        "idle-wake IPI callback ran on the wrong CPU"
    );
    IDLE_IPI_ACKNOWLEDGED.store(true, Ordering::Release);
}

#[cfg(target_arch = "loongarch64")]
fn set_idle_test_timer_irq_enabled(enabled: bool) {
    ax_hal::asm::set_timer_irq_enabled(enabled);
}

#[cfg(not(target_arch = "loongarch64"))]
fn set_idle_test_timer_irq_enabled(_enabled: bool) {}

fn exercise_irq_masked_idle_wake(target_cpu: usize, sender_cpu: usize) {
    IDLE_TARGET_MASKED.store(false, Ordering::Release);
    IDLE_IPI_PUBLISHED.store(false, Ordering::Release);
    IDLE_IPI_ACKNOWLEDGED.store(false, Ordering::Release);

    let target = thread::spawn(move || {
        pin_current_to_cpu(target_cpu);
        ax_hal::asm::disable_irqs();
        // LoongArch has a separate local timer line. Mask it for this narrow
        // window so a later scheduler tick cannot hide a bad return into IDLE
        // after the already-consumed IPI.
        set_idle_test_timer_irq_enabled(false);
        assert!(
            !ax_hal::asm::irqs_enabled(),
            "idle-wake target must publish readiness with IRQs masked"
        );
        IDLE_TARGET_MASKED.store(true, Ordering::Release);

        while !IDLE_IPI_PUBLISHED.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        // The IPI is already pending before this final idle handoff. On
        // LoongArch it is taken between CRMD.IE publication and IDLE, so the
        // trap path must resume after IDLE instead of sleeping after the only
        // wake event was consumed. Other architectures exercise their
        // equivalent IRQ-masked atomic wait primitive.
        ax_hal::asm::wait_for_irqs_disabled();
        set_idle_test_timer_irq_enabled(true);
        assert!(
            ax_hal::asm::irqs_enabled(),
            "IRQ-masked idle wait must return with IRQ delivery enabled"
        );

        for _ in 0..IDLE_WAKE_POLLS {
            if IDLE_IPI_ACKNOWLEDGED.load(Ordering::Acquire) {
                return;
            }
            core::hint::spin_loop();
        }
        panic!("pending IPI did not wake the IRQ-masked idle handoff");
    });

    let sender = thread::spawn(move || {
        pin_current_to_cpu(sender_cpu);
        while !IDLE_TARGET_MASKED.load(Ordering::Acquire) {
            thread::yield_now();
        }
        IDLE_IPI_PUBLISHED.store(true, Ordering::Release);
        // SAFETY: the thunk uses only static atomics and is bounded hard-IRQ
        // work. `call_on_cpu` does not return before it has completed.
        unsafe {
            ax_ipi::call_on_cpu(CpuId(target_cpu), idle_wake_callback, core::ptr::null_mut())
        }
        .expect("idle-wake hard call failed");
    });

    sender.join().unwrap();
    target.join().unwrap();
}

fn verify_wait_queue_deadlines_during_ipi(target_cpu: usize) {
    POST_IPI_STARTED_COUNT.store(0, Ordering::Release);
    POST_IPI_COMPLETED_COUNT.store(0, Ordering::Release);
    POST_IPI_START.store(false, Ordering::Release);
    let waiter_ids = Arc::new(
        (0..POST_IPI_WAITER_COUNT)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>(),
    );
    let shared_deadline_ns = Arc::new(AtomicU64::new(0));
    let mut waiters = Vec::new();
    for index in 0..POST_IPI_WAITER_COUNT {
        let waiter_ids = Arc::clone(&waiter_ids);
        let shared_deadline_ns = Arc::clone(&shared_deadline_ns);
        waiters.push(thread::spawn(move || {
            pin_current_to_cpu(target_cpu);
            waiter_ids[index].store(
                current_thread_id()
                    .expect("deadline waiter must have a scheduler identity")
                    .as_u64(),
                Ordering::Release,
            );
            while !POST_IPI_START.load(Ordering::Acquire) {
                thread::yield_now();
            }
            POST_IPI_STARTED_COUNT.fetch_add(1, Ordering::Release);
            let deadline =
                MonotonicDeadline::from_nanos(shared_deadline_ns.load(Ordering::Acquire))
                    .expect("shared deadline must fit the monotonic clock domain");
            let timed_out = POST_IPI_WAITERS.wait_until_deadline(deadline, || false);
            assert!(
                timed_out,
                "post-IPI waiter must complete through its deadline"
            );
            POST_IPI_COMPLETED_COUNT.fetch_add(1, Ordering::Release);
            POST_IPI_COMPLETED.notify_one();
        }));
    }

    let setup_started = Instant::now();
    while waiter_ids
        .iter()
        .any(|thread| thread.load(Ordering::Acquire) == 0)
    {
        if setup_started.elapsed() >= POST_IPI_PROGRESS_TIMEOUT {
            let ready = waiter_ids
                .iter()
                .filter(|thread| thread.load(Ordering::Acquire) != 0)
                .count();
            panic!(
                "deadline waiters did not publish their scheduler identities: \
                 ready={ready}/{POST_IPI_WAITER_COUNT}"
            );
        }
        thread::yield_now();
    }
    let deadline = MonotonicDeadline::from_duration(
        ax_monotonic_time()
            .checked_add(POST_IPI_SHARED_DEADLINE)
            .expect("shared deadline must not overflow"),
    );
    shared_deadline_ns.store(deadline.as_nanos(), Ordering::Release);
    POST_IPI_START.store(true, Ordering::Release);
    let block_started = Instant::now();
    while waiter_ids.iter().any(|thread| {
        let thread = thread.load(Ordering::Acquire);
        !task_test_hooks::thread_is_blocked(thread)
    }) {
        assert_eq!(
            POST_IPI_COMPLETED_COUNT.load(Ordering::Acquire),
            0,
            "a shared deadline elapsed before every waiter committed its park"
        );
        if block_started.elapsed() >= POST_IPI_PROGRESS_TIMEOUT {
            let blocked = waiter_ids
                .iter()
                .filter(|thread| task_test_hooks::thread_is_blocked(thread.load(Ordering::Acquire)))
                .count();
            panic!(
                "deadline waiters did not all commit their parks: \
                 blocked={blocked}/{POST_IPI_WAITER_COUNT}"
            );
        }
        thread::yield_now();
    }
    assert_eq!(
        POST_IPI_STARTED_COUNT.load(Ordering::Acquire),
        POST_IPI_WAITER_COUNT,
        "every blocked waiter must have entered the shared-deadline wait"
    );
    task_test_hooks::arm_ktimer_pending_yield_probe(target_cpu);
    let calls_before = EXECUTED_HARD_CALLS.load(Ordering::Relaxed);
    // SAFETY: call_on_cpu is synchronous and the argument remains borrowed
    // until the bounded hard-IRQ callback completes.
    unsafe {
        ax_ipi::call_on_cpu(
            CpuId(target_cpu),
            counting_hard_call,
            core::ptr::from_ref(&target_cpu).cast_mut().cast(),
        )
    }
    .expect("deadline-overlap hard call failed");
    assert_eq!(
        EXECUTED_HARD_CALLS.load(Ordering::Relaxed),
        calls_before + 1,
        "hard call must complete while deadline waiters are active"
    );
    let completion_started = Instant::now();
    while POST_IPI_COMPLETED_COUNT.load(Ordering::Acquire) != POST_IPI_WAITER_COUNT {
        assert_eq!(
            task_test_hooks::ktimer_pending_yield_count()
                .expect("ktimer pending-yield probe must remain armed"),
            0,
            "ktimer worker yielded before draining one bounded batch of co-due deadlines"
        );
        if completion_started.elapsed() >= POST_IPI_PROGRESS_TIMEOUT {
            let completed = POST_IPI_COMPLETED_COUNT.load(Ordering::Acquire);
            let blocked = waiter_ids
                .iter()
                .filter(|thread| task_test_hooks::thread_is_blocked(thread.load(Ordering::Acquire)))
                .count();
            panic!(
                "co-due deadline waiters did not all make bounded progress: \
                 completed={completed}/{POST_IPI_WAITER_COUNT}, \
                 blocked={blocked}/{POST_IPI_WAITER_COUNT}"
            );
        }
        thread::yield_now();
    }
    assert_eq!(
        task_test_hooks::take_ktimer_pending_yield_count()
            .expect("ktimer pending-yield probe must remain armed"),
        0,
        "ktimer worker yielded before draining one bounded batch of co-due deadlines"
    );
    for waiter in waiters {
        waiter.join().unwrap();
    }
}

fn wait_for_counter_or_stall(counter: &AtomicUsize, expected: usize) -> bool {
    let mut last_executed = counter.load(Ordering::Acquire);
    let mut stalled_polls = 0;

    loop {
        let executed = counter.load(Ordering::Acquire);
        if executed == expected {
            return true;
        }

        if executed == last_executed {
            stalled_polls += 1;
            if stalled_polls >= STALL_POLLS {
                return false;
            }
        } else {
            last_executed = executed;
            stalled_polls = 0;
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

fn wait_for_fresh_self_ipi(cpu_id: usize) -> bool {
    for _ in 0..STALL_POLLS {
        match ax_ipi::notify_cpu(CpuId(cpu_id)).expect("failed to send self IPI") {
            IpiNotification::Sent => return true,
            IpiNotification::Coalesced => {
                thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }
    false
}

fn verify_self_ipi_delivery(cpu_id: usize) {
    pin_current_to_cpu(cpu_id);
    // A second fresh send is possible only after the handler claims the first
    // physical self-SGI; an undelivered edge remains coalesced indefinitely.
    assert!(
        wait_for_fresh_self_ipi(cpu_id),
        "could not send a fresh self IPI to CPU {cpu_id}"
    );
    assert!(
        wait_for_fresh_self_ipi(cpu_id),
        "self IPI was not claimed on CPU {cpu_id}"
    );
}

fn verify_remote_owner_work_delivery(sender_cpu: usize, target_cpu: usize) {
    pin_current_to_cpu(sender_cpu);
    let target_cpu = u32::try_from(target_cpu).expect("test CPU id must fit in u32");
    assert!(
        task_test_hooks::request_cpu_owner_work(target_cpu)
            .expect("failed to publish remote scheduler owner work"),
        "remote owner work must complete its scheduler doorbell transaction",
    );
}

fn verify_detached_deadline_owner_uses_one_publication(target_cpu: usize) {
    let target_cpu = u32::try_from(target_cpu).expect("test CPU id must fit in u32");
    assert!(
        task_test_hooks::exercise_detached_deadline_owner_work(target_cpu)
            .expect("failed to publish detached Deadline owner work"),
        "one Deadline reservation detach must complete its owner-work delivery",
    );
}

fn verify_bounded_owner_control_rearms_sticky_work() {
    task_test_hooks::publish_bounded_owner_control_remainder()
        .expect("failed to publish a bounded owner-control remainder");
    for _ in 0..256 {
        if let Some(state) = task_test_hooks::take_bounded_owner_control_rearm() {
            assert!(
                !state.after_drain,
                "the claimed owner-work bit must stay clear while the probe publishes an \
                 independent preemption request"
            );
            assert!(
                state.after_ack,
                "a bounded owner-control remainder must rearm sticky owner work independently of \
                 preemption"
            );
            return;
        }
        thread::yield_now();
    }
    panic!("owner-control remainder was not acknowledged");
}

fn verify_pending_owner_control_coalesces_scheduler_request() {
    let publications = task_test_hooks::publish_coalesced_owner_control_twice()
        .expect("failed to publish one coalesced owner-control node");
    assert!(
        publications.previous_owner_work,
        "the deterministic interleaving must retain unrelated sticky owner work"
    );
    assert!(
        publications.first_head,
        "one new owner-control membership must own the inbox head notification"
    );
    assert!(
        !publications.duplicate_head,
        "an already-pending owner-control node must not own another head notification"
    );
}

fn verify_fresh_owner_control_head_rearms_a_pending_request() {
    let publication = task_test_hooks::publish_owner_control_after_pending_request()
        .expect("failed to publish owner control after a pending request");
    assert!(
        publication.previous_owner_work,
        "the fresh head must observe the older sticky owner-work request"
    );
    assert!(
        publication.head,
        "a fresh owner-control inbox head must own a new physical notification attempt"
    );
}

fn verify_sticky_scheduler_reasons_coalesce() {
    let owner_work = task_test_hooks::request_current_owner_work_twice()
        .expect("failed to publish duplicate current-CPU owner work");
    assert!(
        owner_work.first && !owner_work.duplicate,
        "a pending owner-work reason must suppress a duplicate logical publication"
    );

    let combined = task_test_hooks::request_current_combined_scheduler_work_twice()
        .expect("failed to publish duplicate combined scheduler work");
    assert!(
        combined.first && !combined.duplicate,
        "pending preempt/owner-work reasons must remain one sticky publication"
    );
}

fn run_concurrent_hard_calls(target_cpu: usize, sender_cpus: &[usize]) {
    EXECUTED_HARD_CALLS.store(0, Ordering::Relaxed);

    let ready = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(AtomicBool::new(false));
    let mut senders = Vec::with_capacity(sender_cpus.len());

    for &sender_cpu in sender_cpus {
        let ready = ready.clone();
        let start = start.clone();
        senders.push(thread::spawn(move || {
            pin_current_to_cpu(sender_cpu);
            ready.fetch_add(1, Ordering::Release);

            while !start.load(Ordering::Acquire) {
                thread::yield_now();
            }

            // SAFETY: call_on_cpu is synchronous, so this stack-local target
            // remains borrowed until the bounded hard-IRQ-safe thunk returns.
            unsafe {
                ax_ipi::call_on_cpu(
                    CpuId(target_cpu),
                    counting_hard_call,
                    core::ptr::from_ref(&target_cpu).cast_mut().cast(),
                )
            }
            .expect("failed to execute IPI hard call");
        }));
    }

    while ready.load(Ordering::Acquire) != sender_cpus.len() {
        thread::yield_now();
    }
    start.store(true, Ordering::Release);

    assert!(
        wait_for_counter_or_stall(&EXECUTED_HARD_CALLS, sender_cpus.len()),
        "IPI hard calls stalled at {}/{}",
        EXECUTED_HARD_CALLS.load(Ordering::Acquire),
        sender_cpus.len()
    );

    for sender in senders {
        sender.join().unwrap();
    }
}

pub fn run() -> crate::TestResult {
    assert!(
        task_test_hooks::fair_hrtick_tracks_request_deadline(),
        "Fair hrtick expired at RUN_TO_PARITY protection instead of the EEVDF request deadline"
    );
    assert!(
        task_test_hooks::fair_hrtick_uses_linux_minimum_delta(),
        "a Fair hrtick armed below Linux's 10 us timer-DoS floor"
    );
    assert!(
        task_test_hooks::fair_request_renewal_preserves_lag(),
        "Fair request renewal discarded Linux EEVDF positive lag"
    );
    assert!(
        task_test_hooks::equal_slice_wakeup_preserves_current_protection(),
        "an equal-slice Fair wakeup broke Linux RUN_TO_PARITY protection"
    );
    assert!(
        task_test_hooks::sync_wakeup_obeys_migration_cost(),
        "a synchronous Fair wakeup ignored Linux migration-cost batching"
    );
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 2 {
        println!("task_ipi: skipped on single CPU");
        return Ok(());
    }

    let target_cpu = cpu_num - 1;
    let sender_cpus = (0..target_cpu)
        .take(min(MAX_SENDER_CPUS, cpu_num - 1))
        .collect::<Vec<_>>();
    assert!(!sender_cpus.is_empty(), "need at least one sender CPU");

    TARGET_CPU.store(target_cpu, Ordering::Relaxed);
    pin_current_to_cpu(sender_cpus[0]);
    verify_sticky_scheduler_reasons_coalesce();
    verify_fresh_owner_control_head_rearms_a_pending_request();
    verify_pending_owner_control_coalesces_scheduler_request();
    verify_bounded_owner_control_rearms_sticky_work();
    verify_detached_deadline_owner_uses_one_publication(target_cpu);
    verify_remote_owner_work_delivery(sender_cpus[0], target_cpu);
    exercise_irq_masked_idle_wake(target_cpu, sender_cpus[0]);
    verify_self_ipi_delivery(sender_cpus[0]);

    run_concurrent_hard_calls(target_cpu, &sender_cpus);

    pin_current_to_cpu(sender_cpus[0]);
    EXECUTED_HARD_CALLS.store(0, Ordering::Relaxed);
    // SAFETY: call_on_cpu is synchronous, so target_cpu remains borrowed until
    // the hard-IRQ-safe counting thunk completes on the target.
    unsafe {
        ax_ipi::call_on_cpu(
            CpuId(target_cpu),
            counting_hard_call,
            core::ptr::from_ref(&target_cpu).cast_mut().cast(),
        )
    }
    .expect("failed to execute IPI hard call");
    assert_eq!(EXECUTED_HARD_CALLS.load(Ordering::Relaxed), 1);
    verify_wait_queue_deadlines_during_ipi(target_cpu);

    println!(
        "task_ipi: passed self-claim on CPU {} and {} concurrent hard calls on CPU {target_cpu}",
        sender_cpus[0],
        sender_cpus.len()
    );

    Ok(())
}

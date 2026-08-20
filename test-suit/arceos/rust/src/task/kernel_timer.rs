use std::{
    boxed::Box,
    os::arceos::{
        api::time::ax_monotonic_time,
        modules::ax_task::{self, task_test_hooks},
        task::{
            HardKernelTimerAction, HardKernelTimerCallback, KernelTimerAction,
            KernelTimerCancelOutcome, MonotonicDeadline, arm_hard_kernel_timer,
            cancel_kernel_timer, register_hard_restartable_kernel_timer, register_kernel_timer,
            register_restartable_kernel_timer,
        },
    },
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

static CALLBACK_ORDER: AtomicUsize = AtomicUsize::new(0);
static CANCELLED_CALLBACK_RAN: AtomicBool = AtomicBool::new(false);
static RESTARTABLE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static HARD_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static HARD_BURST_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static HARD_BURST_LEFT_IRQ: AtomicBool = AtomicBool::new(false);
static STABLE_HARD_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static HARD_CALLBACK_OBSERVED_IRQ: AtomicBool = AtomicBool::new(false);
static HARD_CALLBACK_DROPPED: AtomicBool = AtomicBool::new(false);
static HARD_CALLBACK_DROPPED_IN_IRQ: AtomicBool = AtomicBool::new(false);

struct HardCallbackDropProbe;

impl Drop for HardCallbackDropProbe {
    fn drop(&mut self) {
        HARD_CALLBACK_DROPPED_IN_IRQ
            .store(task_test_hooks::in_hard_irq_context(), Ordering::Release);
        HARD_CALLBACK_DROPPED.store(true, Ordering::Release);
    }
}

pub fn run() -> crate::TestResult {
    assert!(
        task_test_hooks::softirq_activation_preserves_hard_deadline(),
        "softirq ownership must not hide a scheduler hard deadline"
    );
    CALLBACK_ORDER.store(0, Ordering::Release);
    CANCELLED_CALLBACK_RAN.store(false, Ordering::Release);
    RESTARTABLE_CALLBACKS.store(0, Ordering::Release);
    HARD_CALLBACKS.store(0, Ordering::Release);
    HARD_BURST_CALLBACKS.store(0, Ordering::Release);
    HARD_BURST_LEFT_IRQ.store(false, Ordering::Release);
    STABLE_HARD_CALLBACKS.store(0, Ordering::Release);
    HARD_CALLBACK_OBSERVED_IRQ.store(false, Ordering::Release);
    HARD_CALLBACK_DROPPED.store(false, Ordering::Release);
    HARD_CALLBACK_DROPPED_IN_IRQ.store(false, Ordering::Release);

    let now = ax_monotonic_time();
    let shared_deadline = MonotonicDeadline::from_duration(now + Duration::from_millis(40));
    let cancelled_deadline = MonotonicDeadline::from_duration(now + Duration::from_millis(60));

    register_kernel_timer(
        shared_deadline,
        Box::new(|_| {
            thread::yield_now();
            assert_eq!(CALLBACK_ORDER.fetch_add(1, Ordering::AcqRel), 0);
        }),
    )
    .map_err(|_| "failed to register first kernel timer")?;
    register_kernel_timer(
        shared_deadline,
        Box::new(|_| {
            thread::yield_now();
            assert_eq!(CALLBACK_ORDER.fetch_add(1, Ordering::AcqRel), 1);
        }),
    )
    .map_err(|_| "failed to register second kernel timer")?;
    let cancelled = register_kernel_timer(
        cancelled_deadline,
        Box::new(|_| CANCELLED_CALLBACK_RAN.store(true, Ordering::Release)),
    )
    .map_err(|_| "failed to register cancelled kernel timer")?;
    assert_eq!(
        cancel_kernel_timer(cancelled).map_err(|_| "failed to cancel kernel timer")?,
        KernelTimerCancelOutcome::Cancelled
    );
    let abandoned_probe = task_test_hooks::arm_ktimer_selection_probe(cancelled);
    drop(abandoned_probe);
    let restartable_deadline = MonotonicDeadline::from_duration(now + Duration::from_millis(20));
    let restartable = register_restartable_kernel_timer(
        restartable_deadline,
        Box::new(move |_| {
            let invocation = RESTARTABLE_CALLBACKS.fetch_add(1, Ordering::AcqRel) + 1;
            if invocation < 3 {
                KernelTimerAction::Rearm(restartable_deadline)
            } else {
                KernelTimerAction::Complete
            }
        }),
    )
    .map_err(|_| "failed to register restartable kernel timer")?;
    let selection_probe = task_test_hooks::arm_ktimer_selection_probe(restartable);
    let hard_deadline = MonotonicDeadline::from_duration(now + Duration::from_millis(10));
    let hard_callback = unsafe {
        // SAFETY: this callback performs bounded atomics and captures only a
        // drop probe whose destruction is owned by the timer reaper.
        HardKernelTimerCallback::new(Box::new({
            let drop_probe = HardCallbackDropProbe;
            move |_| {
                let _drop_probe = &drop_probe;
                HARD_CALLBACK_OBSERVED_IRQ
                    .store(task_test_hooks::in_hard_irq_context(), Ordering::Release);
                HARD_CALLBACKS.fetch_add(1, Ordering::AcqRel);
                HardKernelTimerAction::Complete
            }
        }))
    };
    register_hard_restartable_kernel_timer(hard_deadline, hard_callback)
        .map_err(|_| "failed to register hard kernel timer")?;
    for _ in 0..=ax_task::DEFAULT_BATCH_LIMIT {
        let callback = unsafe {
            // SAFETY: this callback performs two bounded atomic operations.
            HardKernelTimerCallback::new(Box::new(|_| {
                if !task_test_hooks::in_hard_irq_context() {
                    HARD_BURST_LEFT_IRQ.store(true, Ordering::Release);
                }
                HARD_BURST_CALLBACKS.fetch_add(1, Ordering::AcqRel);
                HardKernelTimerAction::Complete
            }))
        };
        register_hard_restartable_kernel_timer(hard_deadline, callback)
            .map_err(|_| "failed to register hard kernel timer burst")?;
    }
    let stable_hard_callback = unsafe {
        // SAFETY: this callback performs one bounded atomic operation.
        HardKernelTimerCallback::new(Box::new(|_| {
            STABLE_HARD_CALLBACKS.fetch_add(1, Ordering::AcqRel);
            HardKernelTimerAction::Disarm
        }))
    };
    let stable_hard_timer = register_hard_restartable_kernel_timer(
        MonotonicDeadline::from_duration(now + Duration::from_millis(15)),
        stable_hard_callback,
    )
    .map_err(|_| "failed to register stable hard kernel timer")?;

    let started = std::time::Instant::now();
    while CALLBACK_ORDER.load(Ordering::Acquire) != 2
        || RESTARTABLE_CALLBACKS.load(Ordering::Acquire) != 3
        || HARD_CALLBACKS.load(Ordering::Acquire) != 1
        || HARD_BURST_CALLBACKS.load(Ordering::Acquire) != ax_task::DEFAULT_BATCH_LIMIT + 1
        || STABLE_HARD_CALLBACKS.load(Ordering::Acquire) != 1
        || !HARD_CALLBACK_DROPPED.load(Ordering::Acquire)
    {
        if started.elapsed() >= Duration::from_secs(1) {
            return Err("kernel timer callbacks did not complete");
        }
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(40));
    assert!(!CANCELLED_CALLBACK_RAN.load(Ordering::Acquire));
    assert!(HARD_CALLBACK_OBSERVED_IRQ.load(Ordering::Acquire));
    assert!(
        !HARD_BURST_LEFT_IRQ.load(Ordering::Acquire),
        "hard timer callbacks must never be transferred to a task safe point"
    );
    assert!(!HARD_CALLBACK_DROPPED_IN_IRQ.load(Ordering::Acquire));
    arm_hard_kernel_timer(
        stable_hard_timer,
        MonotonicDeadline::from_duration(ax_monotonic_time() + Duration::from_millis(10)),
    )
    .map_err(|_| "failed to rearm stable hard kernel timer")?;
    let stable_rearm_started = std::time::Instant::now();
    while STABLE_HARD_CALLBACKS.load(Ordering::Acquire) != 2 {
        if stable_rearm_started.elapsed() >= Duration::from_secs(1) {
            return Err("stable hard kernel timer did not rearm");
        }
        thread::yield_now();
    }
    assert_eq!(
        cancel_kernel_timer(stable_hard_timer),
        Ok(KernelTimerCancelOutcome::Cancelled)
    );
    assert_eq!(
        selection_probe.take_base_entries(),
        Some(1),
        "one ktimer selection must promote and claim under one base transaction"
    );
    assert_eq!(
        cancel_kernel_timer(restartable)
            .map_err(|_| "failed to inspect completed restartable kernel timer")?,
        KernelTimerCancelOutcome::NotCancelled
    );
    Ok(())
}

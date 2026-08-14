use std::{
    boxed::Box,
    os::arceos::{
        api::time::ax_monotonic_time,
        modules::ax_task::task_test_hooks,
        task::{
            KernelTimerAction, KernelTimerCancelOutcome, MonotonicDeadline, cancel_kernel_timer,
            register_kernel_timer, register_restartable_kernel_timer,
        },
    },
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

static CALLBACK_ORDER: AtomicUsize = AtomicUsize::new(0);
static CANCELLED_CALLBACK_RAN: AtomicBool = AtomicBool::new(false);
static RESTARTABLE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> crate::TestResult {
    CALLBACK_ORDER.store(0, Ordering::Release);
    CANCELLED_CALLBACK_RAN.store(false, Ordering::Release);
    RESTARTABLE_CALLBACKS.store(0, Ordering::Release);

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

    let started = std::time::Instant::now();
    while CALLBACK_ORDER.load(Ordering::Acquire) != 2
        || RESTARTABLE_CALLBACKS.load(Ordering::Acquire) != 3
    {
        if started.elapsed() >= Duration::from_secs(1) {
            return Err("kernel timer callbacks did not complete");
        }
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(40));
    assert!(!CANCELLED_CALLBACK_RAN.load(Ordering::Acquire));
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

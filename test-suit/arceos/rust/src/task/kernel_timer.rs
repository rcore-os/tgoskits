use std::{
    boxed::Box,
    os::arceos::{
        api::time::ax_monotonic_time,
        task::{
            HardKernelTimerAction, HardKernelTimerCallback, KernelTimerAction,
            KernelTimerCancelOutcome, MonotonicDeadline, arm_hard_kernel_timer,
            cancel_kernel_timer, register_hard_restartable_kernel_timer, register_kernel_timer,
            register_restartable_kernel_timer,
        },
    },
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

static SOFT_CALLBACK_ORDER: AtomicUsize = AtomicUsize::new(0);
static CANCELLED_CALLBACK_RAN: AtomicBool = AtomicBool::new(false);
static RESTARTABLE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static HARD_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static STABLE_HARD_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) -> crate::TestResult {
    let started = Instant::now();
    while !condition() {
        if started.elapsed() >= Duration::from_secs(1) {
            return Err(message);
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

pub fn run() -> crate::TestResult {
    // Exercise the real platform's fixed-point conversion, including Q64 on
    // GHz counters and saturation on slower counters, before arming timers.
    let frequency = ax_hal::time::nanos_to_ticks(ax_hal::time::NANOS_PER_SEC);
    assert_ne!(frequency, 0);
    for ticks in [0, 1, frequency - 1, frequency, u64::MAX] {
        let expected = (u128::from(ticks) * u128::from(ax_hal::time::NANOS_PER_SEC)
            / u128::from(frequency))
        .min(u128::from(u64::MAX)) as u64;
        let actual = ax_hal::time::ticks_to_nanos(ticks);
        assert!(
            actual.abs_diff(expected) <= 1,
            "{frequency} Hz, {ticks} ticks"
        );
    }

    SOFT_CALLBACK_ORDER.store(0, Ordering::Release);
    CANCELLED_CALLBACK_RAN.store(false, Ordering::Release);
    RESTARTABLE_CALLBACKS.store(0, Ordering::Release);
    HARD_CALLBACKS.store(0, Ordering::Release);
    STABLE_HARD_CALLBACKS.store(0, Ordering::Release);

    let now = ax_monotonic_time();
    let shared_deadline = MonotonicDeadline::from_duration(now + Duration::from_millis(40));
    register_kernel_timer(
        shared_deadline,
        Box::new(move |_| {
            assert_eq!(SOFT_CALLBACK_ORDER.fetch_add(1, Ordering::AcqRel), 0);
        }),
    )
    .map_err(|_| "failed to register first kernel timer")?;
    register_kernel_timer(
        shared_deadline,
        Box::new(|_| {
            assert_eq!(SOFT_CALLBACK_ORDER.fetch_add(1, Ordering::AcqRel), 1);
        }),
    )
    .map_err(|_| "failed to register second kernel timer")?;

    let cancelled = register_kernel_timer(
        MonotonicDeadline::from_duration(now + Duration::from_millis(60)),
        Box::new(|_| CANCELLED_CALLBACK_RAN.store(true, Ordering::Release)),
    )
    .map_err(|_| "failed to register cancelled kernel timer")?;
    assert_eq!(
        cancel_kernel_timer(cancelled).map_err(|_| "failed to cancel kernel timer")?,
        KernelTimerCancelOutcome::Cancelled
    );

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

    let hard_callback = unsafe {
        // SAFETY: the hard callback performs one bounded atomic operation and
        // captures no state that requires task-context destruction.
        HardKernelTimerCallback::new(Box::new(|_| {
            HARD_CALLBACKS.fetch_add(1, Ordering::AcqRel);
            HardKernelTimerAction::Complete
        }))
    };
    register_hard_restartable_kernel_timer(
        MonotonicDeadline::from_duration(now + Duration::from_millis(10)),
        hard_callback,
    )
    .map_err(|_| "failed to register hard kernel timer")?;

    let stable_hard_callback = unsafe {
        // SAFETY: the hard callback performs one bounded atomic operation and
        // deliberately keeps its stable registration disarmed for rearming.
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

    wait_until(
        || {
            SOFT_CALLBACK_ORDER.load(Ordering::Acquire) == 2
                && RESTARTABLE_CALLBACKS.load(Ordering::Acquire) == 3
                && HARD_CALLBACKS.load(Ordering::Acquire) == 1
                && STABLE_HARD_CALLBACKS.load(Ordering::Acquire) == 1
        },
        "kernel timer callbacks did not complete",
    )?;
    thread::sleep(Duration::from_millis(40));
    assert!(!CANCELLED_CALLBACK_RAN.load(Ordering::Acquire));

    arm_hard_kernel_timer(
        stable_hard_timer,
        MonotonicDeadline::from_duration(ax_monotonic_time() + Duration::from_millis(10)),
    )
    .map_err(|_| "failed to rearm stable hard kernel timer")?;
    wait_until(
        || STABLE_HARD_CALLBACKS.load(Ordering::Acquire) == 2,
        "stable hard kernel timer did not rearm",
    )?;
    assert_eq!(
        cancel_kernel_timer(stable_hard_timer),
        Ok(KernelTimerCancelOutcome::Cancelled)
    );
    assert_eq!(
        cancel_kernel_timer(restartable)
            .map_err(|_| "failed to inspect completed restartable kernel timer")?,
        KernelTimerCancelOutcome::NotCancelled
    );
    Ok(())
}

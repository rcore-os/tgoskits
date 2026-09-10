//! Real-runtime cancellation ownership and reclaim-context regressions.
use std::sync::atomic::AtomicBool;

use ax_std::os::arceos::{
    guard::PreemptIrqSaveGuard,
    task::{runtime::TaskSystem, thread::TaskError},
};

use super::*;

pub(super) fn run() {
    allocation_failure_rollback();
    resource_failure_rollback();
    reclaim_rejects_atomic_context();
    managed_exit_requires_token();
    cancellation_is_deferred();
}

fn system() -> &'static TaskSystem {
    // SAFETY: this test runs after ArceOS installed its shutdown-lifetime,
    // pinned TaskSystem. The runtime capability is not a CPU-local borrow.
    unsafe {
        let handle = ax_std::os::arceos::task::runtime::task_runtime::task_system_handle();
        &*core::ptr::with_exposed_provenance::<TaskSystem>(handle.into_raw())
    }
}

fn cancellation_is_deferred() {
    let gate = Arc::new(ExtensionProbe::default());
    gate.pause_exit.store(true, Ordering::Release);
    let data = Box::into_raw(Box::new(Arc::clone(&gate))) as usize;
    // SAFETY: PROBE_OPS owns this boxed Arc and keeps the reaper consumer
    // occupied until the coordinator releases pause_exit.
    let extension = unsafe { ThreadExtension::new(data, &PROBE_OPS) };
    let blocker = ax_std::os::arceos::thread::builder("cancel-reaper-gate".into())
        .extension(extension)
        .spawn(|| {})
        .unwrap();
    wait_for(|| gate.exited.load(Ordering::Acquire) == 1);
    let mut cancelled = std::vec::Vec::new();
    for staged in [false, true] {
        let prepared = ax_std::os::arceos::thread::builder("cancel-atomic".into())
            .prepare(|| panic!("cancelled entry executed"))
            .unwrap();
        let handle = prepared.thread_handle();
        if staged {
            let staged = prepared.stage().unwrap();
            let _irq = PreemptIrqSaveGuard::new();
            drop(staged);
        } else {
            let _irq = PreemptIrqSaveGuard::new();
            drop(prepared);
        }
        assert_eq!(
            handle.state(),
            ax_std::os::arceos::task::thread::ThreadState::New,
            "atomic cancellation must defer the registry transaction"
        );
        cancelled.push(handle);
    }
    use ax_std::os::arceos::{
        api::time::ax_monotonic_time,
        task::time::{
            MonotonicDeadline,
            hard_timer::{
                HardKernelTimerAction, HardKernelTimerCallback,
                register_hard_restartable_kernel_timer,
            },
        },
    };
    let prepared = ax_std::os::arceos::thread::builder("cancel-hard-irq".into())
        .prepare(|| panic!("IRQ-cancelled entry executed"))
        .unwrap();
    cancelled.push(prepared.thread_handle());
    let staged = ax_std::os::arceos::thread::builder("cancel-staged-hard-irq".into())
        .prepare(|| panic!("IRQ-cancelled staged entry executed"))
        .unwrap();
    cancelled.push(staged.thread_handle());
    let sole = Arc::new(ExtensionProbe::default());
    let data = Box::into_raw(Box::new(Arc::clone(&sole))) as usize;
    // SAFETY: the extension owns the boxed probe; no management handle is kept.
    let extension = unsafe { ThreadExtension::new(data, &PROBE_OPS) };
    let last_token = ax_std::os::arceos::thread::builder("cancel-last-token-irq".into())
        .extension(extension)
        .prepare(|| panic!("sole cancelled entry executed"))
        .unwrap()
        .stage()
        .unwrap();
    let mut tokens = Some((prepared, staged.stage().unwrap(), last_token));
    let done = Arc::new(AtomicBool::new(false));
    let irq_done = Arc::clone(&done);
    // SAFETY: dropping creation tokens only publishes preallocated nodes. The
    // timer service owns callback destruction; this callback never blocks.
    let callback = unsafe {
        HardKernelTimerCallback::new(Box::new(move |_| {
            drop(tokens.take());
            irq_done.store(true, Ordering::Release);
            HardKernelTimerAction::Complete
        }))
    };
    register_hard_restartable_kernel_timer(
        MonotonicDeadline::from_duration(ax_monotonic_time() + Duration::from_millis(5)),
        callback,
    )
    .unwrap();
    wait_for(|| done.load(Ordering::Acquire));
    for handle in &cancelled {
        assert_eq!(
            handle.state(),
            ax_std::os::arceos::task::thread::ThreadState::New
        );
    }
    gate.pause_exit.store(false, Ordering::Release);
    blocker.join().unwrap();
    for handle in cancelled {
        assert_eq!(handle.join().unwrap(), 0);
    }
    wait_for(|| sole.dropped.load(Ordering::Acquire) == 1);
    assert_eq!(sole.exited.load(Ordering::Acquire), 1);
    assert_eq!(sole.switched_in.load(Ordering::Acquire), 0);
    println!("task_wait_queue: atomic cancellation deferred OK");
}

fn managed_exit_requires_token() {
    let prepared = ax_std::os::arceos::thread::builder("managed-cancel-owner".into())
        .prepare(|| panic!("cancelled entry executed"))
        .unwrap();
    let handle = prepared.thread_handle();
    assert_eq!(
        system().mark_exited(handle.id()),
        Err(TaskError::NotReady),
        "raw exit must not consume managed cancellation ownership"
    );
    drop(prepared);
    assert_eq!(handle.join().unwrap(), 0);
    println!("task_wait_queue: managed cancellation ownership OK");
}

fn reclaim_rejects_atomic_context() {
    let handle = ax_std::os::arceos::thread::builder("atomic-reclaim".into())
        .spawn(|| {})
        .unwrap();
    handle.wait().unwrap();
    wait_for(|| handle.execution_reclaimed());
    use ax_std::os::arceos::task::sync::{RawSpinLock, SpinLock};
    let lock = SpinLock::new(());
    let check = || {
        assert_eq!(
            system().reap_thread(handle.id()),
            Err(TaskError::UnsafeContext)
        );
        assert_eq!(
            system().dispatch_exit_callbacks(1),
            Err(TaskError::UnsafeContext)
        );
        assert_eq!(
            system().reap_unreferenced_exited(1),
            Err(TaskError::UnsafeContext)
        );
        assert!(matches!(
            system().service_deferred_task_work(1),
            Err(TaskError::UnsafeContext)
        ));
    };
    {
        let _irq = PreemptIrqSaveGuard::new();
        check();
    }
    {
        let _rt = lock.lock();
        check();
    }
    {
        let raw = RawSpinLock::new(());
        let _raw = raw.lock();
        check();
    }
    let error = {
        let _irq = PreemptIrqSaveGuard::new();
        system().reap_thread_handle(handle).unwrap_err()
    };
    assert_eq!(error.task_error(), TaskError::UnsafeContext);
    error.into_retry_handle().join().unwrap();
    println!("task_wait_queue: atomic direct reclamation rejected OK");
}

fn resource_failure_rollback() {
    use ax_runtime::thread::creation_probe::{
        CreationEvent as E, CreationStage as S, ThreadCreationProbe,
    };
    let cases: &[(S, &[E])] = &[
        (S::Stack, &[E::Stack]),
        (S::Tls, &[E::Stack, E::Tls, E::DropStack]),
        (
            S::Context,
            &[E::Stack, E::Tls, E::Context, E::DropTls, E::DropStack],
        ),
        (
            S::Bind,
            &[
                E::Stack,
                E::Tls,
                E::Context,
                E::Bind,
                E::DropContext,
                E::DropTls,
                E::DropStack,
            ],
        ),
    ];
    for &(stage, expected) in cases {
        let counters = Arc::new(ExtensionProbe::default());
        let data = Box::into_raw(Box::new(Arc::clone(&counters))) as usize;
        // SAFETY: the extension owns this boxed probe until creation rollback.
        let extension = unsafe { ThreadExtension::new(data, &PROBE_OPS) };
        let probe = ThreadCreationProbe::fail_at(stage).unwrap();
        let result = ax_std::os::arceos::thread::builder("allocation-rollback".into())
            .extension(extension)
            .prepare(|| panic!("failed resource transaction became runnable"));
        assert!(matches!(result, Err(TaskError::RuntimeFailure(code))
            if code == ax_std::os::arceos::task::runtime::RuntimeStatus::NoMemory as u32));
        assert_eq!(probe.events(), expected, "rollback order at {stage:?}");
        assert_eq!(counters.dropped.load(Ordering::Acquire), 1);
        assert_eq!(counters.switched_in.load(Ordering::Acquire), 0);
        assert_eq!(counters.exited.load(Ordering::Acquire), 0);
    }
    println!("task_wait_queue: real resource-stage rollback OK");
}

fn allocation_failure_rollback() {
    use ax_std::os::arceos::task::{
        sched::{DeadlineFlags, DeadlinePolicy},
        thread::ThreadAllocationProbe,
    };
    struct EntryDrop(Arc<AtomicUsize>);
    impl Drop for EntryDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }
    let deadline = SchedulePolicy::deadline(
        DeadlinePolicy::new(9_000_000, 10_000_000, 10_000_000, DeadlineFlags::NONE).unwrap(),
    );
    for policy in [SchedulePolicy::default(), deadline] {
        let probe = ThreadAllocationProbe::fail_at(usize::MAX).unwrap();
        let prepared = ax_runtime::thread::builder("allocation-context".into())
            .policy(policy)
            .prepare(|| {})
            .unwrap();
        let attempts = probe.attempts();
        assert!(attempts > 0);
        drop(probe);
        let handle = prepared.thread_handle();
        drop(prepared);
        handle.wait().unwrap();
        wait_for(|| handle.execution_reclaimed());
        handle.join().unwrap();
        for fail_at in 0..attempts {
            let counters = Arc::new(ExtensionProbe::default());
            let data = Box::into_raw(Box::new(Arc::clone(&counters))) as usize;
            // SAFETY: this extension exclusively owns its boxed Arc until rollback.
            let extension = unsafe { ThreadExtension::new(data, &PROBE_OPS) };
            let entry_drops = Arc::new(AtomicUsize::new(0));
            let entry_drop = EntryDrop(Arc::clone(&entry_drops));
            let probe = ThreadAllocationProbe::fail_at(fail_at).unwrap();
            let result = ax_runtime::thread::builder("allocation-failure".into())
                .policy(policy)
                .extension(extension)
                .prepare(move || {
                    drop(entry_drop);
                    panic!("failed allocation became runnable");
                });
            assert!(
                matches!(result, Err(TaskError::RuntimeFailure(code))
                if code == ax_std::os::arceos::task::runtime::RuntimeStatus::NoMemory as u32),
                "allocation {fail_at} must propagate ENOMEM"
            );
            assert_eq!(probe.attempts(), fail_at + 1);
            drop(probe);
            assert_eq!(entry_drops.load(Ordering::Acquire), 1);
            assert_eq!(counters.dropped.load(Ordering::Acquire), 1);
            assert_eq!(counters.switched_in.load(Ordering::Acquire), 0);
            // Repeated high-utilization Deadline creation detects leaked
            // admission charges; prepare must remain possible after each OOM.
            let recovered = ax_runtime::thread::builder("allocation-recovered".into())
                .policy(policy)
                .prepare(|| {})
                .unwrap();
            let handle = recovered.thread_handle();
            drop(recovered);
            handle.wait().unwrap();
            wait_for(|| handle.execution_reclaimed());
            handle.join().unwrap();
        }
        println!("task_wait_queue: {attempts} heap allocation rollback boundaries OK");
    }
}

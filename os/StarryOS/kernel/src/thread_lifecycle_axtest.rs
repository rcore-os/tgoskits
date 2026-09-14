//! Real user-capable resource preparation without entering an untrusted ABI.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_runtime::thread::{
    TaskAddressSpace, UserContextOptions, builder,
    creation_probe::{CreationEvent as E, CreationStage as S, ThreadCreationProbe},
    prepare_user_thread,
};
use ax_std::os::arceos::task as ax_task;
use ax_task::{runtime::RuntimeStatus, thread::TaskError};

struct MmOwner(Arc<AtomicUsize>);
impl Drop for MmOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

#[axtest::axtest]
fn user_context_resource_failure_rollback() {
    // The bootstrap owns the kernel table for the whole test. Failed contexts
    // never install this root or enter user mode; no fake runtime is involved.
    let root = ax_hal::cpu::mmu::read_kernel_page_table();
    let cases: &[(S, &[E])] = &[
        (S::Mm, &[E::Mm]),
        (S::Stack, &[E::Mm, E::Stack]),
        (S::Tls, &[E::Mm, E::Stack, E::Tls, E::DropStack]),
        (
            S::Context,
            &[E::Mm, E::Stack, E::Tls, E::Context, E::DropStack],
        ),
        (
            S::Fp,
            &[
                E::Mm,
                E::Stack,
                E::Tls,
                E::Context,
                E::Fp,
                E::DropContext,
                E::DropStack,
            ],
        ),
        (
            S::Bind,
            &[
                E::Mm,
                E::Stack,
                E::Tls,
                E::Context,
                E::Fp,
                E::Bind,
                E::DropContext,
                E::DropStack,
            ],
        ),
    ];
    for &(stage, expected) in cases {
        let drops = Arc::new(AtomicUsize::new(0));
        let probe = ThreadCreationProbe::fail_at(stage).unwrap();
        let result = TaskAddressSpace::new(root, MmOwner(Arc::clone(&drops))).and_then(|mm| {
            // SAFETY: every configured fault precedes publication. Even if
            // construction unexpectedly succeeds, the closure cannot enter
            // user mode and dropping its token cancels first activation.
            unsafe {
                prepare_user_thread(
                    builder("user-rollback".into()),
                    || panic!("failed user context became runnable"),
                    UserContextOptions::new(mm),
                )
            }
        });
        assert!(
            matches!(result, Err(TaskError::RuntimeFailure(code))
            if code == RuntimeStatus::NoMemory as u32),
            "failure at {stage:?}"
        );
        assert_eq!(probe.events(), expected, "user rollback order at {stage:?}");
        assert_eq!(
            drops.load(Ordering::Acquire),
            1,
            "MM owner leaked at {stage:?}"
        );
    }
}

#[axtest::axtest]
fn user_context_heap_failure_rollback() {
    use ax_task::thread::ThreadAllocationProbe;
    let root = ax_hal::cpu::mmu::read_kernel_page_table();
    let prepare = |drops: Arc<AtomicUsize>| {
        TaskAddressSpace::new(root, MmOwner(drops)).and_then(|mm| {
            // SAFETY: the kernel root is permanent, and the entry never enters
            // user mode. Every failed allocation remains before publication.
            unsafe {
                prepare_user_thread(
                    builder("user-heap-rollback".into()),
                    || {},
                    UserContextOptions::new(mm),
                )
            }
        })
    };
    let probe = ThreadAllocationProbe::fail_at(usize::MAX).unwrap();
    let prepared = prepare(Arc::new(AtomicUsize::new(0))).unwrap();
    let attempts = probe.attempts();
    assert!(attempts > 0);
    drop(probe);
    let handle = prepared.thread_handle();
    drop(prepared);
    handle.join().unwrap();
    for fail_at in 0..attempts {
        let drops = Arc::new(AtomicUsize::new(0));
        let probe = ThreadAllocationProbe::fail_at(fail_at).unwrap();
        let result = prepare(Arc::clone(&drops));
        assert!(
            matches!(result, Err(TaskError::RuntimeFailure(code))
            if code == RuntimeStatus::NoMemory as u32),
            "user heap allocation {fail_at}"
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => unreachable!("allocation fault must fail preparation"),
        };
        let api_error: ax_io::IoError = ax_std::os::arceos::api::ApiError::Task(error).into();
        assert_eq!(
            (api_error, crate::StarryError::from(error).linux_errno()),
            (ax_io::IoError::NoMemory, syscalls::Errno::ENOMEM),
            "allocation failure must preserve ENOMEM across public error boundaries"
        );
        assert_eq!(probe.attempts(), fail_at + 1);
        drop(probe);
        assert_eq!(
            drops.load(Ordering::Acquire),
            1,
            "MM owner leaked at allocation {fail_at}"
        );
    }
}

#[axtest::axtest]
fn user_kernel_mm_switch_matrix() {
    use ax_task::{
        sched::{CpuSet, RtPriority, SchedulePolicy},
        sync::Semaphore,
        thread::current,
    };
    let cpu = ax_hal::percpu::this_cpu_id();
    let mut affinity = CpuSet::empty(ax_hal::cpu_num());
    assert!(affinity.insert(ax_task::sched::CpuId::new(cpu as u32)));
    let original_affinity = current::current_thread_handle()
        .unwrap()
        .affinity()
        .unwrap();
    current::set_current_thread_affinity(affinity.clone()).unwrap();
    let root = ax_hal::cpu::mmu::read_kernel_page_table();
    let before = ax_runtime::thread::creation_probe::mm_switch_counts();
    let gate = Arc::new(Semaphore::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let mut handles = alloc::vec::Vec::new();
    let mut mm_drops = alloc::vec::Vec::new();
    // Equal-priority FIFO yields form U -> U -> K -> K -> U. Pinning
    // prevents the four contexts from running concurrently on different CPUs.
    for user in [true, true, false, false] {
        let gate = Arc::clone(&gate);
        let entered = Arc::clone(&entered);
        let entry = move || {
            assert!(
                ax_hal::cpu::interrupt::irqs_enabled(),
                "first switch tail retained IRQ ownership"
            );
            current::validate_blocking_context().unwrap();
            gate.down().unwrap();
            for _ in 0..3 {
                if user {
                    assert_eq!(ax_hal::cpu::mmu::read_user_page_table(), root);
                }
                entered.fetch_add(1, Ordering::Release);
                current::yield_current_cpu().unwrap();
                assert!(
                    ax_hal::cpu::interrupt::irqs_enabled(),
                    "resumed switch retained IRQ ownership"
                );
                current::validate_blocking_context().unwrap();
            }
        };
        let builder = builder("mm-switch-matrix".into())
            .affinity(affinity.clone())
            .policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
        let prepared = if user {
            let drops = Arc::new(AtomicUsize::new(0));
            let mm = TaskAddressSpace::new(root, MmOwner(Arc::clone(&drops))).unwrap();
            mm_drops.push(drops);
            // SAFETY: the permanently owned kernel root remains valid through
            // every active-MM lease. These test contexts never enter user mode.
            unsafe { prepare_user_thread(builder, entry, UserContextOptions::new(mm)) }.unwrap()
        } else {
            builder.prepare(entry).unwrap()
        };
        handles.push(prepared.publish().unwrap());
    }
    {
        let _guard = ax_std::os::arceos::guard::PreemptIrqSaveGuard::new();
        for _ in 0..4 {
            gate.up().unwrap();
        }
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(entered.load(Ordering::Acquire), 12);
    wait_for_mm_drop(&mm_drops[0]);
    assert_eq!(
        mm_drops[1].load(Ordering::Acquire),
        0,
        "kernel lazy-MM must retain the last user MM after its thread exits"
    );
    let replacement = TaskAddressSpace::new(root, ()).unwrap();
    // SAFETY: the kernel table remains permanently owned, and this closure
    // never enters user mode. A different MM identity ends the previous lease.
    let replacement = unsafe {
        prepare_user_thread(
            builder("replace-lazy-mm".into()).affinity(affinity),
            || {},
            UserContextOptions::new(replacement),
        )
    }
    .unwrap()
    .publish()
    .unwrap();
    replacement.join().unwrap();
    wait_for_mm_drop(&mm_drops[1]);
    current::set_current_thread_affinity(original_affinity).unwrap();
    let after = ax_runtime::thread::creation_probe::mm_switch_counts();
    for (index, label) in ["KK", "KU", "UK", "UU"].iter().enumerate() {
        assert!(
            after[index] > before[index],
            "missing MM transition {label}"
        );
    }
}

fn wait_for_mm_drop(drops: &AtomicUsize) {
    let start = ax_task::runtime::task_runtime::monotonic_now().as_nanos();
    while drops.load(Ordering::Acquire) == 0 {
        assert!(
            ax_task::runtime::task_runtime::monotonic_now().as_nanos() - start < 2_000_000_000,
            "inactive MM did not reach task-context reclamation"
        );
        ax_task::thread::current::yield_current_cpu().unwrap();
    }
    assert_eq!(
        drops.load(Ordering::Acquire),
        1,
        "MM owner released more than once"
    );
}

#[axtest::axtest]
fn idle_cpu_cycle_rejection_retains_active_mm() {
    use ax_runtime::thread::creation_probe::{
        request_idle_cpu_round_trip, take_idle_cpu_round_trip_result,
    };
    use ax_task::{
        sched::{CpuId, CpuSet},
        thread::current,
    };
    assert!(ax_hal::cpu_num() > 1, "idle MM cycle requires SMP");
    let original = current::current_thread_handle()
        .unwrap()
        .affinity()
        .unwrap();
    let mut coordinator = CpuSet::empty(ax_hal::cpu_num());
    coordinator.insert(CpuId::new(0));
    current::set_current_thread_affinity(coordinator).unwrap();
    let mut target = CpuSet::empty(ax_hal::cpu_num());
    target.insert(CpuId::new(1));
    let drops = Arc::new(AtomicUsize::new(0));
    let mm = TaskAddressSpace::new(
        ax_hal::cpu::mmu::read_kernel_page_table(),
        MmOwner(Arc::clone(&drops)),
    )
    .unwrap();
    // SAFETY: the permanent kernel root remains live, and the closure never
    // enters user mode. The runtime owns the real per-CPU active-MM lease.
    let handle = unsafe {
        prepare_user_thread(
            builder("offline-active-mm".into()).affinity(target),
            || {},
            UserContextOptions::new(mm),
        )
    }
    .unwrap()
    .publish()
    .unwrap();
    handle.wait().unwrap();
    let start = ax_task::runtime::task_runtime::monotonic_now().as_nanos();
    while !handle.execution_reclaimed() {
        assert!(ax_task::runtime::task_runtime::monotonic_now().as_nanos() - start < 2_000_000_000);
        current::yield_current_cpu().unwrap();
    }
    handle.join().unwrap();
    assert_eq!(
        drops.load(Ordering::Acquire),
        0,
        "idle must retain active MM before offline"
    );
    request_idle_cpu_round_trip(1).unwrap();
    loop {
        if let Some(result) = take_idle_cpu_round_trip_result() {
            assert!(
                !result,
                "Starry pinned stopper workers must prevent CPU offline"
            );
            assert_eq!(
                drops.load(Ordering::Acquire),
                0,
                "rejected offline released active MM"
            );
            break;
        }
        assert!(ax_task::runtime::task_runtime::monotonic_now().as_nanos() - start < 2_000_000_000);
        current::yield_current_cpu().unwrap();
    }
    let mut target = CpuSet::empty(ax_hal::cpu_num());
    target.insert(CpuId::new(1));
    let mm = TaskAddressSpace::new(ax_hal::cpu::mmu::read_kernel_page_table(), ()).unwrap();
    // SAFETY: the permanent kernel root is live and this closure stays in kernel mode.
    unsafe {
        prepare_user_thread(
            builder("after-offline-rejection".into()).affinity(target),
            || {},
            UserContextOptions::new(mm),
        )
    }
    .unwrap()
    .publish()
    .unwrap()
    .join()
    .unwrap();
    wait_for_mm_drop(&drops);
    current::set_current_thread_affinity(original).unwrap();
}

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::PiLockIdentity;

fn publish_test_scheduler_work(
    remote: &CpuRemote,
    node: Pin<&'static crate::inbox::InboxNode>,
    slot: u32,
) {
    let message = InboxMessage::remote_wake(ThreadId::from_parts(slot, 1), remote.owner());
    let result = remote.publish_remote_wake(node, message);
    assert_eq!(result, PublishResult::Published);
}

fn test_inbox_node(
    node: &Pin<Box<crate::inbox::InboxNode>>,
) -> Pin<&'static crate::inbox::InboxNode> {
    let node = node.as_ref().get_ref() as *const crate::inbox::InboxNode;
    unsafe {
        // Callers keep the pinned fixture alive until its inbox has drained
        // or the complete owning task system has been dropped.
        Pin::new_unchecked(&*node)
    }
}

#[test]
fn cpu_owners_schedule_while_cold_domains_are_locked() {
    use std::{
        sync::{Barrier, mpsc},
        thread,
        time::Duration,
    };

    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let ready = Arc::new(Barrier::new(3));
    let start = Arc::new(Barrier::new(3));
    let (completed, progress) = mpsc::channel();
    let mut workers = Vec::new();

    for cpu_index in 0..2 {
        let system = Arc::clone(&system);
        let ready = Arc::clone(&ready);
        let start = Arc::clone(&start);
        let completed = completed.clone();
        workers.push(thread::spawn(move || {
            let mut cpu = system.create_cpu_local(CpuId::new(cpu_index)).unwrap();
            system
                .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
            ready.wait();
            start.wait();
            let result = system
                .drain_policy_updates(cpu.as_mut(), 1)
                .and_then(|_| system.schedule(cpu.as_mut(), 1).map(|_| ()));
            completed.send((cpu_index, result)).unwrap();
        }));
    }
    drop(completed);

    ready.wait();
    let registry = system.state.lock();
    let root_domain = system.root_domain.lock();
    start.wait();
    let mut observed = Vec::new();
    let mut timed_out = false;
    for _ in 0..2 {
        match progress.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => observed.push(result),
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }
    drop(root_domain);
    drop(registry);
    for worker in workers {
        worker.join().unwrap();
    }

    assert!(!timed_out, "owner scheduling waited for a cold lock domain");
    assert_eq!(observed.len(), 2);
    for (_, result) in observed {
        result.unwrap();
    }
}

#[test]
fn current_extension_lookup_progresses_while_registry_is_locked() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let extension = unsafe { ThreadExtension::new(0x55, &DEADLINE_TEST_EXTENSION_OPS) };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

    let registry = system.state.lock();
    let lease = crate::current_thread_extension().unwrap().unwrap();
    assert_eq!(lease.data(), 0x55);
    assert!(core::ptr::eq(lease.ops(), &DEADLINE_TEST_EXTENSION_OPS));
    drop(registry);
}

#[test]
fn remote_publication_cannot_be_preempted_before_doorbell() {
    let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::RemoteWake));
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let remote = &system.cpu_remotes[1];
    remote.mark_online();
    crate::test_runtime::reset_irq_state();
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success, 0);

    publish_test_scheduler_work(remote, test_inbox_node(&node), 1);

    assert_eq!(crate::test_runtime::scheduler_ipi_send_count(), 1);
    assert_eq!(
        crate::test_runtime::scheduler_ipi_irq_guards(),
        1,
        "a producer must remain non-preemptible until its published work has a doorbell"
    );
}

#[test]
fn permanent_scheduler_ipi_failure_fails_at_the_publication_boundary() {
    let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::RemoteWake));
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let remote = &system.cpu_remotes[1];
    remote.mark_online();
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::InvalidArgument, 0);

    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        publish_test_scheduler_work(remote, test_inbox_node(&node), 2);
    }));
    assert!(
        failure.is_err(),
        "a scheduler transport that cannot guarantee delivery must fail-stop"
    );
}

#[test]
fn registered_remote_endpoint_is_separate_from_owner_mutable_state() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let owner_address = (cpu.as_ref().get_ref() as *const CpuLocal).addr();
    let endpoint_address = Arc::as_ptr(cpu.as_ref().get_ref().remote()).addr();
    assert_ne!(owner_address, endpoint_address);

    system.bring_cpu_online(cpu.as_mut()).unwrap();
    assert_eq!(
        (system.state.lock().cpu_remote(CpuId::new(0)).unwrap() as *const CpuRemote).addr(),
        endpoint_address
    );

    cpu.as_mut().clear_current();
    assert_eq!(
        (system.state.lock().cpu_remote(CpuId::new(0)).unwrap() as *const CpuRemote).addr(),
        endpoint_address,
        "owner reborrowing must not alias or invalidate the remote endpoint"
    );
}

#[test]
fn configuration_rejects_batch_larger_than_irq_contract() {
    assert!(matches!(
        TaskSystem::new(TaskSystemConfig::new(1).with_batch_limit(crate::DEFAULT_BATCH_LIMIT + 1)),
        Err(TaskError::InvalidConfiguration)
    ));
}

#[test]
fn exhausted_thread_slot_generation_never_wraps_to_the_first_identity() {
    assert_ne!(
        next_generation(u32::MAX),
        1,
        "slot reuse must not make an old generation-1 ThreadId valid again"
    );
    let mut slot = ThreadSlot {
        generation: u32::MAX,
        record: None,
    };
    assert!(
        !advance_thread_slot_generation(&mut slot),
        "an exhausted empty slot must be retired rather than reused"
    );
    assert_eq!(slot.generation, u32::MAX);
}

use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, ThreadExtensionOps};

static DEADLINE_OVERRUN_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

struct InstalledTaskHandles;

impl InstalledTaskHandles {
    fn new(system: Pin<&TaskSystem>, cpu: Pin<&mut CpuLocal>) -> Self {
        crate::test_runtime::install_task_handles(
            (system.get_ref() as *const TaskSystem).expose_provenance(),
            // SAFETY: the test fixture keeps the owner object pinned and
            // serializes every scheduler access until the handle is cleared.
            (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance(),
        );
        Self
    }
}

impl Drop for InstalledTaskHandles {
    fn drop(&mut self) {
        crate::test_runtime::clear_task_handles();
    }
}

#[test]
fn unsuccessful_fair_balance_backs_off_from_scheduler_completion_time() {
    const ENTRY_NOW_NS: u64 = BALANCE_INTERVAL_NS;
    const COMPLETION_NOW_NS: u64 = 10_000;
    const BALANCE_INTERVAL_NS: u64 = 1_000;

    let system = Box::pin(
        TaskSystem::new(TaskSystemConfig::new(1).with_balance_interval_ns(BALANCE_INTERVAL_NS))
            .unwrap(),
    );
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    crate::test_runtime::set_monotonic_ns(COMPLETION_NOW_NS);

    assert!(cpu.fair_balance_due(ENTRY_NOW_NS));
    assert_eq!(system.balance_fair(cpu.as_mut(), ENTRY_NOW_NS), Ok(None));
    assert!(
        !cpu.fair_balance_due(COMPLETION_NOW_NS),
        "completed balance work must not leave its next period already overdue"
    );
    assert!(
        !cpu.fair_balance_due(COMPLETION_NOW_NS.saturating_add(BALANCE_INTERVAL_NS)),
        "a balance pass that moved no work must back off instead of retrying at the minimum \
         interval"
    );
    assert!(
        cpu.fair_balance_due(
            COMPLETION_NOW_NS.saturating_add(BALANCE_INTERVAL_NS.saturating_mul(2))
        ),
        "the first unsuccessful balance pass must double its retry interval"
    );
}

#[test]
fn affinity_constrained_fair_balance_keeps_backing_off() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(2).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let pinned = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu0_only))
        .unwrap();
    system.make_ready(pinned.id()).unwrap();
    system.enqueue(cpu0.as_mut(), pinned.id(), 0).unwrap();

    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert_eq!(system.balance_fair(cpu0.as_mut(), INTERVAL_NS), Ok(None));
    let second_balance_ns = INTERVAL_NS.saturating_mul(3);
    assert!(cpu0.fair_balance_due(second_balance_ns));

    crate::test_runtime::set_monotonic_ns(second_balance_ns);
    assert_eq!(
        system.balance_fair(cpu0.as_mut(), second_balance_ns),
        Ok(None)
    );
    assert!(
        !cpu0.fair_balance_due(second_balance_ns.saturating_add(INTERVAL_NS * 2)),
        "an affinity-constrained domain must continue exponential backoff"
    );
    assert!(cpu0.fair_balance_due(second_balance_ns.saturating_add(INTERVAL_NS * 4)));
}

#[test]
fn successful_fair_balance_resets_the_minimum_interval() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(2).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }

    let movable = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(movable.id()).unwrap();
    system.enqueue(cpu0.as_mut(), movable.id(), 0).unwrap();

    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert_eq!(
        system.balance_fair(cpu0.as_mut(), INTERVAL_NS),
        Ok(Some(movable.id()))
    );
    assert!(
        cpu0.fair_balance_due(INTERVAL_NS.saturating_mul(2)),
        "successful migration must restore the configured minimum interval"
    );
}

#[test]
fn local_timer_is_programmed_from_scheduler_completion_time() {
    const ENTRY_NOW_NS: u64 = 100;
    const COMPLETION_NOW_NS: u64 = 100_000_000;

    let system = Box::pin(
        TaskSystem::new(
            TaskSystemConfig::new(1).with_balance_interval_ns(COMPLETION_NOW_NS.saturating_mul(2)),
        )
        .unwrap(),
    );
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    crate::test_runtime::set_monotonic_ns(ENTRY_NOW_NS);
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .bring_cpu_online_at(cpu.as_mut(), ENTRY_NOW_NS)
        .unwrap();
    let contender = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(contender.id()).unwrap();
    system
        .enqueue(cpu.as_mut(), contender.id(), ENTRY_NOW_NS)
        .unwrap();

    crate::test_runtime::set_monotonic_ns(COMPLETION_NOW_NS);
    TaskSystem::program_local_timer(cpu.as_mut(), ENTRY_NOW_NS).unwrap();
    let update = crate::test_runtime::take_task_deadline_update()
        .expect("local timer programming must publish one complete update");
    let deadline_ns = update
        .deadline()
        .expect("the running fair dispatch must own a scheduler deadline")
        .as_nanos();
    assert!(
        deadline_ns > COMPLETION_NOW_NS,
        "published deadline {deadline_ns} must be relative to completion time \
         {COMPLETION_NOW_NS}, not entry time {ENTRY_NOW_NS}"
    );
}

#[test]
fn context_is_bound_to_the_allocated_thread_before_new_is_published() {
    crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let context = unsafe {
        // SAFETY: the unit runtime models this non-zero scalar as a live
        // context until the task-system fixture is dropped.
        ExecutionContextHandle::from_raw(0x1000)
    };
    let resources = unsafe {
        // SAFETY: the fake runtime accepts the unique context handle above;
        // the remaining resource handles are intentionally absent.
        ThreadResources::new(
            context,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceHandle::NONE,
        )
    };

    let thread = system
        .create_thread(unsafe {
            // SAFETY: this specification is the sole owner of `resources`.
            ThreadSpec::new(Default::default()).with_resources(resources)
        })
        .unwrap();

    assert_eq!(system.thread_state(thread.id()), Ok(ThreadState::New));
    assert_eq!(
        crate::test_runtime::last_context_binding(),
        Some(ContextThreadBinding {
            context,
            identity: ThreadIdentityV1::new(thread.id().slot(), thread.id().generation()),
        })
    );
}

#[test]
fn failed_context_binding_retires_the_allocated_generation() {
    crate::test_runtime::configure_context_binding(RuntimeStatus::InvalidHandle);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let context = unsafe {
        // SAFETY: the unit runtime validates this modeled handle through its
        // configured failing context-binding result.
        ExecutionContextHandle::from_raw(0x2000)
    };
    let resources = unsafe {
        // SAFETY: ownership is transferred once into the failed create path.
        ThreadResources::new(
            context,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceHandle::NONE,
        )
    };

    let error = system
        .create_thread(unsafe {
            // SAFETY: this specification is the sole resource owner.
            ThreadSpec::new(Default::default()).with_resources(resources)
        })
        .unwrap_err();
    assert_eq!(
        error,
        TaskError::RuntimeFailure(RuntimeStatus::InvalidHandle as u32)
    );
    let failed = crate::test_runtime::last_context_binding().unwrap();

    crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
    let replacement = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    assert_eq!(replacement.id().slot(), failed.identity.slot);
    assert_ne!(replacement.id().generation(), failed.identity.generation);
}

#[test]
fn rejected_thread_releases_runtime_resources_before_extension() {
    use crate::test_runtime::ResourceReleaseEvent;

    static RELEASE_ORDER_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: no_extension_hook,
        on_switch_out: no_extension_switch_out,
        on_exit: no_extension_hook,
        on_deadline_overrun: no_extension_hook,
        drop: record_release_order_extension_drop,
    };

    crate::test_runtime::configure_resource_release(RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let resources = unsafe {
        // SAFETY: the unit runtime accepts these unique modeled handles and
        // records their synchronous release order.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(0x3000),
            crate::runtime::StackHandle::from_raw(0x4000),
            crate::runtime::TlsHandle::from_raw(0x5000),
            crate::runtime::AddressSpaceHandle::NONE,
        )
    };
    let extension = unsafe {
        // SAFETY: the callback owns no external data and only records its drop.
        ThreadExtension::new(0, &RELEASE_ORDER_EXTENSION_OPS)
    };
    let result = system.create_thread(unsafe {
        // SAFETY: this specification uniquely owns every modeled resource.
        ThreadSpec::new(SchedulePolicy::default())
            .with_affinity(CpuSet::empty(2))
            .with_extension(extension)
            .with_resources(resources)
    });

    assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [
            ResourceReleaseEvent::DestroyContext,
            ResourceReleaseEvent::DeallocateTls,
            ResourceReleaseEvent::DeallocateStack,
            ResourceReleaseEvent::DropExtension,
        ]
    );
    crate::test_runtime::configure_resource_release(RuntimeStatus::Unsupported);
}

#[test]
fn rejected_thread_retains_extension_until_resource_release_retry() {
    use crate::test_runtime::ResourceReleaseEvent;

    static RETRY_RELEASE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: no_extension_hook,
        on_switch_out: no_extension_switch_out,
        on_exit: no_extension_hook,
        on_deadline_overrun: no_extension_hook,
        drop: record_release_order_extension_drop,
    };

    crate::test_runtime::configure_resource_release(RuntimeStatus::Busy);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let resources = unsafe {
        // SAFETY: the unit runtime accepts these unique modeled handles and
        // retains them until its configured release operation succeeds.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(0x6000),
            crate::runtime::StackHandle::from_raw(0x7000),
            crate::runtime::TlsHandle::from_raw(0x8000),
            crate::runtime::AddressSpaceHandle::NONE,
        )
    };
    let extension = unsafe {
        // SAFETY: the callback owns no external data and only records its drop.
        ThreadExtension::new(0, &RETRY_RELEASE_EXTENSION_OPS)
    };
    let result = system.create_thread(unsafe {
        // SAFETY: this specification uniquely owns every modeled resource.
        ThreadSpec::new(SchedulePolicy::default())
            .with_affinity(CpuSet::empty(2))
            .with_extension(extension)
            .with_resources(resources)
    });

    assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [ResourceReleaseEvent::DestroyContext],
        "the extension must remain owned while context destruction is retryable"
    );
    assert!(system.deferred_task_work_pending());

    crate::test_runtime::configure_resource_release(RuntimeStatus::Success);
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [
            ResourceReleaseEvent::DestroyContext,
            ResourceReleaseEvent::DeallocateTls,
            ResourceReleaseEvent::DeallocateStack,
            ResourceReleaseEvent::DropExtension,
        ]
    );
    crate::test_runtime::configure_resource_release(RuntimeStatus::Unsupported);
}

unsafe extern "Rust" fn record_release_order_extension_drop(_data: usize) {
    crate::test_runtime::record_resource_release_event(
        crate::test_runtime::ResourceReleaseEvent::DropExtension,
    );
}

#[test]
fn generation_rejects_a_stale_registry_identity() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let first = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let first_id = first.id();
    system.mark_exited(first_id).unwrap();
    drop(first);
    system.reap_thread(first_id).unwrap();
    let second = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    assert_eq!(first_id.slot(), second.id().slot());
    assert_eq!(system.thread_state(first_id), Err(TaskError::StaleThreadId));
}

#[test]
fn detached_reaper_waits_for_the_last_external_handle() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let id = thread.id();
    system.mark_exited(id).unwrap();

    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);
    drop(thread);
    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 1);
    assert_eq!(system.thread_state(id), Err(TaskError::StaleThreadId));
}

#[test]
fn last_non_idle_exit_publishes_work_only_after_switch_tail() {
    EXIT_CALLBACK_INVOCATIONS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let extension = unsafe {
        // SAFETY: the callback table ignores its scalar payload and records
        // only ordinary-context exit invocation.
        ThreadExtension::new(0, &EXIT_CALLBACK_TEST_OPS)
    };
    let exiting = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    let exiting_id = exiting.id();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    drop(exiting);
    drop(idle);

    let decision = system.exit_current(cpu.as_mut()).unwrap();
    assert_eq!(decision.next(), cpu.idle().unwrap());
    assert!(
        !system.deferred_task_work_pending(),
        "the outgoing stack remains on_cpu until switch tail"
    );
    assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 0);

    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert!(system.deferred_task_work_pending());
    let batch = system.service_deferred_task_work(64).unwrap();
    assert!(batch.made_progress());
    assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 1);
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId),
        "the dedicated service pass must also reap the detached record"
    );
}

#[test]
fn deadline_task_work_rotates_across_registry_slots() {
    ROTATING_DEADLINE_CALLBACKS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let low_slot = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let high_slot = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::default()).with_extension(unsafe {
                // SAFETY: the static callbacks accept the scalar test payload.
                ThreadExtension::new(0, &ROTATING_DEADLINE_TEST_OPS)
            }),
        )
        .unwrap();
    let high_slot_id = high_slot.id();
    {
        let state = system.state.lock();
        state
            .thread_record(low_slot.id())
            .unwrap()
            .sched
            .lock()
            .deadline_overrun_events = 1;
        state
            .thread_record(high_slot_id)
            .unwrap()
            .sched
            .lock()
            .deadline_overrun_events = 1;
    }
    system.mark_exited(high_slot_id).unwrap();
    drop(high_slot);

    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(system.thread_state(high_slot_id), Ok(ThreadState::Exited));
    {
        let state = system.state.lock();
        state
            .thread_record(low_slot.id())
            .unwrap()
            .sched
            .lock()
            .deadline_overrun_events += 1;
    }

    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(ROTATING_DEADLINE_CALLBACKS.load(Ordering::Acquire), 1);
    assert_eq!(system.thread_state(high_slot_id), Ok(ThreadState::Exited));

    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(system.thread_state(high_slot_id), Ok(ThreadState::Exited));
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        system.thread_state(high_slot_id),
        Err(TaskError::StaleThreadId),
        "a continuously replenished low slot must not starve exit and reaping"
    );
}

#[test]
fn last_handle_drop_publishes_after_its_strong_reference_is_released() {
    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let thread_id = thread.id();
    system.mark_exited(thread_id).unwrap();

    assert!(system.task_work.take_pending());
    let first_pass = system.service_deferred_task_work(64).unwrap();
    assert_eq!(first_pass.reaped_threads, 0);

    let barrier: &'static crate::task_work::TestPublishBarrier =
        Box::leak(Box::new(crate::task_work::TestPublishBarrier::new()));
    system.task_work.install_test_publish_barrier(barrier);
    let dropper = std::thread::spawn(move || drop(thread));
    barrier.wait_until_entered();

    assert!(system.task_work.take_pending());
    let racing_pass = system.service_deferred_task_work(64).unwrap();
    barrier.release();
    dropper.join().unwrap();

    assert_eq!(
        racing_pass.reaped_threads, 1,
        "the drop notification must become visible only after its Arc count decreases"
    );
    assert_eq!(
        system.thread_state(thread_id),
        Err(TaskError::StaleThreadId)
    );
}

#[test]
fn public_task_work_consumers_cannot_bypass_single_consumer_ownership() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let _consumer = system.task_work.try_claim_consumer().unwrap();

    assert_eq!(
        system.dispatch_exit_callbacks(1),
        Err(TaskError::ThreadBusy)
    );
    assert_eq!(
        system.reap_unreferenced_exited(1),
        Err(TaskError::ThreadBusy)
    );
    assert_eq!(
        system.drain_deferred_reclaims(1),
        Err(TaskError::ThreadBusy)
    );
}

#[test]
fn switch_handoff_core_reference_does_not_block_detached_reaping() {
    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let exiting = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let exiting_id = exiting.id();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    drop(exiting);
    drop(idle);

    let decision = system.exit_current(cpu.as_mut()).unwrap();
    assert_eq!(decision.next(), cpu.idle().unwrap());
    let barrier: &'static crate::task_work::TestPublishBarrier =
        Box::leak(Box::new(crate::task_work::TestPublishBarrier::new()));
    system.task_work.install_test_publish_barrier(barrier);
    let service_system = Arc::clone(&system);
    let service = std::thread::spawn(move || {
        barrier.wait_until_entered();
        assert!(service_system.task_work.take_pending());
        let batch = service_system.service_deferred_task_work(64).unwrap();
        barrier.release();
        batch
    });

    system.complete_context_switch(cpu.as_mut()).unwrap();
    let racing_pass = service.join().unwrap();
    assert_eq!(
        racing_pass.reaped_threads, 1,
        "scheduler-internal core references must not count as external lifetime leases"
    );
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId)
    );
}

#[test]
fn owned_reap_returns_handle_until_other_wake_references_are_gone() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let id = thread.id();
    let late_wake = thread.wake_handle();
    system.mark_exited(id).unwrap();

    let error = system.reap_thread_handle(thread).unwrap_err();
    assert_eq!(error.task_error(), TaskError::ThreadBusy);
    let thread = error
        .into_retry_handle()
        .expect("busy owned reap must retain its generation-pinning handle");
    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);

    drop(late_wake);
    system.reap_thread_handle(thread).unwrap();
    assert_eq!(system.thread_state(id), Err(TaskError::StaleThreadId));
}

#[test]
fn current_entry_can_release_its_lookup_lease_before_nonreturning_exit() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    // SAFETY: the no-op callback table accepts the zero-sized test payload.
    let extension = unsafe { ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS) };
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()).with_extension(extension))
        .unwrap();
    let lease = system
        .thread_extension_lease(thread.clone())
        .unwrap()
        .unwrap();

    let view = unsafe {
        // SAFETY: the registry record retains the extension until this
        // test marks and reaps the current-entry model below.
        lease.release_for_current_thread_entry()
    };
    assert!(core::ptr::eq(view.ops(), &DEADLINE_TEST_EXTENSION_OPS));
    system.mark_exited(thread.id()).unwrap();
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    system.reap_thread_handle(thread).unwrap();
}

#[test]
fn reaper_waits_until_embedded_sleep_timer_is_detached() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let id = thread.id();
    thread.core.register_sleep_timer(CpuId::new(0), 1);
    system.mark_exited(id).unwrap();

    assert_eq!(system.reap_thread(id), Err(TaskError::ThreadBusy));
    assert!(thread.core.complete_sleep_timer(1));
    system.reap_thread_handle(thread).unwrap();
}

#[test]
fn exited_context_cannot_be_reaped_before_switch_tail() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let exiting = bootstrap.id();
    drop(bootstrap);
    let decision = system.exit_current(cpu.as_mut()).unwrap();
    assert_ne!(decision.next(), exiting);
    assert_eq!(system.reap_thread(exiting), Err(TaskError::ThreadBusy));

    system.complete_context_switch(cpu.as_mut()).unwrap();
    system.reap_thread(exiting).unwrap();
}

#[test]
fn remote_affinity_published_after_exit_drain_is_a_late_noop() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let exiting = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let exiting_id = exiting.id();
    let exiting_core = Arc::downgrade(&exiting.core);
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    system.drain_owner_work(cpu0.as_mut(), 0).unwrap();
    let mut target_only = CpuSet::empty(2);
    assert!(target_only.insert(CpuId::new(1)));
    system.set_affinity(exiting_id, target_only).unwrap();
    assert!(cpu0.has_remote_work());

    system
        .commit_current_exit_after_owner_drain(cpu0.as_mut(), 1)
        .unwrap();
    drop(exiting);
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert_eq!(
        system.service_deferred_task_work(1).unwrap().processed(),
        0,
        "the in-flight affinity delivery must pin the exited record"
    );

    system.drain_policy_updates(cpu0.as_mut(), 2).unwrap();
    assert_eq!(cpu1.try_runnable_summary(), Some(0));
    let core = exiting_core
        .upgrade()
        .expect("the registry still retains the exited core before reaping");
    assert_eq!(core.scheduler_inbox_delivery_count(), 0);
    assert_eq!(core.sched().lock().placement.migration_target(), None);
    drop(core);
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId)
    );
    assert!(
        exiting_core.upgrade().is_none(),
        "late migration payload Arc must be released after owner drain"
    );
}

#[test]
fn remote_deadline_policy_published_after_exit_drain_cannot_create_a_zombie() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let exiting = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let exiting_id = exiting.id();
    let exiting_core = Arc::downgrade(&exiting.core);
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    system.drain_owner_work(cpu0.as_mut(), 0).unwrap();
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 10, DeadlineFlags::NONE).unwrap());
    system.set_thread_policy(exiting_id, deadline).unwrap();
    assert!(cpu0.has_remote_work());

    system
        .commit_current_exit_after_owner_drain(cpu0.as_mut(), 1)
        .unwrap();
    drop(exiting);
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 0);

    system.drain_policy_updates(cpu0.as_mut(), 2).unwrap();
    assert!(
        cpu0.deadline_members.is_empty(),
        "late policy delivery must not register an exited Deadline member"
    );
    let core = exiting_core
        .upgrade()
        .expect("the registry still retains the exited core before reaping");
    assert_eq!(core.scheduler_inbox_delivery_count(), 0);
    drop(core);
    assert_eq!(
        system.deadline_activity(exiting_id),
        Err(TaskError::InvalidConfiguration),
        "late policy delivery must not register an exited Deadline member"
    );
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId)
    );
    assert!(
        exiting_core.upgrade().is_none(),
        "late policy payload Arc must be released after owner drain"
    );
}

#[test]
fn failed_owner_batch_releases_all_detached_payloads() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let thread_id = thread.id();
    let thread_core = Arc::downgrade(&thread.core);
    system.make_ready(thread_id).unwrap();
    system.enqueue(cpu1.as_mut(), thread_id, 0).unwrap();

    thread
        .core
        .sched()
        .lock()
        .placement
        .set_migration_target(Some(CpuId::new(1)))
        .unwrap();
    assert!(thread.core.reserve_scheduler_inbox_delivery());
    let pointer = Arc::as_ptr(&thread.core);
    unsafe {
        // SAFETY: this test follows the production publication contract and
        // transfers the retained count into the migration inbox below.
        Arc::increment_strong_count(pointer);
    }
    let node = unsafe {
        // SAFETY: the retained Arc pins the embedded migration node until
        // the owner detaches and reconstructs this payload.
        Pin::new_unchecked((*pointer).migration_node())
    };
    let malformed_owner = InboxMessage::migration_with_payload(
        thread_id,
        CpuId::new(0),
        CpuId::new(1),
        thread_id.generation() as u64,
        pointer.expose_provenance(),
    );
    assert_eq!(
        cpu1.remote().publish_migration(node, malformed_owner),
        PublishResult::Published
    );
    system
        .set_thread_policy(
            thread_id,
            SchedulePolicy::fifo(RtPriority::new(80).unwrap()),
        )
        .unwrap();
    assert_eq!(
        system.drain_policy_updates(cpu1.as_mut(), 1),
        Err(TaskError::InvalidConfiguration)
    );
    assert_eq!(thread.core.scheduler_inbox_delivery_count(), 0);

    system.dequeue(cpu1.as_mut(), thread_id).unwrap();
    system.mark_exited(thread_id).unwrap();
    drop(thread);
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(thread_id),
        Err(TaskError::StaleThreadId),
        "an error in one detached message must release later retained payloads"
    );
    assert!(
        thread_core.upgrade().is_none(),
        "the detached batch guard must release every suffix payload Arc"
    );
}

#[test]
fn failed_runtime_switch_tail_keeps_outgoing_context_unreclaimable() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let exiting = bootstrap.id();
    drop(bootstrap);
    system.exit_current(cpu.as_mut()).unwrap();
    crate::test_runtime::configure_context_switch_tail(RuntimeStatus::InvalidHandle);

    assert_eq!(
        system.complete_context_switch(cpu.as_mut()),
        Err(TaskError::RuntimeFailure(
            RuntimeStatus::InvalidHandle as u32
        ))
    );
    assert_eq!(crate::test_runtime::context_switch_tail_count(), 1);
    assert!(cpu.switch_handoff().is_some());
    assert_eq!(system.reap_thread(exiting), Err(TaskError::ThreadBusy));

    crate::test_runtime::configure_context_switch_tail(RuntimeStatus::Success);
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(crate::test_runtime::context_switch_tail_count(), 1);
    system.reap_thread(exiting).unwrap();
}

#[test]
fn invalid_switch_tail_state_is_rejected_before_runtime_commit() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let exiting = bootstrap.id();
    let exiting_core = Arc::clone(&bootstrap.core);
    drop(bootstrap);
    system.exit_current(cpu.as_mut()).unwrap();
    crate::test_runtime::configure_context_switch_tail(RuntimeStatus::Success);

    exiting_core.sched().lock().placement.inject_detached();
    assert_eq!(
        system.complete_context_switch(cpu.as_mut()),
        Err(TaskError::InvalidConfiguration)
    );
    assert_eq!(
        crate::test_runtime::context_switch_tail_count(),
        0,
        "runtime tail must not commit before scheduler handoff validation"
    );
    assert!(
        cpu.switch_handoff().is_some(),
        "a rejected pre-commit handoff must remain retryable"
    );

    exiting_core
        .sched()
        .lock()
        .placement
        .inject_exited_awaiting_tail(cpu.owner());
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(crate::test_runtime::context_switch_tail_count(), 1);
    drop(exiting_core);
    system.reap_thread(exiting).unwrap();
}

#[test]
fn switch_tail_defers_exit_callback_until_scheduler_guards_are_released() {
    EXIT_CALLBACK_INVOCATIONS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    // SAFETY: the test callback table owns no external resource and treats
    // the zero payload as an opaque value.
    let extension = unsafe { ThreadExtension::new(0, &EXIT_CALLBACK_TEST_OPS) };
    let bootstrap = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let exiting = bootstrap.id();
    drop(bootstrap);
    system.exit_current(cpu.as_mut()).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();

    assert_eq!(
        EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire),
        0,
        "context-switch tail must not invoke task-context exit callbacks"
    );
    assert_eq!(system.dispatch_exit_callbacks(1).unwrap(), 1);
    assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 1);
    assert_eq!(system.thread_state(exiting), Ok(ThreadState::Exited));
}

#[test]
fn scheduler_work_without_preemption_preserves_current_dispatch() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    cpu.request_scheduler_work();
    assert!(matches!(
        system.schedule_if_requested(cpu.as_mut(), 1).unwrap(),
        SchedulerOutcome::Quiescent
    ));
    system
        .charge_current(cpu.as_mut(), 2, 1, 0)
        .expect("scheduler-only work must not discard the running dispatch");
}

#[test]
fn fair_policy_update_reweights_lag_without_resetting_service_history() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let second = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&first, &second] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    }

    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), first.id());
    system
        .charge_current(cpu.as_mut(), 400_000, 400_000, 0)
        .unwrap();
    assert_eq!(
        system.yield_current(cpu.as_mut(), 400_000).unwrap().next(),
        second.id()
    );
    system
        .charge_current(cpu.as_mut(), 800_000, 400_000, 0)
        .unwrap();
    assert_eq!(
        system.yield_current(cpu.as_mut(), 800_000).unwrap().next(),
        first.id()
    );
    system
        .charge_current(cpu.as_mut(), 1_050_000, 250_000, 0)
        .unwrap();

    let before = cpu
        .current_dispatch
        .as_ref()
        .unwrap()
        .entity
        .fair()
        .unwrap();
    assert_eq!(before.vruntime(), 650_000);
    assert_eq!(before.remaining_request_ns(), 350_000);
    let virtual_time = cpu.run_queue.virtual_time();
    assert_eq!(virtual_time, 825_000);

    let nice = Nice::new(5).unwrap();
    system
        .set_thread_policy(first.id(), SchedulePolicy::fair(nice, FairMode::Normal))
        .unwrap();
    system
        .drain_policy_updates(cpu.as_mut(), 1_050_000)
        .unwrap();
    let reweighted = system
        .state
        .lock()
        .thread_record(first.id())
        .unwrap()
        .sched
        .lock()
        .entity
        .fair()
        .unwrap();
    let lag =
        (virtual_time as i128 - 650_000_i128) * Nice::ZERO.weight() as i128 / nice.weight() as i128;
    let expected_vruntime = (virtual_time as i128 - lag) as u64;
    let expected_remaining_delta = (350_000_u128 * 1024 / nice.weight() as u128) as u64;
    assert_eq!(reweighted.vruntime(), expected_vruntime);
    assert_eq!(reweighted.remaining_request_ns(), 350_000);
    assert_eq!(
        reweighted.virtual_deadline(),
        expected_vruntime + expected_remaining_delta
    );

    system
        .set_thread_policy(first.id(), SchedulePolicy::fair(nice, FairMode::Batch))
        .unwrap();
    system
        .drain_policy_updates(cpu.as_mut(), 1_050_000)
        .unwrap();
    let batch = system
        .state
        .lock()
        .thread_record(first.id())
        .unwrap()
        .sched
        .lock()
        .entity
        .fair()
        .unwrap();
    assert_eq!(batch.vruntime(), reweighted.vruntime());
    assert_eq!(batch.virtual_deadline(), reweighted.virtual_deadline());
    assert_eq!(batch.remaining_request_ns(), 350_000);

    system
        .set_thread_policy(
            first.id(),
            SchedulePolicy::fair(Nice::new(-20).unwrap(), FairMode::Idle),
        )
        .unwrap();
    system
        .drain_policy_updates(cpu.as_mut(), 1_050_000)
        .unwrap();
    let idle = system
        .state
        .lock()
        .thread_record(first.id())
        .unwrap()
        .sched
        .lock()
        .entity
        .fair()
        .unwrap();
    assert_eq!(idle.nice(), Nice::LOWEST);
    assert_eq!(idle.remaining_request_ns(), 350_000);
}

#[test]
fn running_idle_to_normal_transition_uses_both_class_virtual_times() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let idle = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let normal = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(normal.id()).unwrap();
    system.enqueue(cpu.as_mut(), normal.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        normal.id()
    );
    system
        .charge_current(cpu.as_mut(), 1_000_000, 1_000_000, 0)
        .unwrap();
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        idle.id()
    );
    system
        .charge_current(cpu.as_mut(), 1_001_000, 1_000, 0)
        .unwrap();

    let normal_virtual_time = cpu.run_queue.virtual_time();
    assert_eq!(normal_virtual_time, 1_000_000);
    system
        .set_thread_policy(idle.id(), SchedulePolicy::default())
        .unwrap();
    system
        .drain_policy_updates(cpu.as_mut(), 1_001_000)
        .unwrap();

    let transitioned = system
        .state
        .lock()
        .thread_record(idle.id())
        .unwrap()
        .sched
        .lock()
        .entity
        .fair()
        .unwrap();
    assert_eq!(
        transitioned.vruntime(),
        normal_virtual_time,
        "a zero-lag entity must be rebased onto the destination class's V",
    );
}

#[test]
fn running_normal_to_idle_transition_settles_then_rebases_lag() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let normal = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    system
        .set_thread_policy(
            normal.id(),
            SchedulePolicy::fair(Nice::ZERO, FairMode::Idle),
        )
        .unwrap();
    system
        .drain_policy_updates(cpu.as_mut(), 1_000_000)
        .unwrap();

    let state = system.state.lock();
    let record = state.thread_record(normal.id()).unwrap();
    let sched = record.sched.lock();
    let transitioned = sched.entity.fair().unwrap();
    assert_eq!(sched.charged_runtime_ns, 1_000_000);
    assert_eq!(transitioned.mode(), FairMode::Idle);
    assert_eq!(
        transitioned.vruntime(),
        cpu.run_queue.virtual_time_for_mode(FairMode::Idle),
        "settled zero lag must be expressed relative to the destination V domain",
    );
}

#[test]
fn bounded_inbox_remainder_stays_sticky_across_scheduler_entry() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let mut nodes = Vec::with_capacity(cpu.batch_limit() * 2 + 1);
    for slot in 0..=cpu.batch_limit() * 2 {
        nodes.push(Box::pin(crate::inbox::InboxNode::new(
            crate::inbox::InboxKind::RemoteWake,
        )));
        let message =
            InboxMessage::remote_wake(ThreadId::from_parts(slot as u32, 1), CpuId::new(0));
        assert_eq!(
            cpu.remote()
                .publish_remote_wake(test_inbox_node(nodes.last().unwrap()), message),
            PublishResult::Published
        );
    }

    let first = system.drain_remote_wakes(cpu.as_mut(), 1).unwrap();
    assert_eq!(first.drained(), cpu.batch_limit());
    assert!(first.pending());
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
            .owner_work_pending()
    );
    assert!(cpu.needs_reschedule());

    let second = system.drain_remote_wakes(cpu.as_mut(), 2).unwrap();
    assert_eq!(second.drained(), 1);
    assert!(!second.pending());
    assert!(matches!(
        system.schedule_if_requested(cpu.as_mut(), 2).unwrap(),
        SchedulerOutcome::Quiescent
    ));
    system.charge_current(cpu.as_mut(), 3, 1, 0).unwrap();
}

#[test]
fn running_migration_is_published_only_after_switch_tail() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        thread.id()
    );

    let mut target_only = CpuSet::empty(2);
    target_only.insert(CpuId::new(1));
    system.set_affinity(thread.id(), target_only).unwrap();
    system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
    let decision = system
        .schedule_if_requested(cpu0.as_mut(), 1)
        .unwrap()
        .decision()
        .unwrap();
    assert_eq!(decision.previous(), Some(thread.id()));
    assert!(!cpu1.has_remote_work());

    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert!(cpu1.has_remote_work());
    let transfer = system.drain_policy_updates(cpu1.as_mut(), 2).unwrap();
    assert_eq!(transfer.drained(), 1);
    assert!(!transfer.pending());
}

#[test]
fn placement_rejects_an_unrelated_cpu_claim() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();

    // A stale remote publication cannot manufacture an independent `on_cpu`
    // owner alongside the runqueue owner.
    let result = system
        .state
        .lock()
        .thread_record_mut(thread.id())
        .unwrap()
        .sched
        .lock()
        .placement
        .set_on_cpu(Some(CpuId::new(1)));
    assert_eq!(result, Err(TaskError::InvalidConfiguration));
    assert_eq!(
        system
            .state
            .lock()
            .thread_record(thread.id())
            .unwrap()
            .sched
            .lock()
            .placement
            .queued_cpu(),
        Some(CpuId::new(0))
    );
}

#[test]
fn owner_current_affinity_change_does_not_publish_a_self_request() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let mut target_only = CpuSet::empty(2);
    target_only.insert(CpuId::new(1));
    assert!(
        system
            .set_current_affinity(cpu0.as_mut(), target_only)
            .unwrap()
    );
    assert!(
        !cpu0.has_remote_work(),
        "the owner can commit its migration directly at the next schedule-out"
    );
    assert_eq!(system.thread_state(running.id()), Ok(ThreadState::Running));
}

#[test]
fn schedule_out_rechecks_affinity_under_the_thread_lock() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    // Model the exact SMP interleaving that used to corrupt CpuLocal:
    // the owner observed no migration, then a remote affinity writer made
    // this CPU illegal before requeue acquired the thread lock. Affinity is
    // authoritative even if a stale migration hint has not been installed.
    let mut target_only = CpuSet::empty(2);
    assert!(target_only.insert(CpuId::new(1)));
    {
        let mut sched = running.core.sched().lock();
        sched.affinity = target_only;
        sched.placement.set_migration_target(None).unwrap();
    }
    running.core.set_target_cpu(CpuId::new(1));

    let decision = system.schedule(cpu0.as_mut(), 1).unwrap();
    assert_eq!(decision.switch_reason(), SwitchReason::Migrated);
    assert_eq!(decision.next(), idle0.id());
    assert_eq!(cpu0.current(), Some(idle0.id()));
    assert_eq!(system.thread_state(running.id()), Ok(ThreadState::Ready));
    assert_eq!(
        running.core.sched().lock().placement.migration_target(),
        Some(CpuId::new(1))
    );
}

#[test]
fn initial_placement_hands_affinity_pinned_thread_to_its_owner_cpu() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    let mut cpu1_only = CpuSet::empty(2);
    cpu1_only.insert(CpuId::new(1));
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu1_only))
        .unwrap();
    system.make_ready(thread.id()).unwrap();

    system.place_ready(cpu0.as_mut(), thread.id(), 0).unwrap();
    assert!(cpu1.has_remote_work());
    system.drain_policy_updates(cpu1.as_mut(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        thread.id()
    );
}

#[test]
fn class_order_is_deadline_then_rt_then_fair() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let policies = [
        SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
        SchedulePolicy::fifo(RtPriority::new(1).unwrap()),
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap()),
    ];
    let mut ids = Vec::new();
    for policy in policies {
        let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
        ids.push(thread.id());
    }
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), ids[2]);
}

#[test]
fn deadline_affinity_must_cover_online_root_domain() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let mut affinity = CpuSet::empty(2);
    affinity.insert(CpuId::new(0));
    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
    assert!(matches!(
        system.create_thread(ThreadSpec::new(policy).with_affinity(affinity)),
        Err(TaskError::DeadlineAffinity)
    ));
}

#[test]
fn active_sleep_timer_pins_affinity_placement_to_its_owner_cpu() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    thread.core.register_sleep_timer(CpuId::new(1), 7);

    let mut excludes_owner = CpuSet::empty(2);
    excludes_owner.insert(CpuId::new(0));
    assert_eq!(
        system.set_affinity(thread.id(), excludes_owner),
        Err(TaskError::ActiveTimerAffinity)
    );

    let mut includes_owner = CpuSet::empty(2);
    includes_owner.insert(CpuId::new(1));
    system.set_affinity(thread.id(), includes_owner).unwrap();
    assert_eq!(thread.wake_handle().target_cpu(), Some(CpuId::new(1)));
    assert!(thread.core.complete_sleep_timer(7));
}

#[test]
fn queued_pi_owner_is_requeued_only_by_its_owner_cpu() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(19).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let competitor = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(99).unwrap(),
        )))
        .unwrap();
    for thread in [&owner, &competitor] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    let lock = PiLockIdentity::new().id().unwrap();

    let _wait = system.pi_wait_start(lock, waiter.id(), owner.id()).unwrap();

    assert!(matches!(
        owner.effective_policy(),
        SchedulePolicy::Fifo { priority } if priority.get() == 99
    ));
    assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 2);
    let drain = system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    assert_eq!(drain.drained(), 1);
    assert_eq!(system.schedule(cpu.as_mut(), 1).unwrap().next(), owner.id());
}

#[test]
fn effective_rt_entity_never_replaces_the_base_rr_accounting() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let base = SchedulePolicy::round_robin_with_quantum(RtPriority::new(20).unwrap(), 10).unwrap();
    let owner = system.create_thread(ThreadSpec::new(base)).unwrap();
    let donor = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(80).unwrap(),
        )))
        .unwrap();
    system.make_ready(owner.id()).unwrap();
    system.enqueue(cpu.as_mut(), owner.id(), 0).unwrap();
    let _wait = system
        .pi_wait_start(PiLockIdentity::new().id().unwrap(), donor.id(), owner.id())
        .unwrap();
    system.drain_policy_updates(cpu.as_mut(), 0).unwrap();

    let state = system.state.lock();
    let sched = state.thread_record(owner.id()).unwrap().sched.lock();
    assert!(
        sched.base_entity.matches_policy(base),
        "PI effective entity must not become base RR accounting"
    );
    assert!(matches!(sched.policy, SchedulePolicy::Fifo { .. }));
    assert!(
        sched.entity.matches_policy(sched.policy),
        "effective policy and entity must be published as one coherent snapshot"
    );
}

#[test]
fn chained_and_multi_lock_donations_are_withdrawn_independently() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let first_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(19).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let second_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(10).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let urgent = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(99).unwrap(),
        )))
        .unwrap();
    let first_lock = PiLockIdentity::new().id().unwrap();
    let second_lock = PiLockIdentity::new().id().unwrap();
    let chained = system
        .pi_wait_start(first_lock, second_owner.id(), first_owner.id())
        .unwrap();
    let urgent_wait = system
        .pi_wait_start(second_lock, urgent.id(), second_owner.id())
        .unwrap();
    assert!(matches!(
        first_owner.effective_policy(),
        SchedulePolicy::Fifo { priority } if priority.get() == 99
    ));

    system.pi_wait_cancel(urgent_wait).unwrap();
    assert_eq!(second_owner.effective_policy(), second_owner.policy());
    assert_eq!(first_owner.effective_policy(), second_owner.policy());

    system.pi_wait_cancel(chained).unwrap();
    assert_eq!(first_owner.effective_policy(), first_owner.policy());
}

#[test]
fn deadline_donor_budget_is_debited_and_overrun_callback_is_deferred() {
    DEADLINE_OVERRUN_CALLBACKS.store(0, Ordering::Relaxed);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let deadline = SchedulePolicy::deadline(
        DeadlinePolicy::new(10, 20, 100, DeadlineFlags::DL_OVERRUN).unwrap(),
    );
    let extension = unsafe { ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS) };
    let donor = system
        .create_thread(ThreadSpec::new(deadline).with_extension(extension))
        .unwrap();
    let lock = PiLockIdentity::new().id().unwrap();
    for thread in [&owner, &donor] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), donor.id());
    let _wait = system.pi_wait_start(lock, donor.id(), owner.id()).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 0).unwrap();
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        owner.id()
    );

    let charged = system.charge_current(cpu.as_mut(), 10, 10, 0).unwrap();
    assert!(!charged.slice_expired());
    assert!(charged.deadline_overrun());
    assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 0);
    system.schedule(cpu.as_mut(), 10).unwrap();

    let donor_runtime = system.deadline_runtime(donor.id()).unwrap();
    assert_eq!(donor_runtime.remaining_runtime_ns(), 0);
    assert_eq!(donor_runtime.overruns(), 1);
    let owner_runtime = system.deadline_runtime(owner.id()).unwrap();
    assert_eq!(owner_runtime.donor(), Some(donor.id()));
    assert!(owner_runtime.pi_critical_rescue());
    system
        .set_thread_policy(donor.id(), SchedulePolicy::default())
        .unwrap();
    system.drain_policy_updates(cpu.as_mut(), 10).unwrap();
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(1));
    assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 1);
}

#[test]
fn remote_pi_owner_exclusively_borrows_the_donor_cbs_entity() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let donor_policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(10, 20, 100, DeadlineFlags::RECLAIM).unwrap());
    let donor = system.create_thread(ThreadSpec::new(donor_policy)).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&donor, &owner] {
        system.make_ready(thread.id()).unwrap();
    }
    system.enqueue(cpu0.as_mut(), donor.id(), 0).unwrap();
    system.enqueue(cpu1.as_mut(), owner.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu0.as_mut(), 0).unwrap().next(),
        donor.id()
    );
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        owner.id()
    );

    let _wait = system
        .pi_wait_start(PiLockIdentity::new().id().unwrap(), donor.id(), owner.id())
        .unwrap();
    assert_ne!(
        system.block_current(cpu0.as_mut()).unwrap().next(),
        donor.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.drain_policy_updates(cpu1.as_mut(), 0).unwrap();

    let (borrowed_generation, budget_before_timer) = {
        let state = system.state.lock();
        let sched = state.thread_record(donor.id()).unwrap().sched.lock();
        assert_eq!(sched.deadline_cbs_borrower, Some(owner.id()));
        (
            sched.deadline_cbs_generation,
            sched
                .base_deadline
                .expect("Deadline donor must retain CBS state"),
        )
    };
    assert!(
        cpu0.take_due_scheduler_deadline(20),
        "the donor deadline must first be consumed by its clockevent"
    );
    system.service_deadline_timers(cpu0.as_mut(), 20).unwrap();
    assert_eq!(
        cpu0.scheduler_deadline_ns(),
        None,
        "the donor CPU must not rearm a CBS timer while the remote PI owner holds its baton"
    );
    {
        let state = system.state.lock();
        let sched = state.thread_record(donor.id()).unwrap().sched.lock();
        assert_eq!(sched.deadline_cbs_borrower, Some(owner.id()));
        assert_eq!(sched.deadline_cbs_generation, borrowed_generation);
        assert_eq!(sched.base_deadline, Some(budget_before_timer));
    }
    cpu0.as_mut().scheduler_enter();
    assert!(
        !cpu0.needs_reschedule(),
        "the donor CPU must start the baton-return check without stale work"
    );

    cpu1.as_mut()
        .fields_mut()
        .add_deadline_bandwidth(500_000_000, false)
        .unwrap();
    system.charge_current(cpu1.as_mut(), 5, 5, 0).unwrap();
    system
        .commit_owner_current_dispatch(cpu1.as_mut(), 5)
        .unwrap();
    assert!(
        cpu0.needs_reschedule(),
        "returning a remote CBS baton must publish donor-CPU reconciliation work"
    );
    {
        let state = system.state.lock();
        let sched = state.thread_record(donor.id()).unwrap().sched.lock();
        assert_eq!(sched.deadline_cbs_borrower, None);
        assert!(sched.deadline_cbs_generation > borrowed_generation);
        assert_eq!(
            sched
                .base_deadline
                .expect("committed donor budget must remain Deadline")
                .remaining_runtime_ns(),
            5
        );
    }
    system.service_deadline_timers(cpu0.as_mut(), 20).unwrap();
    assert_eq!(system.deadline_runtime(donor.id()).unwrap().misses(), 1);
}

#[test]
fn wake_before_park_is_consumed_without_blocking() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

    assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        system.prepare_park(cpu.as_mut()).unwrap(),
        ParkPrepare::Notified
    );
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
    let wake = system.drain_remote_wakes(cpu.as_mut(), 0).unwrap();
    assert_eq!(wake.drained(), 1);
    assert!(!wake.pending());
}

#[test]
fn consumed_running_wake_does_not_notify_a_later_park() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

    assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
    assert_eq!(
        system
            .drain_remote_wakes(cpu.as_mut(), 0)
            .unwrap()
            .drained(),
        1,
    );
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running,
    );
    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("a later park must not consume the previous running wake");
    };
    system.cancel_park(cpu.as_mut(), &mut ticket).unwrap();
}

#[test]
fn wake_during_parking_cancels_schedule_out() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("fresh park must publish PARKING");
    };

    assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

    assert!(matches!(
        system.commit_park(cpu.as_mut(), &mut ticket).unwrap(),
        ParkCommit::Notified
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
    assert!(matches!(
        system.commit_park(cpu.as_mut(), &mut ticket),
        Err(TaskError::StaleThreadId)
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running,
        "a resolved park ticket must not start another block transition"
    );
}

#[test]
fn drained_remote_wake_during_parking_is_committed_by_the_owner_thread() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("fresh park must publish PARKING");
    };

    assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
    assert_eq!(
        system
            .drain_remote_wakes(cpu.as_mut(), 0)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Parking,
        "the owner must finish a PARKING handshake before wake can enqueue it"
    );
    assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
    assert!(system.snapshot(cpu.as_ref()).need_resched());
    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 0)
            .unwrap()
            .parking_deferred(),
        "IRQ-return scheduling must defer while current owns a PARKING token"
    );
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Parking
    );
    assert!(!system.snapshot(cpu.as_ref()).need_resched());

    assert!(matches!(
        system.commit_park(cpu.as_mut(), &mut ticket).unwrap(),
        ParkCommit::Notified
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
    assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
    assert!(!system.snapshot(cpu.as_ref()).need_resched());
    assert!(
        matches!(
            system.schedule_if_requested(cpu.as_mut(), 0).unwrap(),
            SchedulerOutcome::Quiescent
        ),
        "a work-only wake must not be upgraded into a preemption"
    );
    assert_eq!(system.snapshot(cpu.as_ref()).current(), Some(running.id()));
    assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
    assert!(!system.snapshot(cpu.as_ref()).need_resched());
    assert!(matches!(
        system.cancel_park(cpu.as_mut(), &mut ticket),
        Err(TaskError::StaleThreadId)
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running,
        "a resolved park ticket must not mutate the current thread"
    );
}

static DEADLINE_TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_hook,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: count_deadline_overrun,
    drop: no_extension_drop,
};

static ROTATING_DEADLINE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

static ROTATING_DEADLINE_TEST_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_hook,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: count_rotating_deadline_overrun,
    drop: no_extension_drop,
};

static EXIT_CALLBACK_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

static EXIT_CALLBACK_TEST_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_hook,
    on_switch_out: no_extension_switch_out,
    on_exit: count_exit_callback,
    on_deadline_overrun: no_extension_hook,
    drop: no_extension_drop,
};

unsafe extern "Rust" fn no_extension_hook(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn no_extension_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: SwitchReason,
) {
}

unsafe extern "Rust" fn count_deadline_overrun(_data: usize, _thread: ThreadId) {
    DEADLINE_OVERRUN_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "Rust" fn count_rotating_deadline_overrun(_data: usize, _thread: ThreadId) {
    ROTATING_DEADLINE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "Rust" fn count_exit_callback(_data: usize, _thread: ThreadId) {
    EXIT_CALLBACK_INVOCATIONS.fetch_add(1, Ordering::Release);
}

unsafe extern "Rust" fn no_extension_drop(_data: usize) {}

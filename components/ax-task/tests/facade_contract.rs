use core::pin::Pin;

use ax_task::{
    CpuId, FairMode, Nice, SchedulePolicy, TaskError, TaskSystem, TaskSystemConfig,
    ThreadExtension, ThreadExtensionOps, ThreadId, ThreadSpec, ThreadState, WakeResult,
    current_cpu_needs_resched, current_thread_extension, current_thread_id, schedule_current_cpu,
    take_current_expired_timers,
    timer::{ExpiredTimer, TimerNode},
    timer_interrupt_current_cpu,
};

mod support;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn switch_trace_observes_the_previous_extension_before_switch_out() {
    let source = include_str!("../src/facade.rs");
    let switch = source
        .split_once("fn execute_switch_plan(")
        .expect("switch executor must exist")
        .1
        .split_once("fn install_next_address_space(")
        .expect("switch executor must remain focused")
        .0;
    let trace = switch
        .find("task_runtime::trace_sched_switch")
        .expect("switch trace must be emitted");
    let switch_out = switch
        .find("extension.ops().on_switch_out")
        .expect("previous extension must receive switch-out");
    assert!(trace < switch_out);
}

#[test]
fn facade_reports_uninitialized_then_uses_runtime_owned_objects() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    assert_eq!(current_thread_id(), Err(TaskError::NotInitialized));

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let extension = unsafe { ThreadExtension::new(0x1234, &TEST_EXTENSION_OPS) };
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
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    assert_eq!(current_thread_id().unwrap(), bootstrap.id());
    assert_eq!(
        std::thread::spawn(current_thread_id).join().unwrap(),
        Err(TaskError::NotInitialized),
        "a host test thread must not inherit another fixture's borrowed handles"
    );
    assert_eq!(current_thread_id().unwrap(), bootstrap.id());
    assert_eq!(current_thread_extension().unwrap().unwrap().data(), 0x1234);
    cpu.request_reschedule();
    assert!(current_cpu_needs_resched().unwrap());
    assert!(schedule_current_cpu().unwrap().decision().is_some());
    assert!(!current_cpu_needs_resched().unwrap());

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        bootstrap.id()
    );
    assert_eq!(sleeper.wake_handle().wake(), WakeResult::Notified);
    let drain = system.drain_remote_wakes(cpu.as_mut(), 2).unwrap();
    assert_eq!(drain.drained(), 1);
    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready
    );

    support::clear_handles();
}

#[test]
fn timer_irq_facade_bounds_and_preserves_unconsumed_expirations() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    let system = Box::pin(
        TaskSystem::new(
            TaskSystemConfig::new(1)
                .with_timer_capacity(3)
                .with_batch_limit(2),
        )
        .unwrap(),
    );
    let timers = [timer(1), timer(2), timer(3)];
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    for node in &timers {
        unsafe { cpu.as_mut().timer_queue().arm(node.as_ref(), 0).unwrap() };
    }
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    let first = timer_interrupt_current_cpu(1, 0).unwrap();
    assert_eq!(first.expired(), 2);
    assert!(first.pending());
    assert_eq!(first.next_deadline_ns(), Some(1));
    let before_drain = timer_interrupt_current_cpu(1, 0).unwrap();
    assert_eq!(before_drain.expired(), 0);
    assert!(before_drain.pending(), "{before_drain:?}");

    let mut expired = [ExpiredTimer::EMPTY; 2];
    assert_eq!(take_current_expired_timers(&mut expired).unwrap(), 2);
    let mut owners = [expired[0].owner(), expired[1].owner(), 0];
    assert_eq!(timer_interrupt_current_cpu(1, 0).unwrap().expired(), 1);
    assert_eq!(take_current_expired_timers(&mut expired).unwrap(), 1);
    owners[2] = expired[0].owner();
    owners.sort_unstable();
    assert_eq!(owners, [1, 2, 3]);
    support::clear_handles();
}

fn timer(owner: usize) -> Pin<Box<TimerNode>> {
    Box::pin(TimerNode::new(owner))
}

static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_hook,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: no_extension_hook,
    drop: no_extension_drop,
};

unsafe extern "Rust" fn no_extension_hook(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn no_extension_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn no_extension_drop(_data: usize) {}

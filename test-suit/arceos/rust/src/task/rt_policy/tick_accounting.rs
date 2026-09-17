use std::{
    hint,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

use ax_std::os::arceos::{
    api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
    modules::ax_hal::percpu::this_cpu_id,
    sync::Mutex,
    task::{
        runtime::service::{SchedulerTickCpuTime, SchedulerTickGate},
        sched::{CpuId, CpuSet, FairMode, Nice, RtPriority, SchedulePolicy},
        thread::{
            SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadHandle, ThreadId,
            current::{current_thread_id, set_current_thread_affinity},
        },
    },
};

static WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static BLOCKED: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);

pub(super) fn run(cpu_count: usize) {
    ax_set_current_affinity(AxCpuMask::one_shot(0)).unwrap();
    READY.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    BLOCKED.store(false, Ordering::Release);
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let accounting = Arc::new(SchedulerTickCpuTime::with_realtime_gate(gate.clone()));
    let extension = accounting_extension(accounting.clone());
    let worker = ax_std::os::arceos::thread::builder("rt-tick-accounting".into())
        .stack_size(256 * 1024)
        .extension(extension)
        .affinity(single_cpu(cpu_count, 0))
        .spawn(move || {
            ThreadHandle::lookup(current_thread_id().unwrap())
                .unwrap()
                .set_policy(SchedulePolicy::fifo(RtPriority::new(10).unwrap()))
                .unwrap();
            let started = Instant::now();
            while accounting
                .realtime_ticks()
                .is_none_or(|(ticks, _)| ticks < 3)
            {
                assert!(
                    started.elapsed() < super::PROGRESS_TIMEOUT,
                    "RT IRQ ticks did not advance"
                );
                hint::spin_loop();
            }
            // This thread is the sole tick writer. Disabling interest on its
            // current CPU leaves no concurrent writer after this call returns.
            gate.set_enabled(false);
            let before = accounting.realtime_ticks().unwrap();
            let system_before = accounting.snapshot().system_ns();
            let started = Instant::now();
            // CPU-time accounting remains active independently of RT interest.
            // Observe real interrupts instead of assuming a delay contains ticks.
            while accounting.snapshot().system_ns() - system_before < 3 * before.1.get() {
                assert!(
                    started.elapsed() < super::PROGRESS_TIMEOUT,
                    "CPU ticks did not advance while RT interest was disabled"
                );
                hint::spin_loop();
            }
            assert_eq!(
                accounting.realtime_ticks().unwrap(),
                before,
                "disabled RT interest changed accumulated ticks"
            );
            thread::yield_now();
            assert_eq!(
                accounting.realtime_ticks().unwrap(),
                before,
                "yield reset RT ticks"
            );
            for cpu in [1, 0] {
                set_current_thread_affinity(single_cpu(cpu_count, cpu)).unwrap();
                assert_eq!(this_cpu_id(), cpu);
                assert_eq!(
                    accounting.realtime_ticks().unwrap(),
                    before,
                    "migration reset RT ticks"
                );
            }
            READY.store(true, Ordering::Release);
            api::ax_wait_queue_wait_until(&WAIT, || GO.load(Ordering::Acquire), None);
            assert_eq!(
                accounting.realtime_ticks().unwrap().0,
                0,
                "blocking wake did not reset RT ticks"
            );
            gate.set_enabled(true);
            let started = Instant::now();
            while accounting.realtime_ticks().unwrap().0 == 0 {
                assert!(
                    started.elapsed() < super::PROGRESS_TIMEOUT,
                    "re-enabled RT ticks did not advance"
                );
                hint::spin_loop();
            }
        })
        .expect("RT accounting worker must spawn");
    super::wait_until(
        || BLOCKED.load(Ordering::Acquire),
        "RT accounting worker did not reach park",
    );
    // The switch-out callback proves a committed block, even if RT bandwidth
    // throttling lets the controller run before the worker reaches its wait.
    GO.store(true, Ordering::Release);
    assert_eq!(api::ax_wait_queue_wake(&WAIT, 1), 1);
    worker
        .join()
        .expect("RT accounting worker must exit normally");
    pi_boost_is_counted(cpu_count);
}

fn pi_boost_is_counted(cpu_count: usize) {
    let mutex = Arc::new(Mutex::new(()));
    let locked = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let accounting = Arc::new(SchedulerTickCpuTime::with_realtime_gate(gate));
    let owner_mutex = mutex.clone();
    let owner_locked = locked.clone();
    let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let donated = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let owner = ax_std::os::arceos::thread::builder("pi-tick-owner".into())
        .stack_size(256 * 1024)
        .extension(accounting_extension(accounting.clone()))
        .affinity(single_cpu(cpu_count, 1))
        .spawn(move || {
            let current = ThreadHandle::lookup(current_thread_id().unwrap()).unwrap();
            assert_eq!(current.base_policy(), fair);
            let guard = owner_mutex.lock();
            owner_locked.store(true, Ordering::Release);
            let started = Instant::now();
            while accounting
                .realtime_ticks()
                .is_none_or(|(ticks, _)| ticks < 3)
            {
                assert!(
                    started.elapsed() < super::PROGRESS_TIMEOUT,
                    "PI-boosted Fair owner did not accumulate RT ticks"
                );
                hint::spin_loop();
            }
            assert_eq!(current.base_policy(), fair);
            assert_eq!(current.effective_policy(), donated);
            drop(guard);
            assert_eq!(current.effective_policy(), fair);
            assert_eq!(
                accounting.realtime_ticks().unwrap().0,
                0,
                "PI deboost to Fair did not reset RT ticks"
            );
        })
        .expect("PI accounting owner must spawn");
    super::wait_until(
        || locked.load(Ordering::Acquire),
        "PI accounting owner did not lock",
    );
    let waiter = thread::spawn(move || {
        set_current_thread_affinity(single_cpu(cpu_count, 2)).unwrap();
        ThreadHandle::lookup(current_thread_id().unwrap())
            .unwrap()
            .set_policy(donated)
            .unwrap();
        // The owner's only source of RT priority is this blocked waiter.
        drop(mutex.lock());
    });
    waiter
        .join()
        .expect("PI accounting waiter must acquire and exit");
    owner
        .join()
        .expect("PI accounting owner must release and exit");
}

fn accounting_extension(accounting: Arc<SchedulerTickCpuTime>) -> ThreadExtension {
    // SAFETY: callbacks ignore the opaque value and own no allocation. The
    // switch-out callback only publishes an atomic flag; it cannot block or
    // re-enter the scheduler. The capability retains its own Arc lifetime.
    unsafe { ThreadExtension::new(0, &OPS) }.with_scheduler_tick_cpu_time(accounting)
}

fn single_cpu(cpu_count: usize, cpu: usize) -> CpuSet {
    let mut affinity = CpuSet::empty(cpu_count);
    assert!(affinity.insert(CpuId::new(cpu as u32)));
    affinity
}

/// # Safety
/// No additional preconditions; this callback ignores all arguments.
unsafe extern "Rust" fn switch_in(_: usize, _: ThreadId, _: SchedulePolicy, _: u64) {}

/// # Safety
/// No additional preconditions; this callback only accesses static atomics.
unsafe extern "Rust" fn switch_out(_: usize, _: ThreadId, reason: SwitchReason) {
    if reason == SwitchReason::Blocked && READY.load(Ordering::Acquire) {
        BLOCKED.store(true, Ordering::Release);
    }
}

/// # Safety
/// No additional preconditions; this callback ignores all arguments.
unsafe extern "Rust" fn event(_: usize, _: ThreadId) {}

/// # Safety
/// No additional preconditions; no opaque allocation is owned.
unsafe extern "Rust" fn drop_extension(_: usize) {}

static OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: switch_in,
    on_switch_out: switch_out,
    on_exit: event,
    on_deadline_overrun: event,
    drop: drop_extension,
};

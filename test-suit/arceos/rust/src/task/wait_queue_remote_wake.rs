use core::{hint, sync::atomic::AtomicUsize};
use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{schedule_current_cpu, task_test_hooks},
        },
    },
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

static READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static SLEEP_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static DONE_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static TIMEOUT_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static OCCUPIER_READY_WQ: AxWaitQueueHandle = AxWaitQueueHandle::new();
static READY: AtomicBool = AtomicBool::new(false);
static MAY_SLEEP: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static OCCUPIER_READY: AtomicBool = AtomicBool::new(false);
static STOP_OCCUPIER: AtomicBool = AtomicBool::new(false);
static SLEEPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

const WAITER_ENQUEUE_RETRIES: usize = 1024;
const REMOTE_WAKE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(1);
const OCCUPIER_MAX_RUNTIME: Duration = Duration::from_secs(2);
const WORKER_READY_TIMEOUT: Duration = Duration::from_secs(1);

struct RemoteSleeper {
    worker: Option<thread::JoinHandle<()>>,
    thread_id: u64,
}

impl RemoteSleeper {
    fn spawn(cpu: usize) -> Self {
        let worker = thread::spawn(move || {
            pin_current_to_cpu(cpu);
            SLEEPER_CPU.store(this_cpu_id(), Ordering::Release);
            READY.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&READY_WQ, 1);

            while !MAY_SLEEP.load(Ordering::Acquire) {
                thread::yield_now();
            }
            api::ax_wait_queue_wait_until(&SLEEP_WQ, || GO.load(Ordering::Acquire), None);
            assert_eq!(this_cpu_id(), cpu, "remote wakeup resumed on the wrong CPU");
            DONE.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&DONE_WQ, 1);
        });
        let thread_id = worker.thread().id().as_u64().get();
        let sleeper = Self {
            worker: Some(worker),
            thread_id,
        };
        assert!(
            !api::ax_wait_queue_wait_until(
                &READY_WQ,
                || READY.load(Ordering::Acquire),
                Some(WORKER_READY_TIMEOUT),
            ),
            "remote sleeper did not become ready"
        );
        sleeper
    }

    const fn thread_id(&self) -> u64 {
        self.thread_id
    }

    fn finish(mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

impl Drop for RemoteSleeper {
    fn drop(&mut self) {
        MAY_SLEEP.store(true, Ordering::Release);
        GO.store(true, Ordering::Release);
        api::ax_wait_queue_wake(&SLEEP_WQ, 1);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct TargetOccupier {
    worker: Option<thread::JoinHandle<()>>,
}

impl TargetOccupier {
    fn spawn(cpu: usize) -> Self {
        let worker = thread::spawn(move || {
            pin_current_to_cpu(cpu);
            OCCUPIER_READY.store(true, Ordering::Release);
            api::ax_wait_queue_wake(&OCCUPIER_READY_WQ, 1);
            let started = Instant::now();
            while !STOP_OCCUPIER.load(Ordering::Acquire) && started.elapsed() < OCCUPIER_MAX_RUNTIME
            {
                hint::spin_loop();
            }
        });
        let occupier = Self {
            worker: Some(worker),
        };
        assert!(
            !api::ax_wait_queue_wait_until(
                &OCCUPIER_READY_WQ,
                || OCCUPIER_READY.load(Ordering::Acquire),
                Some(WORKER_READY_TIMEOUT),
            ),
            "target occupier did not become ready"
        );
        occupier
    }

    fn stop(mut self) {
        STOP_OCCUPIER.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

impl Drop for TargetOccupier {
    fn drop(&mut self) {
        STOP_OCCUPIER.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

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
        "current task did not migrate to CPU {cpu_id}"
    );
}

fn wake_sleep_queue_after_waiter_enqueued(sleeper: u64) {
    for _ in 0..WAITER_ENQUEUE_RETRIES {
        if task_test_hooks::thread_is_blocked(sleeper) {
            task_test_hooks::arm_wake_irq_owner_probe(sleeper);
            task_test_hooks::arm_wake_entity_read_copy_probe(sleeper);
            GO.store(true, Ordering::Release);
            assert_eq!(api::ax_wait_queue_wake(&SLEEP_WQ, 1), 1);
            return;
        }
        thread::yield_now();
    }
    panic!("sleeper did not enter wait queue");
}

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    assert!(
        cpu_num >= 2,
        "task_wait_queue_remote_wake requires at least two CPUs"
    );

    let waker_cpu = 0;
    let sleeper_cpu = 1;
    READY.store(false, Ordering::Release);
    MAY_SLEEP.store(false, Ordering::Release);
    GO.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    OCCUPIER_READY.store(false, Ordering::Release);
    STOP_OCCUPIER.store(false, Ordering::Release);
    SLEEPER_CPU.store(usize::MAX, Ordering::Release);

    pin_current_to_cpu(waker_cpu);
    let sleeper = RemoteSleeper::spawn(sleeper_cpu);
    let sleeper_id = sleeper.thread_id();
    assert_eq!(SLEEPER_CPU.load(Ordering::Acquire), sleeper_cpu);
    assert_eq!(this_cpu_id(), waker_cpu);
    // Keep a normal task runnable on the target rq so wakeup preemption must
    // inspect the authoritative current entity instead of taking the
    // dedicated-idle shortcut.
    let occupier = TargetOccupier::spawn(sleeper_cpu);
    task_test_hooks::arm_park_irq_owner_probe(sleeper_id);
    task_test_hooks::arm_switch_tail_irq_owner_probe(sleeper_id);
    MAY_SLEEP.store(true, Ordering::Release);
    wake_sleep_queue_after_waiter_enqueued(sleeper_id);

    assert!(
        !api::ax_wait_queue_wait_until(
            &DONE_WQ,
            || DONE.load(Ordering::Acquire),
            Some(REMOTE_WAKE_PROGRESS_TIMEOUT),
        ),
        "remote wait-queue wakeup did not make bounded progress"
    );
    sleeper.finish();
    occupier.stop();
    assert_eq!(
        task_test_hooks::take_park_irq_owner_entries(),
        Some(task_test_hooks::ParkIrqOwnerEntries {
            thread_sched: 0,
            run_queue: 0,
        }),
        "one scheduler-frame park transaction must reuse the runtime IRQ baton"
    );
    assert_eq!(
        task_test_hooks::take_switch_tail_irq_owner_entries(),
        Some(task_test_hooks::SwitchTailIrqOwnerEntries {
            thread_sched: 0,
            run_queue: 0,
        }),
        "one scheduler-frame switch tail must reuse the runtime IRQ baton"
    );
    assert_eq!(
        task_test_hooks::take_wake_irq_owner_entries(),
        Some(task_test_hooks::WakeIrqOwnerEntries {
            thread_sched: 1,
            run_queue: 0,
        }),
        "one Linux-style task-sched/rq wake transaction must own one runtime IRQ guard"
    );
    assert_eq!(
        task_test_hooks::take_wake_entity_read_events(),
        Some(task_test_hooks::WakeEntityReadEvents {
            reads: 3,
            copies: 0,
        }),
        "wake placement and preemption must borrow both rq-owned scheduling entities"
    );
    task_test_hooks::arm_park_deadline_publication_probe(this_cpu_id());
    task_test_hooks::request_current_owner_work()
        .expect("the deadline probe must tolerate unrelated pending owner work");
    schedule_current_cpu().expect("the unrelated owner pass must complete before timed park");
    task_test_hooks::arm_deadline_soft_expiry_probe(this_cpu_id());
    assert!(api::ax_wait_queue_wait_until(
        &TIMEOUT_WQ,
        || false,
        Some(Duration::from_millis(1)),
    ));
    assert_eq!(
        task_test_hooks::take_deadline_publication_entries(),
        Some(task_test_hooks::DeadlinePublicationEntries {
            observation: 0,
            rt_period_observation: 0,
            registration: 1,
            publication: 0,
        }),
        "one timed park must register and publish through one deadline-base transaction"
    );
    assert_eq!(
        task_test_hooks::take_deadline_soft_expiry_entries(),
        Some(1),
        "one clockevent must expire task and kernel timers under one deadline-base guard"
    );
    Ok(())
}

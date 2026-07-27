//! AxVM-owned per-CPU VM timer worker.

extern crate alloc;

use alloc::{boxed::Box, collections::VecDeque, format};
use core::{
    pin::Pin,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_kernel_guard::NoPreempt;
use ax_lazyinit::LazyInit;
use ax_std::os::arceos::modules::{
    ax_hal,
    ax_runtime::task::{
        IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, ThreadWakeHandle, WaitQueue,
        current_thread_handle, yield_current_cpu,
    },
};
use ax_sync::PiMutex;
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

use crate::host::{
    HostTime,
    arceos::{spawn_task_with_extension_and_affinity, task_cpu_set_one},
    default_host,
};

const COMMAND_CAPACITY: usize = 256;
const WORK_BUDGET: usize = 64;
const WORKER_STACK_SIZE: usize = 0x40000;

static TOKEN: AtomicUsize = AtomicUsize::new(0);

struct VmTimerEvent {
    token: usize,
    callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmTimerEvent {
    fn new(token: usize, callback: Box<dyn FnOnce(TimeValue) + Send + 'static>) -> Self {
        Self { token, callback }
    }
}

impl TimerEvent for VmTimerEvent {
    fn callback(self, now: TimeValue) {
        (self.callback)(now);
    }
}

enum VmTimerCommand {
    Arm {
        deadline: TimeValue,
        event: VmTimerEvent,
    },
    Cancel {
        token: usize,
    },
}

fn try_push_command(
    commands: &mut VecDeque<VmTimerCommand>,
    capacity: usize,
    command: VmTimerCommand,
) -> Result<(), VmTimerCommand> {
    if commands.len() == capacity {
        return Err(command);
    }
    commands.push_back(command);
    Ok(())
}

struct VmTimerState {
    commands: PiMutex<VecDeque<VmTimerCommand>>,
    command_capacity: usize,
    command_count: AtomicUsize,
    command_space: WaitQueue,
    publication_generation: AtomicU64,
    wake: IrqWaitCell,
}

impl VmTimerState {
    fn new() -> Self {
        let commands = VecDeque::with_capacity(COMMAND_CAPACITY);
        let command_capacity = commands.capacity();
        Self {
            commands: PiMutex::new(commands),
            command_capacity,
            command_count: AtomicUsize::new(0),
            command_space: WaitQueue::new(),
            publication_generation: AtomicU64::new(0),
            wake: IrqWaitCell::new(),
        }
    }

    fn enqueue(&self, command: VmTimerCommand) -> Result<(), VmTimerCommand> {
        let mut commands = self.commands.lock();
        try_push_command(&mut commands, self.command_capacity, command)?;
        self.command_count.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn publish(&self, mut command: VmTimerCommand) {
        assert!(
            !ax_hal::irq::in_irq_context(),
            "VM timer commands must be published from task context"
        );
        loop {
            match self.enqueue(command) {
                Ok(()) => {
                    self.signal();
                    return;
                }
                Err(returned) => command = returned,
            }

            // Ensure the CPU-affine consumer is runnable before sleeping for
            // capacity. The atomic predicate closes a pop-before-park race
            // without taking the sleepable queue lock with IRQs disabled.
            self.signal();
            self.command_space
                .wait_until(|| self.command_count.load(Ordering::Acquire) < self.command_capacity);
        }
    }

    fn pop_command(&self) -> Option<VmTimerCommand> {
        let command = self.commands.lock().pop_front();
        if command.is_some() {
            self.command_count.fetch_sub(1, Ordering::Release);
            self.command_space.notify_one();
        }
        command
    }

    fn has_commands(&self) -> bool {
        self.command_count.load(Ordering::Acquire) != 0
    }

    fn signal(&self) {
        // Equality, rather than ordering, closes the worker's register/sleep
        // window. Wrapping is therefore safe and, unlike an exhaustion assert,
        // keeps this hard-IRQ path panic-free.
        self.publication_generation.fetch_add(1, Ordering::Release);
        let _ = self.wake.notify();
    }
}

struct VmTimerWaiter {
    _wake_owner: ThreadWakeHandle,
    registration: IrqWaitRegistration,
}

#[ax_percpu::def_percpu]
static TIMER_STATE: LazyInit<&'static VmTimerState> = LazyInit::new();

pub(crate) fn register_timer(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    let token = TOKEN
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
            token.checked_add(1)
        })
        .expect("VM timer token space exhausted");
    current_timer_state().publish(VmTimerCommand::Arm {
        deadline: TimeValue::from_nanos(deadline_ns),
        event: VmTimerEvent::new(token, callback),
    });
    token
}

pub(crate) fn cancel_timer(token: usize) {
    current_timer_state().publish(VmTimerCommand::Cancel { token });
}

/// Publishes a bounded wake for the current CPU's timer worker.
///
/// This is safe from hard IRQ: it performs only atomics and at most one direct
/// scheduler wake. VM callbacks and timer-list mutation remain in task context.
pub(crate) fn check_events() {
    current_timer_state().signal();
}

pub(crate) fn init_percpu() {
    info!("Initializing AxVM timer worker...");
    let state: &'static VmTimerState = Box::leak(Box::new(VmTimerState::new()));
    with_current_timer_state_slot(|slot| {
        slot.init_once(state);
    });

    let cpu_id = ax_hal::percpu::this_cpu_id();
    let affinity = task_cpu_set_one(cpu_id);
    let name = format!("axvm-timer/{cpu_id}");
    let _worker = unsafe {
        // SAFETY: no OS extension is transferred, and the affinity is installed
        // before publication so this per-CPU worker cannot run on another CPU.
        spawn_task_with_extension_and_affinity(
            move || timer_worker(state),
            name,
            WORKER_STACK_SIZE,
            None,
            Some(affinity),
        )
    }
    .unwrap_or_else(|error| panic!("failed to spawn AxVM timer worker on CPU {cpu_id}: {error}"));
}

fn timer_worker(state: &'static VmTimerState) -> ! {
    let wake_owner = current_thread_handle()
        .unwrap_or_else(|error| panic!("AxVM timer worker lacks a scheduler thread: {error}"))
        .wake_handle();
    let irq_wake = unsafe {
        // SAFETY: the leaked waiter below owns this wake handle for the
        // shutdown lifetime and is unregistered before every reuse.
        wake_owner.irq_wake_handle()
    };
    let waiter = Box::leak(Box::new(VmTimerWaiter {
        _wake_owner: wake_owner,
        registration: IrqWaitRegistration::new(irq_wake),
    }));
    let registration = unsafe {
        // SAFETY: the leaked allocation is stable, and this sole worker
        // serializes every registration operation.
        Pin::new_unchecked(&waiter.registration)
    };
    let wait_queue = WaitQueue::new();
    let mut timers = TimerList::new();

    loop {
        if service_timer_work(state, &mut timers) == WORK_BUDGET {
            yield_current_cpu().expect("AxVM timer worker yield failed");
            continue;
        }
        wait_for_timer_work(state, &wait_queue, registration, timers.next_deadline());
    }
}

fn service_timer_work(state: &VmTimerState, timers: &mut TimerList<VmTimerEvent>) -> usize {
    let mut processed = 0;
    while processed < WORK_BUDGET {
        if let Some(command) = state.pop_command() {
            match command {
                VmTimerCommand::Arm { deadline, event } => timers.set(deadline, event),
                VmTimerCommand::Cancel { token } => {
                    timers.cancel(|event| event.token == token);
                }
            }
            processed += 1;
            continue;
        }

        let now = default_host().monotonic_time();
        let Some((deadline, event)) = timers.expire_one(now) else {
            break;
        };
        trace!("handle VM timer event scheduled at {deadline:#?}");
        event.callback(now);
        processed += 1;
    }
    processed
}

fn wait_for_timer_work(
    state: &'static VmTimerState,
    wait_queue: &WaitQueue,
    registration: Pin<&'static IrqWaitRegistration>,
    next_deadline: Option<TimeValue>,
) {
    if state.has_commands() {
        return;
    }
    let now = default_host().monotonic_time();
    if next_deadline.is_some_and(|deadline| deadline <= now) {
        return;
    }

    let observed = state.publication_generation.load(Ordering::Acquire);
    match state.wake.register(registration) {
        IrqRegisterResult::Occupied => panic!("AxVM timer worker registration is already attached"),
        IrqRegisterResult::ConsumedPending => return,
        IrqRegisterResult::Registered | IrqRegisterResult::NotificationInFlight => {}
    }

    if state.publication_generation.load(Ordering::Acquire) != observed || state.has_commands() {
        let _ = state.wake.unregister(registration);
        return;
    }

    match next_deadline {
        Some(deadline) => {
            let _timed_out = wait_queue.wait_until_deadline(deadline, || {
                !registration.is_attached()
                    || state.publication_generation.load(Ordering::Acquire) != observed
            });
        }
        None => wait_queue.wait_until(|| {
            !registration.is_attached()
                || state.publication_generation.load(Ordering::Acquire) != observed
        }),
    }
    let _ = state.wake.unregister(registration);
}

fn current_timer_state() -> &'static VmTimerState {
    with_current_timer_state_slot(|slot| {
        *slot
            .get()
            .expect("AxVM timer worker is not initialized on this CPU")
    })
}

fn with_current_timer_state_slot<R>(
    operation: impl FnOnce(&LazyInit<&'static VmTimerState>) -> R,
) -> R {
    let _guard = NoPreempt::new();
    // SAFETY: the guard prevents migration through the non-escaping borrow.
    unsafe { ax_percpu::with_cpu_pin(|pin| TIMER_STATE.with_current(pin, operation)) }
        .expect("AxVM timer access requires an installed CPU area")
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::collections::VecDeque;
    use core::sync::atomic::Ordering;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::{VmTimerCommand, VmTimerState, try_push_command};

    #[test]
    fn irq_signal_generation_wraps_without_panicking() {
        let state = VmTimerState::new();
        state
            .publication_generation
            .store(u64::MAX, Ordering::Relaxed);

        state.signal();

        assert_eq!(state.publication_generation.load(Ordering::Acquire), 0);
        assert!(state.wake.is_pending());
    }

    #[test]
    fn full_command_channel_applies_backpressure_instead_of_panicking() {
        let mut commands = VecDeque::with_capacity(2);
        let capacity = commands.capacity();
        for token in 0..capacity {
            assert!(
                try_push_command(&mut commands, capacity, VmTimerCommand::Cancel { token }).is_ok()
            );
        }

        let overflow = catch_unwind(AssertUnwindSafe(|| {
            try_push_command(
                &mut commands,
                capacity,
                VmTimerCommand::Cancel { token: capacity },
            )
        }));

        assert!(
            overflow.is_ok(),
            "a full task-context channel must not panic the vCPU thread"
        );
        assert!(overflow.unwrap().is_err());
        assert_eq!(commands.len(), capacity);
    }
}

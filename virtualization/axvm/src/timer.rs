//! AxVM-owned per-CPU VM timer worker.

extern crate alloc;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque, btree_map::Entry},
    format,
};
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_kernel_guard::NoPreempt;
use ax_lazyinit::LazyInit;
use ax_std::os::arceos::modules::{
    ax_hal,
    ax_runtime::task::{
        IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, WaitQueue, current_thread_handle,
        quiesce_irq_wait, yield_current_cpu,
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
static TIMER_ROUTES: LazyInit<VmTimerRoutes> = LazyInit::new();

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
        timer_routes().retire(self.token);
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

struct VmTimerRoutes {
    owners: PiMutex<BTreeMap<usize, &'static VmTimerState>>,
}

impl VmTimerRoutes {
    fn new() -> Self {
        Self {
            owners: PiMutex::new(BTreeMap::new()),
        }
    }

    fn register(&self, token: usize, owner: &'static VmTimerState) {
        let inserted = insert_timer_route(&mut self.owners.lock(), token, owner);
        assert!(inserted, "VM timer token must have exactly one owner");
    }

    fn take(&self, token: usize) -> Option<&'static VmTimerState> {
        take_timer_route(&mut self.owners.lock(), token)
    }

    fn retire(&self, token: usize) {
        let _ = self.take(token);
    }
}

fn insert_timer_route(
    routes: &mut BTreeMap<usize, &'static VmTimerState>,
    token: usize,
    owner: &'static VmTimerState,
) -> bool {
    match routes.entry(token) {
        Entry::Vacant(entry) => {
            entry.insert(owner);
            true
        }
        Entry::Occupied(_) => false,
    }
}

fn take_timer_route(
    routes: &mut BTreeMap<usize, &'static VmTimerState>,
    token: usize,
) -> Option<&'static VmTimerState> {
    routes.remove(&token)
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
    registration: IrqWaitRegistration,
}

#[ax_percpu::def_percpu]
static TIMER_STATE: LazyInit<&'static VmTimerState> = LazyInit::new();

pub(crate) fn register_timer(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    assert_timer_task_context();
    let token = TOKEN
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
            token.checked_add(1)
        })
        .expect("VM timer token space exhausted");
    let owner = current_timer_state();
    timer_routes().register(token, owner);
    owner.publish(VmTimerCommand::Arm {
        deadline: TimeValue::from_nanos(deadline_ns),
        event: VmTimerEvent::new(token, callback),
    });
    token
}

pub(crate) fn cancel_timer(token: usize) {
    assert_timer_task_context();
    if let Some(owner) = timer_routes().take(token) {
        owner.publish(VmTimerCommand::Cancel { token });
    }
}

fn assert_timer_task_context() {
    assert!(
        !ax_hal::irq::in_irq_context(),
        "VM timer commands must be published from task context"
    );
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
    let waiter = Box::leak(Box::new(VmTimerWaiter {
        registration: IrqWaitRegistration::new(wake_owner),
    }));
    let registration = &waiter.registration;
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
    registration: &IrqWaitRegistration,
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
    let token = match state.wake.register(registration) {
        IrqRegisterResult::Occupied => panic!("AxVM timer worker registration is already attached"),
        IrqRegisterResult::ConsumedPending => return,
        IrqRegisterResult::Registered(token) | IrqRegisterResult::NotificationInFlight(token) => {
            token
        }
    };

    if state.publication_generation.load(Ordering::Acquire) != observed || state.has_commands() {
        quiesce_irq_wait(&state.wake, token)
            .unwrap_or_else(|error| panic!("AxVM timer waiter could not quiesce: {error}"));
        return;
    }

    match next_deadline {
        Some(deadline) => {
            let _timed_out = wait_queue.wait_until_deadline(deadline, || {
                !token.is_attached()
                    || state.publication_generation.load(Ordering::Acquire) != observed
            });
        }
        None => wait_queue.wait_until(|| {
            !token.is_attached() || state.publication_generation.load(Ordering::Acquire) != observed
        }),
    }
    quiesce_irq_wait(&state.wake, token)
        .unwrap_or_else(|error| panic!("AxVM timer waiter could not quiesce: {error}"));
}

fn current_timer_state() -> &'static VmTimerState {
    with_current_timer_state_slot(|slot| {
        *slot
            .get()
            .expect("AxVM timer worker is not initialized on this CPU")
    })
}

fn timer_routes() -> &'static VmTimerRoutes {
    TIMER_ROUTES.get_or_init(VmTimerRoutes::new)
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

    use alloc::{
        boxed::Box,
        collections::{BTreeMap, VecDeque},
    };
    use core::sync::atomic::Ordering;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::{
        VmTimerCommand, VmTimerState, insert_timer_route, take_timer_route, try_push_command,
    };

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

    #[test]
    fn cancellation_routes_to_the_registration_worker() {
        let registration_worker = Box::leak(Box::new(VmTimerState::new()));
        let unrelated_worker = Box::leak(Box::new(VmTimerState::new()));
        let mut routes = BTreeMap::new();
        assert!(insert_timer_route(&mut routes, 7, registration_worker));
        assert!(!insert_timer_route(&mut routes, 7, unrelated_worker));

        let owner =
            take_timer_route(&mut routes, 7).expect("registered token must retain its owner");
        assert!(core::ptr::eq(owner, registration_worker));
        assert!(!core::ptr::eq(owner, unrelated_worker));
        assert!(take_timer_route(&mut routes, 7).is_none());
    }
}

//! AxVM-owned per-CPU VM timer wheel.

extern crate alloc;

use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_kernel_guard::NoPreempt;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

use crate::host::{HostTime, default_host};

static TOKEN: AtomicUsize = AtomicUsize::new(0);

struct VmTimerEvent {
    token: usize,
    callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmTimerEvent {
    fn new<F>(token: usize, callback: F) -> Self
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        Self {
            token,
            callback: Box::new(callback),
        }
    }
}

impl TimerEvent for VmTimerEvent {
    fn callback(self, now: TimeValue) {
        (self.callback)(now);
    }
}

#[ax_percpu::def_percpu]
static TIMER_LIST: LazyInit<SpinNoIrq<TimerList<VmTimerEvent>>> = LazyInit::new();

pub(crate) fn register_timer(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    let token = TOKEN.fetch_add(1, Ordering::Relaxed);
    let next_deadline = with_current_timer_list(|timer_list| {
        let mut timers = timer_list.lock();
        timers.set(
            TimeValue::from_nanos(deadline_ns),
            VmTimerEvent::new(token, callback),
        );
        timers.next_deadline()
    });
    rearm_host_timer(next_deadline);
    token
}

pub(crate) fn cancel_timer(token: usize) {
    let next_deadline = with_current_timer_list(|timer_list| {
        let mut timers = timer_list.lock();
        timers.cancel(|event| event.token == token);
        timers.next_deadline()
    });
    rearm_host_timer(next_deadline);
}

pub(crate) fn check_events() {
    with_current_timer_list(|timer_list| {
        loop {
            let now = default_host().monotonic_time();
            let expired = timer_list.lock().expire_one(now);
            if let Some((deadline, event)) = expired {
                trace!("handle VM timer event scheduled at {deadline:#?}");
                event.callback(now);
            } else {
                rearm_host_timer(timer_list.lock().next_deadline());
                break;
            }
        }
    });
}

fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    if let Some(deadline) = next_deadline {
        default_host().set_oneshot_timer(deadline.as_nanos() as u64);
    }
}

pub(crate) fn init_percpu() {
    info!("Initializing AxVM timer wheel...");
    with_current_timer_list(|timer_list| {
        timer_list.init_once(SpinNoIrq::new(TimerList::new()));
    });
    crate::arch::register_timer_callback();
}

fn with_current_timer_list<R>(
    operation: impl FnOnce(&LazyInit<SpinNoIrq<TimerList<VmTimerEvent>>>) -> R,
) -> R {
    let _guard = NoPreempt::new();
    // SAFETY: the guard prevents migration through the non-escaping borrow.
    unsafe { ax_percpu::with_cpu_pin(|pin| TIMER_LIST.with_current(pin, operation)) }
        .expect("AxVM timer access requires an installed CPU area")
}

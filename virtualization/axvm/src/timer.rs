//! AxVM-owned per-CPU VM timer wheel.

extern crate alloc;

use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

use crate::host::{HostTime, arceos::arceos_host};

static TOKEN: AtomicUsize = AtomicUsize::new(0);

struct VmTimerEvent {
    #[cfg(target_arch = "x86_64")]
    token: usize,
    callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmTimerEvent {
    fn new<F>(token: usize, callback: F) -> Self
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        #[cfg(not(target_arch = "x86_64"))]
        let _ = token;
        Self {
            #[cfg(target_arch = "x86_64")]
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
    let next_deadline = {
        // SAFETY: The timer list is initialized for each CPU before vCPU tasks
        // are spawned and VM timer callbacks are registered.
        let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
        let mut timers = timer_list.lock();
        timers.set(
            TimeValue::from_nanos(deadline_ns),
            VmTimerEvent::new(token, callback),
        );
        timers.next_deadline()
    };
    rearm_host_timer(next_deadline);
    token
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn cancel_timer(token: usize) {
    let next_deadline = {
        // SAFETY: The timer list is initialized for each CPU before VM timer
        // callbacks are registered or cancelled.
        let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
        let mut timers = timer_list.lock();
        timers.cancel(|event| event.token == token);
        timers.next_deadline()
    };
    rearm_host_timer(next_deadline);
}

pub(crate) fn check_events() {
    // SAFETY: Called from a vCPU task pinned to a CPU whose timer list was
    // initialized during AxVM host initialization.
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    loop {
        let now = arceos_host().monotonic_time();
        let expired = timer_list.lock().expire_one(now);
        if let Some((deadline, event)) = expired {
            trace!("handle VM timer event scheduled at {deadline:#?}");
            event.callback(now);
        } else {
            rearm_host_timer(timer_list.lock().next_deadline());
            break;
        }
    }
}

fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    if let Some(deadline) = next_deadline {
        arceos_host().set_oneshot_timer(deadline.as_nanos() as u64);
    }
}

pub(crate) fn init_percpu() {
    info!("Initializing AxVM timer wheel...");
    // SAFETY: Called once per CPU during hypervisor initialization before this
    // CPU can register VM timers.
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    timer_list.init_once(SpinNoIrq::new(TimerList::new()));
}

//! AxVM-owned per-CPU VM timer wheel.

extern crate alloc;

use alloc::boxed::Box;

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

use crate::host::{HostTime, default_host};

pub(crate) struct VmTimerEvent {
    pub(crate) token: usize,
    pub(crate) callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl TimerEvent for VmTimerEvent {
    fn callback(self, now: TimeValue) {
        let Self { token, callback } = self;
        trace!("run VM timer event token {token}");
        callback(now);
    }
}

#[ax_percpu::def_percpu]
static TIMER_LIST: LazyInit<SpinNoIrq<TimerList<VmTimerEvent>>> = LazyInit::new();

pub(crate) fn with_current_timer_list<R>(
    operation: impl FnOnce(&mut TimerList<VmTimerEvent>) -> R,
) -> R {
    // SAFETY: The timer list is initialized for each CPU before vCPU tasks
    // can inspect, register, or cancel VM timer events. The returned reference
    // remains protected by the per-CPU non-sleeping lock for this operation.
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    operation(&mut timer_list.lock())
}

pub(crate) fn check_events() {
    loop {
        let now = default_host().monotonic_time();
        let expired = with_current_timer_list(|timers| timers.expire_one(now));
        if let Some((deadline, event)) = expired {
            trace!("handle VM timer event scheduled at {deadline:#?}");
            event.callback(now);
        } else {
            rearm_host_timer(with_current_timer_list(|timers| timers.next_deadline()));
            break;
        }
    }
}

pub(crate) fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    if let Some(deadline) = next_deadline {
        default_host().set_oneshot_timer(deadline.as_nanos() as u64);
    }
}

pub(crate) fn init_percpu() {
    info!("Initializing AxVM timer wheel...");
    // SAFETY: Called once per CPU during hypervisor initialization before this
    // CPU can register VM timers.
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    timer_list.init_once(SpinNoIrq::new(TimerList::new()));
    crate::arch::register_timer_callback();
}

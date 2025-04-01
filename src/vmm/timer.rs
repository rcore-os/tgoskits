use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;

use std::os::arceos::modules::{axconfig, axhal};

use alloc::boxed::Box;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use timer_list::{TimeValue, TimerEvent, TimerList};

static TOKEN: AtomicUsize = AtomicUsize::new(0);
const PERIODIC_INTERVAL_NANOS: u64 = axhal::time::NANOS_PER_SEC / axconfig::TICKS_PER_SEC as u64;

/// Represents a timer event in the virtual machine monitor (VMM).
///
/// This struct holds a unique token for the timer and a callback function
/// that will be executed when the timer expires.
pub struct VmmTimerEvent {
    // Unique identifier for the timer event
    token: usize,
    // Callback function to be executed when the timer expires
    timer_callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmmTimerEvent {
    fn new<F>(token: usize, f: F) -> Self
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        Self {
            token: token,
            timer_callback: Box::new(f),
        }
    }
}

impl TimerEvent for VmmTimerEvent {
    fn callback(self, now: TimeValue) {
        (self.timer_callback)(now)
    }
}

#[percpu::def_percpu]
static TIMER_LIST: LazyInit<SpinNoIrq<TimerList<VmmTimerEvent>>> = LazyInit::new();

/// Registers a new timer that will execute at the specified deadline
///
/// # Arguments
/// - `deadline`: The absolute time in nanoseconds when the timer should trigger
/// - `handler`: The callback function to execute when the timer expires
///
/// # Returns
/// A unique token that can be used to cancel this timer later
pub fn register_timer<F>(deadline: u64, handler: F) -> usize
where
    F: FnOnce(TimeValue) + Send + 'static,
{
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    let mut timers = timer_list.lock();
    let token = TOKEN.fetch_add(1, Ordering::Release);
    let event = VmmTimerEvent::new(token, handler);
    timers.set(TimeValue::from_nanos(deadline as u64), event);
    token
}

/// Cancels a timer with the specified token.
///
/// # Parameters
/// - `token`: The unique token of the timer to cancel.
pub fn cancel_timer(token: usize) {
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    let mut timers = timer_list.lock();
    timers.cancel(|event| event.token == token);
}

/// Check and process any pending timer events
pub fn check_events() {
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    loop {
        let now = axhal::time::wall_time();
        let event = timer_list.lock().expire_one(now);
        if let Some((_deadline, event)) = event {
            trace!("pick one {:#?} to handler!!!", _deadline);
            event.callback(now);
        } else {
            break;
        }
    }
}

/// Schedule the next timer event based on the periodic interval
pub fn scheduler_next_event() {
    let now_ns = axhal::time::monotonic_time_nanos();
    let deadline = now_ns + PERIODIC_INTERVAL_NANOS;
    trace!("PHY deadline {} !!!", deadline);
    axhal::time::set_oneshot_timer(deadline);
}

/// Initialize the hypervisor timer system
pub fn init_percpu() {
    info!("Initing HV Timer...");
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    timer_list.init_once(SpinNoIrq::new(TimerList::new()));
}

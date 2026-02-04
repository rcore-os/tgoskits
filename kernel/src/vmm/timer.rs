// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;

use std::os::arceos::modules::axhal;

use alloc::boxed::Box;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use timer_list::{TimeValue, TimerEvent, TimerList};

static TOKEN: AtomicUsize = AtomicUsize::new(0);
// const PERIODIC_INTERVAL_NANOS: u64 = axhal::time::NANOS_PER_SEC / axconfig::TICKS_PER_SEC as u64;

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
            token,
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
    trace!("Registering timer...");
    trace!(
        "deadline is {:#?} = {:#?}",
        deadline,
        TimeValue::from_nanos(deadline)
    );
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    let mut timers = timer_list.lock();
    let token = TOKEN.fetch_add(1, Ordering::Release);
    let event = VmmTimerEvent::new(token, handler);
    timers.set(TimeValue::from_nanos(deadline), event);
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
    // info!("Checking timer events...");
    // info!("now is {:#?}", axhal::time::wall_time());
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    loop {
        let now = axhal::time::wall_time();
        let event = timer_list.lock().expire_one(now);
        if let Some((_deadline, event)) = event {
            trace!("pick one {_deadline:#?} to handle!!!");
            event.callback(now);
        } else {
            break;
        }
    }
}

// /// Schedule the next timer event based on the periodic interval
// pub fn scheduler_next_event() {
//     trace!("Scheduling next event...");
//     let now_ns = axhal::time::monotonic_time_nanos();
//     let deadline = now_ns + PERIODIC_INTERVAL_NANOS;
//     debug!("PHY deadline {} !!!", deadline);
//     axhal::time::set_oneshot_timer(deadline);
// }

/// Initialize the hypervisor timer system
pub fn init_percpu() {
    info!("Initing HV Timer...");
    let timer_list = unsafe { TIMER_LIST.current_ref_mut_raw() };
    timer_list.init_once(SpinNoIrq::new(TimerList::new()));
}

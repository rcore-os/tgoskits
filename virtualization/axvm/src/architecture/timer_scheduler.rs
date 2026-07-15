//! Tokenized VM timer scheduling for architectures with callback-backed timers.

use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_timer_list::TimeValue;

static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn register(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let next_deadline = crate::timer::with_current_timer_list(|timers| {
        timers.set(
            TimeValue::from_nanos(deadline_ns),
            crate::timer::VmTimerEvent { token, callback },
        );
        timers.next_deadline()
    });
    crate::timer::rearm_host_timer(next_deadline);
    token
}

pub(crate) fn cancel(token: usize) {
    let next_deadline = crate::timer::with_current_timer_list(|timers| {
        timers.cancel(|event| event.token == token);
        timers.next_deadline()
    });
    crate::timer::rearm_host_timer(next_deadline);
}

use alloc::{boxed::Box, vec::Vec};

use axhal::time::{TimeValue, wall_time};
use kernel_guard::NoPreemptIrqSave;

percpu_static! {
    TIMER_CALLBACKS: Vec<Box<dyn Fn(TimeValue) + Send + Sync>> = Vec::new(),
}

/// Registers a callback function to be called on each timer tick.
pub fn register_timer_callback<F>(callback: F)
where
    F: Fn(TimeValue) + Send + Sync + 'static,
{
    let _g = NoPreemptIrqSave::new();
    unsafe {
        TIMER_CALLBACKS
            .current_ref_mut_raw()
            .push(Box::new(callback))
    };
}

pub(crate) fn check_events() {
    for callback in unsafe { TIMER_CALLBACKS.current_ref_raw().iter() } {
        callback(wall_time());
    }
    crate::future::check_timer_events();
}

use axvisor_api::time::{Nanos, TimeIf, TimeValue};

struct TimeImpl;

#[axvisor_api::api_impl]
impl TimeIf for TimeImpl {
    fn current_time_nanos() -> Nanos {
        ax_hal::time::monotonic_time_nanos()
    }

    fn set_oneshot_timer(deadline: TimeValue) {
        ax_hal::time::set_oneshot_timer(deadline.as_nanos() as u64)
    }
}

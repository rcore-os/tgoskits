use axvisor_api::time::{CancelToken, Nanos, Ticks, TimeIf, TimeValue};

struct TimeImpl;

#[axvisor_api::api_impl]
impl TimeIf for TimeImpl {
    fn current_ticks() -> Ticks {
        crate::host::time::current_ticks()
    }

    fn ticks_to_nanos(ticks: Ticks) -> Nanos {
        crate::host::time::ticks_to_nanos(ticks)
    }

    fn nanos_to_ticks(nanos: Nanos) -> Ticks {
        crate::host::time::nanos_to_ticks(nanos)
    }

    fn register_timer(
        deadline: TimeValue,
        handler: alloc::boxed::Box<dyn FnOnce(TimeValue) + Send + 'static>,
    ) -> CancelToken {
        crate::host::timer::register_timer(deadline.as_nanos() as u64, handler)
    }

    fn cancel_timer(token: CancelToken) {
        crate::host::timer::cancel_timer(token)
    }
}

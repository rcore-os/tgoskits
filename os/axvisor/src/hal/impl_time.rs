use axvisor_api::time::{CancelToken, Nanos, Ticks, TimeIf, TimeValue};

use crate::vmm;

struct TimeImpl;

#[axvisor_api::api_impl]
impl TimeIf for TimeImpl {
    fn current_ticks() -> Ticks {
        ax_hal::time::current_ticks()
    }

    fn ticks_to_nanos(ticks: Ticks) -> Nanos {
        ax_hal::time::ticks_to_nanos(ticks)
    }

    fn nanos_to_ticks(nanos: Nanos) -> Ticks {
        ax_hal::time::nanos_to_ticks(nanos)
    }

    fn register_timer(
        deadline: TimeValue,
        handler: alloc::boxed::Box<dyn FnOnce(TimeValue) + Send + 'static>,
    ) -> CancelToken {
        vmm::timer::register_timer(deadline.as_nanos() as u64, handler)
    }

    fn cancel_timer(token: CancelToken) {
        vmm::timer::cancel_timer(token)
    }
}

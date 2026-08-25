//! Narrow publication capability from the logical timer owner to the runtime.

/// Publishes an earlier local deadline to the physical clockevent owner.
///
/// Implementations may conservatively leave an already-programmed later edge
/// pending, but must never move another CPU's comparator or execute timer
/// payloads from this call.
#[ax_crate_interface::def_interface]
pub trait ClockEventControl {
    /// Requests that the calling CPU observe `deadline_nanos`.
    ///
    /// The deadline is an absolute finite monotonic-clock value. This method is
    /// called only after the logical timer-base lock has been released.
    fn request_local_reprogram(deadline_nanos: u64);
}

pub(super) fn publish_earlier_deadline(deadline_nanos: u64) {
    #[cfg(not(any(test, feature = "host-test")))]
    ax_crate_interface::call_interface!(ClockEventControl::request_local_reprogram, deadline_nanos);
    #[cfg(any(test, feature = "host-test"))]
    let _ = deadline_nanos;
}

#[path = "../src/scheduler/hrtick.rs"]
mod hrtick;

use hrtick::finish_hrtick_delta_ns;

#[test]
fn saturated_irq_utilization_keeps_the_unscaled_deadline_like_linux() {
    assert_eq!(finish_hrtick_delta_ns(100_000, 1_024), 100_000);
}

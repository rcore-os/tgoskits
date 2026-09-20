//! Linux-compatible fair hrtick deadline finishing.

const SCHED_CAPACITY_SCALE: u64 = 1_024;
const MIN_HRTICK_DELTA_NS: u64 = 10_000;

pub(crate) fn finish_hrtick_delta_ns(delta_ns: u64, irq_util_avg: u32) -> u64 {
    // Linux `hrtick_start_fair()` corrects the deadline only while IRQ
    // utilization is strictly between zero and full capacity. A saturated
    // sample keeps the default scale instead of dividing by `1024 - util`.
    let scaled_delta = if irq_util_avg != 0 && irq_util_avg < SCHED_CAPACITY_SCALE as u32 {
        let scale = u128::from(SCHED_CAPACITY_SCALE) * u128::from(SCHED_CAPACITY_SCALE)
            / u128::from(SCHED_CAPACITY_SCALE - u64::from(irq_util_avg));
        (u128::from(delta_ns) * scale / u128::from(SCHED_CAPACITY_SCALE)).min(u128::from(u64::MAX))
            as u64
    } else {
        delta_ns
    };
    scaled_delta.max(MIN_HRTICK_DELTA_NS)
}

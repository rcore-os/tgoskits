//! Runqueue-owned IRQ PELT state.

const LOAD_AVG_PERIOD: u64 = 32;
const LOAD_AVG_MAX: u64 = 47_742;
const PELT_MIN_DIVIDER: u64 = LOAD_AVG_MAX - 1_024;
const PELT_SEGMENT_UNITS: u64 = 1_024;
const PELT_TIME_SHIFT: u32 = 10;
pub(super) const SCHED_CAPACITY_SCALE: u32 = 1_024;

// Linux v7.1 `kernel/sched/sched-pelt.h`. The table represents y^n in
// 32-bit fixed point, where y^32 is approximately one half.
const RUNNABLE_AVG_YN_INV: [u32; 32] = [
    0xffff_ffff,
    0xfa83_b2da,
    0xf525_7d14,
    0xefe4_b99a,
    0xeac0_c6e6,
    0xe5b9_06e6,
    0xe0cc_deeb,
    0xdbfb_b796,
    0xd744_fcc9,
    0xd2a8_1d91,
    0xce24_8c14,
    0xc9b9_bd85,
    0xc567_2a10,
    0xc12c_4cc9,
    0xbd08_a39e,
    0xb8fb_af46,
    0xb504_f333,
    0xb123_f581,
    0xad58_3ee9,
    0xa9a1_5ab4,
    0xa5fe_d6a9,
    0xa270_4302,
    0x9ef5_325f,
    0x9b8d_39b9,
    0x9837_f050,
    0x94f4_efa8,
    0x91c3_d373,
    0x8ea4_398a,
    0x8b95_c1e3,
    0x8898_0e80,
    0x85aa_c367,
    0x82cd_8698,
];

#[derive(Debug)]
pub(super) struct IrqPelt {
    last_update_time_ns: u64,
    period_contrib: u32,
    util_sum: u64,
    util_avg: u32,
}

impl IrqPelt {
    pub(super) const fn new() -> Self {
        Self {
            last_update_time_ns: 0,
            period_contrib: 0,
            util_sum: 0,
            util_avg: 0,
        }
    }

    pub(super) const fn util_avg(&self) -> u32 {
        self.util_avg
    }

    /// Applies Linux v7.1 `update_irq_load_avg()` to one owner-rq sample.
    pub(super) fn update(
        &mut self,
        now_ns: u64,
        irq_runtime_ns: u64,
        frequency_capacity: u32,
        cpu_capacity: u32,
    ) {
        debug_assert!(frequency_capacity <= SCHED_CAPACITY_SCALE);
        debug_assert!(cpu_capacity <= SCHED_CAPACITY_SCALE);
        let irq_runtime_ns = cap_scale(irq_runtime_ns, frequency_capacity);
        let irq_runtime_ns = cap_scale(irq_runtime_ns, cpu_capacity);
        let periods = self.update_load_sum(now_ns.wrapping_sub(irq_runtime_ns), false)
            + self.update_load_sum(now_ns, true);
        if periods != 0 {
            let divider = PELT_MIN_DIVIDER + u64::from(self.period_contrib);
            self.util_avg = (self.util_sum / divider) as u32;
        }
    }

    fn update_load_sum(&mut self, now_ns: u64, running: bool) -> u64 {
        let elapsed_ns = now_ns.wrapping_sub(self.last_update_time_ns);
        if (elapsed_ns as i64) < 0 {
            self.last_update_time_ns = now_ns;
            return 0;
        }
        let elapsed_units = elapsed_ns >> PELT_TIME_SHIFT;
        if elapsed_units == 0 {
            return 0;
        }
        self.last_update_time_ns = self
            .last_update_time_ns
            .wrapping_add(elapsed_units << PELT_TIME_SHIFT);
        self.accumulate_sum(elapsed_units, running)
    }

    fn accumulate_sum(&mut self, elapsed_units: u64, running: bool) -> u64 {
        let previous_period_contrib = self.period_contrib;
        let mut contribution = elapsed_units as u32;
        let total = elapsed_units + u64::from(previous_period_contrib);
        let periods = total / PELT_SEGMENT_UNITS;
        let remainder = total % PELT_SEGMENT_UNITS;

        if periods != 0 {
            self.util_sum = decay_load(self.util_sum, periods);
            if running {
                contribution = accumulate_segments(
                    periods,
                    PELT_SEGMENT_UNITS as u32 - previous_period_contrib,
                    remainder as u32,
                );
            }
        }
        self.period_contrib = remainder as u32;
        if running {
            self.util_sum = self
                .util_sum
                .saturating_add(u64::from(contribution) << PELT_TIME_SHIFT);
        }
        periods
    }
}

fn cap_scale(value: u64, scale: u32) -> u64 {
    ((u128::from(value) * u128::from(scale)) >> PELT_TIME_SHIFT).min(u128::from(u64::MAX)) as u64
}

fn decay_load(mut value: u64, periods: u64) -> u64 {
    if periods > LOAD_AVG_PERIOD * 63 {
        return 0;
    }
    let mut local_periods = periods as usize;
    if local_periods >= LOAD_AVG_PERIOD as usize {
        value >>= local_periods / LOAD_AVG_PERIOD as usize;
        local_periods %= LOAD_AVG_PERIOD as usize;
    }
    ((u128::from(value) * u128::from(RUNNABLE_AVG_YN_INV[local_periods])) >> 32) as u64
}

fn accumulate_segments(periods: u64, previous_remainder: u32, current_remainder: u32) -> u32 {
    let previous = decay_load(u64::from(previous_remainder), periods) as u32;
    let full_periods = LOAD_AVG_MAX
        .saturating_sub(decay_load(LOAD_AVG_MAX, periods))
        .saturating_sub(PELT_SEGMENT_UNITS) as u32;
    previous
        .saturating_add(full_periods)
        .saturating_add(current_remainder)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MILLIS_IN_PELT_NS: u64 = 1 << 20;

    #[test]
    fn irq_pelt_matches_linux_v7_1_reference_sequence() {
        let mut pelt = IrqPelt::new();

        pelt.update(MILLIS_IN_PELT_NS, 0, 1_024, 1_024);
        assert_eq!(pelt.util_avg(), 0);
        pelt.update(2 * MILLIS_IN_PELT_NS, 0, 1_024, 1_024);
        assert_eq!(pelt.util_avg(), 0);
        pelt.update(3 * MILLIS_IN_PELT_NS, 512 << 10, 1_024, 1_024);
        assert_eq!(
            (pelt.period_contrib, pelt.util_sum, pelt.util_avg()),
            (0, 513_024, 10)
        );
        pelt.update(4 * MILLIS_IN_PELT_NS, 0, 1_024, 1_024);
        assert_eq!(
            (pelt.period_contrib, pelt.util_sum, pelt.util_avg()),
            (0, 502_030, 10)
        );
        pelt.update(10 * MILLIS_IN_PELT_NS, MILLIS_IN_PELT_NS, 1_024, 1_024);
        assert_eq!(
            (pelt.period_contrib, pelt.util_sum, pelt.util_avg()),
            (0, 1_466_892, 31)
        );
    }

    #[test]
    fn irq_pelt_applies_frequency_then_cpu_capacity_scaling() {
        let mut pelt = IrqPelt::new();

        pelt.update(MILLIS_IN_PELT_NS, MILLIS_IN_PELT_NS, 512, 512);

        assert_eq!(pelt.util_sum, 256_000);
        assert_eq!(pelt.util_avg(), 5);
    }

    #[test]
    fn sustained_full_irq_load_converges_like_linux() {
        let mut pelt = IrqPelt::new();

        for period in 1..=300 {
            pelt.update(period * MILLIS_IN_PELT_NS, MILLIS_IN_PELT_NS, 1_024, 1_024);
            if period == 32 {
                assert_eq!(pelt.util_avg(), 512);
            }
            if period == 64 {
                assert_eq!(pelt.util_avg(), 768);
            }
        }
        assert_eq!(pelt.util_avg(), 1_023);
    }
}

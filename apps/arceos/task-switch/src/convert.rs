//! Pure counter and time conversions for the task-switch benchmark.
//!
//! The benchmark reports latencies and a CPU frequency derived from raw System
//! Counter ticks (`CNTVCT_EL0`) and PMU cycle counts (`PMCCNTR_EL0`). Keeping
//! the arithmetic here, with the counter frequency passed in as an explicit
//! argument, lets the conversions be verified through `cargo xtask cross-test`
//! without reading hardware registers.
//!
//! Every entry point reports `None` for an input it cannot represent instead of
//! panicking or wrapping, so a zero frequency or a result outside `u64` cannot
//! be mistaken for a valid measurement.

/// Nanoseconds in one second, the numerator shared by every time conversion.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Converts a System Counter tick count to nanoseconds using `freq_hz`.
///
/// `freq_hz` is the counter frequency in Hz (`CNTFRQ_EL0` on the board, 24 MHz
/// on RK3588). This is the single production time conversion: `main` and
/// `Bencher::show` both route their tick counts through it. The product is
/// formed in `u128` before the division so the scale fraction is kept and
/// tick counts whose intermediate product exceeds `u64` are still converted.
/// A zero frequency and a result that does not fit `u64` are rejected.
pub fn ticks_to_nanos(ticks: u64, freq_hz: u64) -> Option<u64> {
    if freq_hz == 0 {
        return None;
    }
    let nanos = u128::from(ticks) * u128::from(NANOS_PER_SECOND) / u128::from(freq_hz);
    u64::try_from(nanos).ok()
}

/// Converts a CPU cycle count to Hz using the matching System Counter span.
///
/// `cpu_cycle` is the PMU cycle count over the interval, `timer_sum` the System
/// Counter ticks over the same interval, and `timer_freq_hz` the counter
/// frequency. The result is `cpu_cycle * timer_freq_hz / timer_sum`, formed in
/// `u128` before the division so the sub-tick ratio is kept. A zero tick span
/// and a result that does not fit `u64` are rejected.
pub fn cpu_freq_hz(cpu_cycle: u64, timer_sum: u64, timer_freq_hz: u64) -> Option<u64> {
    if timer_sum == 0 {
        return None;
    }
    let freq = u128::from(cpu_cycle) * u128::from(timer_freq_hz) / u128::from(timer_sum);
    u64::try_from(freq).ok()
}

/// Rounds `n / d` to the nearest integer.
///
/// Returns `None` for `d == 0`. The intermediate sum is computed in `u128`, so
/// `n + d / 2` cannot overflow `u64` for inputs close to `u64::MAX`.
pub fn div_round(n: u64, d: u64) -> Option<u64> {
    if d == 0 {
        return None;
    }
    let rounded = (u128::from(n) + u128::from(d) / 2) / u128::from(d);
    u64::try_from(rounded).ok()
}

#[cfg(test)]
mod tests {
    use super::{cpu_freq_hz, div_round, ticks_to_nanos};

    /// RK3588 System Counter frequency, the value `CNTFRQ_EL0` reports.
    const RK3588_COUNTER_HZ: u64 = 24_000_000;

    #[test]
    fn time_conversion_rule() {
        // One second at 24 MHz is exactly one billion nanoseconds; the scale
        // must be multiplied first instead of truncating 1e9 / 24e6 to 41.
        assert_eq!(
            ticks_to_nanos(RK3588_COUNTER_HZ, RK3588_COUNTER_HZ),
            Some(1_000_000_000)
        );
        // 7,000,000 ticks at 24 MHz is 7e6 * 1e9 / 24e6 = 291,666,666.67 ns,
        // so the sub-tick fraction must survive the conversion.
        assert_eq!(
            ticks_to_nanos(7_000_000, RK3588_COUNTER_HZ),
            Some(291_666_666)
        );
        // 1e11 ticks needs a 1e20 intermediate product, far above u64::MAX, and
        // reports 4,166,666,666,666 ns.
        assert_eq!(
            ticks_to_nanos(100_000_000_000, RK3588_COUNTER_HZ),
            Some(4_166_666_666_666)
        );
        // A zero frequency has no time scale.
        assert_eq!(ticks_to_nanos(1_000, 0), None);
        // A 1 Hz counter makes u64::MAX ticks far more than u64::MAX ns.
        assert_eq!(ticks_to_nanos(u64::MAX, 1), None);
    }

    #[test]
    fn frequency_conversion_rule() {
        // 1,000,000 cycles over 3,000,000 ticks at 24 MHz is 8,000,000 Hz; the
        // sub-tick ratio must not collapse to zero.
        assert_eq!(
            cpu_freq_hz(1_000_000, 3_000_000, RK3588_COUNTER_HZ),
            Some(8_000_000)
        );
        // A zero tick span has no frequency.
        assert_eq!(cpu_freq_hz(1_000, 0, RK3588_COUNTER_HZ), None);
        // u64::MAX cycles over a single tick at 24 MHz overflows u64.
        assert_eq!(cpu_freq_hz(u64::MAX, 1, RK3588_COUNTER_HZ), None);
    }

    #[test]
    fn rounding_rule() {
        // `(u64::MAX + 1) / 2` needs the full u64 range; the bare `n + d / 2`
        // sum used to overflow on this input.
        assert_eq!(div_round(u64::MAX, 2), Some(1 << 63));
        // A zero denominator has no rounded result.
        assert_eq!(div_round(1_000, 0), None);
    }
}

//! SG2002 capture constraints, following Linux v6.6 pcm_native/pcm_lib and
//! alsa-lib v1.2.14 pcm_params/interval semantics.
//!
//! Refinement projects feasible tuples onto interval envelopes: gaps between
//! rates, aligned periods, and correlated dimensions cannot be encoded on the
//! wire. Every call checks the complete request, treating rmask as a hint, and
//! HW_PARAMS selects an actual feasible tuple, never an arbitrary envelope value.
//! At most 2 * 128 * 15 tuples are visited, without allocation or a rule engine.

use alsa_pcm_uapi::{HwParams, Interval, Mask};
use ax_driver::audio::Config;

use crate::{StarryError, StarryResult};

const OPEN_MIN: u32 = 1;
const OPEN_MAX: u32 = 2;
const INTEGER: u32 = 4;
// ACCESS_RW_INTERLEAVED, FORMAT_S16_LE, SUBFORMAT_STD (Linux UAPI bit positions).
const MASK_BITS: [u32; 3] = [1 << 3, 1 << 2, 1];
const RATE: usize = 3;

/// Intersect every constraint with the supported capture configurations.
/// An empty intersection returns EINVAL and leaves the request untouched.
pub(super) fn refine(params: &mut HwParams) -> StarryResult<()> {
    let (envelope, _) = solve(params)?;
    publish(params, envelope);
    Ok(())
}

/// Select minimum rate, minimum period time, then maximum buffer size, as in
/// snd_pcm_hw_params_choose. Fixed unsupported values are never rounded.
/// This only negotiates parameters; hardware and session commitment belong to
/// the caller. Failure leaves the request untouched.
pub(super) fn configure(params: &mut HwParams) -> StarryResult<Config> {
    let (_, config) = solve(params)?;
    publish(params, intervals(config));
    Ok(config)
}

fn solve(params: &HwParams) -> StarryResult<([Interval; 12], Config)> {
    // NORESAMPLE is satisfied by the discrete hardware rates. Buffer export,
    // suppressed period wakeups, and other flags are outside this capture API.
    if params.flags & !1 != 0
        || params
            .masks
            .iter()
            .zip(MASK_BITS)
            .any(|(mask, bit)| mask.bits[0] & bit == 0)
        || params.intervals.iter().any(|i| {
            i.flags & !(OPEN_MIN | OPEN_MAX | INTEGER) != 0
                || i.min > i.max
                || (i.min == i.max && i.flags & (OPEN_MIN | OPEN_MAX) != 0)
        })
    {
        return Err(StarryError::InvalidInput);
    }

    let mut selected = None;
    let mut envelope = [point(0); 12];
    for config in Config::supported() {
        let values = intervals(config);
        if !params
            .intervals
            .iter()
            .zip(values)
            .all(|(request, value)| admits(*request, value))
        {
            continue;
        }
        // Enumeration order implements Linux's choice priority. The complete
        // tuple passed every constraint, including correlated sizes and times.
        if selected.is_none() {
            selected = Some(config);
            envelope = values;
        } else {
            for (bound, value) in envelope.iter_mut().zip(values) {
                *bound = hull(*bound, value);
            }
        }
    }
    Ok((envelope, selected.ok_or(StarryError::InvalidInput)?))
}

fn intervals(config: Config) -> [Interval; 12] {
    [
        point(16),                                          // SAMPLE_BITS
        point(16),                                          // FRAME_BITS
        point(1),                                           // CHANNELS
        point(config.rate),                                 // RATE
        time(config.period_frames, config.rate),            // PERIOD_TIME
        point(config.period_frames),                        // PERIOD_SIZE
        point(config.period_frames * 2),                    // PERIOD_BYTES
        point(config.buffer_frames / config.period_frames), // PERIODS
        time(config.buffer_frames, config.rate),            // BUFFER_TIME
        point(config.buffer_frames),                        // BUFFER_SIZE
        point(config.buffer_frames * 2),                    // BUFFER_BYTES
        point(0), // TICK_TIME: obsolete tick-driven capture is unsupported.
    ]
}

fn point(value: u32) -> Interval {
    Interval {
        min: value,
        max: value,
        flags: INTEGER,
    }
}

fn time(frames: u32, rate: u32) -> Interval {
    // The bounded hardware geometry keeps the quotient below u32::MAX.
    let numerator = u64::from(frames) * 1_000_000;
    let min = (numerator / u64::from(rate)) as u32;
    if numerator.is_multiple_of(u64::from(rate)) {
        point(min)
    } else {
        // snd_interval_mulkdiv and alsa-lib's directional near/set operations
        // represent fractional microseconds by (floor, ceil), not a rounded int.
        Interval {
            min,
            max: min + 1,
            flags: OPEN_MIN | OPEN_MAX,
        }
    }
}

fn admits(request: Interval, value: Interval) -> bool {
    // value is the smallest wire interval enclosing one exact candidate value.
    // Since request endpoints are integers, containment is exact even for time.
    (request.flags & INTEGER == 0 || value.flags & INTEGER != 0)
        && (request.min < value.min
            || (request.min == value.min
                && (request.flags & OPEN_MIN == 0 || value.flags & OPEN_MIN != 0)))
        && (request.max > value.max
            || (request.max == value.max
                && (request.flags & OPEN_MAX == 0 || value.flags & OPEN_MAX != 0)))
}

fn hull(a: Interval, b: Interval) -> Interval {
    let min = a.min.min(b.min);
    let max = a.max.max(b.max);
    let open_min =
        (a.min != min || a.flags & OPEN_MIN != 0) && (b.min != min || b.flags & OPEN_MIN != 0);
    let open_max =
        (a.max != max || a.flags & OPEN_MAX != 0) && (b.max != max || b.flags & OPEN_MAX != 0);
    Interval {
        min,
        max,
        flags: u32::from(open_min) | (u32::from(open_max) << 1) | (a.flags & b.flags & INTEGER),
    }
}

fn publish(params: &mut HwParams, intervals: [Interval; 12]) {
    for (index, bit) in MASK_BITS.into_iter().enumerate() {
        let mut mask = Mask { bits: [0; 8] };
        mask.bits[0] = bit;
        if params.masks[index].bits != mask.bits {
            params.cmask |= 1 << index;
            params.masks[index] = mask;
        }
    }
    for (index, (old, new)) in params.intervals.iter_mut().zip(intervals).enumerate() {
        if (old.min, old.max, old.flags) != (new.min, new.max, new.flags) {
            params.cmask |= 1 << (index + 8);
            *old = new;
        }
    }
    params.rmask = 0;
    params.info = 0x100; // INTERLEAVED only; no mmap, pause, resume, or sync claims.
    params.msbits = 16;
    let rate = intervals[RATE];
    params.rate_num = if rate.min == rate.max { rate.min } else { 0 };
    params.rate_den = u32::from(rate.min == rate.max);
    params.fifo_size = 0;
}

#[cfg(test)]
mod tests {
    use alsa_pcm_uapi::{HwParams, Interval, Mask};
    use bytemuck::Zeroable;

    use super::{Config, configure, refine};
    use crate::StarryError;

    fn any() -> HwParams {
        HwParams {
            masks: [Mask {
                bits: [u32::MAX; 8],
            }; 3],
            intervals: [range(0, u32::MAX, 0); 12],
            rmask: u32::MAX,
            ..HwParams::zeroed()
        }
    }

    fn range(min: u32, max: u32, flags: u32) -> Interval {
        Interval { min, max, flags }
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn coupled_refinement_and_selection_preserve_constraints() {
        let mut p = any();
        p.intervals[5] = range(128, 512, 4); // PERIOD_SIZE
        p.intervals[7] = range(4, 4, 4); // PERIODS
        p.intervals[10] = range(1536, 2048, 4); // BUFFER_BYTES
        p.cmask = 1 << 31;
        refine(&mut p).unwrap();
        assert_eq!((p.intervals[5].min, p.intervals[5].max), (192, 256));
        assert_eq!((p.intervals[9].min, p.intervals[9].max), (768, 1024));
        assert_eq!((p.intervals[6].min, p.intervals[6].max), (384, 512));
        assert_eq!((p.intervals[3].min, p.intervals[3].max), (16_000, 48_000));
        assert_ne!(p.cmask & (1 << 31), 0);
        assert_ne!(p.cmask & (1 << 13), 0);
        assert_eq!(p.rmask, 0);

        p.cmask = 0;
        refine(&mut p).unwrap();
        assert_eq!(p.cmask, 0);
        assert_eq!(
            configure(&mut p).unwrap(),
            Config {
                rate: 16_000,
                period_frames: 192,
                buffer_frames: 768
            }
        );
        assert_eq!((p.rate_num, p.rate_den, p.msbits), (16_000, 1, 16));

        let mut broad = any();
        assert_eq!(
            configure(&mut broad).unwrap(),
            Config {
                rate: 16_000,
                period_frames: 64,
                buffer_frames: 1024
            }
        );

        // The largest period leaves room for only four periods in the ring.
        let mut large = any();
        large.intervals[5] = range(8192, 8192, 4);
        refine(&mut large).unwrap();
        assert_eq!((large.intervals[7].min, large.intervals[7].max), (2, 4));
        assert_eq!(configure(&mut large).unwrap().buffer_frames, 32_768);
        large.intervals[7] = range(5, 5, 4);
        large.intervals[9] = range(40_960, 40_960, 4);
        large.intervals[10] = range(81_920, 81_920, 4);
        large.intervals[8] = range(0, u32::MAX, 0);
        assert!(matches!(
            configure(&mut large),
            Err(StarryError::InvalidInput)
        ));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn fractional_near_times_remain_open_and_couple_to_frames() {
        let mut p = any();
        p.intervals[3] = range(48_000, 48_000, 4);
        p.intervals[4] = range(1333, 1334, 1); // PERIOD_TIME: alsa-lib first/near bound
        p.intervals[8] = range(5333, 5334, 3); // BUFFER_TIME: 5333 + 1/3 us
        refine(&mut p).unwrap();
        assert_eq!(
            configure(&mut p).unwrap(),
            Config {
                rate: 48_000,
                period_frames: 64,
                buffer_frames: 256
            }
        );
        assert_eq!(
            (p.intervals[4].min, p.intervals[4].max, p.intervals[4].flags),
            (1333, 1334, 3)
        );
        assert_eq!(p.intervals[7].min, 4);
        p.intervals[4].flags |= 4;
        assert!(matches!(configure(&mut p), Err(StarryError::InvalidInput)));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn incompatible_requests_fail_without_rounding_or_partial_output() {
        let mut base = any();
        base.intervals[5] = range(128, 128, 4);
        base.intervals[7] = range(4, 4, 4);
        for (index, constraint) in [
            (3, range(22_050, 22_050, 4)), // Hole in the rate envelope.
            (3, range(16_000, 48_000, 3)), // Both supported rates excluded.
            (5, range(129, 129, 4)),       // Unaligned period, not rounded to 192.
            (5, range(8256, 8256, 4)),     // Period exceeds the hardware limit.
            (6, range(128, 128, 4)),       // Bytes contradict the 128-frame period.
            (7, range(17, 17, 4)),         // Too many periods.
            (9, range(384, 384, 4)),       // Individually legal, but not period * 4.
            (4, range(0, u32::MAX, 8)),    // Explicit empty interval.
        ] {
            let mut p = base;
            p.intervals[index] = constraint;
            let before = p;
            assert!(matches!(refine(&mut p), Err(StarryError::InvalidInput)));
            assert!(matches!(configure(&mut p), Err(StarryError::InvalidInput)));
            assert_eq!(bytemuck::bytes_of(&p), bytemuck::bytes_of(&before));
        }
        base.masks[0].bits = [1, 0, 0, 0, 0, 0, 0, 0]; // MMAP_INTERLEAVED only.
        assert!(matches!(
            configure(&mut base),
            Err(StarryError::InvalidInput)
        ));
        base = any();
        base.flags = 4; // NO_PERIOD_WAKEUP is unsupported.
        assert!(matches!(refine(&mut base), Err(StarryError::InvalidInput)));
    }
}

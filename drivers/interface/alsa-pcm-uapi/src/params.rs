//! ALSA hardware-parameter intersection and validation.
//!
//! This module is deliberately independent of a PCM backend. Callers provide
//! the device's effective constraints, which should be the intersection of the
//! device capabilities with the formats the runtime actually implements.

use crate::abi::{
    PcmHwParams, PcmInterval, PcmMask, SNDRV_PCM_ACCESS_RW_INTERLEAVED, SNDRV_PCM_FORMAT_S16_LE,
    SNDRV_PCM_HW_PARAM_ACCESS, SNDRV_PCM_HW_PARAM_BUFFER_BYTES, SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
    SNDRV_PCM_HW_PARAM_BUFFER_TIME, SNDRV_PCM_HW_PARAM_CHANNELS, SNDRV_PCM_HW_PARAM_FORMAT,
    SNDRV_PCM_HW_PARAM_FRAME_BITS, SNDRV_PCM_HW_PARAM_PERIOD_BYTES, SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
    SNDRV_PCM_HW_PARAM_PERIOD_TIME, SNDRV_PCM_HW_PARAM_PERIODS, SNDRV_PCM_HW_PARAM_RATE,
    SNDRV_PCM_HW_PARAM_SAMPLE_BITS, SNDRV_PCM_HW_PARAM_SUBFORMAT, SNDRV_PCM_HW_PARAM_TICK_TIME,
    SNDRV_PCM_SUBFORMAT_STD,
};

mod format;
mod sw;

use self::format::validate_refined;
pub use self::{format::PcmFormat, sw::SwParamsError};

/// A parameter whose intersection became empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParamError {
    #[error("PCM parameter {param} has no value after refinement")]
    Empty { param: u32 },
    #[error("PCM parameter {param} is not concrete for HW_PARAMS")]
    NotConcrete { param: u32 },
    #[error("arithmetic overflow while deriving PCM parameter {param}")]
    Overflow { param: u32 },
    #[error("unsupported PCM access, format, or sample layout")]
    Unsupported,
    #[error("invalid PCM constraint: {reason}")]
    Invalid { reason: &'static str },
}

const fn param_bit(param: u32) -> u32 {
    1u32 << param
}

const fn interval_index(param: u32) -> Option<usize> {
    if param >= SNDRV_PCM_HW_PARAM_SAMPLE_BITS && param <= SNDRV_PCM_HW_PARAM_TICK_TIME {
        Some((param - SNDRV_PCM_HW_PARAM_SAMPLE_BITS) as usize)
    } else {
        None
    }
}

const fn mask_index(param: u32) -> Option<usize> {
    if param <= SNDRV_PCM_HW_PARAM_SUBFORMAT {
        Some(param as usize)
    } else {
        None
    }
}

impl PcmHwParams {
    /// Returns the concrete selected hardware format.
    ///
    /// This is the HW_PARAMS validation entry point. It refines a copy first,
    /// so a failed call does not modify the caller's request. All parameters
    /// needed to program a stream must be concrete after refinement.
    pub fn accept(&self, constraints: &PcmConstraints) -> Result<(Self, PcmFormat), ParamError> {
        let refined = refine_hw_params(self, constraints)?;
        let format = validate_refined(&refined)?;
        Ok((refined, format))
    }

    /// Refines a copy of this request while preserving non-singleton ranges.
    pub fn refine(&self, constraints: &PcmConstraints) -> Result<Self, ParamError> {
        refine_hw_params(self, constraints)
    }
}

/// Device capabilities and runtime policy expressed as ALSA constraints.
///
/// Interval units are explicit in the field names:
///
/// - `*_bits` are bits, except time fields which are microseconds;
/// - `*_size` and `*_frames` fields are frames;
/// - `*_bytes` fields are bytes;
/// - `rate` is Hz;
/// - `channels` is a channel count.
///
/// `PcmConstraints::s16le_48k_stereo_rw()` is the fixed supported profile.
/// Call `intersect` with the device's advertised constraints before using it,
/// so an unsupported device format cannot be accepted by the helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmConstraints {
    pub access: PcmMask,
    pub format: PcmMask,
    pub subformat: PcmMask,
    /// Bits per sample.
    pub sample_bits: PcmInterval,
    /// Bits per frame.
    pub frame_bits: PcmInterval,
    pub channels: PcmInterval,
    /// Sample rate in Hz.
    pub rate: PcmInterval,
    /// Period duration in microseconds.
    pub period_time_us: PcmInterval,
    /// Period size in frames.
    pub period_size: PcmInterval,
    /// Period size in bytes.
    pub period_bytes: PcmInterval,
    /// Number of periods in the buffer.
    pub periods: PcmInterval,
    /// Buffer duration in microseconds.
    pub buffer_time_us: PcmInterval,
    /// Buffer size in frames.
    pub buffer_size: PcmInterval,
    /// Buffer size in bytes.
    pub buffer_bytes: PcmInterval,
    /// Tick duration in microseconds.
    pub tick_time_us: PcmInterval,
}

impl PcmConstraints {
    /// An empty constraint set. It cannot accept any HW_PARAMS request.
    pub const fn empty() -> Self {
        Self {
            access: PcmMask::empty(),
            format: PcmMask::empty(),
            subformat: PcmMask::empty(),
            sample_bits: PcmInterval::empty(),
            frame_bits: PcmInterval::empty(),
            channels: PcmInterval::empty(),
            rate: PcmInterval::empty(),
            period_time_us: PcmInterval::empty(),
            period_size: PcmInterval::empty(),
            period_bytes: PcmInterval::empty(),
            periods: PcmInterval::empty(),
            buffer_time_us: PcmInterval::empty(),
            buffer_size: PcmInterval::empty(),
            buffer_bytes: PcmInterval::empty(),
            tick_time_us: PcmInterval::empty(),
        }
    }

    /// The fixed first-version profile: `S16_LE`, 48 kHz, two channels, and
    /// `RW_INTERLEAVED`. Period and buffer ranges remain intervals so ALSA's
    /// near-time negotiation can choose a device-compatible combination.
    pub const fn s16le_48k_stereo_rw() -> Self {
        Self {
            access: PcmMask::single(SNDRV_PCM_ACCESS_RW_INTERLEAVED),
            format: PcmMask::single(SNDRV_PCM_FORMAT_S16_LE),
            subformat: PcmMask::single(SNDRV_PCM_SUBFORMAT_STD),
            sample_bits: PcmInterval::point(16),
            frame_bits: PcmInterval::point(32),
            channels: PcmInterval::point(2),
            rate: PcmInterval::point(48_000),
            period_time_us: PcmInterval::positive(),
            period_size: PcmInterval::positive(),
            period_bytes: PcmInterval::new(4, u32::MAX),
            periods: PcmInterval::positive(),
            buffer_time_us: PcmInterval::positive(),
            buffer_size: PcmInterval::positive(),
            buffer_bytes: PcmInterval::new(4, u32::MAX),
            tick_time_us: PcmInterval::full(),
        }
    }

    /// Intersects the fixed supported profile with caller-provided device
    /// constraints. An empty result means the device cannot satisfy the first
    /// version's format contract.
    pub fn supported_s16le_48k_stereo_rw(device: &Self) -> Result<Self, ParamError> {
        Self::s16le_48k_stereo_rw().intersect(device)
    }

    /// Intersects every parameter represented by `PcmConstraints`.
    pub fn intersect(&self, other: &Self) -> Result<Self, ParamError> {
        let result = Self {
            access: self.access.intersection(other.access),
            format: self.format.intersection(other.format),
            subformat: self.subformat.intersection(other.subformat),
            sample_bits: self.sample_bits.intersection(other.sample_bits),
            frame_bits: self.frame_bits.intersection(other.frame_bits),
            channels: self.channels.intersection(other.channels),
            rate: self.rate.intersection(other.rate),
            period_time_us: self.period_time_us.intersection(other.period_time_us),
            period_size: self.period_size.intersection(other.period_size),
            period_bytes: self.period_bytes.intersection(other.period_bytes),
            periods: self.periods.intersection(other.periods),
            buffer_time_us: self.buffer_time_us.intersection(other.buffer_time_us),
            buffer_size: self.buffer_size.intersection(other.buffer_size),
            buffer_bytes: self.buffer_bytes.intersection(other.buffer_bytes),
            tick_time_us: self.tick_time_us.intersection(other.tick_time_us),
        };
        result.require_nonempty()?;
        Ok(result)
    }

    fn require_nonempty(&self) -> Result<(), ParamError> {
        for (param, empty) in [
            (SNDRV_PCM_HW_PARAM_ACCESS, self.access.is_empty()),
            (SNDRV_PCM_HW_PARAM_FORMAT, self.format.is_empty()),
            (SNDRV_PCM_HW_PARAM_SUBFORMAT, self.subformat.is_empty()),
            (SNDRV_PCM_HW_PARAM_SAMPLE_BITS, self.sample_bits.is_empty()),
            (SNDRV_PCM_HW_PARAM_FRAME_BITS, self.frame_bits.is_empty()),
            (SNDRV_PCM_HW_PARAM_CHANNELS, self.channels.is_empty()),
            (SNDRV_PCM_HW_PARAM_RATE, self.rate.is_empty()),
            (
                SNDRV_PCM_HW_PARAM_PERIOD_TIME,
                self.period_time_us.is_empty(),
            ),
            (SNDRV_PCM_HW_PARAM_PERIOD_SIZE, self.period_size.is_empty()),
            (
                SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
                self.period_bytes.is_empty(),
            ),
            (SNDRV_PCM_HW_PARAM_PERIODS, self.periods.is_empty()),
            (
                SNDRV_PCM_HW_PARAM_BUFFER_TIME,
                self.buffer_time_us.is_empty(),
            ),
            (SNDRV_PCM_HW_PARAM_BUFFER_SIZE, self.buffer_size.is_empty()),
            (
                SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
                self.buffer_bytes.is_empty(),
            ),
            (SNDRV_PCM_HW_PARAM_TICK_TIME, self.tick_time_us.is_empty()),
        ] {
            if empty {
                return Err(ParamError::Empty { param });
            }
        }
        Ok(())
    }

    fn interval(&self, param: u32) -> PcmInterval {
        match param {
            SNDRV_PCM_HW_PARAM_SAMPLE_BITS => self.sample_bits,
            SNDRV_PCM_HW_PARAM_FRAME_BITS => self.frame_bits,
            SNDRV_PCM_HW_PARAM_CHANNELS => self.channels,
            SNDRV_PCM_HW_PARAM_RATE => self.rate,
            SNDRV_PCM_HW_PARAM_PERIOD_TIME => self.period_time_us,
            SNDRV_PCM_HW_PARAM_PERIOD_SIZE => self.period_size,
            SNDRV_PCM_HW_PARAM_PERIOD_BYTES => self.period_bytes,
            SNDRV_PCM_HW_PARAM_PERIODS => self.periods,
            SNDRV_PCM_HW_PARAM_BUFFER_TIME => self.buffer_time_us,
            SNDRV_PCM_HW_PARAM_BUFFER_SIZE => self.buffer_size,
            SNDRV_PCM_HW_PARAM_BUFFER_BYTES => self.buffer_bytes,
            SNDRV_PCM_HW_PARAM_TICK_TIME => self.tick_time_us,
            _ => PcmInterval::empty(),
        }
    }
}

/// Refines a userspace HW_REFINE request against a device constraint set.
///
/// The function operates on a copy, so an empty intersection cannot leak a
/// partially modified request. It keeps ranges as ranges; only the final
/// HW_PARAMS validation requires singleton values. This is what lets
/// `set_buffer_time_near()` and `set_period_time_near()` negotiate a period
/// and buffer pair instead of forcing one hard-coded period size.
pub fn refine_hw_params(
    params: &PcmHwParams,
    constraints: &PcmConstraints,
) -> Result<PcmHwParams, ParamError> {
    constraints.require_nonempty()?;
    let mut refined = *params;
    for (index, constraint) in [
        (SNDRV_PCM_HW_PARAM_ACCESS, constraints.access),
        (SNDRV_PCM_HW_PARAM_FORMAT, constraints.format),
        (SNDRV_PCM_HW_PARAM_SUBFORMAT, constraints.subformat),
    ] {
        let index = mask_index(index).ok_or(ParamError::Invalid {
            reason: "internal mask parameter index",
        })?;
        let merged = refined.masks[index].intersection(constraint);
        if merged.is_empty() {
            return Err(ParamError::Empty {
                param: index as u32,
            });
        }
        if merged != refined.masks[index] {
            refined.cmask |= param_bit(index as u32);
        }
        refined.masks[index] = merged;
    }

    for (index, interval) in refined.intervals.iter_mut().enumerate() {
        let param = SNDRV_PCM_HW_PARAM_SAMPLE_BITS + index as u32;
        let merged = interval.intersection(constraints.interval(param));
        if merged.is_empty() {
            return Err(ParamError::Empty { param });
        }
        if merged != *interval {
            refined.cmask |= param_bit(param);
        }
        *interval = merged;
    }

    for _ in 0..8 {
        let before = refined;
        if !derive_once(&mut refined, constraints)? || refined == before {
            break;
        }
    }
    Ok(refined)
}

/// Refines and validates a HW_PARAMS request.
pub fn accept_hw_params(
    params: &PcmHwParams,
    constraints: &PcmConstraints,
) -> Result<(PcmHwParams, PcmFormat), ParamError> {
    let refined = refine_hw_params(params, constraints)?;
    let format = validate_refined(&refined)?;
    Ok((refined, format))
}

fn derive_once(params: &mut PcmHwParams, constraints: &PcmConstraints) -> Result<bool, ParamError> {
    let mut changed = false;

    if let Some(bits) = single_format_bits(params.masks[1])? {
        changed |= set_interval(
            params,
            constraints,
            SNDRV_PCM_HW_PARAM_SAMPLE_BITS,
            PcmInterval::point(bits),
        )?;
    }

    let sample_bits = params.intervals[0];
    let channels = params.intervals[2];
    let frame_bits = mul_intervals(sample_bits, channels, SNDRV_PCM_HW_PARAM_FRAME_BITS)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_FRAME_BITS,
        frame_bits,
    )?;

    let frame_bits = params.intervals[1];
    let frame_bytes = bits_to_bytes(frame_bits)?;
    let period_size = params.intervals[5];
    let period_bytes = mul_intervals(period_size, frame_bytes, SNDRV_PCM_HW_PARAM_PERIOD_BYTES)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
        period_bytes,
    )?;

    let periods = params.intervals[7];
    let buffer_size = mul_intervals(params.intervals[5], periods, SNDRV_PCM_HW_PARAM_BUFFER_SIZE)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
        buffer_size,
    )?;

    let buffer_bytes = mul_intervals(
        params.intervals[9],
        frame_bytes,
        SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
    )?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
        buffer_bytes,
    )?;

    let rate = params.intervals[3];
    // Convert both directions even while a range is still open. The floor/ceil
    // bounds below are set-valued bounds: they never require a non-integral
    // frame count to match an exact rational duration.
    let period_time = size_to_time(params.intervals[5], rate, SNDRV_PCM_HW_PARAM_PERIOD_TIME)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_PERIOD_TIME,
        period_time,
    )?;
    let buffer_time = size_to_time(params.intervals[9], rate, SNDRV_PCM_HW_PARAM_BUFFER_TIME)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_BUFFER_TIME,
        buffer_time,
    )?;
    let period_size = time_to_size(params.intervals[4], rate, SNDRV_PCM_HW_PARAM_PERIOD_SIZE)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
        period_size,
    )?;
    let buffer_size = time_to_size(params.intervals[8], rate, SNDRV_PCM_HW_PARAM_BUFFER_SIZE)?;
    changed |= set_interval(
        params,
        constraints,
        SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
        buffer_size,
    )?;

    if let Some(frame_bytes) = frame_bytes.exact() {
        let period_size = div_interval(
            params.intervals[6],
            frame_bytes,
            SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
        )?;
        changed |= set_interval(
            params,
            constraints,
            SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
            period_size,
        )?;
        let buffer_size = div_interval(
            params.intervals[10],
            frame_bytes,
            SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
        )?;
        changed |= set_interval(
            params,
            constraints,
            SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
            buffer_size,
        )?;
    }

    if let Some(period_size) = params.intervals[5].exact() {
        let derived = div_interval(params.intervals[9], period_size, SNDRV_PCM_HW_PARAM_PERIODS)?;
        changed |= set_interval(params, constraints, SNDRV_PCM_HW_PARAM_PERIODS, derived)?;
    }
    if let Some(period_count) = params.intervals[7].exact() {
        let derived = div_interval(
            params.intervals[9],
            period_count,
            SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
        )?;
        changed |= set_interval(params, constraints, SNDRV_PCM_HW_PARAM_PERIOD_SIZE, derived)?;
    }

    Ok(changed)
}

fn set_interval(
    params: &mut PcmHwParams,
    constraints: &PcmConstraints,
    param: u32,
    candidate: PcmInterval,
) -> Result<bool, ParamError> {
    let index = interval_index(param).ok_or(ParamError::Invalid {
        reason: "internal interval parameter index",
    })?;
    let merged = params.intervals[index]
        .intersection(candidate)
        .intersection(constraints.interval(param));
    if merged.is_empty() {
        return Err(ParamError::Empty { param });
    }
    let changed = merged != params.intervals[index];
    if changed {
        params.cmask |= param_bit(param);
        params.intervals[index] = merged;
    }
    Ok(changed)
}

fn single_format_bits(mask: PcmMask) -> Result<Option<u32>, ParamError> {
    if mask.is_empty() {
        return Err(ParamError::Empty {
            param: SNDRV_PCM_HW_PARAM_FORMAT,
        });
    }
    if mask.count() != 1 {
        return Ok(None);
    }
    Ok(match mask.first() {
        Some(SNDRV_PCM_FORMAT_S16_LE) => Some(16),
        Some(_) => None,
        None => {
            return Err(ParamError::Empty {
                param: SNDRV_PCM_HW_PARAM_FORMAT,
            });
        }
    })
}

fn mul_intervals(
    left: PcmInterval,
    right: PcmInterval,
    param: u32,
) -> Result<PcmInterval, ParamError> {
    let left = left.normalized();
    let right = right.normalized();
    if left.is_empty() || right.is_empty() {
        return Err(ParamError::Empty { param });
    }
    let min = left
        .min
        .checked_mul(right.min)
        .ok_or(ParamError::Overflow { param })?;
    let max = left
        .max
        .checked_mul(right.max)
        .ok_or(ParamError::Overflow { param })?;
    Ok(PcmInterval::new(min, max))
}

fn bits_to_bytes(bits: PcmInterval) -> Result<PcmInterval, ParamError> {
    let bits = bits.normalized();
    if bits.is_empty() {
        return Err(ParamError::Empty {
            param: SNDRV_PCM_HW_PARAM_FRAME_BITS,
        });
    }
    if bits.min == 0 {
        return Err(ParamError::Empty {
            param: SNDRV_PCM_HW_PARAM_FRAME_BITS,
        });
    }
    let min = ceil_div_u64(bits.min as u64, 8).ok_or(ParamError::Invalid {
        reason: "invalid frame bit alignment",
    })?;
    let max = bits.max as u64 / 8;
    if min > max {
        return Err(ParamError::Empty {
            param: SNDRV_PCM_HW_PARAM_FRAME_BITS,
        });
    }
    Ok(PcmInterval::new(min as u32, max as u32))
}

fn size_to_time(
    size: PcmInterval,
    rate: PcmInterval,
    param: u32,
) -> Result<PcmInterval, ParamError> {
    let size = size.normalized();
    let rate = rate.normalized();
    if size.is_empty() || rate.is_empty() || rate.min == 0 {
        return Err(ParamError::Empty { param });
    }
    // Use an over-approximation in both directions. A frame duration is a
    // rational number and a microsecond interval is integral; rounding both
    // endpoints outward prevents a valid frame from being discarded by an
    // intermediate floor/ceil step.
    let min = (size.min as u64)
        .checked_mul(1_000_000)
        .ok_or(ParamError::Overflow { param })?
        / rate.max as u64;
    let max = ceil_div_u64(
        (size.max as u64)
            .checked_mul(1_000_000)
            .ok_or(ParamError::Overflow { param })?,
        rate.min as u64,
    )
    .ok_or(ParamError::Overflow { param })?;
    let min = u32::try_from(min).map_err(|_| ParamError::Overflow { param })?;
    let max = u32::try_from(max).map_err(|_| ParamError::Overflow { param })?;
    Ok(PcmInterval::new(min, max))
}

fn time_to_size(
    time: PcmInterval,
    rate: PcmInterval,
    param: u32,
) -> Result<PcmInterval, ParamError> {
    let time = time.normalized();
    let rate = rate.normalized();
    if time.is_empty() || rate.is_empty() || rate.min == 0 {
        return Err(ParamError::Empty { param });
    }
    let min = (time.min as u64)
        .checked_mul(rate.min as u64)
        .ok_or(ParamError::Overflow { param })?
        / 1_000_000;
    let max = ceil_div_u64(
        (time.max as u64)
            .checked_mul(rate.max as u64)
            .ok_or(ParamError::Overflow { param })?,
        1_000_000,
    )
    .ok_or(ParamError::Overflow { param })?;
    let min = u32::try_from(min).map_err(|_| ParamError::Overflow { param })?;
    let max = u32::try_from(max).map_err(|_| ParamError::Overflow { param })?;
    Ok(PcmInterval::new(min, max))
}

fn div_interval(
    interval: PcmInterval,
    divisor: u32,
    param: u32,
) -> Result<PcmInterval, ParamError> {
    let interval = interval.normalized();
    if interval.is_empty() || divisor == 0 {
        return Err(ParamError::Empty { param });
    }
    let min =
        ceil_div_u64(interval.min as u64, divisor as u64).ok_or(ParamError::Overflow { param })?;
    let max = interval
        .max
        .checked_div(divisor)
        .ok_or(ParamError::Empty { param })?;
    let min = u32::try_from(min).map_err(|_| ParamError::Overflow { param })?;
    Ok(PcmInterval::new(min, max))
}

fn ceil_div_u64(value: u64, divisor: u64) -> Option<u64> {
    let quotient = value.checked_div(divisor)?;
    Some(quotient + if value.is_multiple_of(divisor) { 0 } else { 1 })
}

//! Concrete HW_PARAMS result validation.

use super::ParamError;
use crate::abi::{
    PcmHwParams, PcmInterval, PcmMask, SNDRV_PCM_ACCESS_RW_INTERLEAVED, SNDRV_PCM_FORMAT_S16_LE,
    SNDRV_PCM_HW_PARAM_ACCESS, SNDRV_PCM_HW_PARAM_BUFFER_BYTES, SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
    SNDRV_PCM_HW_PARAM_BUFFER_TIME, SNDRV_PCM_HW_PARAM_CHANNELS, SNDRV_PCM_HW_PARAM_FORMAT,
    SNDRV_PCM_HW_PARAM_FRAME_BITS, SNDRV_PCM_HW_PARAM_PERIOD_BYTES, SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
    SNDRV_PCM_HW_PARAM_PERIOD_TIME, SNDRV_PCM_HW_PARAM_PERIODS, SNDRV_PCM_HW_PARAM_RATE,
    SNDRV_PCM_HW_PARAM_SAMPLE_BITS, SNDRV_PCM_HW_PARAM_SUBFORMAT, SNDRV_PCM_SUBFORMAT_STD,
};

/// The concrete configuration accepted by HW_PARAMS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmFormat {
    pub access: u32,
    pub format: u32,
    pub subformat: u32,
    /// Sample rate in Hz.
    pub rate_hz: u32,
    /// Channel count.
    pub channels: u32,
    /// Bits per sample.
    pub sample_bits: u32,
    /// Bits per frame.
    pub frame_bits: u32,
    /// Bytes per frame.
    pub frame_bytes: u32,
    /// Frames in one period.
    pub period_frames: u32,
    /// Number of periods in the buffer.
    pub period_count: u32,
    /// Frames in the whole buffer.
    pub buffer_frames: u32,
    /// Bytes in one period.
    pub period_bytes: u32,
    /// Bytes in the whole buffer.
    pub buffer_bytes: u32,
    /// Period duration in microseconds.
    pub period_time_us: u32,
    /// Buffer duration in microseconds.
    pub buffer_time_us: u32,
}

impl PcmFormat {
    pub const fn is_s16le_48k_stereo_rw(&self) -> bool {
        self.access == SNDRV_PCM_ACCESS_RW_INTERLEAVED
            && self.format == SNDRV_PCM_FORMAT_S16_LE
            && self.subformat == SNDRV_PCM_SUBFORMAT_STD
            && self.rate_hz == 48_000
            && self.channels == 2
            && self.sample_bits == 16
            && self.frame_bits == 32
            && self.frame_bytes == 4
    }
}

pub(super) fn validate_refined(params: &PcmHwParams) -> Result<PcmFormat, ParamError> {
    let access = concrete_mask(params.masks[0], SNDRV_PCM_HW_PARAM_ACCESS)?;
    let format = concrete_mask(params.masks[1], SNDRV_PCM_HW_PARAM_FORMAT)?;
    let subformat = concrete_mask(params.masks[2], SNDRV_PCM_HW_PARAM_SUBFORMAT)?;
    let sample_bits = concrete_interval(params.intervals[0], SNDRV_PCM_HW_PARAM_SAMPLE_BITS)?;
    let frame_bits = concrete_interval(params.intervals[1], SNDRV_PCM_HW_PARAM_FRAME_BITS)?;
    let channels = concrete_interval(params.intervals[2], SNDRV_PCM_HW_PARAM_CHANNELS)?;
    let rate_hz = concrete_interval(params.intervals[3], SNDRV_PCM_HW_PARAM_RATE)?;
    let period_time_us = concrete_interval(params.intervals[4], SNDRV_PCM_HW_PARAM_PERIOD_TIME)?;
    let period_frames = concrete_interval(params.intervals[5], SNDRV_PCM_HW_PARAM_PERIOD_SIZE)?;
    let period_bytes = concrete_interval(params.intervals[6], SNDRV_PCM_HW_PARAM_PERIOD_BYTES)?;
    let period_count = concrete_interval(params.intervals[7], SNDRV_PCM_HW_PARAM_PERIODS)?;
    let buffer_time_us = concrete_interval(params.intervals[8], SNDRV_PCM_HW_PARAM_BUFFER_TIME)?;
    let buffer_frames = concrete_interval(params.intervals[9], SNDRV_PCM_HW_PARAM_BUFFER_SIZE)?;
    let buffer_bytes = concrete_interval(params.intervals[10], SNDRV_PCM_HW_PARAM_BUFFER_BYTES)?;

    if format != SNDRV_PCM_FORMAT_S16_LE || sample_bits != 16 {
        return Err(ParamError::Unsupported);
    }
    if frame_bits == 0
        || frame_bits % 8 != 0
        || channels == 0
        || rate_hz == 0
        || period_frames == 0
        || period_count == 0
        || buffer_frames == 0
        || period_bytes == 0
        || buffer_bytes == 0
    {
        return Err(ParamError::Unsupported);
    }
    let frame_bytes = frame_bits / 8;
    let expected_period_bytes =
        period_frames
            .checked_mul(frame_bytes)
            .ok_or(ParamError::Overflow {
                param: SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
            })?;
    let expected_buffer_frames =
        period_frames
            .checked_mul(period_count)
            .ok_or(ParamError::Overflow {
                param: SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
            })?;
    let expected_buffer_bytes =
        buffer_frames
            .checked_mul(frame_bytes)
            .ok_or(ParamError::Overflow {
                param: SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
            })?;
    if period_bytes != expected_period_bytes
        || buffer_frames != expected_buffer_frames
        || buffer_bytes != expected_buffer_bytes
    {
        return Err(ParamError::Invalid {
            reason: "period and buffer parameters are inconsistent",
        });
    }

    Ok(PcmFormat {
        access,
        format,
        subformat,
        rate_hz,
        channels,
        sample_bits,
        frame_bits,
        frame_bytes,
        period_frames,
        period_count,
        buffer_frames,
        period_bytes,
        buffer_bytes,
        period_time_us,
        buffer_time_us,
    })
}

fn concrete_mask(mask: PcmMask, param: u32) -> Result<u32, ParamError> {
    if mask.is_empty() {
        return Err(ParamError::Empty { param });
    }
    if mask.count() != 1 {
        return Err(ParamError::NotConcrete { param });
    }
    mask.first().ok_or(ParamError::Empty { param })
}

fn concrete_interval(interval: PcmInterval, param: u32) -> Result<u32, ParamError> {
    let interval = interval.normalized();
    if interval.is_empty() {
        return Err(ParamError::Empty { param });
    }
    interval.exact().ok_or(ParamError::NotConcrete { param })
}

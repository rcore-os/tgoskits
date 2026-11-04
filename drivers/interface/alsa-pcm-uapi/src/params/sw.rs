//! Linux software-parameter validation and canonicalization.

use super::PcmFormat;
use crate::abi::{
    PcmSwParams, SNDRV_PCM_SW_PARAMS_PROTO_2_0_12, SNDRV_PCM_TSTAMP_ENABLE, SNDRV_PCM_TSTAMP_NONE,
    SNDRV_PCM_TSTAMP_TYPE_LAST,
};

/// Software-parameter validation errors that must map to `EINVAL`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SwParamsError {
    #[error("accepted PCM format has zero geometry")]
    InvalidFormat,
    #[error("timestamp mode is not supported")]
    InvalidTimestampMode,
    #[error("timestamp type exceeds the Linux v6.12 range")]
    InvalidTimestampType,
    #[error("avail_min must be nonzero")]
    InvalidAvailMin,
    #[error("canonical HW_PARAMS boundary must be nonzero")]
    InvalidBoundary,
    #[error("silence size and threshold are inconsistent")]
    InvalidSilence,
}

impl PcmSwParams {
    /// Validates Linux v6.12 software-parameter inputs and returns the
    /// canonical userspace view.
    ///
    /// `canonical_boundary` is the authoritative value produced by HW_PARAMS
    /// and owned by the PCM runtime. The input `self.boundary` is advisory and
    /// ignored; the returned copy always contains `canonical_boundary`.
    ///
    /// The timestamp, `avail_min`, and silence checks follow the Linux v6.12
    /// PCM core. Numeric timestamp type values accepted by that core (including
    /// `MONOTONIC_RAW`) remain valid here; unsupported capabilities are a
    /// runtime policy. `start_threshold`, `stop_threshold`, `period_step`, and
    /// the obsolete `xfer_align` field are not input validation constraints.
    pub fn validate_and_canonicalize(
        self,
        format: &PcmFormat,
        canonical_boundary: u64,
    ) -> Result<Self, SwParamsError> {
        if format.buffer_frames == 0 || format.frame_bytes == 0 {
            return Err(SwParamsError::InvalidFormat);
        }
        if canonical_boundary == 0 {
            return Err(SwParamsError::InvalidBoundary);
        }
        if self.tstamp_mode != SNDRV_PCM_TSTAMP_NONE && self.tstamp_mode != SNDRV_PCM_TSTAMP_ENABLE
        {
            return Err(SwParamsError::InvalidTimestampMode);
        }
        if self.proto >= SNDRV_PCM_SW_PARAMS_PROTO_2_0_12
            && self.tstamp_type > SNDRV_PCM_TSTAMP_TYPE_LAST
        {
            return Err(SwParamsError::InvalidTimestampType);
        }
        if self.avail_min == 0 {
            return Err(SwParamsError::InvalidAvailMin);
        }
        if self.silence_size >= canonical_boundary {
            if self.silence_threshold != 0 {
                return Err(SwParamsError::InvalidSilence);
            }
        } else if self.silence_size > self.silence_threshold
            || self.silence_threshold > u64::from(format.buffer_frames)
        {
            return Err(SwParamsError::InvalidSilence);
        }

        let mut canonical = self;
        canonical.boundary = canonical_boundary;
        Ok(canonical)
    }
}

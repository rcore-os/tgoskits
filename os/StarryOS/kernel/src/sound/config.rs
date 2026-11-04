use alsa_pcm_uapi::{
    CtlCardInfo, ParamError, PcmConstraints, PcmInfo, PcmInterval, PcmMask,
    SNDRV_PCM_ACCESS_RW_INTERLEAVED, SNDRV_PCM_CLASS_GENERIC, SNDRV_PCM_FORMAT_S16_LE,
    SNDRV_PCM_INFO_BLOCK_TRANSFER, SNDRV_PCM_INFO_INTERLEAVED, SNDRV_PCM_SUBCLASS_GENERIC_MIX,
    SNDRV_PCM_SUBFORMAT_STD,
};
use rdif_audio::{PERIOD_FRAME_ALIGNMENT, PcmDirection, PcmStreamCapabilities, SampleFormat};

/// The device and runtime policy intersection for one PCM direction.
#[derive(Clone, Copy)]
pub(crate) struct DirectionProfile {
    pub(crate) constraints: PcmConstraints,
}

pub(crate) fn profile(
    capabilities: &PcmStreamCapabilities,
) -> Result<DirectionProfile, ParamError> {
    if capabilities.sample_format != SampleFormat::S16Le
        || capabilities.sample_rate_hz != 48_000
        || capabilities.channels_min > 2
        || capabilities.channels_max < 2
    {
        return Err(ParamError::Unsupported);
    }
    let alignment = PERIOD_FRAME_ALIGNMENT;
    if alignment == 0 {
        return Err(ParamError::Invalid {
            reason: "period alignment must be nonzero",
        });
    }
    let period_min =
        align_up(capabilities.period_frames_min, alignment).ok_or(ParamError::Overflow {
            param: alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
        })?;
    let period_max = align_down(capabilities.period_frames_max, alignment);
    if period_min == 0 || period_max < period_min {
        return Err(ParamError::Empty {
            param: alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
        });
    }
    if capabilities.period_count_min == 0
        || capabilities.period_count_max < capabilities.period_count_min
    {
        return Err(ParamError::Empty {
            param: alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIODS,
        });
    }

    let frame_bytes = 4u32;
    let period_count_min = u32::from(capabilities.period_count_min);
    let period_count_max = u32::from(capabilities.period_count_max);
    let period_bytes_min = checked_mul(
        period_min,
        frame_bytes,
        alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
    )?;
    let period_bytes_max = checked_mul(
        period_max,
        frame_bytes,
        alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
    )?;
    let buffer_min = checked_mul(
        period_min,
        period_count_min,
        alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
    )?;
    let buffer_max = checked_mul(
        period_max,
        period_count_max,
        alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
    )?;
    let buffer_bytes_min = checked_mul(
        buffer_min,
        frame_bytes,
        alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
    )?;
    let buffer_bytes_max = checked_mul(
        buffer_max,
        frame_bytes,
        alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
    )?;
    let period_time_min = frames_to_us(period_min, capabilities.sample_rate_hz)?;
    let period_time_max = frames_to_us(period_max, capabilities.sample_rate_hz)?;
    let buffer_time_min = frames_to_us(buffer_min, capabilities.sample_rate_hz)?;
    let buffer_time_max = frames_to_us(buffer_max, capabilities.sample_rate_hz)?;

    let mut device = PcmConstraints::s16le_48k_stereo_rw();
    device.access = PcmMask::single(SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    device.format = PcmMask::single(SNDRV_PCM_FORMAT_S16_LE);
    device.subformat = PcmMask::single(SNDRV_PCM_SUBFORMAT_STD);
    device.channels = PcmInterval::point(2);
    device.rate = PcmInterval::point(capabilities.sample_rate_hz);
    device.period_size = PcmInterval::new(period_min, period_max);
    device.period_bytes = PcmInterval::new(period_bytes_min, period_bytes_max);
    device.periods = PcmInterval::new(
        capabilities.period_count_min.into(),
        capabilities.period_count_max.into(),
    );
    device.buffer_size = PcmInterval::new(buffer_min, buffer_max);
    device.buffer_bytes = PcmInterval::new(buffer_bytes_min, buffer_bytes_max);
    device.period_time_us = PcmInterval::new(period_time_min, period_time_max);
    device.buffer_time_us = PcmInterval::new(buffer_time_min, buffer_time_max);
    let constraints = PcmConstraints::supported_s16le_48k_stereo_rw(&device)?;

    Ok(DirectionProfile { constraints })
}

pub(crate) const fn hw_info_flags() -> u32 {
    SNDRV_PCM_INFO_INTERLEAVED | SNDRV_PCM_INFO_BLOCK_TRANSFER
}

pub(crate) fn pcm_info(direction: PcmDirection, stream: i32, subdevices_avail: u32) -> PcmInfo {
    PcmInfo {
        device: 0,
        subdevice: 0,
        stream,
        card: 0,
        id: fixed_bytes(b"VirtIO Sound"),
        name: fixed_bytes(match direction {
            PcmDirection::Playback => b"VirtIO Sound Playback",
            PcmDirection::Capture => b"VirtIO Sound Capture",
        }),
        subname: fixed_bytes(b"virtio-snd PCM"),
        dev_class: SNDRV_PCM_CLASS_GENERIC,
        dev_subclass: SNDRV_PCM_SUBCLASS_GENERIC_MIX,
        subdevices_count: 1,
        subdevices_avail,
        ..PcmInfo::default()
    }
}

pub(crate) fn ctl_card_info() -> CtlCardInfo {
    CtlCardInfo {
        card: 0,
        id: fixed_bytes(b"VirtIOCard"),
        driver: fixed_bytes(b"virtio-snd"),
        name: fixed_bytes(b"VirtIO Sound"),
        longname: fixed_bytes(b"VirtIO Sound Card"),
        mixername: fixed_bytes(b"VirtIO Mixer"),
        components: fixed_bytes(b"virtio-snd"),
        ..CtlCardInfo::default()
    }
}

fn checked_mul(left: u32, right: u32, param: u32) -> Result<u32, ParamError> {
    left.checked_mul(right)
        .ok_or(ParamError::Overflow { param })
}

fn frames_to_us(frames: u32, rate_hz: u32) -> Result<u32, ParamError> {
    let numerator = u64::from(frames)
        .checked_mul(1_000_000)
        .ok_or(ParamError::Overflow {
            param: alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIOD_TIME,
        })?;
    u32::try_from(numerator / u64::from(rate_hz))
        .map(|value| value.max(1))
        .map_err(|_| ParamError::Overflow {
            param: alsa_pcm_uapi::SNDRV_PCM_HW_PARAM_PERIOD_TIME,
        })
}

const fn align_up(value: u32, alignment: u32) -> Option<u32> {
    let remainder = value % alignment;
    if remainder == 0 {
        Some(value)
    } else {
        value.checked_add(alignment - remainder)
    }
}

const fn align_down(value: u32, alignment: u32) -> u32 {
    value - value % alignment
}

fn fixed_bytes<const N: usize>(text: &[u8]) -> [u8; N] {
    let mut bytes = [0u8; N];
    let len = text.len().min(N.saturating_sub(1));
    bytes[..len].copy_from_slice(&text[..len]);
    bytes
}

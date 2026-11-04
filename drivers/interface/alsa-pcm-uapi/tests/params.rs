use alsa_pcm_uapi::{
    ParamError, PcmConstraints, PcmFormat, PcmHwParams, PcmInterval, PcmMask, PcmSwParams,
    SNDRV_INTERVAL_OPENMAX, SNDRV_INTERVAL_OPENMIN, SNDRV_PCM_ACCESS_RW_INTERLEAVED,
    SNDRV_PCM_FORMAT_S16_LE, SNDRV_PCM_HW_PARAM_FORMAT, SNDRV_PCM_SUBFORMAT_STD,
    SNDRV_PCM_SW_PARAMS_PROTO_2_0_12, SNDRV_PCM_TSTAMP_ENABLE, SNDRV_PCM_TSTAMP_TYPE_LAST,
    SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW, SwParamsError, accept_hw_params, refine_hw_params,
};

fn full_hw_params() -> PcmHwParams {
    let mut params = PcmHwParams::default();
    params.masks = [PcmMask::full(); 3];
    params.intervals = [PcmInterval::full(); 12];
    params
}

fn device_constraints() -> PcmConstraints {
    let mut constraints = PcmConstraints::s16le_48k_stereo_rw();
    constraints.period_size = PcmInterval::new(32, 8192);
    constraints.period_time_us = PcmInterval::new(667, 170_667);
    constraints.periods = PcmInterval::new(2, 8);
    constraints.buffer_size = PcmInterval::new(64, 65_536);
    constraints.buffer_time_us = PcmInterval::new(1_334, 1_365_333);
    constraints.period_bytes = PcmInterval::new(128, 32_768);
    constraints.buffer_bytes = PcmInterval::new(256, 262_144);
    constraints
}

fn set_format(params: &mut PcmHwParams) {
    params.masks[0] = PcmMask::single(SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    params.masks[1] = PcmMask::single(SNDRV_PCM_FORMAT_S16_LE);
    params.masks[2] = PcmMask::single(SNDRV_PCM_SUBFORMAT_STD);
}

#[test]
fn wire_interval_and_mask_rules_are_complete() {
    let left = PcmInterval::from_raw(10, 20, SNDRV_INTERVAL_OPENMIN | SNDRV_INTERVAL_OPENMAX);
    assert_eq!(left.normalized(), PcmInterval::new(11, 19));
    assert_eq!(
        left.intersection(PcmInterval::new(15, 30)),
        PcmInterval::new(15, 19)
    );
    assert!(left.contains(11));
    assert!(!left.contains(10));
    assert_eq!(PcmInterval::new(30, 20), PcmInterval::empty());

    let mut mask = PcmMask::empty();
    assert!(mask.insert(SNDRV_PCM_FORMAT_S16_LE));
    assert!(!mask.insert(SNDRV_PCM_FORMAT_S16_LE));
    assert!(mask.has(SNDRV_PCM_FORMAT_S16_LE));
    assert_eq!(mask.count(), 1);
    assert_eq!(
        mask.intersection(PcmMask::single(SNDRV_PCM_FORMAT_S16_LE)),
        mask
    );
    assert!(mask.intersection(PcmMask::single(4)).is_empty());
    assert!(mask.remove(SNDRV_PCM_FORMAT_S16_LE));
    assert!(!mask.remove(SNDRV_PCM_FORMAT_S16_LE));
    assert!(mask.is_empty());
}

#[test]
fn aplay_default_times_refine_without_a_fixed_period() {
    let constraints = device_constraints();
    let mut params = full_hw_params();
    set_format(&mut params);
    params.intervals[5] = PcmInterval::new(3000, 4096);
    params.intervals[4] = PcmInterval::new(62_500, 85_333);
    params.intervals[7] = PcmInterval::new(4, 8);
    params.intervals[9] = PcmInterval::new(24_000, 32_768);
    params.intervals[8] = PcmInterval::new(500_000, 682_667);
    params.intervals[6] = PcmInterval::new(12_000, 16_384);
    params.intervals[10] = PcmInterval::new(48_000, 131_072);

    let refined = refine_hw_params(&params, &constraints).unwrap();
    assert_eq!(
        refined.intervals[5].normalized(),
        PcmInterval::new(3000, 4096)
    );
    assert_eq!(refined.intervals[7].normalized(), PcmInterval::new(4, 8));
    assert!(!refined.intervals[5].is_singleton());
}

#[test]
fn concrete_aplay_parameters_accept() {
    let constraints = device_constraints();
    let mut params = full_hw_params();
    set_format(&mut params);
    params.intervals[0] = PcmInterval::point(16);
    params.intervals[1] = PcmInterval::point(32);
    params.intervals[2] = PcmInterval::point(2);
    params.intervals[3] = PcmInterval::point(48_000);
    params.intervals[4] = PcmInterval::point(125_000);
    params.intervals[5] = PcmInterval::point(6000);
    params.intervals[6] = PcmInterval::point(24_000);
    params.intervals[7] = PcmInterval::point(4);
    params.intervals[8] = PcmInterval::point(500_000);
    params.intervals[9] = PcmInterval::point(24_000);
    params.intervals[10] = PcmInterval::point(96_000);

    let (_, format) = accept_hw_params(&params, &constraints).unwrap();
    assert_eq!(format.period_frames, 6000);
    assert_eq!(format.period_count, 4);
    assert_eq!(format.buffer_frames, 24_000);
    assert_eq!(format.period_bytes, 24_000);
    assert_eq!(format.buffer_bytes, 96_000);
    assert!(format.is_s16le_48k_stereo_rw());
}

#[test]
fn unsupported_format_and_overflow_are_rejected() {
    let constraints = device_constraints();
    let mut device = constraints;
    device.format = PcmMask::single(4);
    assert!(matches!(
        PcmConstraints::supported_s16le_48k_stereo_rw(&device),
        Err(ParamError::Empty {
            param: SNDRV_PCM_HW_PARAM_FORMAT
        })
    ));

    let mut params = full_hw_params();
    set_format(&mut params);
    params.masks[1] = PcmMask::single(4);
    assert!(matches!(
        refine_hw_params(&params, &constraints),
        Err(ParamError::Empty {
            param: SNDRV_PCM_HW_PARAM_FORMAT
        })
    ));

    let mut broad = PcmConstraints::s16le_48k_stereo_rw();
    broad.period_size = PcmInterval::positive();
    broad.periods = PcmInterval::positive();
    broad.period_bytes = PcmInterval::positive();
    broad.buffer_size = PcmInterval::positive();
    broad.buffer_bytes = PcmInterval::positive();
    let mut overflowing = full_hw_params();
    set_format(&mut overflowing);
    overflowing.intervals[5] = PcmInterval::new(u32::MAX, u32::MAX);
    overflowing.intervals[7] = PcmInterval::new(2, 2);
    assert!(matches!(
        refine_hw_params(&overflowing, &broad),
        Err(ParamError::Overflow { .. })
    ));
}

#[test]
fn sw_params_follow_linux_input_and_canonical_boundary() {
    let format = PcmFormat {
        access: SNDRV_PCM_ACCESS_RW_INTERLEAVED,
        format: SNDRV_PCM_FORMAT_S16_LE,
        subformat: SNDRV_PCM_SUBFORMAT_STD,
        rate_hz: 48_000,
        channels: 2,
        sample_bits: 16,
        frame_bits: 32,
        frame_bytes: 4,
        period_frames: 6000,
        period_count: 4,
        buffer_frames: 24_000,
        period_bytes: 24_000,
        buffer_bytes: 96_000,
        period_time_us: 125_000,
        buffer_time_us: 500_000,
    };
    let params = PcmSwParams {
        tstamp_mode: SNDRV_PCM_TSTAMP_ENABLE,
        avail_min: 6000,
        start_threshold: 30_000,
        stop_threshold: 96_000,
        xfer_align: 1,
        silence_threshold: 12_000,
        silence_size: 12_000,
        boundary: 23_999,
        proto: SNDRV_PCM_SW_PARAMS_PROTO_2_0_12,
        tstamp_type: SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW,
        ..PcmSwParams::default()
    };
    let canonical = params.validate_and_canonicalize(&format, 96_000).unwrap();
    assert_eq!(canonical.boundary, 96_000);

    let mut at_boundary = canonical;
    at_boundary.silence_size = 96_000;
    at_boundary.silence_threshold = 0;
    at_boundary.boundary = 0;
    assert!(
        at_boundary
            .validate_and_canonicalize(&format, 96_000)
            .is_ok()
    );

    let mut bad_silence = at_boundary;
    bad_silence.silence_threshold = 1;
    assert_eq!(
        bad_silence.validate_and_canonicalize(&format, 96_000),
        Err(SwParamsError::InvalidSilence)
    );

    let mut bad_avail = canonical;
    bad_avail.avail_min = 0;
    assert_eq!(
        bad_avail.validate_and_canonicalize(&format, 96_000),
        Err(SwParamsError::InvalidAvailMin)
    );

    let mut bad_tstamp = canonical;
    bad_tstamp.tstamp_type = SNDRV_PCM_TSTAMP_TYPE_LAST + 1;
    assert_eq!(
        bad_tstamp.validate_and_canonicalize(&format, 96_000),
        Err(SwParamsError::InvalidTimestampType)
    );

    let mut bad_mode = canonical;
    bad_mode.tstamp_mode = SNDRV_PCM_TSTAMP_ENABLE + 1;
    assert_eq!(
        bad_mode.validate_and_canonicalize(&format, 96_000),
        Err(SwParamsError::InvalidTimestampMode)
    );

    assert_eq!(
        canonical.validate_and_canonicalize(&format, 0),
        Err(SwParamsError::InvalidBoundary)
    );

    let mut bad_format = format;
    bad_format.buffer_frames = 0;
    assert_eq!(
        canonical.validate_and_canonicalize(&bad_format, 96_000),
        Err(SwParamsError::InvalidFormat)
    );
}

#[test]
fn period_and_buffer_ranges_remain_open() {
    let mut constraints = PcmConstraints::s16le_48k_stereo_rw();
    constraints.period_size = PcmInterval::new(480, 960);
    constraints.periods = PcmInterval::new(4, 8);
    constraints.period_time_us = PcmInterval::new(10_000, 20_000);
    constraints.buffer_size = PcmInterval::new(1920, 7680);
    constraints.buffer_time_us = PcmInterval::new(40_000, 160_000);
    constraints.period_bytes = PcmInterval::new(1920, 3840);
    constraints.buffer_bytes = PcmInterval::new(7680, 30_720);

    let mut params = full_hw_params();
    set_format(&mut params);
    params.intervals[4] = PcmInterval::new(10_000, 20_000);
    params.intervals[5] = PcmInterval::new(480, 960);
    params.intervals[7] = PcmInterval::new(4, 8);
    params.intervals[8] = PcmInterval::new(40_000, 160_000);
    params.intervals[9] = PcmInterval::new(1920, 7680);

    let refined = refine_hw_params(&params, &constraints).unwrap();
    assert_eq!(
        refined.intervals[5].normalized(),
        PcmInterval::new(480, 960)
    );
    assert_eq!(refined.intervals[7].normalized(), PcmInterval::new(4, 8));
    assert!(!refined.intervals[5].is_singleton());
    assert!(!refined.intervals[9].is_singleton());
}

#[test]
fn non_integral_time_bounds_use_floor_and_ceil_conservatively() {
    let mut constraints = PcmConstraints::s16le_48k_stereo_rw();
    constraints.period_size = PcmInterval::new(480, 960);
    constraints.period_time_us = PcmInterval::new(10_001, 19_999);
    constraints.periods = PcmInterval::new(4, 8);
    constraints.buffer_size = PcmInterval::new(1920, 7680);
    constraints.buffer_time_us = PcmInterval::new(40_001, 159_999);
    constraints.period_bytes = PcmInterval::new(1920, 3840);
    constraints.buffer_bytes = PcmInterval::new(7680, 30_720);

    let mut params = full_hw_params();
    set_format(&mut params);
    params.intervals[4] = PcmInterval::new(10_001, 19_999);
    params.intervals[5] = PcmInterval::new(480, 960);
    params.intervals[7] = PcmInterval::new(4, 8);
    params.intervals[9] = PcmInterval::new(1920, 7680);

    let refined = refine_hw_params(&params, &constraints).unwrap();
    let period = refined.intervals[5].normalized();
    assert!(!period.is_empty());
    assert!(period.min >= 480);
    assert!(period.max <= 960);
    assert!(!refined.intervals[4].is_singleton());
}

// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_modes.c`.
//!
//! Source-faithful transcription of Linux 7.2.3: display-mode creation,
//! duplication and comparison, the analog TV, CVT and GTF timing generators,
//! mode sorting and pruning, the video-mode and command-line parsers, and the
//! uAPI `drm_mode_modeinfo` conversions.
//!
//! Deviations:
//! - Device-tree (`of_get_drm_display_mode`, `drm_display_mode_*_videomode`)
//!   helpers are not ported: the `video/` and `of` subsystems are not part of
//!   the x86_64 target.
//! - Kernel `va_format` debug output is replaced by pre-formatted `&str`
//!   messages routed through `drm_kapi::print::drm_log_at`.

use alloc::{format, string::String};
use core::{ffi::c_char, mem::offset_of};

use drm_kapi::{
    assert::warn_on,
    config::{EINVAL, ERANGE},
    list::{list_add_tail, list_del},
    memory::{kfree, kzalloc_obj},
    mutex::mutex_is_locked,
    print::{DrmLogLevel, drm_log_at},
    types::ListHead,
};

use crate::{
    drm_connector_types::{
        DRM_FORCE_OFF, DRM_FORCE_ON, DRM_FORCE_ON_DIGITAL, DRM_FORCE_UNSPECIFIED,
        DRM_MODE_PANEL_ORIENTATION_BOTTOM_UP, DRM_MODE_PANEL_ORIENTATION_LEFT_UP,
        DRM_MODE_PANEL_ORIENTATION_NORMAL, DRM_MODE_PANEL_ORIENTATION_RIGHT_UP,
        DRM_MODE_PANEL_ORIENTATION_UNKNOWN, DRM_MODE_TV_MODE_MONOCHROME, DRM_MODE_TV_MODE_NTSC,
        DRM_MODE_TV_MODE_NTSC_443, DRM_MODE_TV_MODE_NTSC_J, DRM_MODE_TV_MODE_PAL,
        DRM_MODE_TV_MODE_PAL_M, DRM_MODE_TV_MODE_PAL_N, DRM_MODE_TV_MODE_SECAM, DrmCmdlineMode,
        DrmConnector, DrmConnectorTvMode, DrmDisplayInfo,
    },
    drm_device_types::DrmDevice,
    drm_modes_types::*,
    hdmi_types::{
        HDMI_PICTURE_ASPECT_4_3, HDMI_PICTURE_ASPECT_16_9, HDMI_PICTURE_ASPECT_64_27,
        HDMI_PICTURE_ASPECT_256_135, HDMI_PICTURE_ASPECT_NONE,
    },
    uapi_drm_consts::*,
    uapi_drm_mode_types::DrmModeModeinfo,
};

// upstream: drm_modes.c DRM_DEBUG_KMS / drm_err routed to the kernel log.
fn drm_dbg_kms(msg: &str) {
    drm_log_at(DrmLogLevel::Debug, "drm_kms", file!(), line!(), msg);
}

fn drm_err(msg: &str) {
    drm_log_at(
        DrmLogLevel::Error,
        "drm",
        file!(),
        line!(),
        &format!("*ERROR* {msg}"),
    );
}

// upstream: include/drm/drm_modes.h DRM_MODE_FMT / DRM_MODE_ARG
fn mode_fmt(mode: *const DrmDisplayMode) -> String {
    // SAFETY: mode is required to be non-null, properly aligned, and pointing to a valid
    // initialized DrmDisplayMode by callers within this module.
    let m = unsafe { &*mode };
    format!(
        "\"{}\": {} {} {} {} {} {} {} {} {} {} 0x{:x} 0x{:x}",
        name_str(&m.name),
        // SAFETY: mode is a valid DrmDisplayMode pointer as asserted above, valid for reading.
        unsafe { drm_mode_vrefresh(mode) },
        m.clock,
        m.hdisplay,
        m.hsync_start,
        m.hsync_end,
        m.htotal,
        m.vdisplay,
        m.vsync_start,
        m.vsync_end,
        m.vtotal,
        m.r#type,
        m.flags
    )
}

// NUL-terminated `name[32]` to &str.
fn name_str(name: &[c_char; 32]) -> &str {
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    // SAFETY: name is a live &[c_char; 32] array; casting to *const u8 with len <= 32 yields
    // a valid contiguous byte slice within the array bounds.
    let bytes = unsafe { core::slice::from_raw_parts(name.as_ptr().cast::<u8>(), len) };
    core::str::from_utf8(bytes).unwrap_or("")
}

// upstream: drm_modes.c drm_mode_debug_printmodeline()
#[unsafe(no_mangle)]
/// # Safety
///
/// `mode` must be non-null and point to a valid, initialized [`DrmDisplayMode`] that remains
/// readable for the duration of the call.
pub unsafe extern "C" fn drm_mode_debug_printmodeline(mode: *const DrmDisplayMode) {
    drm_dbg_kms(&format!("Modeline {}", mode_fmt(mode)));
}

// upstream: drm_modes.c drm_mode_create()
#[unsafe(no_mangle)]
/// # Safety
///
/// `_dev` may be null or point to a valid [`DrmDevice`]. If allocation succeeds, the returned
/// pointer is non-null, uniquely owned, zero-initialized, and must eventually be freed via
/// [`drm_mode_destroy`] or [`kfree`].
pub unsafe extern "C" fn drm_mode_create(_dev: *mut DrmDevice) -> *mut DrmDisplayMode {
    let nmode = kzalloc_obj::<DrmDisplayMode>();
    if nmode.is_null() {
        return core::ptr::null_mut();
    }
    nmode
}

// upstream: drm_modes.c drm_mode_destroy()
#[unsafe(no_mangle)]
/// # Safety
///
/// `mode` may be null, or must point to a valid [`DrmDisplayMode`] previously allocated via
/// [`drm_mode_create`] (or `kmalloc`). After this call, `mode` is freed and must not be used.
pub unsafe extern "C" fn drm_mode_destroy(_dev: *mut DrmDevice, mode: *mut DrmDisplayMode) {
    if mode.is_null() {
        return;
    }
    // SAFETY: mode is checked non-null above, points to a valid allocation previously created
    // via kzalloc/kmalloc, and is freed here without subsequent reuse.
    unsafe { kfree(mode) };
}

// upstream: drm_modes.c drm_mode_probed_add()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `connector` must point to a valid, initialized [`DrmConnector`] with a valid `dev` pointer
///   and initialized `mode_config`.
/// - `mode` must point to a valid, initialized [`DrmDisplayMode`] whose `head` list node can be mutated.
/// - The caller must hold `(*(*connector).dev).mode_config.mutex`.
pub unsafe extern "C" fn drm_mode_probed_add(
    connector: *mut DrmConnector,
    mode: *mut DrmDisplayMode,
) {
    // SAFETY: connector is non-null and valid per caller contract; dereferencing connector->dev->mode_config.mutex
    // yields a valid pointer to the mode_config mutex.
    let mutex = unsafe { core::ptr::addr_of_mut!((*(*connector).dev).mode_config.mutex) };
    // SAFETY: mutex points to the valid mode_config mutex derived above.
    warn_on(!unsafe { mutex_is_locked(mutex) });
    // SAFETY: mode and connector are valid non-null pointers per caller contract; connector->probed_modes
    // and mode->head are valid, initialized ListHead nodes, and caller holds mode_config.mutex protecting the list.
    unsafe {
        list_add_tail(
            core::ptr::addr_of_mut!((*mode).head),
            core::ptr::addr_of_mut!((*connector).probed_modes),
        );
    }
}

// upstream: drm_modes.c enum drm_mode_analog
const DRM_MODE_ANALOG_NTSC: usize = 0;
const DRM_MODE_ANALOG_PAL: usize = 1;

// upstream: drm_modes.c NTSC/PAL duration constants (ns)
const NTSC_LINE_DURATION_NS: u32 = 63556;
const NTSC_LINES_NUMBER: u32 = 525;
const NTSC_HBLK_DURATION_TYP_NS: u32 = 10900;
const NTSC_HBLK_DURATION_MIN_NS: u32 = NTSC_HBLK_DURATION_TYP_NS - 200;
const NTSC_HBLK_DURATION_MAX_NS: u32 = NTSC_HBLK_DURATION_TYP_NS + 200;
const NTSC_HACT_DURATION_TYP_NS: u32 = NTSC_LINE_DURATION_NS - NTSC_HBLK_DURATION_TYP_NS;
const NTSC_HACT_DURATION_MIN_NS: u32 = NTSC_LINE_DURATION_NS - NTSC_HBLK_DURATION_MAX_NS;
const NTSC_HACT_DURATION_MAX_NS: u32 = NTSC_LINE_DURATION_NS - NTSC_HBLK_DURATION_MIN_NS;
const NTSC_HFP_DURATION_TYP_NS: u32 = 1500;
const NTSC_HFP_DURATION_MIN_NS: u32 = 1270;
const NTSC_HFP_DURATION_MAX_NS: u32 = 2220;
const NTSC_HSLEN_DURATION_TYP_NS: u32 = 4700;
const NTSC_HSLEN_DURATION_MIN_NS: u32 = NTSC_HSLEN_DURATION_TYP_NS - 100;
const NTSC_HSLEN_DURATION_MAX_NS: u32 = NTSC_HSLEN_DURATION_TYP_NS + 100;
const NTSC_HBP_DURATION_TYP_NS: u32 = 4700;
const NTSC_HBP_DURATION_MIN_NS: u32 = NTSC_HBP_DURATION_TYP_NS - 100;
const NTSC_HBP_DURATION_MAX_NS: u32 = NTSC_HBP_DURATION_TYP_NS + 100;
const PAL_LINE_DURATION_NS: u32 = 64000;
const PAL_LINES_NUMBER: u32 = 625;
const PAL_HACT_DURATION_TYP_NS: u32 = 51950;
const PAL_HACT_DURATION_MIN_NS: u32 = PAL_HACT_DURATION_TYP_NS - 100;
const PAL_HACT_DURATION_MAX_NS: u32 = PAL_HACT_DURATION_TYP_NS + 400;
const PAL_HBLK_DURATION_TYP_NS: u32 = PAL_LINE_DURATION_NS - PAL_HACT_DURATION_TYP_NS;
const PAL_HBLK_DURATION_MIN_NS: u32 = PAL_LINE_DURATION_NS - PAL_HACT_DURATION_MAX_NS;
const PAL_HBLK_DURATION_MAX_NS: u32 = PAL_LINE_DURATION_NS - PAL_HACT_DURATION_MIN_NS;
const PAL_HFP_DURATION_TYP_NS: u32 = 1650;
const PAL_HFP_DURATION_MIN_NS: u32 = PAL_HFP_DURATION_TYP_NS - 100;
const PAL_HFP_DURATION_MAX_NS: u32 = PAL_HFP_DURATION_TYP_NS + 400;
const PAL_HSLEN_DURATION_TYP_NS: u32 = 4700;
const PAL_HSLEN_DURATION_MIN_NS: u32 = PAL_HSLEN_DURATION_TYP_NS - 200;
const PAL_HSLEN_DURATION_MAX_NS: u32 = PAL_HSLEN_DURATION_TYP_NS + 200;
const PAL_HBP_DURATION_TYP_NS: u32 = 5700;
const PAL_HBP_DURATION_MIN_NS: u32 = PAL_HBP_DURATION_TYP_NS - 200;
const PAL_HBP_DURATION_MAX_NS: u32 = PAL_HBP_DURATION_TYP_NS + 200;

// upstream: drm_modes.c struct analog_param_field / analog_param_range / analog_parameters
#[derive(Clone, Copy)]
struct AnalogParamField {
    even: u32,
    odd: u32,
}

#[derive(Clone, Copy)]
struct AnalogParamRange {
    min: u32,
    typ: u32,
    max: u32,
}

#[derive(Clone, Copy)]
struct AnalogParameters {
    num_lines: u32,
    line_duration_ns: u32,
    hact_ns: AnalogParamRange,
    hfp_ns: AnalogParamRange,
    hslen_ns: AnalogParamRange,
    hbp_ns: AnalogParamRange,
    hblk_ns: AnalogParamRange,
    bt601_hfp: u32,
    vfp_lines: AnalogParamField,
    vslen_lines: AnalogParamField,
    vbp_lines: AnalogParamField,
}

const fn param_field(odd: u32, even: u32) -> AnalogParamField {
    AnalogParamField { even, odd }
}

const fn param_range(min: u32, typ: u32, max: u32) -> AnalogParamRange {
    AnalogParamRange { min, typ, max }
}

// upstream: drm_modes.c tv_modes_parameters[] (indexed by enum drm_mode_analog)
static TV_MODES_PARAMETERS: [AnalogParameters; 2] = [
    AnalogParameters {
        num_lines: NTSC_LINES_NUMBER,
        line_duration_ns: NTSC_LINE_DURATION_NS,
        hact_ns: param_range(
            NTSC_HACT_DURATION_MIN_NS,
            NTSC_HACT_DURATION_TYP_NS,
            NTSC_HACT_DURATION_MAX_NS,
        ),
        hfp_ns: param_range(
            NTSC_HFP_DURATION_MIN_NS,
            NTSC_HFP_DURATION_TYP_NS,
            NTSC_HFP_DURATION_MAX_NS,
        ),
        hslen_ns: param_range(
            NTSC_HSLEN_DURATION_MIN_NS,
            NTSC_HSLEN_DURATION_TYP_NS,
            NTSC_HSLEN_DURATION_MAX_NS,
        ),
        hbp_ns: param_range(
            NTSC_HBP_DURATION_MIN_NS,
            NTSC_HBP_DURATION_TYP_NS,
            NTSC_HBP_DURATION_MAX_NS,
        ),
        hblk_ns: param_range(
            NTSC_HBLK_DURATION_MIN_NS,
            NTSC_HBLK_DURATION_TYP_NS,
            NTSC_HBLK_DURATION_MAX_NS,
        ),
        bt601_hfp: 16,
        vfp_lines: param_field(3, 3),
        vslen_lines: param_field(3, 3),
        vbp_lines: param_field(16, 17),
    },
    AnalogParameters {
        num_lines: PAL_LINES_NUMBER,
        line_duration_ns: PAL_LINE_DURATION_NS,
        hact_ns: param_range(
            PAL_HACT_DURATION_MIN_NS,
            PAL_HACT_DURATION_TYP_NS,
            PAL_HACT_DURATION_MAX_NS,
        ),
        hfp_ns: param_range(
            PAL_HFP_DURATION_MIN_NS,
            PAL_HFP_DURATION_TYP_NS,
            PAL_HFP_DURATION_MAX_NS,
        ),
        hslen_ns: param_range(
            PAL_HSLEN_DURATION_MIN_NS,
            PAL_HSLEN_DURATION_TYP_NS,
            PAL_HSLEN_DURATION_MAX_NS,
        ),
        hbp_ns: param_range(
            PAL_HBP_DURATION_MIN_NS,
            PAL_HBP_DURATION_TYP_NS,
            PAL_HBP_DURATION_MAX_NS,
        ),
        hblk_ns: param_range(
            PAL_HBLK_DURATION_MIN_NS,
            PAL_HBLK_DURATION_TYP_NS,
            PAL_HBLK_DURATION_MAX_NS,
        ),
        bt601_hfp: 12,
        vfp_lines: param_field(3, 2),
        vslen_lines: param_field(3, 3),
        vbp_lines: param_field(19, 20),
    },
];

// upstream: include/linux/math.h DIV_ROUND_UP()
#[inline]
fn div_round_up_u32(n: u32, d: u32) -> u32 {
    n.div_ceil(d)
}

#[inline]
fn div_round_up_i32(n: i32, d: i32) -> i32 {
    (n + d - 1) / d
}

const NSEC_PER_SEC: u64 = 1_000_000_000;

// upstream: drm_modes.c fill_analog_mode()
/// # Safety
///
/// - `mode` must point to an allocated, properly aligned, and writable [`DrmDisplayMode`].
/// - The caller must ensure exclusive access to `*mode` for initialization.
unsafe fn fill_analog_mode(
    dev: *mut DrmDevice,
    mode: *mut DrmDisplayMode,
    params: &AnalogParameters,
    pixel_clock_hz: u64,
    hactive: u32,
    vactive: u32,
    interlace: bool,
) -> i32 {
    let _ = dev;
    let pixel_duration_ns = (NSEC_PER_SEC / pixel_clock_hz) as u32;
    let mut bt601 = false;

    drm_dbg_kms(&format!(
        "Generating a {}x{}{}, {}-line mode with a {} kHz clock",
        hactive,
        vactive,
        if interlace { 'i' } else { 'p' },
        params.num_lines,
        pixel_clock_hz / 1000
    ));

    let max_hact = params.hact_ns.max / pixel_duration_ns;
    if pixel_clock_hz == 13_500_000 && hactive > max_hact && hactive <= 720 {
        drm_dbg_kms("Trying to generate a BT.601 mode. Disabling checks.");
        bt601 = true;
    }

    // do_div(result, NSEC_PER_SEC)
    let result: u64 = (params.line_duration_ns as u64) * pixel_clock_hz;
    let htotal = (result / NSEC_PER_SEC) as u32;
    drm_dbg_kms(&format!("Total Horizontal Number of Pixels: {}", htotal));

    let hact_duration_ns = hactive * pixel_duration_ns;
    if !bt601 && (hact_duration_ns < params.hact_ns.min || hact_duration_ns > params.hact_ns.max) {
        drm_err(&format!(
            "Invalid horizontal active area duration: {}ns (min: {}, max {})",
            hact_duration_ns, params.hact_ns.min, params.hact_ns.max
        ));
        return -EINVAL;
    }

    let hblk = htotal - hactive;
    drm_dbg_kms(&format!("Horizontal Blanking Period: {}", hblk));

    let hblk_duration_ns = hblk * pixel_duration_ns;
    if !bt601 && (hblk_duration_ns < params.hblk_ns.min || hblk_duration_ns > params.hblk_ns.max) {
        drm_err(&format!(
            "Invalid horizontal blanking duration: {}ns (min: {}, max {})",
            hblk_duration_ns, params.hblk_ns.min, params.hblk_ns.max
        ));
        return -EINVAL;
    }

    let hslen = div_round_up_u32(params.hslen_ns.typ, pixel_duration_ns);
    drm_dbg_kms(&format!("Horizontal Sync Period: {}", hslen));

    let hslen_duration_ns = hslen * pixel_duration_ns;
    if !bt601
        && (hslen_duration_ns < params.hslen_ns.min || hslen_duration_ns > params.hslen_ns.max)
    {
        drm_err(&format!(
            "Invalid horizontal sync duration: {}ns (min: {}, max {})",
            hslen_duration_ns, params.hslen_ns.min, params.hslen_ns.max
        ));
        return -EINVAL;
    }

    let mut porches = hblk - hslen;
    drm_dbg_kms(&format!(
        "Remaining horizontal pixels for both porches: {}",
        porches
    ));

    let porches_duration_ns = porches * pixel_duration_ns;
    if !bt601
        && (porches_duration_ns > params.hfp_ns.max + params.hbp_ns.max
            || porches_duration_ns < params.hfp_ns.min + params.hbp_ns.min)
    {
        drm_err(&format!(
            "Invalid horizontal porches duration: {}ns",
            porches_duration_ns
        ));
        return -EINVAL;
    }

    let hfp: u32 = if bt601 {
        params.bt601_hfp
    } else {
        let hfp_min = div_round_up_u32(params.hfp_ns.min, pixel_duration_ns);
        let hbp_min = div_round_up_u32(params.hbp_ns.min, pixel_duration_ns);
        let porches_rem = porches as i32 - hfp_min as i32 - hbp_min as i32;
        (hfp_min as i32 + div_round_up_i32(porches_rem, 2)) as u32
    };
    drm_dbg_kms(&format!("Horizontal Front Porch: {}", hfp));

    let hfp_duration_ns = hfp * pixel_duration_ns;
    if !bt601 && (hfp_duration_ns < params.hfp_ns.min || hfp_duration_ns > params.hfp_ns.max) {
        drm_err(&format!(
            "Invalid horizontal front porch duration: {}ns (min: {}, max {})",
            hfp_duration_ns, params.hfp_ns.min, params.hfp_ns.max
        ));
        return -EINVAL;
    }

    let hbp = porches - hfp;
    drm_dbg_kms(&format!("Horizontal Back Porch: {}", hbp));

    let hbp_duration_ns = hbp * pixel_duration_ns;
    if !bt601 && (hbp_duration_ns < params.hbp_ns.min || hbp_duration_ns > params.hbp_ns.max) {
        drm_err(&format!(
            "Invalid horizontal back porch duration: {}ns (min: {}, max {})",
            hbp_duration_ns, params.hbp_ns.min, params.hbp_ns.max
        ));
        return -EINVAL;
    }

    if htotal != hactive + hfp + hslen + hbp {
        return -EINVAL;
    }

    // SAFETY: mode points to a valid, properly aligned writable DrmDisplayMode per caller contract,
    // exclusively accessible for initialization.
    let m = unsafe { &mut *mode };
    m.clock = (pixel_clock_hz / 1000) as i32;
    m.hdisplay = hactive as u16;
    m.hsync_start = (m.hdisplay as u32 + hfp) as u16;
    m.hsync_end = (m.hsync_start as u32 + hslen) as u16;
    m.htotal = (m.hsync_end as u32 + hbp) as u16;

    let (vfp_min, vbp_min, vslen);
    if interlace {
        vfp_min = params.vfp_lines.even + params.vfp_lines.odd;
        vbp_min = params.vbp_lines.even + params.vbp_lines.odd;
        vslen = params.vslen_lines.even + params.vslen_lines.odd;
    } else {
        vfp_min = params.vfp_lines.odd;
        vbp_min = params.vbp_lines.odd;
        vslen = params.vslen_lines.odd;
    }
    drm_dbg_kms(&format!("Vertical Sync Period: {}", vslen));

    porches = params.num_lines - vactive - vslen;
    drm_dbg_kms(&format!(
        "Remaining vertical pixels for both porches: {}",
        porches
    ));

    let porches_rem = porches as i32 - vfp_min as i32 - vbp_min as i32;
    let vfp = (vfp_min as i32 + porches_rem / 2) as u32;
    drm_dbg_kms(&format!("Vertical Front Porch: {}", vfp));

    let vbp = porches - vfp;
    drm_dbg_kms(&format!("Vertical Back Porch: {}", vbp));

    let vtotal = vactive + vfp + vslen + vbp;
    if params.num_lines != vtotal {
        drm_err(&format!(
            "Invalid vertical total: {}px (expected {}px)",
            vtotal, params.num_lines
        ));
        return -EINVAL;
    }

    m.vdisplay = vactive as u16;
    m.vsync_start = (m.vdisplay as u32 + vfp) as u16;
    m.vsync_end = (m.vsync_start as u32 + vslen) as u16;
    m.vtotal = (m.vsync_end as u32 + vbp) as u16;
    if m.vtotal as u32 != params.num_lines {
        return -EINVAL;
    }

    m.r#type = DRM_MODE_TYPE_DRIVER as u8;
    m.flags = DRM_MODE_FLAG_NVSYNC | DRM_MODE_FLAG_NHSYNC;
    if interlace {
        m.flags |= DRM_MODE_FLAG_INTERLACE;
    }

    // SAFETY: mode points to the valid DrmDisplayMode being initialized, with valid timing fields set above.
    unsafe { drm_mode_set_name(mode) };
    drm_dbg_kms(&format!("Generated mode {}", mode_fmt(mode)));
    0
}

// upstream: drm_modes.c drm_analog_tv_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` may be null or point to a valid [`DrmDevice`]. If successful, returns a newly allocated,
/// initialized [`DrmDisplayMode`] that must eventually be freed via [`drm_mode_destroy`].
pub unsafe extern "C" fn drm_analog_tv_mode(
    dev: *mut DrmDevice,
    tv_mode: DrmConnectorTvMode,
    pixel_clock_hz: u64,
    hdisplay: u32,
    vdisplay: u32,
    interlace: bool,
) -> *mut DrmDisplayMode {
    let analog = match tv_mode {
        DRM_MODE_TV_MODE_NTSC
        | DRM_MODE_TV_MODE_NTSC_443
        | DRM_MODE_TV_MODE_NTSC_J
        | DRM_MODE_TV_MODE_PAL_M => DRM_MODE_ANALOG_NTSC,
        DRM_MODE_TV_MODE_PAL
        | DRM_MODE_TV_MODE_PAL_N
        | DRM_MODE_TV_MODE_SECAM
        | DRM_MODE_TV_MODE_MONOCHROME => DRM_MODE_ANALOG_PAL,
        _ => return core::ptr::null_mut(),
    };

    // SAFETY: drm_mode_create allocates a zero-initialized DrmDisplayMode for dev.
    let mode = unsafe { drm_mode_create(dev) };
    if mode.is_null() {
        return core::ptr::null_mut();
    }

    // SAFETY: mode is a newly allocated non-null DrmDisplayMode, uniquely owned and valid for initialization.
    let ret = unsafe {
        fill_analog_mode(
            dev,
            mode,
            &TV_MODES_PARAMETERS[analog],
            pixel_clock_hz,
            hdisplay,
            vdisplay,
            interlace,
        )
    };
    if ret != 0 {
        // SAFETY: mode was allocated by drm_mode_create above and failed initialization; destroying it frees the allocation.
        unsafe { drm_mode_destroy(dev, mode) };
        return core::ptr::null_mut();
    }

    mode
}

// upstream: drm_modes.c HV_FACTOR, CVT_* timing constants
const HV_FACTOR: i32 = 1000;
const CVT_MARGIN_PERCENTAGE: i32 = 18;
const CVT_H_GRANULARITY: i32 = 8;
const CVT_MIN_V_PORCH: i32 = 3;
const CVT_MIN_V_BPORCH: i32 = 6;
const CVT_CLOCK_STEP: i32 = 250;
const CVT_MIN_VSYNC_BP: i32 = 550;
const CVT_HSYNC_PERCENTAGE: i32 = 8;
const CVT_M_FACTOR: i32 = 600;
const CVT_C_FACTOR: i32 = 40;
const CVT_K_FACTOR: i32 = 128;
const CVT_J_FACTOR: i32 = 20;
const CVT_M_PRIME: i32 = CVT_M_FACTOR * CVT_K_FACTOR / 256;
const CVT_C_PRIME: i32 = (CVT_C_FACTOR - CVT_J_FACTOR) * CVT_K_FACTOR / 256 + CVT_J_FACTOR;
const CVT_RB_MIN_VBLANK: i32 = 460;
const CVT_RB_H_SYNC: i32 = 32;
const CVT_RB_H_BLANK: i32 = 160;
const CVT_RB_VFPORCH: i32 = 3;

// upstream: drm_modes.c drm_cvt_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` may be null or point to a valid [`DrmDevice`]. If allocation succeeds, returns a newly
/// allocated, initialized [`DrmDisplayMode`] that must eventually be freed via [`drm_mode_destroy`].
pub unsafe extern "C" fn drm_cvt_mode(
    dev: *mut DrmDevice,
    hdisplay: i32,
    vdisplay: i32,
    mut vrefresh: i32,
    reduced: bool,
    interlaced: bool,
    margins: bool,
) -> *mut DrmDisplayMode {
    if hdisplay == 0 || vdisplay == 0 {
        return core::ptr::null_mut();
    }

    // SAFETY: drm_mode_create allocates a zero-initialized DrmDisplayMode for dev.
    let drm_mode = unsafe { drm_mode_create(dev) };
    if drm_mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: drm_mode is checked non-null above, properly aligned, and uniquely owned for initialization.
    let m = unsafe { &mut *drm_mode };

    if vrefresh == 0 {
        vrefresh = 60;
    }

    // unsigned int vfieldrate
    let vfieldrate: u32 = if interlaced {
        (vrefresh * 2) as u32
    } else {
        vrefresh as u32
    };

    let hdisplay_rnd = hdisplay - (hdisplay % CVT_H_GRANULARITY);
    let mut hmargin = 0i32;
    if margins {
        hmargin = hdisplay_rnd * CVT_MARGIN_PERCENTAGE / 1000;
        hmargin -= hmargin % CVT_H_GRANULARITY;
    }
    m.hdisplay = (hdisplay_rnd + 2 * hmargin) as u16;

    let vdisplay_rnd = if interlaced { vdisplay / 2 } else { vdisplay };

    let mut vmargin = 0i32;
    if margins {
        vmargin = vdisplay_rnd * CVT_MARGIN_PERCENTAGE / 1000;
    }
    m.vdisplay = (vdisplay + 2 * vmargin) as u16;

    let interlace: i32 = if interlaced { 1 } else { 0 };

    let vsync: i32 = if vdisplay % 3 == 0 && (vdisplay * 4 / 3) == hdisplay {
        4
    } else if vdisplay % 9 == 0 && (vdisplay * 16 / 9) == hdisplay {
        5
    } else if vdisplay % 10 == 0 && (vdisplay * 16 / 10) == hdisplay {
        6
    } else if (vdisplay % 4 == 0 && (vdisplay * 5 / 4) == hdisplay)
        || (vdisplay % 9 == 0 && (vdisplay * 15 / 9) == hdisplay)
    {
        7
    } else {
        10
    };

    let hperiod: u32;
    if !reduced {
        let tmp1: i32 = HV_FACTOR.wrapping_mul(1_000_000).wrapping_sub(
            CVT_MIN_VSYNC_BP
                .wrapping_mul(HV_FACTOR)
                .wrapping_mul(vfieldrate as i32),
        );
        let tmp2: i32 = (vdisplay_rnd + 2 * vmargin + CVT_MIN_V_PORCH) * 2 + interlace;
        // int / unsigned: the division is performed unsigned
        hperiod = ((tmp1.wrapping_mul(2)) as u32) / ((tmp2 as u32).wrapping_mul(vfieldrate));
        let tmp1 = (CVT_MIN_VSYNC_BP * HV_FACTOR) as u32 / hperiod + 1;
        let tmp1 = tmp1 as i32;
        let vsyncandback_porch = if tmp1 < vsync + CVT_MIN_V_PORCH {
            vsync + CVT_MIN_V_PORCH
        } else {
            tmp1
        };
        let _vback_porch = vsyncandback_porch - vsync;
        m.vtotal = (vdisplay_rnd + 2 * vmargin + vsyncandback_porch + CVT_MIN_V_PORCH) as u16;

        // unsigned int hblank_percentage
        let mut hblank_percentage: u32 =
            (CVT_C_PRIME * HV_FACTOR) as u32 - ((CVT_M_PRIME as u32).wrapping_mul(hperiod) / 1000);
        if hblank_percentage < (20 * HV_FACTOR) as u32 {
            hblank_percentage = (20 * HV_FACTOR) as u32;
        }
        // int hblank = hdisplay * hblank_percentage / (100 * HV_FACTOR - hblank_percentage)
        let hblank = ((m.hdisplay as u32).wrapping_mul(hblank_percentage)
            / ((100 * HV_FACTOR) as u32 - hblank_percentage)) as i32;
        let hblank = hblank - hblank % (2 * CVT_H_GRANULARITY);

        m.htotal = (m.hdisplay as i32 + hblank) as u16;
        m.hsync_end = (m.hdisplay as i32 + hblank / 2) as u16;
        m.hsync_start =
            (m.hsync_end as i32 - (m.htotal as i32 * CVT_HSYNC_PERCENTAGE) / 100) as u16;
        m.hsync_start = (m.hsync_start as i32 + CVT_H_GRANULARITY
            - (m.hsync_start as i32 % CVT_H_GRANULARITY)) as u16;
        m.vsync_start = (m.vdisplay as i32 + CVT_MIN_V_PORCH) as u16;
        m.vsync_end = (m.vsync_start as i32 + vsync) as u16;
    } else {
        let tmp1: i32 = HV_FACTOR.wrapping_mul(1_000_000).wrapping_sub(
            CVT_RB_MIN_VBLANK
                .wrapping_mul(HV_FACTOR)
                .wrapping_mul(vfieldrate as i32),
        );
        let tmp2: i32 = vdisplay_rnd + 2 * vmargin;
        hperiod = (tmp1 as u32) / ((tmp2 as u32).wrapping_mul(vfieldrate));

        let mut vbilines = ((CVT_RB_MIN_VBLANK * HV_FACTOR) as u32 / hperiod + 1) as i32;
        if vbilines < CVT_RB_VFPORCH + vsync + CVT_MIN_V_BPORCH {
            vbilines = CVT_RB_VFPORCH + vsync + CVT_MIN_V_BPORCH;
        }
        m.vtotal = (vdisplay_rnd + 2 * vmargin + vbilines) as u16;
        m.htotal = (m.hdisplay as i32 + CVT_RB_H_BLANK) as u16;
        m.hsync_end = (m.hdisplay as i32 + CVT_RB_H_BLANK / 2) as u16;
        m.hsync_start = (m.hsync_end as i32 - CVT_RB_H_SYNC) as u16;
        m.vsync_start = (m.vdisplay as i32 + CVT_RB_VFPORCH) as u16;
        m.vsync_end = (m.vsync_start as i32 + vsync) as u16;
    }

    // tmp = htotal * (HV_FACTOR * 1000) / hperiod; tmp -= clock % CVT_CLOCK_STEP
    let mut tmp: u64 = m.htotal as u64;
    tmp *= (HV_FACTOR * 1000) as u64;
    tmp /= hperiod as u64;
    tmp = tmp.wrapping_sub((m.clock % CVT_CLOCK_STEP) as i64 as u64);
    m.clock = tmp as i32;

    if interlaced {
        m.vtotal = m.vtotal.wrapping_mul(2);
        m.flags |= DRM_MODE_FLAG_INTERLACE;
    }

    // SAFETY: drm_mode is valid, uniquely owned, and has its timing fields populated above.
    unsafe { drm_mode_set_name(drm_mode) };

    if reduced {
        m.flags |= DRM_MODE_FLAG_PHSYNC | DRM_MODE_FLAG_NVSYNC;
    } else {
        m.flags |= DRM_MODE_FLAG_PVSYNC | DRM_MODE_FLAG_NHSYNC;
    }

    drm_mode
}

// upstream: drm_modes.c GTF_* timing constants
const GTF_MARGIN_PERCENTAGE: u32 = 18;
const GTF_CELL_GRAN: u32 = 8;
const GTF_MIN_V_PORCH: u32 = 1;
const V_SYNC_RQD: u32 = 3;
const H_SYNC_PERCENT: u32 = 8;
const MIN_VSYNC_PLUS_BP: u32 = 550;

// upstream: drm_modes.c drm_gtf_mode_complex()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` may be null or point to a valid [`DrmDevice`]. If allocation succeeds, returns a newly
/// allocated, initialized [`DrmDisplayMode`] that must eventually be freed via [`drm_mode_destroy`].
pub unsafe extern "C" fn drm_gtf_mode_complex(
    dev: *mut DrmDevice,
    hdisplay: i32,
    vdisplay: i32,
    vrefresh: i32,
    interlaced: bool,
    margins: i32,
    gtf_m: i32,
    gtf_2c: i32,
    gtf_k: i32,
    gtf_2j: i32,
) -> *mut DrmDisplayMode {
    // GTF_C_PRIME / GTF_M_PRIME with int arithmetic
    let gtf_c_prime: u32 = ((((gtf_2c - gtf_2j) * gtf_k / 256) + gtf_2j) / 2) as u32;
    let gtf_m_prime: u32 = (gtf_k * gtf_m / 256) as u32;

    if hdisplay == 0 || vdisplay == 0 {
        return core::ptr::null_mut();
    }

    // SAFETY: drm_mode_create allocates a zero-initialized DrmDisplayMode for dev.
    let drm_mode = unsafe { drm_mode_create(dev) };
    if drm_mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: drm_mode is checked non-null above, properly aligned, and uniquely owned for initialization.
    let m = unsafe { &mut *drm_mode };

    let hdisplay_rnd = ((hdisplay as u32) + GTF_CELL_GRAN / 2) / GTF_CELL_GRAN * GTF_CELL_GRAN;
    let vdisplay_rnd: u32 = if interlaced {
        vdisplay as u32 / 2
    } else {
        vdisplay as u32
    };
    let vfieldrate_rqd: u32 = if interlaced {
        (vrefresh * 2) as u32
    } else {
        vrefresh as u32
    };

    let mut top_margin: u32 = 0;
    if margins != 0 {
        top_margin = (vdisplay_rnd * GTF_MARGIN_PERCENTAGE + 500) / 1000;
    }
    let bottom_margin = top_margin;
    let interlace: u32 = if interlaced { 1 } else { 0 };

    let hfreq_est: u32;
    {
        let tmp1 = (1_000_000u32 - MIN_VSYNC_PLUS_BP * vfieldrate_rqd) / 500;
        let tmp2 = (vdisplay_rnd + 2 * top_margin + GTF_MIN_V_PORCH) * 2 + interlace;
        hfreq_est = (tmp2 * 1000 * vfieldrate_rqd) / tmp1;
    }

    let mut vsync_plus_bp = (MIN_VSYNC_PLUS_BP * hfreq_est / 1000) as i32;
    vsync_plus_bp = (vsync_plus_bp + 500) / 1000;
    let _vback_porch = vsync_plus_bp - V_SYNC_RQD as i32;
    let vtotal_lines =
        vdisplay_rnd + top_margin + bottom_margin + vsync_plus_bp as u32 + GTF_MIN_V_PORCH;
    let _vfieldrate_est = hfreq_est / vtotal_lines;
    let _hperiod = 1_000_000 / (vfieldrate_rqd * vtotal_lines);
    let vfield_rate = hfreq_est / vtotal_lines;
    let _vframe_rate = if interlaced {
        vfield_rate / 2
    } else {
        vfield_rate
    };

    let left_margin: u32 = if margins != 0 {
        (hdisplay_rnd * GTF_MARGIN_PERCENTAGE + 500) / 1000
    } else {
        0
    };
    let right_margin = left_margin;
    let total_active_pixels = hdisplay_rnd + left_margin + right_margin;
    let ideal_duty_cycle = gtf_c_prime * 1000 - (gtf_m_prime * 1_000_000 / hfreq_est);
    let mut hblank = total_active_pixels * ideal_duty_cycle / (100_000 - ideal_duty_cycle);
    hblank = (hblank + GTF_CELL_GRAN) / (2 * GTF_CELL_GRAN);
    hblank = hblank * 2 * GTF_CELL_GRAN;
    let total_pixels = total_active_pixels + hblank;
    let pixel_freq = total_pixels * hfreq_est / 1000;

    let mut hsync = H_SYNC_PERCENT * total_pixels / 100;
    hsync = (hsync + GTF_CELL_GRAN / 2) / GTF_CELL_GRAN;
    hsync *= GTF_CELL_GRAN;
    let hfront_porch = (hblank / 2) as i32 - hsync as i32;
    let vodd_front_porch_lines = GTF_MIN_V_PORCH;

    m.hdisplay = hdisplay_rnd as u16;
    m.hsync_start = (hdisplay_rnd as i32 + hfront_porch) as u16;
    m.hsync_end = (m.hsync_start as u32 + hsync) as u16;
    m.htotal = total_pixels as u16;
    m.vdisplay = vdisplay_rnd as u16;
    m.vsync_start = (vdisplay_rnd + vodd_front_porch_lines) as u16;
    m.vsync_end = m.vsync_start.wrapping_add(V_SYNC_RQD as u16);
    m.vtotal = vtotal_lines as u16;
    m.clock = pixel_freq as i32;

    if interlaced {
        m.vtotal = m.vtotal.wrapping_mul(2);
        m.flags |= DRM_MODE_FLAG_INTERLACE;
    }

    // SAFETY: drm_mode is valid, uniquely owned, and has its timing fields populated above.
    unsafe { drm_mode_set_name(drm_mode) };

    if gtf_m == 600 && gtf_2c == 80 && gtf_k == 128 && gtf_2j == 40 {
        m.flags = DRM_MODE_FLAG_NHSYNC | DRM_MODE_FLAG_PVSYNC;
    } else {
        m.flags = DRM_MODE_FLAG_PHSYNC | DRM_MODE_FLAG_NVSYNC;
    }

    drm_mode
}

// upstream: drm_modes.c drm_gtf_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` may be null or point to a valid [`DrmDevice`]. If allocation succeeds, returns a newly
/// allocated, initialized [`DrmDisplayMode`] that must eventually be freed via [`drm_mode_destroy`].
pub unsafe extern "C" fn drm_gtf_mode(
    dev: *mut DrmDevice,
    hdisplay: i32,
    vdisplay: i32,
    vrefresh: i32,
    interlaced: bool,
    margins: i32,
) -> *mut DrmDisplayMode {
    // SAFETY: Forwards dev and timing parameters to drm_gtf_mode_complex with default GTF parameters.
    unsafe {
        drm_gtf_mode_complex(
            dev,
            hdisplay,
            vdisplay,
            vrefresh,
            interlaced,
            margins,
            600,
            40 * 2,
            128,
            20 * 2,
        )
    }
}

// upstream: include/drm/drm_modes.h CRTC_* adjust flags (drm_mode_set_crtcinfo)
pub(crate) const CRTC_INTERLACE_HALVE_V: u32 = 1 << 0;
pub(crate) const CRTC_STEREO_DOUBLE: u32 = 1 << 1;
pub(crate) const CRTC_NO_DBLSCAN: u32 = 1 << 2;
pub(crate) const CRTC_NO_VSCAN: u32 = 1 << 3;
pub(crate) const CRTC_STEREO_DOUBLE_ONLY: u32 =
    CRTC_STEREO_DOUBLE | CRTC_NO_DBLSCAN | CRTC_NO_VSCAN;

// upstream: include/drm/drm_modes.h DRM_MODE_MATCH_* (drm_mode_match)
// upstream: include/uapi/drm/drm_mode.h DRM_MODE_FLAG_3D_MAX
pub(crate) const DRM_MODE_FLAG_3D_MAX: u32 = DRM_MODE_FLAG_3D_SIDE_BY_SIDE_HALF;

pub(crate) const DRM_MODE_MATCH_TIMINGS: u32 = 1 << 0;
pub(crate) const DRM_MODE_MATCH_CLOCK: u32 = 1 << 1;
pub(crate) const DRM_MODE_MATCH_FLAGS: u32 = 1 << 2;
pub(crate) const DRM_MODE_MATCH_3D_FLAGS: u32 = 1 << 3;
pub(crate) const DRM_MODE_MATCH_ASPECT_RATIO: u32 = 1 << 4;

// upstream: include/drm/drm_modes.h KHZ2PICOS()
#[inline]
fn khz2picos(a: i32) -> u32 {
    (1_000_000_000u32).wrapping_div(a as u32)
}

// upstream: drm_modes.c drm_mode_set_name()
#[unsafe(no_mangle)]
/// # Safety
///
/// `mode` must point to a valid, properly aligned, and writable [`DrmDisplayMode`].
pub unsafe extern "C" fn drm_mode_set_name(mode: *mut DrmDisplayMode) {
    // SAFETY: mode is valid, properly aligned, and exclusively accessible per caller contract.
    let m = unsafe { &mut *mode };
    let interlaced = m.flags & DRM_MODE_FLAG_INTERLACE != 0;
    let text = format!(
        "{}x{}{}",
        m.hdisplay,
        m.vdisplay,
        if interlaced { "i" } else { "" }
    );
    // snprintf(name, DRM_DISPLAY_MODE_LEN, ...): truncate to 31 bytes + NUL
    let bytes = text.as_bytes();
    let n = bytes.len().min(DRM_DISPLAY_MODE_LEN as usize - 1);
    for (i, slot) in m.name.iter_mut().enumerate() {
        *slot = if i < n { bytes[i] as c_char } else { 0 };
    }
}

// upstream: drm_modes.c drm_mode_vrefresh()
#[unsafe(no_mangle)]
/// # Safety
///
/// `mode` must point to a valid, properly aligned, and initialized [`DrmDisplayMode`] that remains
/// readable for the duration of the call.
pub unsafe extern "C" fn drm_mode_vrefresh(mode: *const DrmDisplayMode) -> i32 {
    // SAFETY: mode is valid, properly aligned, and initialized per caller contract.
    let m = unsafe { &*mode };
    let mut num: u32 = 1;
    let mut den: u32 = 1;

    if m.htotal == 0 || m.vtotal == 0 {
        return 0;
    }
    if m.flags & DRM_MODE_FLAG_INTERLACE != 0 {
        num *= 2;
    }
    if m.flags & DRM_MODE_FLAG_DBLSCAN != 0 {
        den *= 2;
    }
    if m.vscan > 1 {
        den *= m.vscan as u32;
    }

    // check_mul_overflow(mode->clock, num, &num)
    let n = (m.clock as i128) * (num as i128);
    if n < 0 || n > u32::MAX as i128 {
        return 0;
    }
    num = n as u32;

    // check_mul_overflow(mode->htotal * mode->vtotal, den, &den)
    let d = (m.htotal as i128) * (m.vtotal as i128) * (den as i128);
    if d < 0 || d > u32::MAX as i128 {
        return 0;
    }
    den = d as u32;

    // DIV_ROUND_CLOSEST_ULL(mul_u32_u32(num, 1000), den)
    let x = (num as u64) * 1000;
    ((x + (den as u64) / 2) / den as u64) as i32
}

// upstream: drm_modes.c drm_mode_get_hv_timing()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `mode` must point to a valid, initialized [`DrmDisplayMode`] readable for the duration of the call.
/// - `hdisplay` and `vdisplay` must point to valid, properly aligned, writable `i32` locations.
pub unsafe extern "C" fn drm_mode_get_hv_timing(
    mode: *const DrmDisplayMode,
    hdisplay: *mut i32,
    vdisplay: *mut i32,
) {
    let mut adjusted = core::mem::MaybeUninit::<DrmDisplayMode>::uninit();
    let adjusted = adjusted.as_mut_ptr();
    // SAFETY: adjusted points to locally declared uninitialized DrmDisplayMode storage, mode is valid
    // and readable, and hdisplay/vdisplay are valid writable pointers per caller contract.
    unsafe {
        drm_mode_init(adjusted, mode);
        drm_mode_set_crtcinfo(adjusted, CRTC_STEREO_DOUBLE_ONLY as i32);
        *hdisplay = (*adjusted).crtc_hdisplay as i32;
        *vdisplay = (*adjusted).crtc_vdisplay as i32;
    }
}

// upstream: drm_modes.c drm_mode_set_crtcinfo()
#[unsafe(no_mangle)]
/// # Safety
///
/// `p` may be null or must point to a valid, properly aligned, and writable [`DrmDisplayMode`].
pub unsafe extern "C" fn drm_mode_set_crtcinfo(p: *mut DrmDisplayMode, adjust_flags: i32) {
    if p.is_null() {
        return;
    }
    let adjust_flags = adjust_flags as u32;
    // SAFETY: p is checked non-null above, properly aligned, and exclusively accessible for writing per caller contract.
    let p = unsafe { &mut *p };

    // u16 arithmetic: C promotes to int and truncates on store.
    fn mul16(a: u16, b: u16) -> u16 {
        ((a as u32) * (b as u32)) as u16
    }

    p.crtc_clock = p.clock;
    p.crtc_hdisplay = p.hdisplay;
    p.crtc_hsync_start = p.hsync_start;
    p.crtc_hsync_end = p.hsync_end;
    p.crtc_htotal = p.htotal;
    p.crtc_hskew = p.hskew;
    p.crtc_vdisplay = p.vdisplay;
    p.crtc_vsync_start = p.vsync_start;
    p.crtc_vsync_end = p.vsync_end;
    p.crtc_vtotal = p.vtotal;

    if p.flags & DRM_MODE_FLAG_INTERLACE != 0 && adjust_flags & CRTC_INTERLACE_HALVE_V != 0 {
        p.crtc_vdisplay /= 2;
        p.crtc_vsync_start /= 2;
        p.crtc_vsync_end /= 2;
        p.crtc_vtotal /= 2;
    }

    if adjust_flags & CRTC_NO_DBLSCAN == 0 && p.flags & DRM_MODE_FLAG_DBLSCAN != 0 {
        p.crtc_vdisplay = mul16(p.crtc_vdisplay, 2);
        p.crtc_vsync_start = mul16(p.crtc_vsync_start, 2);
        p.crtc_vsync_end = mul16(p.crtc_vsync_end, 2);
        p.crtc_vtotal = mul16(p.crtc_vtotal, 2);
    }

    if adjust_flags & CRTC_NO_VSCAN == 0 && p.vscan > 1 {
        let v = p.vscan;
        p.crtc_vdisplay = mul16(p.crtc_vdisplay, v);
        p.crtc_vsync_start = mul16(p.crtc_vsync_start, v);
        p.crtc_vsync_end = mul16(p.crtc_vsync_end, v);
        p.crtc_vtotal = mul16(p.crtc_vtotal, v);
    }

    if adjust_flags & CRTC_STEREO_DOUBLE != 0 {
        let layout = p.flags & DRM_MODE_FLAG_3D_MASK;
        if layout == DRM_MODE_FLAG_3D_FRAME_PACKING {
            p.crtc_clock *= 2;
            p.crtc_vdisplay = p.crtc_vdisplay.wrapping_add(p.crtc_vtotal);
            p.crtc_vsync_start = p.crtc_vsync_start.wrapping_add(p.crtc_vtotal);
            p.crtc_vsync_end = p.crtc_vsync_end.wrapping_add(p.crtc_vtotal);
            p.crtc_vtotal = p.crtc_vtotal.wrapping_add(p.crtc_vtotal);
        }
    }

    p.crtc_vblank_start = p.crtc_vsync_start.min(p.crtc_vdisplay);
    p.crtc_vblank_end = p.crtc_vsync_end.max(p.crtc_vtotal);
    p.crtc_hblank_start = p.crtc_hsync_start.min(p.crtc_hdisplay);
    p.crtc_hblank_end = p.crtc_hsync_end.max(p.crtc_htotal);
}

// upstream: drm_modes.c drm_mode_copy()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dst` must point to a valid, properly aligned, writable [`DrmDisplayMode`].
/// - `src` must point to a valid, properly aligned, readable [`DrmDisplayMode`].
/// - `dst` and `src` must not overlap.
pub unsafe extern "C" fn drm_mode_copy(dst: *mut DrmDisplayMode, src: *const DrmDisplayMode) {
    // struct list_head head = dst->head; *dst = *src; dst->head = head;
    // SAFETY: dst points to a valid DrmDisplayMode per caller contract; preserving existing list linkage.
    let head = unsafe {
        ListHead {
            next: (*dst).head.next,
            prev: (*dst).head.prev,
        }
    };
    // SAFETY: src and dst point to valid, properly aligned DrmDisplayMode values of size 1,
    // non-overlapping per caller contract; restoring head preserves dst's list linkage.
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, 1);
        (*dst).head = head;
    }
}

// upstream: drm_modes.c drm_mode_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dst` must point to a properly aligned, writable memory location of at least `size_of::<DrmDisplayMode>()`.
/// - `src` must point to a valid, initialized [`DrmDisplayMode`].
/// - `dst` and `src` must not overlap.
pub unsafe extern "C" fn drm_mode_init(dst: *mut DrmDisplayMode, src: *const DrmDisplayMode) {
    // SAFETY: dst is valid and aligned for writing a DrmDisplayMode; write_bytes zero-initializes it,
    // and drm_mode_copy copies src into dst.
    unsafe {
        core::ptr::write_bytes(dst, 0, 1);
        drm_mode_copy(dst, src);
    }
}

// upstream: drm_modes.c drm_mode_duplicate()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` may be null or point to a valid [`DrmDevice`].
/// - `mode` must point to a valid, initialized [`DrmDisplayMode`] readable for the call.
///
/// If allocation succeeds, returns a newly allocated duplicate [`DrmDisplayMode`] that must be freed via [`drm_mode_destroy`].
pub unsafe extern "C" fn drm_mode_duplicate(
    dev: *mut DrmDevice,
    mode: *const DrmDisplayMode,
) -> *mut DrmDisplayMode {
    // SAFETY: drm_mode_create allocates a new zero-initialized DrmDisplayMode for dev.
    let nmode = unsafe { drm_mode_create(dev) };
    if nmode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: nmode is checked non-null above and newly allocated; mode is valid and readable per caller contract.
    unsafe { drm_mode_copy(nmode, mode) };
    nmode
}

// upstream: drm_modes.c drm_mode_match_timings()
fn drm_mode_match_timings(m1: &DrmDisplayMode, m2: &DrmDisplayMode) -> bool {
    m1.hdisplay == m2.hdisplay
        && m1.hsync_start == m2.hsync_start
        && m1.hsync_end == m2.hsync_end
        && m1.htotal == m2.htotal
        && m1.hskew == m2.hskew
        && m1.vdisplay == m2.vdisplay
        && m1.vsync_start == m2.vsync_start
        && m1.vsync_end == m2.vsync_end
        && m1.vtotal == m2.vtotal
        && m1.vscan == m2.vscan
}

// upstream: drm_modes.c drm_mode_match_clock()
fn drm_mode_match_clock(m1: &DrmDisplayMode, m2: &DrmDisplayMode) -> bool {
    if m1.clock != 0 && m2.clock != 0 {
        khz2picos(m1.clock) == khz2picos(m2.clock)
    } else {
        m1.clock == m2.clock
    }
}

// upstream: drm_modes.c drm_mode_match_flags()
fn drm_mode_match_flags(m1: &DrmDisplayMode, m2: &DrmDisplayMode) -> bool {
    (m1.flags & !DRM_MODE_FLAG_3D_MASK) == (m2.flags & !DRM_MODE_FLAG_3D_MASK)
}

// upstream: drm_modes.c drm_mode_match_3d_flags()
fn drm_mode_match_3d_flags(m1: &DrmDisplayMode, m2: &DrmDisplayMode) -> bool {
    (m1.flags & DRM_MODE_FLAG_3D_MASK) == (m2.flags & DRM_MODE_FLAG_3D_MASK)
}

// upstream: drm_modes.c drm_mode_match_aspect_ratio()
fn drm_mode_match_aspect_ratio(m1: &DrmDisplayMode, m2: &DrmDisplayMode) -> bool {
    m1.picture_aspect_ratio == m2.picture_aspect_ratio
}

// upstream: drm_modes.c drm_mode_match()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `mode1` and `mode2` must point to valid, properly aligned, and readable [`DrmDisplayMode`] objects.
pub unsafe extern "C" fn drm_mode_match(
    mode1: *const DrmDisplayMode,
    mode2: *const DrmDisplayMode,
    match_flags: u32,
) -> bool {
    if mode1.is_null() && mode2.is_null() {
        return true;
    }
    if mode1.is_null() || mode2.is_null() {
        return false;
    }
    // SAFETY: mode1 and mode2 are checked non-null above, properly aligned, and readable per caller contract.
    let (m1, m2) = unsafe { (&*mode1, &*mode2) };

    if match_flags & DRM_MODE_MATCH_TIMINGS != 0 && !drm_mode_match_timings(m1, m2) {
        return false;
    }
    if match_flags & DRM_MODE_MATCH_CLOCK != 0 && !drm_mode_match_clock(m1, m2) {
        return false;
    }
    if match_flags & DRM_MODE_MATCH_FLAGS != 0 && !drm_mode_match_flags(m1, m2) {
        return false;
    }
    if match_flags & DRM_MODE_MATCH_3D_FLAGS != 0 && !drm_mode_match_3d_flags(m1, m2) {
        return false;
    }
    if match_flags & DRM_MODE_MATCH_ASPECT_RATIO != 0 && !drm_mode_match_aspect_ratio(m1, m2) {
        return false;
    }
    true
}

// upstream: drm_modes.c drm_mode_equal()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `mode1` and `mode2` must point to valid, properly aligned, and readable [`DrmDisplayMode`] objects.
pub unsafe extern "C" fn drm_mode_equal(
    mode1: *const DrmDisplayMode,
    mode2: *const DrmDisplayMode,
) -> bool {
    // SAFETY: Forwards mode1 and mode2 to drm_mode_match with full matching flags.
    unsafe {
        drm_mode_match(
            mode1,
            mode2,
            DRM_MODE_MATCH_TIMINGS
                | DRM_MODE_MATCH_CLOCK
                | DRM_MODE_MATCH_FLAGS
                | DRM_MODE_MATCH_3D_FLAGS
                | DRM_MODE_MATCH_ASPECT_RATIO,
        )
    }
}

// upstream: drm_modes.c drm_mode_equal_no_clocks()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `mode1` and `mode2` must point to valid, properly aligned, and readable [`DrmDisplayMode`] objects.
pub unsafe extern "C" fn drm_mode_equal_no_clocks(
    mode1: *const DrmDisplayMode,
    mode2: *const DrmDisplayMode,
) -> bool {
    // SAFETY: Forwards mode1 and mode2 to drm_mode_match ignoring clock match flags.
    unsafe {
        drm_mode_match(
            mode1,
            mode2,
            DRM_MODE_MATCH_TIMINGS | DRM_MODE_MATCH_FLAGS | DRM_MODE_MATCH_3D_FLAGS,
        )
    }
}

// upstream: drm_modes.c drm_mode_equal_no_clocks_no_stereo()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `mode1` and `mode2` must point to valid, properly aligned, and readable [`DrmDisplayMode`] objects.
pub unsafe extern "C" fn drm_mode_equal_no_clocks_no_stereo(
    mode1: *const DrmDisplayMode,
    mode2: *const DrmDisplayMode,
) -> bool {
    // SAFETY: Forwards mode1 and mode2 to drm_mode_match comparing only timings and flags.
    unsafe { drm_mode_match(mode1, mode2, DRM_MODE_MATCH_TIMINGS | DRM_MODE_MATCH_FLAGS) }
}

// upstream: drm_modes.c drm_mode_validate_basic()
fn drm_mode_validate_basic(mode: &DrmDisplayMode) -> DrmModeStatus {
    if mode.r#type as u32 & !DRM_MODE_TYPE_ALL != 0 {
        return MODE_BAD;
    }
    if mode.flags & !DRM_MODE_FLAG_ALL != 0 {
        return MODE_BAD;
    }
    if mode.flags & DRM_MODE_FLAG_3D_MASK > DRM_MODE_FLAG_3D_MAX {
        return MODE_BAD;
    }
    if mode.clock == 0 {
        return MODE_CLOCK_LOW;
    }
    if mode.hdisplay == 0
        || mode.hsync_start < mode.hdisplay
        || mode.hsync_end < mode.hsync_start
        || mode.htotal < mode.hsync_end
    {
        return MODE_H_ILLEGAL;
    }
    if mode.vdisplay == 0
        || mode.vsync_start < mode.vdisplay
        || mode.vsync_end < mode.vsync_start
        || mode.vtotal < mode.vsync_end
    {
        return MODE_V_ILLEGAL;
    }
    MODE_OK
}

// upstream: drm_modes.c drm_mode_validate_driver()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, initialized [`DrmDevice`] whose `mode_config.funcs` table is valid and live.
/// - `mode` must point to a valid, properly aligned, and initialized [`DrmDisplayMode`] readable for the duration of the call.
pub unsafe extern "C" fn drm_mode_validate_driver(
    dev: *mut DrmDevice,
    mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    // SAFETY: mode is a valid, properly aligned DrmDisplayMode readable per caller contract.
    let status = drm_mode_validate_basic(unsafe { &*mode });
    if status != MODE_OK {
        return status;
    }
    // SAFETY: dev points to a valid DrmDevice per caller contract; mode_config is initialized.
    let funcs = unsafe { (*dev).mode_config.funcs };
    // SAFETY: funcs is a valid pointer to DrmModeConfigFuncs associated with dev.
    match unsafe { (*funcs).mode_valid } {
        // SAFETY: mode_valid callback is called with valid dev and mode pointers.
        Some(mode_valid) => unsafe { mode_valid(dev, mode as *mut DrmDisplayMode) },
        None => MODE_OK,
    }
}

// upstream: drm_modes.c drm_mode_validate_size()
#[unsafe(no_mangle)]
/// # Safety
///
/// `mode` must point to a valid, properly aligned, and readable [`DrmDisplayMode`].
pub unsafe extern "C" fn drm_mode_validate_size(
    mode: *const DrmDisplayMode,
    max_x: i32,
    max_y: i32,
) -> DrmModeStatus {
    // SAFETY: mode is a valid, properly aligned DrmDisplayMode readable per caller contract.
    let m = unsafe { &*mode };
    if max_x > 0 && m.hdisplay as i32 > max_x {
        return MODE_VIRTUAL_X;
    }
    if max_y > 0 && m.vdisplay as i32 > max_y {
        return MODE_VIRTUAL_Y;
    }
    MODE_OK
}

// upstream: drm_modes.c drm_mode_validate_ycbcr420()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `mode` must point to a valid, properly aligned, and readable [`DrmDisplayMode`].
/// - `connector` must point to a valid, properly aligned, and initialized [`DrmConnector`].
pub unsafe extern "C" fn drm_mode_validate_ycbcr420(
    mode: *const DrmDisplayMode,
    connector: *mut DrmConnector,
) -> DrmModeStatus {
    // SAFETY: connector is a valid, properly aligned DrmConnector per caller contract.
    let c = unsafe { &*connector };
    // SAFETY: &c.display_info is a valid reference embedded in connector, and mode is valid per caller contract.
    if !c.ycbcr_420_allowed && unsafe { drm_mode_is_420_only(&c.display_info, mode) } {
        return MODE_NO_420;
    }
    MODE_OK
}

// upstream: drm_modes.c drm_mode_status_names[] (designated by MODE_* + 3)
fn drm_mode_status_name(status: DrmModeStatus) -> Option<&'static core::ffi::CStr> {
    Some(match status {
        MODE_OK => c"OK",
        MODE_HSYNC => c"HSYNC",
        MODE_VSYNC => c"VSYNC",
        MODE_H_ILLEGAL => c"H_ILLEGAL",
        MODE_V_ILLEGAL => c"V_ILLEGAL",
        MODE_BAD_WIDTH => c"BAD_WIDTH",
        MODE_NOMODE => c"NOMODE",
        MODE_NO_INTERLACE => c"NO_INTERLACE",
        MODE_NO_DBLESCAN => c"NO_DBLESCAN",
        MODE_NO_VSCAN => c"NO_VSCAN",
        MODE_MEM => c"MEM",
        MODE_VIRTUAL_X => c"VIRTUAL_X",
        MODE_VIRTUAL_Y => c"VIRTUAL_Y",
        MODE_MEM_VIRT => c"MEM_VIRT",
        MODE_NOCLOCK => c"NOCLOCK",
        MODE_CLOCK_HIGH => c"CLOCK_HIGH",
        MODE_CLOCK_LOW => c"CLOCK_LOW",
        MODE_CLOCK_RANGE => c"CLOCK_RANGE",
        MODE_BAD_HVALUE => c"BAD_HVALUE",
        MODE_BAD_VVALUE => c"BAD_VVALUE",
        MODE_BAD_VSCAN => c"BAD_VSCAN",
        MODE_HSYNC_NARROW => c"HSYNC_NARROW",
        MODE_HSYNC_WIDE => c"HSYNC_WIDE",
        MODE_HBLANK_NARROW => c"HBLANK_NARROW",
        MODE_HBLANK_WIDE => c"HBLANK_WIDE",
        MODE_VSYNC_NARROW => c"VSYNC_NARROW",
        MODE_VSYNC_WIDE => c"VSYNC_WIDE",
        MODE_VBLANK_NARROW => c"VBLANK_NARROW",
        MODE_VBLANK_WIDE => c"VBLANK_WIDE",
        MODE_PANEL => c"PANEL",
        MODE_INTERLACE_WIDTH => c"INTERLACE_WIDTH",
        MODE_ONE_WIDTH => c"ONE_WIDTH",
        MODE_ONE_HEIGHT => c"ONE_HEIGHT",
        MODE_ONE_SIZE => c"ONE_SIZE",
        MODE_NO_REDUCED => c"NO_REDUCED",
        MODE_NO_STEREO => c"NO_STEREO",
        MODE_NO_420 => c"NO_420",
        MODE_STALE => c"STALE",
        MODE_BAD => c"BAD",
        MODE_ERROR => c"ERROR",
        _ => return None,
    })
}

// upstream: drm_modes.c drm_get_mode_status_name()
#[unsafe(no_mangle)]
/// # Safety
///
/// This function is safe to call with any [`DrmModeStatus`] value. It returns a pointer to a
/// static, null-terminated C string with `'static` lifetime.
pub unsafe extern "C" fn drm_get_mode_status_name(status: DrmModeStatus) -> *const c_char {
    // index = status + 3 must lie in [0, array_size(drm_mode_status_names)) = [0, 40)
    let index = status + 3;
    if warn_on(!(0..40).contains(&index)) {
        return c"".as_ptr();
    }
    match drm_mode_status_name(status) {
        Some(name) => name.as_ptr(),
        None => c"".as_ptr(),
    }
}

// upstream: drm_modes.c drm_mode_prune_invalid()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` may be null or point to the valid [`DrmDevice`] owning the modes in `mode_list`.
/// - `mode_list` must point to a valid, circular doubly-linked [`ListHead`] containing [`DrmDisplayMode`]
///   elements linked via their `head` field.
/// - The caller must synchronize access to `mode_list` (e.g. holding `mode_config.mutex`).
pub unsafe extern "C" fn drm_mode_prune_invalid(
    dev: *mut DrmDevice,
    mode_list: *mut ListHead,
    verbose: bool,
) {
    // SAFETY: mode_list points to a valid circular ListHead per caller contract.
    let mut link = unsafe { (*mode_list).next };
    while link != mode_list {
        // SAFETY: link is a valid node in mode_list, not equal to the sentinel.
        let next = unsafe { (*link).next };
        // SAFETY: link is embedded in an enclosing DrmDisplayMode at offset_of!(DrmDisplayMode, head).
        let mode = unsafe {
            link.cast::<u8>().sub(offset_of!(DrmDisplayMode, head)) as *mut DrmDisplayMode
        };
        // SAFETY: mode points to the valid DrmDisplayMode enclosing link, which remains allocated.
        let m = unsafe { &*mode };
        if m.status != MODE_OK {
            // SAFETY: link is a valid node in mode_list and is deleted from the list.
            unsafe { list_del(link) };
            if m.r#type as u32 & DRM_MODE_TYPE_USERDEF != 0 {
                drm_log_at(
                    DrmLogLevel::Warn,
                    "drm",
                    file!(),
                    line!(),
                    &format!("[drm] User-defined mode not supported: {}", mode_fmt(mode)),
                );
            }
            if verbose {
                // SAFETY: drm_get_mode_status_name returns a pointer to a static null-terminated C string.
                let name = unsafe { core::ffi::CStr::from_ptr(drm_get_mode_status_name(m.status)) }
                    .to_str()
                    .unwrap_or("");
                drm_dbg_kms(&format!("Rejected mode: {} ({})", mode_fmt(mode), name));
            }
            // SAFETY: mode was removed from mode_list above and has invalid status; destroying it frees its memory.
            unsafe { drm_mode_destroy(dev, mode) };
        }
        link = next;
    }
}

// upstream: drm_modes.c drm_mode_compare()
fn drm_mode_compare(a: &DrmDisplayMode, b: &DrmDisplayMode) -> i64 {
    let mut diff: i64 = ((b.r#type as u32 & DRM_MODE_TYPE_PREFERRED != 0) as i64)
        - ((a.r#type as u32 & DRM_MODE_TYPE_PREFERRED != 0) as i64);
    if diff != 0 {
        return diff;
    }

    diff = (b.hdisplay as i32 * b.vdisplay as i32) as i64
        - (a.hdisplay as i32 * a.vdisplay as i32) as i64;
    if diff != 0 {
        return diff;
    }

    // SAFETY: a and b are valid references to DrmDisplayMode, so their pointers are valid,
    // properly aligned, and live for drm_mode_vrefresh.
    diff = unsafe {
        drm_mode_vrefresh(b as *const _) as i64 - drm_mode_vrefresh(a as *const _) as i64
    };
    if diff != 0 {
        return diff;
    }

    b.clock as i64 - a.clock as i64
}

// upstream: drm_modes.c drm_mode_sort() (list_sort is a stable merge sort)
#[unsafe(no_mangle)]
/// # Safety
///
/// `mode_list` must point to a valid, circular doubly-linked [`ListHead`] containing [`DrmDisplayMode`]
/// elements linked via their `head` field. The caller must synchronize access to the list.
pub unsafe extern "C" fn drm_mode_sort(mode_list: *mut ListHead) {
    // Collect the entries, stable-sort them by the Linux comparator, relink.
    let mut nodes: alloc::vec::Vec<*mut DrmDisplayMode> = alloc::vec::Vec::new();
    // SAFETY: mode_list points to a valid circular ListHead per caller contract.
    let mut link = unsafe { (*mode_list).next };
    while link != mode_list {
        // SAFETY: link is embedded in a valid DrmDisplayMode at offset_of!(DrmDisplayMode, head).
        let mode = unsafe {
            link.cast::<u8>().sub(offset_of!(DrmDisplayMode, head)) as *mut DrmDisplayMode
        };
        nodes.push(mode);
        // SAFETY: link is a valid node in the circular list.
        link = unsafe { (*link).next };
    }
    nodes.sort_by(|&a, &b| {
        // SAFETY: a and b point to valid DrmDisplayMode elements collected from mode_list.
        let d = drm_mode_compare(unsafe { &*a }, unsafe { &*b });
        d.cmp(&0)
    });
    for &mode in nodes.iter() {
        // SAFETY: mode points to a valid DrmDisplayMode from mode_list, and mode_list is the target list head.
        unsafe { list_move_tail_raw(core::ptr::addr_of_mut!((*mode).head), mode_list) };
    }
}

#[inline]
/// # Safety
///
/// `entry` and `head` must point to valid, properly aligned, live [`ListHead`] nodes in
/// properly synchronized linked lists.
unsafe fn list_move_tail_raw(entry: *mut ListHead, head: *mut ListHead) {
    // SAFETY: entry and head are valid, properly aligned ListHead pointers per caller contract.
    unsafe { drm_kapi::list::list_move_tail(entry, head) };
}

// upstream: drm_modes.c drm_connector_list_update()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `connector` must point to a valid, initialized [`DrmConnector`] with a valid `dev` pointer
///   and initialized `mode_config`.
/// - The caller must hold `(*(*connector).dev).mode_config.mutex`.
pub unsafe extern "C" fn drm_connector_list_update(connector: *mut DrmConnector) {
    // SAFETY: connector is a valid DrmConnector per caller contract; dereferencing dev->mode_config.mutex
    // yields a valid pointer to the mode_config mutex.
    let mutex = unsafe { core::ptr::addr_of_mut!((*(*connector).dev).mode_config.mutex) };
    // SAFETY: mutex points to the valid mode_config.mutex derived above.
    warn_on(!unsafe { mutex_is_locked(mutex) });

    // SAFETY: connector is a valid DrmConnector; probed_modes is an embedded ListHead.
    let probed = unsafe { core::ptr::addr_of_mut!((*connector).probed_modes) };
    // SAFETY: connector is a valid DrmConnector; modes is an embedded ListHead.
    let modes_head = unsafe { core::ptr::addr_of_mut!((*connector).modes) };
    // SAFETY: probed points to the connector's valid probed_modes list head.
    let mut plink = unsafe { (*probed).next };
    while plink != probed {
        // SAFETY: plink is a valid node in probed_modes.
        let pnext = unsafe { (*plink).next };
        // SAFETY: plink is embedded in a DrmDisplayMode at offset_of!(DrmDisplayMode, head).
        let pmode = unsafe {
            plink.cast::<u8>().sub(offset_of!(DrmDisplayMode, head)) as *mut DrmDisplayMode
        };

        let mut found_it = false;
        // SAFETY: modes_head points to the connector's valid modes list head.
        let mut mlink = unsafe { (*modes_head).next };
        while mlink != modes_head {
            // SAFETY: mlink is embedded in a DrmDisplayMode at offset_of!(DrmDisplayMode, head).
            let mode = unsafe {
                mlink.cast::<u8>().sub(offset_of!(DrmDisplayMode, head)) as *mut DrmDisplayMode
            };
            // SAFETY: mlink is a valid node in modes list.
            mlink = unsafe { (*mlink).next };
            // SAFETY: pmode and mode are valid DrmDisplayMode pointers.
            if !unsafe { drm_mode_equal(pmode, mode) } {
                continue;
            }
            found_it = true;
            // SAFETY: mode and pmode are valid DrmDisplayMode pointers; plink is removed from probed_modes
            // and pmode is destroyed.
            unsafe {
                if (*mode).status == MODE_STALE {
                    drm_mode_copy(mode, pmode);
                } else if (*mode).r#type as u32 & DRM_MODE_TYPE_PREFERRED == 0
                    && (*pmode).r#type as u32 & DRM_MODE_TYPE_PREFERRED != 0
                {
                    (*pmode).r#type |= (*mode).r#type;
                    drm_mode_copy(mode, pmode);
                } else {
                    (*mode).r#type |= (*pmode).r#type;
                }
                list_del(plink);
                drm_mode_destroy((*connector).dev, pmode);
            }
            break;
        }

        if !found_it {
            // SAFETY: plink is a valid unmerged node and modes_head is the connector's modes list head.
            unsafe { list_move_tail_raw(plink, modes_head) };
        }
        plink = pnext;
    }
}

// ---------------------------------------------------------------------------
// Command-line mode parsing.
//
// The C parser walks a NUL-terminated `const char *`. Here the string is a
// byte slice and positions are indices into it; `at()` yields NUL past the
// end, which is what the C code observes at the terminator.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Cs<'a> {
    b: &'a [u8],
}

impl<'a> Cs<'a> {
    fn at(&self, i: isize) -> u8 {
        if i < 0 {
            return 0;
        }
        self.b.get(i as usize).copied().unwrap_or(0)
    }

    fn strlen_from(&self, i: isize) -> isize {
        let mut n = 0isize;
        while self.at(i + n) != 0 {
            n += 1;
        }
        n
    }

    // strchr(s + from, c): index of the first `c` at or after `from`, or None.
    fn strchr(&self, from: isize, c: u8) -> Option<isize> {
        let mut i = from;
        loop {
            let ch = self.at(i);
            if ch == c {
                return Some(i);
            }
            if ch == 0 {
                return None;
            }
            i += 1;
        }
    }

    // strnchr(s + from, count, c)
    fn strnchr(&self, from: isize, count: isize, c: u8) -> Option<isize> {
        let mut i = 0isize;
        while i < count && self.at(from + i) != 0 {
            if self.at(from + i) == c {
                return Some(from + i);
            }
            i += 1;
        }
        None
    }
}

// upstream: lib/vsprintf.c simple_strtol(): returns (value, end index).
fn simple_strtol(s: Cs<'_>, start: isize) -> (i64, isize) {
    if s.at(start) == b'-' {
        let (v, end) = simple_strtoul(s, start + 1);
        return (-(v as i64), end);
    }
    let (v, end) = simple_strtoul(s, start);
    (v as i64, end)
}

fn simple_strtoul(s: Cs<'_>, start: isize) -> (u64, isize) {
    let mut i = start;
    let mut v: u64 = 0;
    while s.at(i).is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add((s.at(i) - b'0') as u64);
        i += 1;
    }
    if i == start {
        return (0, start);
    }
    (v, i)
}

// upstream: string.h strncmp(a, b, n) == 0 with `a` the first `n` bytes of
// the token at `tok` (no NULs inside the token).
fn strncmp_token(s: Cs<'_>, tok: isize, tok_len: isize, name: &str) -> bool {
    if tok_len < 0 {
        return false;
    }
    let n = tok_len as usize;
    let nb = name.as_bytes();
    // Compare the first n bytes; NUL in name ends the comparison.
    for i in 0..n {
        let a = s.at(tok + i as isize);
        let b = nb.get(i).copied().unwrap_or(0);
        if a != b {
            return false;
        }
        if a == 0 {
            return true;
        }
    }
    true
}

// upstream: drm_modes.c drm_mode_parse_cmdline_bpp()
fn drm_mode_parse_cmdline_bpp(s: Cs<'_>, start: isize, mode: &mut DrmCmdlineMode) -> (i32, isize) {
    if s.at(start) != b'-' {
        return (-EINVAL, start);
    }
    let str_ = start + 1;
    let (bpp, end) = simple_strtol(s, str_);
    if end == str_ {
        return (-EINVAL, end);
    }
    mode.bpp = bpp as u32 as i32;
    mode.bpp_specified = true;
    (0, end)
}

// upstream: drm_modes.c drm_mode_parse_cmdline_refresh()
fn drm_mode_parse_cmdline_refresh(
    s: Cs<'_>,
    start: isize,
    mode: &mut DrmCmdlineMode,
) -> (i32, isize) {
    if s.at(start) != b'@' {
        return (-EINVAL, start);
    }
    let str_ = start + 1;
    let (refresh, end) = simple_strtol(s, str_);
    if end == str_ {
        return (-EINVAL, end);
    }
    mode.refresh = refresh as u32 as i32;
    mode.refresh_specified = true;
    (0, end)
}

// upstream: drm_modes.c drm_mode_parse_cmdline_extra()
fn drm_mode_parse_cmdline_extra(
    s: Cs<'_>,
    start: isize,
    length: isize,
    freestanding: bool,
    connector_type: u32,
    mode: &mut DrmCmdlineMode,
) -> i32 {
    for i in 0..length.max(0) {
        match s.at(start + i) {
            b'i' => {
                if freestanding {
                    return -EINVAL;
                }
                mode.interlace = true;
            }
            b'm' => {
                if freestanding {
                    return -EINVAL;
                }
                mode.margins = true;
            }
            b'D' => {
                if mode.force != DRM_FORCE_UNSPECIFIED {
                    return -EINVAL;
                }
                if connector_type != DRM_MODE_CONNECTOR_DVII
                    && connector_type != DRM_MODE_CONNECTOR_HDMIB
                {
                    mode.force = DRM_FORCE_ON;
                } else {
                    mode.force = DRM_FORCE_ON_DIGITAL;
                }
            }
            b'd' => {
                if mode.force != DRM_FORCE_UNSPECIFIED {
                    return -EINVAL;
                }
                mode.force = DRM_FORCE_OFF;
            }
            b'e' => {
                if mode.force != DRM_FORCE_UNSPECIFIED {
                    return -EINVAL;
                }
                mode.force = DRM_FORCE_ON;
            }
            _ => return -EINVAL,
        }
    }
    0
}

// upstream: drm_modes.c drm_mode_parse_cmdline_res_mode()
fn drm_mode_parse_cmdline_res_mode(
    s: Cs<'_>,
    start: isize,
    length: u32,
    extras: bool,
    connector_type: u32,
    mode: &mut DrmCmdlineMode,
) -> i32 {
    let str_start = start;
    let mut rb = false;
    let mut cvt = false;

    let (xres, end) = simple_strtol(s, start);
    if end == start {
        return -EINVAL;
    }
    if s.at(end) != b'x' {
        return -EINVAL;
    }
    let end = end + 1;
    let str_ = end;

    let (yres, end) = simple_strtol(s, str_);
    if end == str_ {
        return -EINVAL;
    }

    let remaining = length as i64 - (end - str_start) as i64;
    if remaining < 0 {
        return -EINVAL;
    }

    for i in 0..remaining as isize {
        match s.at(end + i) {
            b'M' => cvt = true,
            b'R' => rb = true,
            _ => {
                if extras {
                    let ret =
                        drm_mode_parse_cmdline_extra(s, end + i, 1, false, connector_type, mode);
                    if ret != 0 {
                        return ret;
                    }
                } else {
                    return -EINVAL;
                }
            }
        }
    }

    mode.xres = xres as i32;
    mode.yres = yres as i32;
    mode.cvt = cvt;
    mode.rb = rb;
    0
}

// upstream: drm_modes.c drm_mode_parse_cmdline_int(): `delim` is the '=' position.
fn drm_mode_parse_cmdline_int(s: Cs<'_>, delim: isize) -> (i32, u32) {
    if s.at(delim) != b'=' {
        return (-EINVAL, 0);
    }
    let value = delim + 1;
    let (v, endp) = simple_strtol(s, value);
    if endp == value {
        return (-EINVAL, 0);
    }
    (0, v as u32)
}

// upstream: drm_modes.c drm_mode_parse_panel_orientation()
fn drm_mode_parse_panel_orientation(s: Cs<'_>, delim: isize, mode: &mut DrmCmdlineMode) -> i32 {
    if s.at(delim) != b'=' {
        return -EINVAL;
    }
    let value = delim + 1;
    let end = match s.strchr(value, b',') {
        Some(p) => p,
        None => value + s.strlen_from(value),
    };
    let len = end - value;

    if strncmp_token(s, value, len, "normal") {
        mode.panel_orientation = DRM_MODE_PANEL_ORIENTATION_NORMAL;
    } else if strncmp_token(s, value, len, "upside_down") {
        mode.panel_orientation = DRM_MODE_PANEL_ORIENTATION_BOTTOM_UP;
    } else if strncmp_token(s, value, len, "left_side_up") {
        mode.panel_orientation = DRM_MODE_PANEL_ORIENTATION_LEFT_UP;
    } else if strncmp_token(s, value, len, "right_side_up") {
        mode.panel_orientation = DRM_MODE_PANEL_ORIENTATION_RIGHT_UP;
    } else {
        return -EINVAL;
    }
    0
}

// upstream: drm_connector.c drm_get_tv_mode_from_name() (connector batch)
unsafe extern "C" {
    fn drm_get_tv_mode_from_name(name: *const c_char, len: usize) -> i32;
    fn drm_mode_fixup_1366x768(mode: *mut DrmDisplayMode);
    fn drm_match_cea_mode(mode: *const DrmDisplayMode) -> u8;
}

// upstream: drm_modes.c drm_mode_parse_tv_mode()
fn drm_mode_parse_tv_mode(s: Cs<'_>, delim: isize, mode: &mut DrmCmdlineMode) -> i32 {
    if s.at(delim) != b'=' {
        return -EINVAL;
    }
    let value = delim + 1;
    let end = match s.strchr(value, b',') {
        Some(p) => p,
        None => value + s.strlen_from(value),
    };
    // SAFETY: value and end are valid byte offsets within s.b (value <= end <= s.b.len()),
    // so offset(value) is within the allocation and the length (end - value) accurately describes the slice.
    let ret = unsafe {
        drm_get_tv_mode_from_name(
            s.b.as_ptr().offset(value) as *const c_char,
            (end - value) as usize,
        )
    };
    if ret < 0 {
        return ret;
    }
    mode.tv_mode_specified = true;
    mode.tv_mode = ret as DrmConnectorTvMode;
    0
}

// upstream: drm_modes.c drm_mode_parse_cmdline_options()
fn drm_mode_parse_cmdline_options(
    s: Cs<'_>,
    start: isize,
    freestanding: bool,
    connector_type: u32,
    mode: &mut DrmCmdlineMode,
) -> i32 {
    let mut rotation: u32 = 0;
    let mut option = start;

    loop {
        // delim = strchr(option, '='); else strchr(option, ','); else end of string
        let delim = match s.strchr(option, b'=') {
            Some(p) => p,
            None => match s.strchr(option, b',') {
                Some(p) => p,
                None => option + s.strlen_from(option),
            },
        };
        let tok_len = delim - option;
        let tok = |name: &str| strncmp_token(s, option, tok_len, name);

        if tok("rotate") {
            let (ret, deg) = drm_mode_parse_cmdline_int(s, delim);
            if ret != 0 {
                return -EINVAL;
            }
            match deg {
                0 => rotation |= DRM_MODE_ROTATE_0,
                90 => rotation |= DRM_MODE_ROTATE_90,
                180 => rotation |= DRM_MODE_ROTATE_180,
                270 => rotation |= DRM_MODE_ROTATE_270,
                _ => return -EINVAL,
            }
        } else if tok("reflect_x") {
            rotation |= DRM_MODE_REFLECT_X;
        } else if tok("reflect_y") {
            rotation |= DRM_MODE_REFLECT_Y;
        } else if tok("margin_right") {
            let (ret, margin) = drm_mode_parse_cmdline_int(s, delim);
            if ret != 0 {
                return -EINVAL;
            }
            mode.tv_margins.right = margin;
        } else if tok("margin_left") {
            let (ret, margin) = drm_mode_parse_cmdline_int(s, delim);
            if ret != 0 {
                return -EINVAL;
            }
            mode.tv_margins.left = margin;
        } else if tok("margin_top") {
            let (ret, margin) = drm_mode_parse_cmdline_int(s, delim);
            if ret != 0 {
                return -EINVAL;
            }
            mode.tv_margins.top = margin;
        } else if tok("margin_bottom") {
            let (ret, margin) = drm_mode_parse_cmdline_int(s, delim);
            if ret != 0 {
                return -EINVAL;
            }
            mode.tv_margins.bottom = margin;
        } else if tok("panel_orientation") {
            if drm_mode_parse_panel_orientation(s, delim, mode) != 0 {
                return -EINVAL;
            }
        } else if tok("tv_mode") {
            if drm_mode_parse_tv_mode(s, delim, mode) != 0 {
                return -EINVAL;
            }
        } else {
            return -EINVAL;
        }

        // sep = strchr(delim, ','); option = sep + 1; while (sep)
        match s.strchr(delim, b',') {
            Some(sep) => option = sep + 1,
            None => break,
        }
    }

    if rotation != 0 && freestanding {
        return -EINVAL;
    }
    if rotation & DRM_MODE_ROTATE_MASK == 0 {
        rotation |= DRM_MODE_ROTATE_0;
    }
    if !is_power_of_2(rotation & DRM_MODE_ROTATE_MASK) {
        return -EINVAL;
    }
    mode.rotation_reflection = rotation;
    let _ = connector_type;
    0
}

// upstream: include/linux/log2.h is_power_of_2()
#[inline]
fn is_power_of_2(x: u32) -> bool {
    x != 0 && (x & (x - 1)) == 0
}

// upstream: drm_modes.c struct drm_named_mode
struct DrmNamedMode {
    name: &'static str,
    pixel_clock_khz: u32,
    xres: u32,
    yres: u32,
    flags: u32,
    tv_mode: DrmConnectorTvMode,
}

// upstream: drm_modes.c drm_named_modes[]
static DRM_NAMED_MODES: [DrmNamedMode; 4] = [
    DrmNamedMode {
        name: "NTSC",
        pixel_clock_khz: 13500,
        xres: 720,
        yres: 480,
        flags: DRM_MODE_FLAG_INTERLACE,
        tv_mode: DRM_MODE_TV_MODE_NTSC,
    },
    DrmNamedMode {
        name: "NTSC-J",
        pixel_clock_khz: 13500,
        xres: 720,
        yres: 480,
        flags: DRM_MODE_FLAG_INTERLACE,
        tv_mode: DRM_MODE_TV_MODE_NTSC_J,
    },
    DrmNamedMode {
        name: "PAL",
        pixel_clock_khz: 13500,
        xres: 720,
        yres: 576,
        flags: DRM_MODE_FLAG_INTERLACE,
        tv_mode: DRM_MODE_TV_MODE_PAL,
    },
    DrmNamedMode {
        name: "PAL-M",
        pixel_clock_khz: 13500,
        xres: 720,
        yres: 480,
        flags: DRM_MODE_FLAG_INTERLACE,
        tv_mode: DRM_MODE_TV_MODE_PAL_M,
    },
];

// upstream: drm_modes.c drm_mode_parse_cmdline_named_mode()
fn drm_mode_parse_cmdline_named_mode(
    s: Cs<'_>,
    start: isize,
    name_end: u32,
    cmd: &mut DrmCmdlineMode,
) -> i32 {
    if name_end == 0 {
        return 0;
    }
    if s.at(start).is_ascii_digit() {
        return 0;
    }
    if s.strnchr(start, name_end as isize, b'=').is_some() {
        return 0;
    }
    if name_end == 1 && matches!(s.at(start), b'd' | b'D' | b'e') {
        return 0;
    }

    for named in DRM_NAMED_MODES.iter() {
        // str_has_prefix(name, mode->name): length of the matching prefix, or 0
        let nb = named.name.as_bytes();
        let ret = if (0..nb.len()).all(|i| s.at(start + i as isize) == nb[i]) {
            nb.len() as u32
        } else {
            0
        };
        if ret != name_end {
            continue;
        }

        // strscpy(cmdline_mode->name, mode->name, sizeof(...))
        for (i, slot) in cmd.name.iter_mut().enumerate() {
            *slot = if i < nb.len() && i < 31 {
                nb[i] as c_char
            } else {
                0
            };
        }
        cmd.pixel_clock = named.pixel_clock_khz;
        cmd.xres = named.xres as i32;
        cmd.yres = named.yres as i32;
        cmd.interlace = named.flags & DRM_MODE_FLAG_INTERLACE != 0;
        cmd.tv_mode = named.tv_mode;
        cmd.tv_mode_specified = true;
        cmd.specified = true;
        return 1;
    }

    -EINVAL
}

// upstream: drm_modes.c drm_mode_parse_command_line_for_connector()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `mode_option` may be null or must point to a valid null-terminated C string.
/// - `connector` may be null or must point to a valid, initialized [`DrmConnector`].
/// - `mode` must point to a valid, properly aligned, writable [`DrmCmdlineMode`].
pub unsafe extern "C" fn drm_mode_parse_command_line_for_connector(
    mode_option: *const c_char,
    connector: *const DrmConnector,
    mode: *mut DrmCmdlineMode,
) -> bool {
    // SAFETY: mode is a valid, properly aligned writable pointer to DrmCmdlineMode per caller contract;
    // write_bytes zero-initializes it.
    unsafe { core::ptr::write_bytes(mode, 0, 1) };
    // SAFETY: mode is valid, properly aligned, and uniquely owned per caller contract.
    let mode = unsafe { &mut *mode };
    mode.panel_orientation = DRM_MODE_PANEL_ORIENTATION_UNKNOWN;

    if mode_option.is_null() {
        return false;
    }
    // SAFETY: mode_option is non-null and points to a valid null-terminated C string per caller contract.
    let bytes = unsafe { core::ffi::CStr::from_ptr(mode_option) }.to_bytes();
    let s = Cs { b: bytes };
    let connector_type = if connector.is_null() {
        0
    } else {
        // SAFETY: connector is non-null and points to a valid DrmConnector per caller contract.
        unsafe { (*connector).connector_type as u32 }
    };

    let name: isize = 0;
    let mut freestanding = false;
    let mut parse_extras = false;
    let mode_end: isize;
    let mut extra_ptr: Option<isize> = None;

    let options_ptr: Option<isize> = s.strchr(name, b',');
    let options_off: isize = match options_ptr {
        Some(p) => p - name,
        None => s.strlen_from(name),
    };

    // bpp_ptr = strnchr(name, options_off, '-'); while (bpp_ptr && !isdigit(bpp_ptr[1])) ...
    let mut bpp_ptr = s.strnchr(name, options_off, b'-');
    while let Some(p) = bpp_ptr {
        if s.at(p + 1).is_ascii_digit() {
            break;
        }
        bpp_ptr = s.strnchr(p + 1, options_off, b'-');
    }
    let bpp_off: isize = bpp_ptr.map(|p| p - name).unwrap_or(0);

    let refresh_ptr = s.strnchr(name, options_off, b'@');
    let refresh_off: isize = refresh_ptr.map(|p| p - name).unwrap_or(0);

    if bpp_ptr.is_some() {
        mode_end = bpp_off;
    } else if refresh_ptr.is_some() {
        mode_end = refresh_off;
    } else if options_ptr.is_some() {
        mode_end = options_off;
        parse_extras = true;
    } else {
        mode_end = s.strlen_from(name);
        parse_extras = true;
    }

    if mode_end == 0 {
        return false;
    }

    let ret = drm_mode_parse_cmdline_named_mode(s, name, mode_end as u32, mode);
    if ret < 0 {
        return false;
    }
    if ret != 0 && refresh_ptr.is_some() {
        return false;
    }

    if !mode.specified && s.at(name).is_ascii_digit() {
        let ret = drm_mode_parse_cmdline_res_mode(
            s,
            name,
            mode_end as u32,
            parse_extras,
            connector_type,
            mode,
        );
        if ret != 0 {
            return false;
        }
        mode.specified = true;
    }

    // Option indices may be -1 (`mode_option - 1` in the C code).
    let mut options_ptr: Option<isize> = options_ptr;
    if !mode.specified {
        let len = s.strlen_from(0);
        if bpp_ptr.is_some() || refresh_ptr.is_some() {
            return false;
        }
        if len == 1 || (len >= 2 && s.at(1) == b',') {
            extra_ptr = Some(0);
        } else {
            options_ptr = Some(-1);
        }
        freestanding = true;
    }

    let mut bpp_end: Option<isize> = None;
    let mut refresh_end: Option<isize> = None;

    if let Some(bp) = bpp_ptr {
        let (ret, end) = drm_mode_parse_cmdline_bpp(s, bp, mode);
        if ret != 0 {
            return false;
        }
        bpp_end = Some(end);
        mode.bpp_specified = true;
    }

    if let Some(rp) = refresh_ptr {
        let (ret, end) = drm_mode_parse_cmdline_refresh(s, rp, mode);
        if ret != 0 {
            return false;
        }
        refresh_end = Some(end);
        mode.refresh_specified = true;
    }

    if bpp_ptr.is_some() && refresh_ptr.is_some() {
        extra_ptr = Some(bpp_end.unwrap().max(refresh_end.unwrap()));
    } else if bpp_ptr.is_some() {
        extra_ptr = bpp_end;
    } else if refresh_ptr.is_some() {
        extra_ptr = refresh_end;
    }

    if let Some(ep) = extra_ptr {
        let len = match options_ptr {
            Some(op) => op - ep,
            None => s.strlen_from(ep),
        };
        let ret = drm_mode_parse_cmdline_extra(s, ep, len, freestanding, connector_type, mode);
        if ret != 0 {
            return false;
        }
    }

    if let Some(op) = options_ptr {
        let ret = drm_mode_parse_cmdline_options(s, op + 1, freestanding, connector_type, mode);
        if ret != 0 {
            return false;
        }
    }

    true
}

// upstream: drm_modes.c drm_named_mode()
/// # Safety
///
/// - `dev` may be null or point to a valid [`DrmDevice`].
/// - `cmd` must point to a valid, initialized [`DrmCmdlineMode`] with a null-terminated `name`.
unsafe fn drm_named_mode(dev: *mut DrmDevice, cmd: *mut DrmCmdlineMode) -> *mut DrmDisplayMode {
    // SAFETY: cmd points to a valid, initialized DrmCmdlineMode per caller contract.
    let cmd = unsafe { &*cmd };
    // SAFETY: cmd.name is an embedded null-terminated C string buffer inside DrmCmdlineMode.
    let cmd_name = unsafe { core::ffi::CStr::from_ptr(cmd.name.as_ptr()) }.to_bytes();
    for named in DRM_NAMED_MODES.iter() {
        if cmd_name != named.name.as_bytes() {
            continue;
        }
        if !cmd.tv_mode_specified {
            continue;
        }
        // SAFETY: Forwards dev and named TV mode parameters to drm_analog_tv_mode.
        return unsafe {
            drm_analog_tv_mode(
                dev,
                named.tv_mode,
                named.pixel_clock_khz as u64 * 1000,
                named.xres,
                named.yres,
                named.flags & DRM_MODE_FLAG_INTERLACE != 0,
            )
        };
    }
    core::ptr::null_mut()
}

// upstream: drm_modes.c drm_mode_create_from_cmdline_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` may be null or point to a valid [`DrmDevice`].
/// - `cmd` must point to a valid, initialized [`DrmCmdlineMode`].
///
/// If allocation succeeds, returns a newly created [`DrmDisplayMode`] that must be freed via [`drm_mode_destroy`].
pub unsafe extern "C" fn drm_mode_create_from_cmdline_mode(
    dev: *mut DrmDevice,
    cmd: *mut DrmCmdlineMode,
) -> *mut DrmDisplayMode {
    // SAFETY: cmd points to a valid, initialized DrmCmdlineMode per caller contract.
    let c = unsafe { &*cmd };
    if c.xres == 0 || c.yres == 0 {
        return core::ptr::null_mut();
    }

    let mode = if c.name[0] != 0 {
        // SAFETY: cmd.name is non-empty; drm_named_mode matches against known named modes.
        unsafe { drm_named_mode(dev, cmd) }
    } else if c.cvt {
        // SAFETY: Calls drm_cvt_mode with parameters extracted from cmdline mode.
        unsafe {
            drm_cvt_mode(
                dev,
                c.xres,
                c.yres,
                if c.refresh_specified { c.refresh } else { 60 },
                c.rb,
                c.interlace,
                c.margins,
            )
        }
    } else {
        // SAFETY: Calls drm_gtf_mode with parameters extracted from cmdline mode.
        unsafe {
            drm_gtf_mode(
                dev,
                c.xres,
                c.yres,
                if c.refresh_specified { c.refresh } else { 60 },
                c.interlace,
                c.margins as i32,
            )
        }
    };
    if mode.is_null() {
        return core::ptr::null_mut();
    }

    // SAFETY: mode is non-null and uniquely owned from the mode creator functions above;
    // valid for setting mode flags and CRTC info.
    unsafe {
        (*mode).r#type |= DRM_MODE_TYPE_USERDEF as u8;
        if c.xres == 1366 {
            drm_mode_fixup_1366x768(mode);
        }
        drm_mode_set_crtcinfo(mode, CRTC_INTERLACE_HALVE_V as i32);
    }
    mode
}

// upstream: drm_modes.c drm_mode_convert_to_umode()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `out` must point to a valid, properly aligned, writable [`DrmModeModeinfo`].
/// - `in_` must point to a valid, properly aligned, readable [`DrmDisplayMode`].
/// - `out` and `in_` must not overlap.
pub unsafe extern "C" fn drm_mode_convert_to_umode(
    out: *mut DrmModeModeinfo,
    in_: *const DrmDisplayMode,
) {
    // SAFETY: out and in_ are valid, properly aligned, non-overlapping pointers per caller contract.
    let (out, inp) = unsafe { (&mut *out, &*in_) };
    out.clock = inp.clock as u32;
    out.hdisplay = inp.hdisplay;
    out.hsync_start = inp.hsync_start;
    out.hsync_end = inp.hsync_end;
    out.htotal = inp.htotal;
    out.hskew = inp.hskew;
    out.vdisplay = inp.vdisplay;
    out.vsync_start = inp.vsync_start;
    out.vsync_end = inp.vsync_end;
    out.vtotal = inp.vtotal;
    out.vscan = inp.vscan;
    // SAFETY: in_ points to a valid DrmDisplayMode readable per caller contract.
    out.vrefresh = unsafe { drm_mode_vrefresh(in_) } as u32;
    out.flags = inp.flags;
    out.r#type = inp.r#type as u32;

    match inp.picture_aspect_ratio {
        HDMI_PICTURE_ASPECT_4_3 => out.flags |= DRM_MODE_FLAG_PIC_AR_4_3,
        HDMI_PICTURE_ASPECT_16_9 => out.flags |= DRM_MODE_FLAG_PIC_AR_16_9,
        HDMI_PICTURE_ASPECT_64_27 => out.flags |= DRM_MODE_FLAG_PIC_AR_64_27,
        HDMI_PICTURE_ASPECT_256_135 => out.flags |= DRM_MODE_FLAG_PIC_AR_256_135,
        HDMI_PICTURE_ASPECT_NONE => out.flags |= DRM_MODE_FLAG_PIC_AR_NONE,
        _ => {
            // WARN(1, "Invalid aspect ratio ...") then fall through to NONE
            warn_on(true);
            out.flags |= DRM_MODE_FLAG_PIC_AR_NONE;
        }
    }

    strscpy_pad(&mut out.name, &inp.name);
}

// upstream: lib/string.c strscpy_pad(): copy up to size-1 bytes, zero-fill.
fn strscpy_pad(dst: &mut [c_char; 32], src: &[c_char; 32]) {
    let mut i = 0usize;
    while i < 31 && src[i] != 0 {
        dst[i] = src[i];
        i += 1;
    }
    while i < 32 {
        dst[i] = 0;
        i += 1;
    }
}

// upstream: drm_modes.c drm_mode_convert_umode()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, initialized [`DrmDevice`].
/// - `out` must point to a valid, properly aligned, writable [`DrmDisplayMode`].
/// - `in_` must point to a valid, properly aligned, readable [`DrmModeModeinfo`].
/// - `out` and `in_` must not overlap.
pub unsafe extern "C" fn drm_mode_convert_umode(
    dev: *mut DrmDevice,
    out: *mut DrmDisplayMode,
    in_: *const DrmModeModeinfo,
) -> i32 {
    // SAFETY: in_ points to a valid, properly aligned DrmModeModeinfo per caller contract.
    let inp = unsafe { &*in_ };
    if inp.clock > i32::MAX as u32 || inp.vrefresh > i32::MAX as u32 {
        return -ERANGE;
    }
    // SAFETY: out points to a valid, properly aligned, writable DrmDisplayMode per caller contract.
    let out_ref = unsafe { &mut *out };

    out_ref.clock = inp.clock as i32;
    out_ref.hdisplay = inp.hdisplay;
    out_ref.hsync_start = inp.hsync_start;
    out_ref.hsync_end = inp.hsync_end;
    out_ref.htotal = inp.htotal;
    out_ref.hskew = inp.hskew;
    out_ref.vdisplay = inp.vdisplay;
    out_ref.vsync_start = inp.vsync_start;
    out_ref.vsync_end = inp.vsync_end;
    out_ref.vtotal = inp.vtotal;
    out_ref.vscan = inp.vscan;
    out_ref.flags = inp.flags;
    out_ref.r#type = (inp.r#type & DRM_MODE_TYPE_ALL) as u8;
    strscpy_pad(&mut out_ref.name, &inp.name);

    out_ref.flags &= !DRM_MODE_FLAG_PIC_AR_MASK;
    out_ref.picture_aspect_ratio = match inp.flags & DRM_MODE_FLAG_PIC_AR_MASK {
        DRM_MODE_FLAG_PIC_AR_4_3 => HDMI_PICTURE_ASPECT_4_3,
        DRM_MODE_FLAG_PIC_AR_16_9 => HDMI_PICTURE_ASPECT_16_9,
        DRM_MODE_FLAG_PIC_AR_64_27 => HDMI_PICTURE_ASPECT_64_27,
        DRM_MODE_FLAG_PIC_AR_256_135 => HDMI_PICTURE_ASPECT_256_135,
        DRM_MODE_FLAG_PIC_AR_NONE => HDMI_PICTURE_ASPECT_NONE,
        _ => return -EINVAL,
    };

    // SAFETY: dev and out are valid pointers per caller contract; driver validates out.
    out_ref.status = unsafe { drm_mode_validate_driver(dev, out) };
    if out_ref.status != MODE_OK {
        return -EINVAL;
    }

    // SAFETY: out is a valid, initialized DrmDisplayMode.
    unsafe { drm_mode_set_crtcinfo(out, CRTC_INTERLACE_HALVE_V as i32) };
    0
}

// upstream: drm_modes.c drm_mode_is_420_only(): test_bit(vic, y420_vdb_modes)
#[inline]
fn test_bit_vic(vic: u8, bitmap: &[u64; 4]) -> bool {
    (bitmap[(vic / 64) as usize] >> (vic % 64)) & 1 != 0
}

// upstream: drm_modes.c drm_mode_is_420_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `display` must point to a valid, properly aligned, readable [`DrmDisplayInfo`].
/// - `mode` must point to a valid, properly aligned, readable [`DrmDisplayMode`].
pub unsafe extern "C" fn drm_mode_is_420_only(
    display: *const DrmDisplayInfo,
    mode: *const DrmDisplayMode,
) -> bool {
    // SAFETY: mode points to a valid DrmDisplayMode readable per caller contract.
    let vic = unsafe { drm_match_cea_mode(mode) };
    // SAFETY: display points to a valid DrmDisplayInfo readable per caller contract.
    test_bit_vic(vic, unsafe { &(*display).hdmi.y420_vdb_modes })
}

// upstream: drm_modes.c drm_mode_is_420_also()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `display` must point to a valid, properly aligned, readable [`DrmDisplayInfo`].
/// - `mode` must point to a valid, properly aligned, readable [`DrmDisplayMode`].
pub unsafe extern "C" fn drm_mode_is_420_also(
    display: *const DrmDisplayInfo,
    mode: *const DrmDisplayMode,
) -> bool {
    // SAFETY: mode points to a valid DrmDisplayMode readable per caller contract.
    let vic = unsafe { drm_match_cea_mode(mode) };
    // SAFETY: display points to a valid DrmDisplayInfo readable per caller contract.
    test_bit_vic(vic, unsafe { &(*display).hdmi.y420_cmdb_modes })
}

// upstream: drm_modes.c drm_mode_is_420()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `display` must point to a valid, properly aligned, readable [`DrmDisplayInfo`].
/// - `mode` must point to a valid, properly aligned, readable [`DrmDisplayMode`].
pub unsafe extern "C" fn drm_mode_is_420(
    display: *const DrmDisplayInfo,
    mode: *const DrmDisplayMode,
) -> bool {
    // SAFETY: Forwards display and mode to drm_mode_is_420_only and drm_mode_is_420_also.
    unsafe { drm_mode_is_420_only(display, mode) || drm_mode_is_420_also(display, mode) }
}

// upstream: drm_modes.c drm_set_preferred_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// `connector` must point to a valid, initialized [`DrmConnector`] whose `probed_modes` list is valid
/// and synchronized by the caller.
pub unsafe extern "C" fn drm_set_preferred_mode(
    connector: *mut DrmConnector,
    hpref: i32,
    vpref: i32,
) {
    // SAFETY: connector is a valid DrmConnector per caller contract; probed_modes is an embedded ListHead.
    let probed = unsafe { core::ptr::addr_of_mut!((*connector).probed_modes) };
    // SAFETY: probed is a valid ListHead; accessing next traverses the circular list.
    let mut link = unsafe { (*probed).next };
    while link != probed {
        // SAFETY: link is embedded in a valid DrmDisplayMode at offset_of!(DrmDisplayMode, head).
        let mode = unsafe {
            link.cast::<u8>().sub(offset_of!(DrmDisplayMode, head)) as *mut DrmDisplayMode
        };
        // SAFETY: mode points to the valid DrmDisplayMode enclosing link.
        if unsafe { (*mode).hdisplay as i32 == hpref && (*mode).vdisplay as i32 == vpref } {
            // SAFETY: mode points to the valid DrmDisplayMode enclosing link.
            unsafe { (*mode).r#type |= DRM_MODE_TYPE_PREFERRED as u8 };
        }
        // SAFETY: link is a valid node in probed_modes.
        link = unsafe { (*link).next };
    }
}

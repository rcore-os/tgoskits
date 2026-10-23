// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/display/drm_hdmi_state_helper.c`.
//!
//! Implements HDMI connector state calculation, format and BPC selection,
//! infoframe generation (AVI, SPD, HDR, HDMI-VS), connector mode validation,
//! and infoframes update/clear routines.

use core::{
    cmp::{max, min},
    ffi::{c_char, c_void},
};

use drm_kapi::{
    config::{EINVAL, EOPNOTSUPP, is_err, ptr_err},
    mutex::{mutex_lock, mutex_unlock},
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::{
        CONNECTOR_STATUS_CONNECTED, CONNECTOR_STATUS_DISCONNECTED, DRM_HDMI_BROADCAST_RGB_AUTO,
        DRM_HDMI_BROADCAST_RGB_FULL, DRM_HDMI_BROADCAST_RGB_LIMITED,
        DRM_OUTPUT_COLOR_FORMAT_RGB444, DRM_OUTPUT_COLOR_FORMAT_YCBCR420,
        DRM_OUTPUT_COLOR_FORMAT_YCBCR422, DRM_OUTPUT_COLOR_FORMAT_YCBCR444, DrmConnector,
        DrmConnectorHdmiInfoframe, DrmConnectorInfoframeFuncs, DrmConnectorState,
        DrmConnectorStatus, DrmDisplayInfo, DrmOutputColorFormat,
    },
    drm_connector_upstream::drm_hdmi_connector_get_output_format_name,
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_edid_upstream::{
        DRM_EDID_HDMI_DC_30, DRM_EDID_HDMI_DC_36, DRM_EDID_YCBCR420_DC_30, DRM_EDID_YCBCR420_DC_36,
        DRM_EDID_YCBCR420_DC_48, DrmEdid, drm_default_rgb_quant_range, drm_edid_connector_update,
        drm_edid_free, drm_edid_read, drm_hdmi_avi_infoframe_from_display_mode,
        drm_hdmi_avi_infoframe_quant_range, drm_hdmi_vendor_infoframe_from_display_mode,
        drm_match_cea_mode,
    },
    drm_hdmi_helper_upstream::{
        drm_hdmi_avi_infoframe_bars, drm_hdmi_avi_infoframe_colorimetry,
        drm_hdmi_compute_mode_clock, drm_hdmi_infoframe_set_hdr_metadata,
    },
    drm_modes_types::{DrmDisplayMode, DrmModeStatus, MODE_CLOCK_HIGH, MODE_ERROR, MODE_OK},
    drm_modes_upstream::{drm_mode_is_420, drm_mode_is_420_only, drm_mode_vrefresh},
    hdmi_types::*,
    hdmi_upstream::*,
};

macro_rules! drm_dbg_kms {
    ($device:expr, $format:expr $(, $argument:expr)* $(,)?) => {
        drm_kapi::drm_dbg!($device, $format $(, $argument)*)
    };
}

macro_rules! drm_warn {
    ($device:expr, $format:expr $(, $argument:expr)* $(,)?) => {
        drm_kapi::drm_dbg!($device, $format $(, $argument)*)
    };
}

// Provided by drm_atomic_upstream.
unsafe extern "C" {
    /// Returns the atomic CRTC state for the specified CRTC.
    ///
    /// # Safety
    ///
    /// If non-null, `state` and `crtc` must point to valid, initialized `DrmAtomicCommit`
    /// and `DrmCrtc` structures. The caller must verify the returned pointer with `is_err`
    /// before dereferencing.
    fn drm_atomic_get_crtc_state(
        state: *mut DrmAtomicCommit,
        crtc: *mut DrmCrtc,
    ) -> *mut DrmCrtcState;
}

// Inlined atomic state getters (mirrors include/drm/drm_atomic.h)
/// Retrieves the new CRTC state for the given CRTC from the atomic commit state.
///
/// # Safety
///
/// If non-null, `state` and `crtc` must point to valid, initialized `DrmAtomicCommit`
/// and `DrmCrtc` structures belonging to the same DRM device. `(*crtc).index` must be
/// within the allocated bounds of `(*state).crtcs`.
#[inline]
unsafe fn drm_atomic_get_new_crtc_state_inline(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    if state.is_null() || crtc.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `crtc` is verified non-null above, properly aligned, and points to a valid `DrmCrtc`.
    let idx = unsafe { (*crtc).index as usize };
    // SAFETY: `state` is verified non-null above. The caller guarantees `crtc->index`
    // is a valid index into the `crtcs` array of `*state`.
    unsafe { (*(*state).crtcs.add(idx)).new_state }
}

/// Retrieves the new connector state for the given connector from the atomic commit state.
///
/// # Safety
///
/// If non-null, `state` and `connector` must point to valid, initialized `DrmAtomicCommit`
/// and `DrmConnector` structures belonging to the same DRM device.
#[inline]
unsafe fn drm_atomic_get_new_connector_state_inline(
    state: *mut DrmAtomicCommit,
    connector: *mut DrmConnector,
) -> *mut DrmConnectorState {
    if state.is_null() || connector.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `connector` is verified non-null above, properly aligned, and points to a valid `DrmConnector`.
    let idx = unsafe { (*connector).index as i32 };
    // SAFETY: `state` is verified non-null above, properly aligned, and points to a valid `DrmAtomicCommit`.
    if idx >= unsafe { (*state).num_connector } {
        return core::ptr::null_mut();
    }
    // SAFETY: `state` is non-null, and `idx` is non-negative and strictly less than `num_connector`,
    // so `connectors.add(idx)` is within the allocated bounds of the connector state array.
    unsafe { (*(*state).connectors.add(idx as usize)).new_state }
}

/// Retrieves the old connector state for the given connector from the atomic commit state.
///
/// # Safety
///
/// If non-null, `state` and `connector` must point to valid, initialized `DrmAtomicCommit`
/// and `DrmConnector` structures belonging to the same DRM device.
#[inline]
unsafe fn drm_atomic_get_old_connector_state_inline(
    state: *mut DrmAtomicCommit,
    connector: *mut DrmConnector,
) -> *mut DrmConnectorState {
    if state.is_null() || connector.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `connector` is verified non-null above, properly aligned, and points to a valid `DrmConnector`.
    let idx = unsafe { (*connector).index as i32 };
    // SAFETY: `state` is verified non-null above, properly aligned, and points to a valid `DrmAtomicCommit`.
    if idx >= unsafe { (*state).num_connector } {
        return core::ptr::null_mut();
    }
    // SAFETY: `state` is non-null, and `idx` is non-negative and strictly less than `num_connector`,
    // so `connectors.add(idx)` is within the allocated bounds of the connector state array.
    unsafe { (*(*state).connectors.add(idx as usize)).old_state }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c __drm_atomic_helper_connector_hdmi_reset()
#[unsafe(no_mangle)]
/// Resets HDMI-specific properties of a connector state to default values.
///
/// # Safety
///
/// If non-null, `connector` and `new_conn_state` must point to valid, properly aligned,
/// initialized `DrmConnector` and `DrmConnectorState` structures. Caller must guarantee
/// exclusive mutable access to `*new_conn_state`.
pub unsafe extern "C" fn __drm_atomic_helper_connector_hdmi_reset(
    connector: *mut DrmConnector,
    new_conn_state: *mut DrmConnectorState,
) {
    if connector.is_null() || new_conn_state.is_null() {
        return;
    }
    // SAFETY: `connector` is verified non-null above, properly aligned, and points to a valid `DrmConnector`.
    let max_bpc = unsafe { (*connector).max_bpc as u8 };
    // SAFETY: `new_conn_state` is verified non-null above, properly aligned, points to an initialized
    // `DrmConnectorState`, and caller guarantees exclusive mutable access during reset.
    let state_ref = unsafe { &mut *new_conn_state };
    state_ref.max_bpc = max_bpc;
    state_ref.max_requested_bpc = max_bpc;
    state_ref.hdmi.broadcast_rgb = DRM_HDMI_BROADCAST_RGB_AUTO;
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c output_color_format_to_hdmi_colorspace()
fn output_color_format_to_hdmi_colorspace(
    connector: &DrmConnector,
    fmt: DrmOutputColorFormat,
) -> HdmiColorspace {
    match fmt {
        DRM_OUTPUT_COLOR_FORMAT_YCBCR420 => HDMI_COLORSPACE_YUV420,
        DRM_OUTPUT_COLOR_FORMAT_YCBCR422 => HDMI_COLORSPACE_YUV422,
        DRM_OUTPUT_COLOR_FORMAT_YCBCR444 => HDMI_COLORSPACE_YUV444,
        _ => {
            if fmt != DRM_OUTPUT_COLOR_FORMAT_RGB444 {
                drm_warn!(
                    connector.dev as *const c_void,
                    "Unsupported output color format. Defaulting to RGB."
                );
            }
            HDMI_COLORSPACE_RGB
        }
    }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c connector_state_get_mode()
/// Returns a pointer to the display mode associated with the given connector state.
///
/// # Safety
///
/// If non-null, `conn_state` must point to a valid, aligned, initialized `DrmConnectorState`
/// whose atomic `state` and `crtc` fields (if non-null) point to valid DRM structures.
unsafe fn connector_state_get_mode(conn_state: *const DrmConnectorState) -> *const DrmDisplayMode {
    if conn_state.is_null() {
        return core::ptr::null();
    }
    // SAFETY: `conn_state` is verified non-null above, properly aligned, and points to a valid `DrmConnectorState`.
    let state = unsafe { (*conn_state).state };
    if state.is_null() {
        return core::ptr::null();
    }
    // SAFETY: `conn_state` is verified non-null above, properly aligned, and points to a valid `DrmConnectorState`.
    let crtc = unsafe { (*conn_state).crtc };
    if crtc.is_null() {
        return core::ptr::null();
    }
    // SAFETY: `state` and `crtc` are non-null pointers obtained from `conn_state`.
    let crtc_state = unsafe { drm_atomic_get_new_crtc_state_inline(state, crtc) };
    if crtc_state.is_null() {
        return core::ptr::null();
    }
    // SAFETY: `crtc_state` is non-null and points to a valid `DrmCrtcState`; `mode` is an embedded field.
    unsafe { core::ptr::addr_of!((*crtc_state).mode) }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_is_limited_range()
/// Determines whether the HDMI connector output should use limited quantization range.
///
/// # Safety
///
/// `connector` and `conn_state` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorState` structures. The connector state must be attached
/// to a valid atomic commit state with an associated CRTC state containing a valid mode.
unsafe fn hdmi_is_limited_range(
    connector: *const DrmConnector,
    conn_state: *const DrmConnectorState,
) -> bool {
    // SAFETY: Caller guarantees `conn_state` is non-null, properly aligned, and points to a valid
    // `DrmConnectorState` without concurrent mutation.
    let conn_state_ref = unsafe { &*conn_state };
    // SAFETY: Caller guarantees `connector` is non-null, properly aligned, and points to a valid
    // `DrmConnector`; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };
    // SAFETY: `conn_state` is a valid pointer whose atomic CRTC mode is inspected.
    let mode = unsafe { connector_state_get_mode(conn_state) };

    if conn_state_ref.hdmi.output_format != DRM_OUTPUT_COLOR_FORMAT_RGB444 {
        return true;
    }

    if conn_state_ref.hdmi.broadcast_rgb == DRM_HDMI_BROADCAST_RGB_FULL {
        return false;
    }

    if conn_state_ref.hdmi.broadcast_rgb == DRM_HDMI_BROADCAST_RGB_LIMITED {
        return true;
    }

    if !info.is_hdmi {
        return false;
    }

    // SAFETY: `mode` is obtained from the connector state's CRTC mode and points to a valid `DrmDisplayMode`.
    unsafe { drm_default_rgb_quant_range(mode) == HDMI_QUANTIZATION_RANGE_LIMITED }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c sink_supports_format_bpc()
/// Checks if the connected sink supports the specified color format and bits-per-component (BPC).
///
/// # Safety
///
/// `connector`, `info`, and `mode` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector`, `DrmDisplayInfo`, and `DrmDisplayMode` structs respectively.
unsafe fn sink_supports_format_bpc(
    connector: *const DrmConnector,
    info: *const DrmDisplayInfo,
    mode: *const DrmDisplayMode,
    format: DrmOutputColorFormat,
    bpc: u32,
) -> bool {
    // SAFETY: `connector` is non-null, properly aligned, and points to a valid `DrmConnector`.
    let dev = unsafe { (*connector).dev as *const c_void };
    // SAFETY: `mode` is non-null, properly aligned, and points to a valid `DrmDisplayMode`.
    let vic = unsafe { drm_match_cea_mode(mode) };

    if vic == 1 && bpc != 8 {
        drm_dbg_kms!(dev, "VIC1 requires a bpc of 8, got {}\n", bpc);
        return false;
    }

    // SAFETY: `info` is non-null, properly aligned, and points to a valid `DrmDisplayInfo`.
    if unsafe { !(*info).is_hdmi } && (format != DRM_OUTPUT_COLOR_FORMAT_RGB444 || bpc != 8) {
        drm_dbg_kms!(dev, "DVI Monitors require an RGB output at 8 bpc\n");
        return false;
    }

    // SAFETY: `connector` is non-null, properly aligned, and points to a valid `DrmConnector`.
    let supported_formats = unsafe { (*connector).hdmi.supported_formats };
    if (supported_formats & (1 << format)) == 0 {
        // SAFETY: `drm_hdmi_connector_get_output_format_name` returns a static null-terminated C string
        // for known format enums, which is dereferenceable.
        unsafe {
            let fmt_name = drm_hdmi_connector_get_output_format_name(format);
            let s = core::ffi::CStr::from_ptr(fmt_name)
                .to_str()
                .unwrap_or("unknown");
            drm_dbg_kms!(dev, "{} format unsupported by the connector.\n", s);
        }
        return false;
    }

    // SAFETY: `info` and `mode` are valid, initialized pointers.
    if unsafe { drm_mode_is_420_only(info, mode) } && format != DRM_OUTPUT_COLOR_FORMAT_YCBCR420 {
        drm_dbg_kms!(dev, "Mode can be only supported in YUV420 format.\n");
        return false;
    }

    match format {
        DRM_OUTPUT_COLOR_FORMAT_RGB444 => {
            drm_dbg_kms!(dev, "RGB Format, checking the constraints.\n");

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if (unsafe { (*info).color_formats } & (1 << DRM_OUTPUT_COLOR_FORMAT_RGB444)) == 0 {
                drm_warn!(dev, "HDMI Sink doesn't support RGB, something's wrong.\n");
            }

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if bpc == 10
                && (unsafe { (*info).edid_hdmi_rgb444_dc_modes } & DRM_EDID_HDMI_DC_30) == 0
            {
                drm_dbg_kms!(dev, "10 BPC but sink doesn't support Deep Color 30.\n");
                return false;
            }

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if bpc == 12
                && (unsafe { (*info).edid_hdmi_rgb444_dc_modes } & DRM_EDID_HDMI_DC_36) == 0
            {
                drm_dbg_kms!(dev, "12 BPC but sink doesn't support Deep Color 36.\n");
                return false;
            }

            drm_dbg_kms!(dev, "RGB format supported in that configuration.\n");
            true
        }
        DRM_OUTPUT_COLOR_FORMAT_YCBCR420 => {
            drm_dbg_kms!(dev, "YUV420 format, checking the constraints.\n");

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if (unsafe { (*info).color_formats } & (1 << DRM_OUTPUT_COLOR_FORMAT_YCBCR420)) == 0 {
                drm_dbg_kms!(dev, "Sink doesn't support YUV420.\n");
                return false;
            }

            // SAFETY: `info` and `mode` are valid, initialized pointers.
            if unsafe { !drm_mode_is_420(info, mode) } {
                drm_dbg_kms!(dev, "Mode cannot be supported in YUV420 format.\n");
                return false;
            }

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            let y420_dc = unsafe { (*info).hdmi.y420_dc_modes };
            if bpc == 10 && (y420_dc & DRM_EDID_YCBCR420_DC_30) == 0 {
                drm_dbg_kms!(dev, "10 BPC but sink doesn't support Deep Color 30.\n");
                return false;
            }

            if bpc == 12 && (y420_dc & DRM_EDID_YCBCR420_DC_36) == 0 {
                drm_dbg_kms!(dev, "12 BPC but sink doesn't support Deep Color 36.\n");
                return false;
            }

            if bpc == 16 && (y420_dc & DRM_EDID_YCBCR420_DC_48) == 0 {
                drm_dbg_kms!(dev, "16 BPC but sink doesn't support Deep Color 48.\n");
                return false;
            }

            drm_dbg_kms!(dev, "YUV420 format supported in that configuration.\n");
            true
        }
        DRM_OUTPUT_COLOR_FORMAT_YCBCR422 => {
            drm_dbg_kms!(dev, "YUV422 format, checking the constraints.\n");

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if (unsafe { (*info).color_formats } & (1 << DRM_OUTPUT_COLOR_FORMAT_YCBCR422)) == 0 {
                drm_dbg_kms!(dev, "Sink doesn't support YUV422.\n");
                return false;
            }

            if bpc > 12 {
                drm_dbg_kms!(dev, "YUV422 only supports 12 bpc or lower.\n");
                return false;
            }

            drm_dbg_kms!(dev, "YUV422 format supported in that configuration.\n");
            true
        }
        DRM_OUTPUT_COLOR_FORMAT_YCBCR444 => {
            drm_dbg_kms!(dev, "YUV444 format, checking the constraints.\n");

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if (unsafe { (*info).color_formats } & (1 << DRM_OUTPUT_COLOR_FORMAT_YCBCR444)) == 0 {
                drm_dbg_kms!(dev, "Sink doesn't support YUV444.\n");
                return false;
            }

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if bpc == 10
                && (unsafe { (*info).edid_hdmi_ycbcr444_dc_modes } & DRM_EDID_HDMI_DC_30) == 0
            {
                drm_dbg_kms!(dev, "10 BPC but sink doesn't support Deep Color 30.\n");
                return false;
            }

            // SAFETY: `info` is valid and points to an initialized `DrmDisplayInfo`.
            if bpc == 12
                && (unsafe { (*info).edid_hdmi_ycbcr444_dc_modes } & DRM_EDID_HDMI_DC_36) == 0
            {
                drm_dbg_kms!(dev, "12 BPC but sink doesn't support Deep Color 36.\n");
                return false;
            }

            drm_dbg_kms!(dev, "YUV444 format supported in that configuration.\n");
            true
        }
        _ => {
            drm_dbg_kms!(dev, "Unsupported pixel format.\n");
            false
        }
    }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_clock_valid()
/// Validates whether the given clock rate is acceptable for the connector and display mode.
///
/// # Safety
///
/// `connector` and `mode` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmDisplayMode` structs.
unsafe fn hdmi_clock_valid(
    connector: *const DrmConnector,
    mode: *const DrmDisplayMode,
    clock: u64,
) -> DrmModeStatus {
    // SAFETY: `connector` is non-null, properly aligned, and points to a valid `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `connector` is non-null and valid; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };

    if info.max_tmds_clock != 0 && clock > (info.max_tmds_clock as u64) * 1000 {
        return MODE_CLOCK_HIGH;
    }

    // SAFETY: `funcs` is non-null if reaching here, pointing to valid `DrmConnectorHdmiFuncs`.
    if !funcs.is_null()
        && let Some(tmds_valid) = unsafe { (*funcs).tmds_char_rate_valid }
    {
        // SAFETY: `tmds_valid` is a valid function pointer; `connector` and `mode` are valid DRM pointers.
        let status = unsafe {
            tmds_valid(
                connector as *mut DrmConnector,
                mode as *mut DrmDisplayMode,
                clock,
            )
        };
        if status != MODE_OK {
            return status;
        }
    }

    MODE_OK
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_compute_clock()
/// Computes the TMDS character rate for the given mode, BPC, and color format, and verifies validity.
///
/// # Safety
///
/// `connector`, `conn_state`, and `mode` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector`, `DrmConnectorState`, and `DrmDisplayMode` structs.
unsafe fn hdmi_compute_clock(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
    mode: *const DrmDisplayMode,
    bpc: u32,
    fmt: DrmOutputColorFormat,
) -> i32 {
    // SAFETY: `mode` is non-null and points to a valid `DrmDisplayMode`.
    let clock = unsafe { drm_hdmi_compute_mode_clock(mode, bpc, fmt) };
    if clock == 0 {
        return -EINVAL;
    }

    // SAFETY: `connector` and `mode` are valid pointers forwarded from caller.
    let status = unsafe { hdmi_clock_valid(connector, mode, clock) };
    if status != MODE_OK {
        return -EINVAL;
    }

    // SAFETY: `conn_state` is valid and aligned; caller provides mutable access to connector state.
    unsafe { (*conn_state).hdmi.tmds_char_rate = clock };
    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_try_format_bpc()
/// Attempts to configure the connector state with the specified BPC and format.
///
/// # Safety
///
/// `connector`, `conn_state`, and `mode` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector`, `DrmConnectorState`, and `DrmDisplayMode` structs.
unsafe fn hdmi_try_format_bpc(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
    mode: *const DrmDisplayMode,
    bpc: u32,
    fmt: DrmOutputColorFormat,
) -> bool {
    // SAFETY: `connector` is valid, non-null, and points to an initialized `DrmConnector`.
    let info = unsafe { &(*connector).display_info };
    // SAFETY: `connector` is valid, non-null, and points to an initialized `DrmConnector`.
    let dev = unsafe { (*connector).dev as *const c_void };

    // SAFETY: `drm_hdmi_connector_get_output_format_name` returns a static null-terminated C string.
    unsafe {
        let fmt_name = drm_hdmi_connector_get_output_format_name(fmt);
        let s = core::ffi::CStr::from_ptr(fmt_name)
            .to_str()
            .unwrap_or("unknown");
        drm_dbg_kms!(dev, "Trying {} output format with {} bpc\n", s, bpc);
    }

    // SAFETY: `connector`, `info`, and `mode` are valid, initialized pointers.
    if unsafe { !sink_supports_format_bpc(connector, info, mode, fmt, bpc) } {
        // SAFETY: `drm_hdmi_connector_get_output_format_name` returns a static null-terminated C string.
        unsafe {
            let fmt_name = drm_hdmi_connector_get_output_format_name(fmt);
            let s = core::ffi::CStr::from_ptr(fmt_name)
                .to_str()
                .unwrap_or("unknown");
            drm_dbg_kms!(dev, "{} output format not supported with {} bpc\n", s, bpc);
        }
        return false;
    }

    // SAFETY: `connector`, `conn_state`, and `mode` are valid pointers.
    let ret = unsafe { hdmi_compute_clock(connector, conn_state, mode, bpc, fmt) };
    if ret != 0 {
        // SAFETY: `drm_hdmi_connector_get_output_format_name` returns a static null-terminated C string.
        unsafe {
            let fmt_name = drm_hdmi_connector_get_output_format_name(fmt);
            let s = core::ffi::CStr::from_ptr(fmt_name)
                .to_str()
                .unwrap_or("unknown");
            drm_dbg_kms!(
                dev,
                "Couldn't compute clock for {} output format and {} bpc\n",
                s,
                bpc
            );
        }
        return false;
    }

    // SAFETY: `conn_state` is valid and its `tmds_char_rate` was populated by `hdmi_compute_clock`.
    // Format name returns a static null-terminated C string.
    unsafe {
        let fmt_name = drm_hdmi_connector_get_output_format_name(fmt);
        let s = core::ffi::CStr::from_ptr(fmt_name)
            .to_str()
            .unwrap_or("unknown");
        drm_dbg_kms!(
            dev,
            "{} output format supported with {} bpc (TMDS char rate: {} Hz)\n",
            s,
            bpc,
            (*conn_state).hdmi.tmds_char_rate
        );
    }

    true
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_compute_format_bpc()
/// Searches for a valid BPC configuration down from `max_bpc` to 8 for the given color format.
///
/// # Safety
///
/// `connector`, `conn_state`, and `mode` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector`, `DrmConnectorState`, and `DrmDisplayMode` structs.
unsafe fn hdmi_compute_format_bpc(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
    mode: *const DrmDisplayMode,
    max_bpc: u32,
    fmt: DrmOutputColorFormat,
) -> i32 {
    // SAFETY: `connector` is non-null and points to a valid `DrmConnector`.
    let dev = unsafe { (*connector).dev as *const c_void };
    let mut bpc = max_bpc;

    while bpc >= 8 {
        // SAFETY: `connector`, `conn_state`, and `mode` are valid pointers.
        let ret = unsafe { hdmi_try_format_bpc(connector, conn_state, mode, bpc, fmt) };
        if ret {
            // SAFETY: `conn_state` is valid; writing output fields and logging mode details.
            // Mode and connector device pointers are valid; format name is a static C string.
            unsafe {
                (*conn_state).hdmi.output_bpc = bpc;
                (*conn_state).hdmi.output_format = fmt;

                let fmt_name =
                    drm_hdmi_connector_get_output_format_name((*conn_state).hdmi.output_format);
                let s = core::ffi::CStr::from_ptr(fmt_name)
                    .to_str()
                    .unwrap_or("unknown");
                drm_dbg_kms!(
                    dev,
                    "Mode {}x{} @ {}Hz: Found configuration: bpc: {}, fmt: {}, clock: {}\n",
                    (*mode).hdisplay,
                    (*mode).vdisplay,
                    drm_mode_vrefresh(mode),
                    (*conn_state).hdmi.output_bpc,
                    s,
                    (*conn_state).hdmi.tmds_char_rate
                );
            }
            return 0;
        }
        if bpc < 2 {
            break;
        }
        bpc -= 2;
    }

    // SAFETY: `drm_hdmi_connector_get_output_format_name` returns a static null-terminated C string.
    unsafe {
        let fmt_name = drm_hdmi_connector_get_output_format_name(fmt);
        let s = core::ffi::CStr::from_ptr(fmt_name)
            .to_str()
            .unwrap_or("unknown");
        drm_dbg_kms!(
            dev,
            "Failed. {} output format not supported for any bpc count.\n",
            s
        );
    }

    -EINVAL
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_compute_config()
/// Computes HDMI configuration (BPC and color format) for the connector state.
///
/// # Safety
///
/// `connector`, `conn_state`, and `mode` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector`, `DrmConnectorState`, and `DrmDisplayMode` structs.
unsafe fn hdmi_compute_config(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
    mode: *const DrmDisplayMode,
) -> i32 {
    // SAFETY: `conn_state` is non-null and points to a valid `DrmConnectorState`.
    let raw_max = unsafe { (*conn_state).max_bpc as u32 };
    // SAFETY: `connector` is non-null and points to a valid `DrmConnector`.
    let conn_max = unsafe { (*connector).max_bpc };
    let max_bpc = min(max(raw_max, 8), conn_max);

    // SAFETY: `connector`, `conn_state`, and `mode` are valid pointers.
    let mut ret = unsafe {
        hdmi_compute_format_bpc(
            connector,
            conn_state,
            mode,
            max_bpc,
            DRM_OUTPUT_COLOR_FORMAT_RGB444,
        )
    };

    if ret != 0 {
        // SAFETY: `connector` is non-null and points to a valid `DrmConnector`.
        if unsafe { (*connector).ycbcr_420_allowed } {
            // SAFETY: `connector`, `conn_state`, and `mode` are valid pointers.
            ret = unsafe {
                hdmi_compute_format_bpc(
                    connector,
                    conn_state,
                    mode,
                    max_bpc,
                    DRM_OUTPUT_COLOR_FORMAT_YCBCR420,
                )
            };
            if ret != 0 {
                // SAFETY: `connector` is valid; dereferencing `dev` for debug logging.
                unsafe {
                    drm_dbg_kms!(
                        (*connector).dev as *const c_void,
                        "YUV420 output format doesn't work.\n"
                    )
                };
            }
        } else {
            // SAFETY: `connector` is valid; dereferencing `dev` for debug logging.
            unsafe {
                drm_dbg_kms!(
                    (*connector).dev as *const c_void,
                    "YUV420 output format not allowed for connector.\n"
                )
            };
            ret = -EINVAL;
        }
    }

    ret
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_generate_avi_infoframe()
/// Generates the AVI (Auxiliary Video Information) infoframe for the HDMI connector state.
///
/// # Safety
///
/// `connector` and `conn_state` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorState` structs. `conn_state` must have a valid mode configured.
unsafe fn hdmi_generate_avi_infoframe(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
) -> i32 {
    // SAFETY: `conn_state` is valid and points to an initialized `DrmConnectorState`.
    let mode = unsafe { connector_state_get_mode(conn_state) };
    // SAFETY: `conn_state` is valid; `core::ptr::addr_of_mut!` generates raw pointer to embedded `avi` infoframe.
    let infoframe = unsafe { core::ptr::addr_of_mut!((*conn_state).hdmi.infoframes.avi) };
    // SAFETY: `infoframe` is valid and points to the `avi` field within `conn_state`.
    let frame = unsafe { core::ptr::addr_of_mut!((*infoframe).data.avi) as *mut HdmiAviInfoframe };
    // SAFETY: `conn_state` is valid and points to an initialized `DrmConnectorState`.
    let is_lim_range = unsafe { (*conn_state).hdmi.is_limited_range };
    let rgb_quant_range = if is_lim_range {
        HDMI_QUANTIZATION_RANGE_LIMITED
    } else {
        HDMI_QUANTIZATION_RANGE_FULL
    };

    // SAFETY: `infoframe` is a valid pointer to the connector state's AVI infoframe storage.
    unsafe { (*infoframe).set = false };

    // SAFETY: `frame`, `connector`, and `mode` are valid pointers to their respective DRM structs.
    let ret = unsafe {
        drm_hdmi_avi_infoframe_from_display_mode(frame, connector as *mut DrmConnector, mode)
    };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `frame`, `connector`, and `conn_state` are valid initialized pointers.
    // The infoframe fields and helpers safely populate the AVI frame.
    unsafe {
        (*frame).colorspace =
            output_color_format_to_hdmi_colorspace(&*connector, (*conn_state).hdmi.output_format);

        drm_hdmi_avi_infoframe_quant_range(
            frame,
            connector as *mut DrmConnector,
            mode,
            rgb_quant_range,
        );
        drm_hdmi_avi_infoframe_colorimetry(frame, conn_state);
        drm_hdmi_avi_infoframe_bars(frame, conn_state);

        (*infoframe).set = true;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_generate_spd_infoframe()
/// Generates the Source Product Description (SPD) infoframe for the HDMI connector state.
///
/// # Safety
///
/// `connector` and `conn_state` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorState` structs.
unsafe fn hdmi_generate_spd_infoframe(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
) -> i32 {
    // SAFETY: `conn_state` is valid; `core::ptr::addr_of_mut!` takes address of embedded `spd` infoframe.
    let infoframe = unsafe { core::ptr::addr_of_mut!((*conn_state).hdmi.infoframes.spd) };
    // SAFETY: `infoframe` is valid and points to the `spd` field within `conn_state`.
    let frame = unsafe { core::ptr::addr_of_mut!((*infoframe).data.spd) as *mut HdmiSpdInfoframe };

    // SAFETY: `infoframe` is a valid pointer to the connector state's SPD infoframe.
    unsafe { (*infoframe).set = false };

    // SAFETY: `connector` is valid and points to an initialized `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `funcs` is non-null if reaching here, pointing to valid `DrmConnectorHdmiFuncs`.
    if funcs.is_null() || unsafe { (*funcs).spd.write_infoframe.is_none() } {
        return 0;
    }

    // SAFETY: `connector` is valid; `vendor` and `product` are fixed-size byte arrays within `hdmi`.
    let vendor_ptr = unsafe { (*connector).hdmi.vendor.as_ptr() as *const c_char };
    // SAFETY: `connector` is valid; `product` is a fixed-size byte array within `hdmi`.
    let prod_ptr = unsafe { (*connector).hdmi.product.as_ptr() as *const c_char };
    // SAFETY: `frame`, `vendor_ptr`, and `prod_ptr` point to valid memory buffers.
    let ret = unsafe { hdmi_spd_infoframe_init(frame, vendor_ptr, prod_ptr) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `frame` and `infoframe` point to valid memory within `conn_state`.
    unsafe {
        (*frame).sdi = HDMI_SPD_SDI_PC;
        (*infoframe).set = true;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_generate_hdr_infoframe()
/// Generates the Dynamic Range and Mastering (DRM/HDR) infoframe for the HDMI connector state.
///
/// # Safety
///
/// `connector` and `conn_state` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorState` structs.
unsafe fn hdmi_generate_hdr_infoframe(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
) -> i32 {
    // SAFETY: `conn_state` is valid; taking raw address of embedded `hdr_drm` infoframe.
    let infoframe = unsafe { core::ptr::addr_of_mut!((*conn_state).hdmi.infoframes.hdr_drm) };
    // SAFETY: `infoframe` is valid and points to the `hdr_drm` field within `conn_state`.
    let frame = unsafe { core::ptr::addr_of_mut!((*infoframe).data.drm) as *mut HdmiDrmInfoframe };

    // SAFETY: `infoframe` points to valid memory within `conn_state`.
    unsafe { (*infoframe).set = false };

    // SAFETY: `connector` is valid and points to an initialized `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `funcs` is non-null if reaching here, pointing to valid `DrmConnectorHdmiFuncs`.
    if funcs.is_null() || unsafe { (*funcs).hdr_drm.write_infoframe.is_none() } {
        return 0;
    }

    // SAFETY: `connector` is valid and points to an initialized `DrmConnector`.
    if unsafe { (*connector).max_bpc } < 10 {
        return 0;
    }

    // SAFETY: `conn_state` is valid and points to an initialized `DrmConnectorState`.
    if unsafe { (*conn_state).hdr_output_metadata.is_null() } {
        return 0;
    }

    // SAFETY: `frame` and `conn_state` are valid pointers; `conn_state->hdr_output_metadata` is non-null.
    let ret = unsafe { drm_hdmi_infoframe_set_hdr_metadata(frame, conn_state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `infoframe` is a valid pointer to the connector state's HDR infoframe.
    unsafe { (*infoframe).set = true };
    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_generate_hdmi_vendor_infoframe()
/// Generates the HDMI Vendor-Specific infoframe for the HDMI connector state.
///
/// # Safety
///
/// `connector` and `conn_state` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorState` structs. `conn_state` must have a valid mode configured.
unsafe fn hdmi_generate_hdmi_vendor_infoframe(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
) -> i32 {
    // SAFETY: `connector` is valid; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };
    // SAFETY: `conn_state` is valid and points to an initialized `DrmConnectorState`.
    let mode = unsafe { connector_state_get_mode(conn_state) };
    // SAFETY: `conn_state` is valid; taking raw address of embedded `hdmi` infoframe.
    let infoframe = unsafe { core::ptr::addr_of_mut!((*conn_state).hdmi.infoframes.hdmi) };
    // SAFETY: `infoframe` is valid and points to the `hdmi` vendor infoframe data within `conn_state`.
    let frame = unsafe { core::ptr::addr_of_mut!((*infoframe).data) as *mut HdmiVendorInfoframe };

    // SAFETY: `infoframe` points to valid memory within `conn_state`.
    unsafe { (*infoframe).set = false };

    if !info.has_hdmi_infoframe {
        return 0;
    }

    // SAFETY: `frame`, `connector`, and `mode` are valid pointers to initialized DRM objects.
    let ret = unsafe {
        drm_hdmi_vendor_infoframe_from_display_mode(frame, connector as *mut DrmConnector, mode)
    };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `infoframe` points to valid memory within `conn_state`.
    unsafe { (*infoframe).set = true };
    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c hdmi_generate_infoframes()
/// Generates all applicable HDMI infoframes for the connector state.
///
/// # Safety
///
/// `connector` and `conn_state` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorState` structs. `conn_state` must have a valid mode configured.
unsafe fn hdmi_generate_infoframes(
    connector: *const DrmConnector,
    conn_state: *mut DrmConnectorState,
) -> i32 {
    // SAFETY: `connector` is valid; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };
    if !info.is_hdmi {
        return 0;
    }

    // SAFETY: `connector` and `conn_state` are valid pointers with a valid mode.
    let mut ret = unsafe { hdmi_generate_avi_infoframe(connector, conn_state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `connector` and `conn_state` are valid pointers.
    ret = unsafe { hdmi_generate_spd_infoframe(connector, conn_state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `connector` and `conn_state` are valid pointers.
    ret = unsafe { hdmi_generate_hdr_infoframe(connector, conn_state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `connector` and `conn_state` are valid pointers with a valid mode.
    ret = unsafe { hdmi_generate_hdmi_vendor_infoframe(connector, conn_state) };
    if ret != 0 {
        return ret;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_check()
#[unsafe(no_mangle)]
/// DRM atomic helper check hook for HDMI connectors.
///
/// Validates and computes HDMI configuration and infoframes during atomic check.
///
/// # Safety
///
/// If non-null, `connector` and `state` must point to valid, initialized DRM connector
/// and atomic commit state structures belonging to the same DRM device.
pub unsafe extern "C" fn drm_atomic_helper_connector_hdmi_check(
    connector: *mut DrmConnector,
    state: *mut DrmAtomicCommit,
) -> i32 {
    if connector.is_null() || state.is_null() {
        return -EINVAL;
    }

    // SAFETY: `state` and `connector` are checked non-null above and point to valid DRM structures.
    let old_conn_state = unsafe { drm_atomic_get_old_connector_state_inline(state, connector) };
    // SAFETY: `state` and `connector` are checked non-null above and point to valid DRM structures.
    let new_conn_state = unsafe { drm_atomic_get_new_connector_state_inline(state, connector) };
    if old_conn_state.is_null() || new_conn_state.is_null() {
        return 0;
    }

    // SAFETY: `new_conn_state` is verified non-null above and points to a valid `DrmConnectorState`.
    if unsafe { (*new_conn_state).crtc.is_null() || (*new_conn_state).best_encoder.is_null() } {
        return 0;
    }

    // SAFETY: `new_conn_state` is non-null and points to a valid `DrmConnectorState`.
    let mode = unsafe { connector_state_get_mode(new_conn_state) };
    // SAFETY: `connector`, `new_conn_state`, and `mode` are valid DRM structures.
    let mut ret = unsafe { hdmi_compute_config(connector, new_conn_state, mode) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `new_conn_state` is non-null and valid; updating its `is_limited_range` field.
    // `connector` and `new_conn_state` are valid initialized pointers.
    unsafe {
        (*new_conn_state).hdmi.is_limited_range = hdmi_is_limited_range(connector, new_conn_state);
    }

    // SAFETY: `connector` and `new_conn_state` are valid pointers with valid mode.
    ret = unsafe { hdmi_generate_infoframes(connector, new_conn_state) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `old_conn_state` is verified non-null and points to a valid `DrmConnectorState`.
    let old_broadcast = unsafe { (*old_conn_state).hdmi.broadcast_rgb };
    // SAFETY: `new_conn_state` is verified non-null and points to a valid `DrmConnectorState`.
    let new_broadcast = unsafe { (*new_conn_state).hdmi.broadcast_rgb };
    // SAFETY: `old_conn_state` is verified non-null and points to a valid `DrmConnectorState`.
    let old_bpc = unsafe { (*old_conn_state).hdmi.output_bpc };
    // SAFETY: `new_conn_state` is verified non-null and points to a valid `DrmConnectorState`.
    let new_bpc = unsafe { (*new_conn_state).hdmi.output_bpc };
    // SAFETY: `old_conn_state` is verified non-null and points to a valid `DrmConnectorState`.
    let old_fmt = unsafe { (*old_conn_state).hdmi.output_format };
    // SAFETY: `new_conn_state` is verified non-null and points to a valid `DrmConnectorState`.
    let new_fmt = unsafe { (*new_conn_state).hdmi.output_format };

    if old_broadcast != new_broadcast || old_bpc != new_bpc || old_fmt != new_fmt {
        // SAFETY: `new_conn_state` is verified non-null and its `crtc` was verified non-null above.
        let crtc = unsafe { (*new_conn_state).crtc };
        // SAFETY: `state` and `crtc` are valid non-null pointers to atomic state and CRTC.
        let crtc_state = unsafe { drm_atomic_get_crtc_state(state, crtc) };
        if is_err(crtc_state as *const c_void) {
            return ptr_err(crtc_state as *const c_void);
        }

        // crtc_state->mode_changed = true; (bit 1 in _bitfields_10)
        // SAFETY: `crtc_state` is checked against `is_err` above, properly aligned, and points to a valid `DrmCrtcState`.
        unsafe { (*crtc_state)._bitfields_10[0] |= 1 << 1 };
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_hdmi_connector_mode_valid()
#[unsafe(no_mangle)]
/// Validates whether a display mode is supported by an HDMI connector.
///
/// # Safety
///
/// If non-null, `connector` and `mode` must point to valid, initialized `DrmConnector`
/// and `DrmDisplayMode` structs.
pub unsafe extern "C" fn drm_hdmi_connector_mode_valid(
    connector: *mut DrmConnector,
    mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    if connector.is_null() || mode.is_null() {
        return MODE_ERROR;
    }

    // SAFETY: `mode` is verified non-null above and points to a valid `DrmDisplayMode`.
    let clock = unsafe { drm_hdmi_compute_mode_clock(mode, 8, DRM_OUTPUT_COLOR_FORMAT_RGB444) };
    if clock == 0 {
        return MODE_ERROR;
    }

    // SAFETY: `connector` and `mode` are verified non-null and valid.
    unsafe { hdmi_clock_valid(connector, mode, clock) }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c clear_infoframe()
/// Clears an infoframe via the connector's HDMI function table.
///
/// # Safety
///
/// `connector` must be a valid, aligned, non-null pointer to an initialized `DrmConnector`.
/// `funcs` must either be null or point to a valid `DrmConnectorInfoframeFuncs`.
unsafe fn clear_infoframe(
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorInfoframeFuncs,
    info_type: &str,
) -> i32 {
    // SAFETY: `connector` is valid and points to an initialized `DrmConnector`.
    let dev = unsafe { (*connector).dev as *const c_void };
    drm_dbg_kms!(dev, "Clearing {} InfoFrame\n", info_type);

    if funcs.is_null() {
        return -EOPNOTSUPP;
    }
    // SAFETY: `funcs` is verified non-null above, pointing to a valid `DrmConnectorInfoframeFuncs`.
    let clear_fn = unsafe { (*funcs).clear_infoframe };
    let Some(clear_fn) = clear_fn else {
        drm_dbg_kms!(dev, "Function not implemented, bailing.\n");
        return -EOPNOTSUPP;
    };

    // SAFETY: `clear_fn` is verified non-null/Some above, and `connector` is a valid pointer.
    let ret = unsafe { clear_fn(connector) };
    if ret != 0 {
        drm_dbg_kms!(dev, "Call failed: {}\n", ret);
        return ret;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c write_infoframe()
/// Writes an infoframe via the connector's HDMI function table.
///
/// # Safety
///
/// `connector` and `new_frame` must be valid, aligned, non-null pointers to initialized
/// `DrmConnector` and `DrmConnectorHdmiInfoframe` structs. `funcs` must either be null
/// or point to a valid `DrmConnectorInfoframeFuncs`.
unsafe fn write_infoframe(
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorInfoframeFuncs,
    info_type: &str,
    new_frame: *mut DrmConnectorHdmiInfoframe,
) -> i32 {
    // SAFETY: `connector` is valid and points to an initialized `DrmConnector`.
    let dev = unsafe { (*connector).dev as *const c_void };
    let mut buffer = [0u8; HDMI_INFOFRAME_HEADER_SIZE + HDMI_MAX_INFOFRAME_SIZE];

    drm_dbg_kms!(dev, "Writing {} InfoFrame\n", info_type);

    if funcs.is_null() {
        return -EOPNOTSUPP;
    }
    // SAFETY: `funcs` is verified non-null above, pointing to a valid `DrmConnectorInfoframeFuncs`.
    let write_fn = unsafe { (*funcs).write_infoframe };
    let Some(write_fn) = write_fn else {
        drm_dbg_kms!(dev, "Function not implemented, bailing.\n");
        return -EOPNOTSUPP;
    };

    // SAFETY: `new_frame` is valid; taking raw pointer to `data` union and writing into local stack `buffer`.
    let len = unsafe {
        hdmi_infoframe_pack(
            core::ptr::addr_of_mut!((*new_frame).data),
            buffer.as_mut_ptr() as *mut c_void,
            buffer.len(),
        )
    };
    if len < 0 {
        return len as i32;
    }

    // SAFETY: `write_fn` is valid, `connector` is valid, and `buffer` contains `len` initialized bytes.
    let ret = unsafe { write_fn(connector, buffer.as_mut_ptr(), len as usize) };
    if ret != 0 {
        drm_dbg_kms!(dev, "Call failed: {}\n", ret);
        return ret;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c write_or_clear_infoframe()
/// Writes or clears an infoframe depending on whether it is set in `new_frame`.
///
/// # Safety
///
/// `connector`, `old_frame`, and `new_frame` must be valid, aligned, non-null pointers to
/// initialized `DrmConnector` and `DrmConnectorHdmiInfoframe` structs. `funcs` must either be
/// null or point to a valid `DrmConnectorInfoframeFuncs`.
unsafe fn write_or_clear_infoframe(
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorInfoframeFuncs,
    info_type: &str,
    old_frame: *mut DrmConnectorHdmiInfoframe,
    new_frame: *mut DrmConnectorHdmiInfoframe,
) -> i32 {
    // SAFETY: `new_frame` is valid and points to an initialized `DrmConnectorHdmiInfoframe`.
    if unsafe { (*new_frame).set } {
        // SAFETY: `connector`, `funcs`, and `new_frame` are valid pointers forwarded from caller.
        return unsafe { write_infoframe(connector, funcs, info_type, new_frame) };
    }

    // SAFETY: `old_frame` and `new_frame` are valid pointers to initialized `DrmConnectorHdmiInfoframe` structs.
    if unsafe { (*old_frame).set && !(*new_frame).set } {
        // SAFETY: `connector` and `funcs` are valid pointers forwarded from caller.
        return unsafe { clear_infoframe(connector, funcs, info_type) };
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_update_infoframes()
#[unsafe(no_mangle)]
/// Updates all HDMI infoframes for a connector during atomic commit.
///
/// # Safety
///
/// If non-null, `connector` and `state` must point to valid, initialized `DrmConnector`
/// and `DrmAtomicCommit` structs belonging to the same DRM device.
pub unsafe extern "C" fn drm_atomic_helper_connector_hdmi_update_infoframes(
    connector: *mut DrmConnector,
    state: *mut DrmAtomicCommit,
) -> i32 {
    if connector.is_null() || state.is_null() {
        return -EINVAL;
    }

    // SAFETY: `connector` is checked non-null above, properly aligned, and points to a valid `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `state` and `connector` are checked non-null above and point to valid DRM structures.
    let old_conn_state = unsafe { drm_atomic_get_old_connector_state_inline(state, connector) };
    // SAFETY: `state` and `connector` are checked non-null above and point to valid DRM structures.
    let new_conn_state = unsafe { drm_atomic_get_new_connector_state_inline(state, connector) };
    // SAFETY: `connector` is non-null and valid; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };

    if !info.is_hdmi {
        return 0;
    }

    if funcs.is_null() {
        // SAFETY: `connector` is non-null; dereferencing `dev` for debug logging.
        unsafe {
            drm_dbg_kms!(
                (*connector).dev as *const c_void,
                "Function not implemented, bailing.\n"
            )
        };
        return -EINVAL;
    }

    // SAFETY: `connector` is valid; raw pointer to embedded `lock` mutex within `hdmi.infoframes`.
    let lock_ptr = unsafe { core::ptr::addr_of_mut!((*connector).hdmi.infoframes.lock) };
    // SAFETY: `lock_ptr` points to an initialized mutex within `connector`.
    unsafe { mutex_lock(lock_ptr) };

    let mut ret = 0;
    if !old_conn_state.is_null() && !new_conn_state.is_null() {
        // SAFETY: `funcs` is non-null and `old_conn_state`/`new_conn_state` are verified non-null;
        // raw pointers to `avi` infoframes and `funcs->avi` are valid and aligned.
        ret = unsafe {
            write_or_clear_infoframe(
                connector,
                core::ptr::addr_of!((*funcs).avi),
                "AVI",
                core::ptr::addr_of_mut!((*old_conn_state).hdmi.infoframes.avi),
                core::ptr::addr_of_mut!((*new_conn_state).hdmi.infoframes.avi),
            )
        };
        if ret != 0 {
            // SAFETY: `lock_ptr` was locked above and must be released on error return.
            unsafe { mutex_unlock(lock_ptr) };
            return ret;
        }

        // SAFETY: `connector` is valid and points to an initialized `DrmConnector`.
        if unsafe { (*connector).hdmi.infoframes.audio.set } {
            // SAFETY: `connector` is valid; `funcs->audio` and embedded `audio` infoframe pointers are valid.
            ret = unsafe {
                write_infoframe(
                    connector,
                    core::ptr::addr_of!((*funcs).audio),
                    "Audio",
                    core::ptr::addr_of_mut!((*connector).hdmi.infoframes.audio),
                )
            };
            if ret != 0 {
                // SAFETY: `lock_ptr` was locked above and must be released on error return.
                unsafe { mutex_unlock(lock_ptr) };
                return ret;
            }
        }

        // SAFETY: `funcs` is non-null and state pointers are non-null; raw pointers to `hdr_drm` are valid.
        ret = unsafe {
            write_or_clear_infoframe(
                connector,
                core::ptr::addr_of!((*funcs).hdr_drm),
                "HDR DRM",
                core::ptr::addr_of_mut!((*old_conn_state).hdmi.infoframes.hdr_drm),
                core::ptr::addr_of_mut!((*new_conn_state).hdmi.infoframes.hdr_drm),
            )
        };
        if ret != 0 {
            // SAFETY: `lock_ptr` was locked above and must be released on error return.
            unsafe { mutex_unlock(lock_ptr) };
            return ret;
        }

        // SAFETY: `funcs` is non-null and state pointers are non-null; raw pointers to `spd` are valid.
        ret = unsafe {
            write_or_clear_infoframe(
                connector,
                core::ptr::addr_of!((*funcs).spd),
                "SPD",
                core::ptr::addr_of_mut!((*old_conn_state).hdmi.infoframes.spd),
                core::ptr::addr_of_mut!((*new_conn_state).hdmi.infoframes.spd),
            )
        };
        if ret != 0 {
            // SAFETY: `lock_ptr` was locked above and must be released on error return.
            unsafe { mutex_unlock(lock_ptr) };
            return ret;
        }

        if info.has_hdmi_infoframe {
            // SAFETY: `funcs` is non-null and state pointers are non-null; raw pointers to `hdmi` are valid.
            ret = unsafe {
                write_or_clear_infoframe(
                    connector,
                    core::ptr::addr_of!((*funcs).hdmi),
                    "HDMI-VS",
                    core::ptr::addr_of_mut!((*old_conn_state).hdmi.infoframes.hdmi),
                    core::ptr::addr_of_mut!((*new_conn_state).hdmi.infoframes.hdmi),
                )
            };
            if ret != 0 {
                // SAFETY: `lock_ptr` was locked above and must be released on error return.
                unsafe { mutex_unlock(lock_ptr) };
                return ret;
            }
        }
    }

    // SAFETY: `lock_ptr` was acquired above and is released before exiting.
    unsafe { mutex_unlock(lock_ptr) };
    ret
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_update_audio_infoframe()
#[unsafe(no_mangle)]
/// Updates the audio infoframe for an HDMI connector.
///
/// # Safety
///
/// If non-null, `connector` and `frame` must point to valid, initialized `DrmConnector`
/// and `HdmiAudioInfoframe` structs.
pub unsafe extern "C" fn drm_atomic_helper_connector_hdmi_update_audio_infoframe(
    connector: *mut DrmConnector,
    frame: *mut HdmiAudioInfoframe,
) -> i32 {
    if connector.is_null() || frame.is_null() {
        return -EINVAL;
    }

    // SAFETY: `connector` is checked non-null above, properly aligned, and points to a valid `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `connector` is non-null and valid; taking raw address of embedded `audio` infoframe.
    let infoframe = unsafe { core::ptr::addr_of_mut!((*connector).hdmi.infoframes.audio) };
    // SAFETY: `connector` is non-null and valid; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };

    if !info.is_hdmi {
        return 0;
    }

    // SAFETY: `funcs` is non-null if reaching second condition, pointing to valid `DrmConnectorHdmiFuncs`.
    if funcs.is_null() || unsafe { (*funcs).audio.write_infoframe.is_none() } {
        // SAFETY: `connector` is non-null; dereferencing `dev` for debug logging.
        unsafe {
            drm_dbg_kms!(
                (*connector).dev as *const c_void,
                "Function not implemented, bailing.\n"
            )
        };
        return -EINVAL;
    }

    // SAFETY: `connector` is valid; raw pointer to embedded `lock` mutex within `hdmi.infoframes`.
    let lock_ptr = unsafe { core::ptr::addr_of_mut!((*connector).hdmi.infoframes.lock) };
    // SAFETY: `lock_ptr` points to an initialized mutex within `connector`.
    unsafe { mutex_lock(lock_ptr) };

    // SAFETY: `frame` is verified non-null and points to a valid `HdmiAudioInfoframe`.
    // `dest` points to valid memory within `(*connector).hdmi.infoframes.audio.data.audio`,
    // and both pointers are aligned and non-overlapping.
    unsafe {
        let dest = core::ptr::addr_of_mut!((*infoframe).data.audio) as *mut HdmiAudioInfoframe;
        core::ptr::copy_nonoverlapping(frame, dest, 1);
        (*infoframe).set = true;
    }

    // SAFETY: `connector`, `funcs->audio`, and `infoframe` are valid initialized pointers.
    let ret = unsafe {
        write_infoframe(
            connector,
            core::ptr::addr_of!((*funcs).audio),
            "Audio",
            infoframe,
        )
    };

    // SAFETY: `lock_ptr` was locked above and must be released before exiting.
    unsafe { mutex_unlock(lock_ptr) };
    ret
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_clear_audio_infoframe()
#[unsafe(no_mangle)]
/// Clears the audio infoframe for an HDMI connector.
///
/// # Safety
///
/// If non-null, `connector` must point to a valid, initialized `DrmConnector`.
pub unsafe extern "C" fn drm_atomic_helper_connector_hdmi_clear_audio_infoframe(
    connector: *mut DrmConnector,
) -> i32 {
    if connector.is_null() {
        return -EINVAL;
    }

    // SAFETY: `connector` is checked non-null above, properly aligned, and points to a valid `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `connector` is non-null and valid; taking raw address of embedded `audio` infoframe.
    let infoframe = unsafe { core::ptr::addr_of_mut!((*connector).hdmi.infoframes.audio) };
    // SAFETY: `connector` is non-null and valid; `display_info` is an embedded field.
    let info = unsafe { &(*connector).display_info };

    if !info.is_hdmi {
        return 0;
    }

    // SAFETY: `funcs` is non-null if reaching second condition, pointing to valid `DrmConnectorHdmiFuncs`.
    if funcs.is_null() || unsafe { (*funcs).audio.write_infoframe.is_none() } {
        // SAFETY: `connector` is non-null; dereferencing `dev` for debug logging.
        unsafe {
            drm_dbg_kms!(
                (*connector).dev as *const c_void,
                "Function not implemented, bailing.\n"
            )
        };
        return -EINVAL;
    }

    // SAFETY: `connector` is valid; taking raw pointer to embedded `lock` mutex.
    let lock_ptr = unsafe { core::ptr::addr_of_mut!((*connector).hdmi.infoframes.lock) };
    // SAFETY: `lock_ptr` points to an initialized mutex within `connector`.
    unsafe { mutex_lock(lock_ptr) };

    // SAFETY: `infoframe` points to valid memory within `connector`.
    unsafe { (*infoframe).set = false };

    // SAFETY: `connector` and `funcs->audio` are valid initialized pointers.
    let ret = unsafe { clear_infoframe(connector, core::ptr::addr_of!((*funcs).audio), "Audio") };

    // SAFETY: `infoframe` is valid; zeroes the infoframe data union and releases the lock.
    unsafe {
        core::ptr::write_bytes(core::ptr::addr_of_mut!((*infoframe).data), 0, 1);
        mutex_unlock(lock_ptr);
    }

    ret
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_update()
/// Updates HDMI connector state on hotplug or force events, reading EDID and updating CEC.
///
/// # Safety
///
/// If non-null, `connector` must point to a valid, initialized `DrmConnector`.
unsafe fn drm_atomic_helper_connector_hdmi_update(
    connector: *mut DrmConnector,
    status: DrmConnectorStatus,
) {
    if connector.is_null() {
        return;
    }

    if status == CONNECTOR_STATUS_DISCONNECTED {
        // SAFETY: `connector` is verified non-null above and points to a valid `DrmConnector`.
        unsafe {
            drm_edid_connector_update(connector, core::ptr::null());
            crate::drm_connector_upstream::drm_connector_cec_phys_addr_invalidate(connector);
        }
        return;
    }

    // SAFETY: `connector` is verified non-null above and points to a valid `DrmConnector`.
    let funcs = unsafe { (*connector).hdmi.funcs };
    // SAFETY: `funcs` is checked non-null before dereferencing `read_edid`.
    let drm_edid = if !funcs.is_null() && unsafe { (*funcs).read_edid.is_some() } {
        // SAFETY: `funcs` is non-null and `read_edid` is Some.
        let read_fn = unsafe { (*funcs).read_edid.unwrap() };
        // SAFETY: `read_fn` is a valid EDID reading function pointer from `funcs`.
        unsafe { read_fn(connector) as *const DrmEdid }
    } else {
        // SAFETY: Fallback `drm_edid_read` operates on valid non-null `connector`.
        unsafe { drm_edid_read(connector) }
    };

    // SAFETY: `connector` is valid; `drm_edid` is the pointer obtained above (or null).
    // `drm_edid_free` handles null safely and frees the allocated EDID.
    unsafe {
        drm_edid_connector_update(connector, drm_edid);
        drm_edid_free(drm_edid);
    }

    if status == CONNECTOR_STATUS_CONNECTED {
        // SAFETY: `connector` is verified non-null above and points to a valid `DrmConnector`.
        unsafe {
            crate::drm_connector_upstream::drm_connector_cec_phys_addr_set(connector);
        }
    }
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_hotplug()
#[unsafe(no_mangle)]
/// Handles HDMI connector hotplug detection event.
///
/// # Safety
///
/// If non-null, `connector` must point to a valid, initialized `DrmConnector`.
pub unsafe extern "C" fn drm_atomic_helper_connector_hdmi_hotplug(
    connector: *mut DrmConnector,
    status: DrmConnectorStatus,
) {
    // SAFETY: `connector` is forwarded to `drm_atomic_helper_connector_hdmi_update`,
    // which handles null checking and valid connector state update.
    unsafe { drm_atomic_helper_connector_hdmi_update(connector, status) };
}

// upstream: drivers/gpu/drm/display/drm_hdmi_state_helper.c drm_atomic_helper_connector_hdmi_force()
#[unsafe(no_mangle)]
/// Forces update of HDMI connector state using its current status.
///
/// # Safety
///
/// If non-null, `connector` must point to a valid, initialized `DrmConnector`.
pub unsafe extern "C" fn drm_atomic_helper_connector_hdmi_force(connector: *mut DrmConnector) {
    if connector.is_null() {
        return;
    }
    // SAFETY: `connector` is verified non-null above and points to a valid `DrmConnector`.
    let status = unsafe { (*connector).status };
    // SAFETY: `connector` is non-null and valid.
    unsafe { drm_atomic_helper_connector_hdmi_update(connector, status) };
}

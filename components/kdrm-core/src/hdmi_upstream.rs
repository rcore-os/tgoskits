// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/video/hdmi.c`.
//!
//! Handles initialization, validation, packing, unpacking, and logging of
//! HDMI Infoframes: AVI, SPD, Audio, Vendor (HDMI VSIF), DRM.

#![allow(non_snake_case)]

use core::{
    cmp::min,
    ffi::{c_char, c_void},
    slice,
};

use drm_kapi::{
    config::{EINVAL, ENOSPC},
    print::{DrmLogLevel, drm_log_at},
};

use crate::{drm_dp_types::DpSdp, hdmi_types::*};

// Constants from Linux include/linux/hdmi.h & include/drm/display/drm_dp.h
pub const HDMI_IEEE_OUI: u32 = 0x000c03;
pub const HDMI_FORUM_IEEE_OUI: u32 = 0xc45dd8;
pub const HDMI_INFOFRAME_HEADER_SIZE: usize = 4;
pub const HDMI_AVI_INFOFRAME_SIZE: usize = 13;
pub const HDMI_SPD_INFOFRAME_SIZE: usize = 25;
pub const HDMI_AUDIO_INFOFRAME_SIZE: usize = 10;
pub const HDMI_DRM_INFOFRAME_SIZE: usize = 26;
pub const HDMI_VENDOR_INFOFRAME_SIZE: usize = 4;
pub const HDMI_MAX_INFOFRAME_SIZE: usize = 27;

pub const DP_SDP_AUDIO_INFOFRAME_HB2: u8 = 0x1b;

macro_rules! hdmi_log {
    ($level:expr, $($arg:tt)*) => {
        drm_log_at($level, "hdmi", file!(), line!(), &alloc::format!($($arg)*))
    };
}

// upstream: drivers/video/hdmi.c hdmi_infoframe_checksum()
fn hdmi_infoframe_checksum(ptr: &[u8]) -> u8 {
    let mut csum: u8 = 0;
    for &b in ptr {
        csum = csum.wrapping_add(b);
    }
    (256u16.wrapping_sub(csum as u16)) as u8
}

// upstream: drivers/video/hdmi.c hdmi_infoframe_set_checksum()
fn hdmi_infoframe_set_checksum(buffer: &mut [u8]) {
    buffer[3] = hdmi_infoframe_checksum(buffer);
}

// ============================================================================
// AVI Infoframe
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_avi_infoframe_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an allocated, writable `HdmiAviInfoframe`.
pub unsafe extern "C" fn hdmi_avi_infoframe_init(frame: *mut HdmiAviInfoframe) {
    if frame.is_null() {
        return;
    }
    // SAFETY: `frame` was checked non-null above; writing default AVI infoframe fields.
    unsafe {
        core::ptr::write_bytes(frame, 0, 1);
        (*frame).r#type = HDMI_INFOFRAME_TYPE_AVI;
        (*frame).version = 2;
        (*frame).length = HDMI_AVI_INFOFRAME_SIZE as u8;
    }
}

// upstream: drivers/video/hdmi.c hdmi_avi_infoframe_check_only()
fn hdmi_avi_infoframe_check_only(frame: &HdmiAviInfoframe) -> i32 {
    if frame.r#type != HDMI_INFOFRAME_TYPE_AVI
        || frame.version != 2
        || frame.length as usize != HDMI_AVI_INFOFRAME_SIZE
    {
        return -EINVAL;
    }

    if frame.picture_aspect > HDMI_PICTURE_ASPECT_16_9 {
        return -EINVAL;
    }

    0
}

// upstream: drivers/video/hdmi.c hdmi_avi_infoframe_check()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an initialized `HdmiAviInfoframe`.
pub unsafe extern "C" fn hdmi_avi_infoframe_check(frame: *mut HdmiAviInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` was checked non-null; dereferencing read-only AVI infoframe.
    hdmi_avi_infoframe_check_only(unsafe { &*frame })
}

// upstream: drivers/video/hdmi.c hdmi_avi_infoframe_pack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiAviInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_avi_infoframe_pack_only(
    frame: *const HdmiAviInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null and points to an initialized `HdmiAviInfoframe`.
    let frame_ref = unsafe { &*frame };
    let ret = hdmi_avi_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret as isize;
    }

    let length = HDMI_INFOFRAME_HEADER_SIZE + frame_ref.length as usize;
    if size < length {
        return -ENOSPC as isize;
    }

    // SAFETY: `buffer` was checked non-null and `size >= length` bytes for AVI infoframe.
    let buf = unsafe { slice::from_raw_parts_mut(buffer as *mut u8, length) };
    buf.fill(0);

    buf[0] = frame_ref.r#type as u8;
    buf[1] = frame_ref.version;
    buf[2] = frame_ref.length;
    buf[3] = 0; /* checksum */

    let ptr = &mut buf[HDMI_INFOFRAME_HEADER_SIZE..];

    ptr[0] = (((frame_ref.colorspace & 0x3) << 5) | (frame_ref.scan_mode & 0x3)) as u8;
    if (frame_ref.active_aspect & 0xf) != 0 {
        ptr[0] |= 1 << 4;
    }
    if frame_ref.top_bar != 0 || frame_ref.bottom_bar != 0 {
        ptr[0] |= 1 << 3;
    }
    if frame_ref.left_bar != 0 || frame_ref.right_bar != 0 {
        ptr[0] |= 1 << 2;
    }

    ptr[1] = (((frame_ref.colorimetry & 0x3) << 6)
        | ((frame_ref.picture_aspect & 0x3) << 4)
        | (frame_ref.active_aspect & 0xf)) as u8;

    ptr[2] = (((frame_ref.extended_colorimetry & 0x7) << 4)
        | ((frame_ref.quantization_range & 0x3) << 2)
        | (frame_ref.nups & 0x3)) as u8;
    if frame_ref.itc {
        ptr[2] |= 1 << 7;
    }

    ptr[3] = frame_ref.video_code & 0x7f;

    ptr[4] = (((frame_ref.ycc_quantization_range & 0x3) << 6)
        | ((frame_ref.content_type & 0x3) << 4)
        | ((frame_ref.pixel_repeat as u32) & 0xf)) as u8;

    ptr[5] = (frame_ref.top_bar & 0xff) as u8;
    ptr[6] = ((frame_ref.top_bar >> 8) & 0xff) as u8;
    ptr[7] = (frame_ref.bottom_bar & 0xff) as u8;
    ptr[8] = ((frame_ref.bottom_bar >> 8) & 0xff) as u8;
    ptr[9] = (frame_ref.left_bar & 0xff) as u8;
    ptr[10] = ((frame_ref.left_bar >> 8) & 0xff) as u8;
    ptr[11] = (frame_ref.right_bar & 0xff) as u8;
    ptr[12] = ((frame_ref.right_bar >> 8) & 0xff) as u8;

    hdmi_infoframe_set_checksum(buf);

    length as isize
}

// upstream: drivers/video/hdmi.c hdmi_avi_infoframe_pack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiAviInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_avi_infoframe_pack(
    frame: *mut HdmiAviInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `frame` is validated by `hdmi_avi_infoframe_check`.
    let ret = unsafe { hdmi_avi_infoframe_check(frame) };
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Forwarded to `hdmi_avi_infoframe_pack_only` with checked `frame`.
    unsafe { hdmi_avi_infoframe_pack_only(frame, buffer, size) }
}

// ============================================================================
// SPD Infoframe
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_spd_infoframe_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to writable memory for `HdmiSpdInfoframe`.
/// - `vendor` and `product` must be null or valid null-terminated C strings.
pub unsafe extern "C" fn hdmi_spd_infoframe_init(
    frame: *mut HdmiSpdInfoframe,
    vendor: *const c_char,
    product: *const c_char,
) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` is checked non-null and writable; `vendor` and `product` are null or null-terminated C strings.
    unsafe {
        core::ptr::write_bytes(frame, 0, 1);
        (*frame).r#type = HDMI_INFOFRAME_TYPE_SPD;
        (*frame).version = 1;
        (*frame).length = HDMI_SPD_INFOFRAME_SIZE as u8;

        if !vendor.is_null() {
            let v_len = libc_strlen(vendor);
            let copy_len = min(v_len, (*frame).vendor.len());
            core::ptr::copy_nonoverlapping(vendor, (*frame).vendor.as_mut_ptr(), copy_len);
        }
        if !product.is_null() {
            let p_len = libc_strlen(product);
            let copy_len = min(p_len, (*frame).product.len());
            core::ptr::copy_nonoverlapping(product, (*frame).product.as_mut_ptr(), copy_len);
        }
    }
    0
}

/// # Safety
///
/// `s` must point to a valid null-terminated C string.
unsafe fn libc_strlen(s: *const c_char) -> usize {
    let mut len = 0;
    // SAFETY: Caller guarantees `s` points to a null-terminated C string.
    while unsafe { *s.add(len) } != 0 {
        len += 1;
    }
    len
}

// upstream: drivers/video/hdmi.c hdmi_spd_infoframe_check_only()
fn hdmi_spd_infoframe_check_only(frame: &HdmiSpdInfoframe) -> i32 {
    if frame.r#type != HDMI_INFOFRAME_TYPE_SPD
        || frame.version != 1
        || frame.length as usize != HDMI_SPD_INFOFRAME_SIZE
    {
        return -EINVAL;
    }
    0
}

// upstream: drivers/video/hdmi.c hdmi_spd_infoframe_check()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an initialized `HdmiSpdInfoframe`.
pub unsafe extern "C" fn hdmi_spd_infoframe_check(frame: *mut HdmiSpdInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` is checked non-null; dereferencing read-only SPD infoframe.
    hdmi_spd_infoframe_check_only(unsafe { &*frame })
}

// upstream: drivers/video/hdmi.c hdmi_spd_infoframe_pack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiSpdInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_spd_infoframe_pack_only(
    frame: *const HdmiSpdInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null; reading SPD infoframe fields for packing.
    let frame_ref = unsafe { &*frame };
    let ret = hdmi_spd_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret as isize;
    }

    let length = HDMI_INFOFRAME_HEADER_SIZE + frame_ref.length as usize;
    if size < length {
        return -ENOSPC as isize;
    }

    // SAFETY: `buffer` is non-null and `size >= length` bytes for SPD infoframe.
    let buf = unsafe { slice::from_raw_parts_mut(buffer as *mut u8, length) };
    buf.fill(0);

    buf[0] = frame_ref.r#type as u8;
    buf[1] = frame_ref.version;
    buf[2] = frame_ref.length;
    buf[3] = 0; /* checksum */

    let ptr = &mut buf[HDMI_INFOFRAME_HEADER_SIZE..];

    let v_src = frame_ref.vendor.as_ptr().cast::<u8>();
    let p_src = frame_ref.product.as_ptr().cast::<u8>();
    // SAFETY: `vendor` is at least 8 bytes, `product` is at least 16 bytes, and `ptr` has capacity for 25 bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(v_src, ptr.as_mut_ptr(), 8);
        core::ptr::copy_nonoverlapping(p_src, ptr.as_mut_ptr().add(8), 16);
    }

    ptr[24] = frame_ref.sdi as u8;

    hdmi_infoframe_set_checksum(buf);

    length as isize
}

// upstream: drivers/video/hdmi.c hdmi_spd_infoframe_pack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiSpdInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_spd_infoframe_pack(
    frame: *mut HdmiSpdInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `frame` is validated by `hdmi_spd_infoframe_check`.
    let ret = unsafe { hdmi_spd_infoframe_check(frame) };
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Forwarded to `hdmi_spd_infoframe_pack_only`.
    unsafe { hdmi_spd_infoframe_pack_only(frame, buffer, size) }
}

// ============================================================================
// Audio Infoframe
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an allocated, writable `HdmiAudioInfoframe`.
pub unsafe extern "C" fn hdmi_audio_infoframe_init(frame: *mut HdmiAudioInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` was checked non-null above; writing default audio infoframe fields.
    unsafe {
        core::ptr::write_bytes(frame, 0, 1);
        (*frame).r#type = HDMI_INFOFRAME_TYPE_AUDIO;
        (*frame).version = 1;
        (*frame).length = HDMI_AUDIO_INFOFRAME_SIZE as u8;
    }
    0
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_check_only()
fn hdmi_audio_infoframe_check_only(frame: &HdmiAudioInfoframe) -> i32 {
    if frame.r#type != HDMI_INFOFRAME_TYPE_AUDIO
        || frame.version != 1
        || frame.length as usize != HDMI_AUDIO_INFOFRAME_SIZE
    {
        return -EINVAL;
    }
    0
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_check()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an initialized `HdmiAudioInfoframe`.
pub unsafe extern "C" fn hdmi_audio_infoframe_check(frame: *const HdmiAudioInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` is checked non-null; validating audio infoframe fields.
    hdmi_audio_infoframe_check_only(unsafe { &*frame })
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_pack_payload()
fn hdmi_audio_infoframe_pack_payload(frame: &HdmiAudioInfoframe, buffer: &mut [u8]) {
    let channels = if frame.channels >= 2 {
        frame.channels - 1
    } else {
        0
    };

    buffer[0] = (((frame.coding_type & 0xf) << 4) | (channels as u32 & 0x7)) as u8;
    buffer[1] = (((frame.sample_frequency & 0x7) << 2) | (frame.sample_size & 0x3)) as u8;
    buffer[2] = (frame.coding_type_ext & 0x1f) as u8;
    buffer[3] = frame.channel_allocation;
    buffer[4] = ((frame.level_shift_value as u32 & 0xf) << 3) as u8;

    if frame.downmix_inhibit {
        buffer[4] |= 1 << 7;
    }
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_pack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiAudioInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_audio_infoframe_pack_only(
    frame: *const HdmiAudioInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null; reading audio infoframe fields for packing.
    let frame_ref = unsafe { &*frame };
    let ret = hdmi_audio_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret as isize;
    }

    let length = HDMI_INFOFRAME_HEADER_SIZE + frame_ref.length as usize;
    if size < length {
        return -ENOSPC as isize;
    }

    // SAFETY: `buffer` is non-null and `size >= length` bytes for audio infoframe.
    let buf = unsafe { slice::from_raw_parts_mut(buffer as *mut u8, length) };
    buf.fill(0);

    buf[0] = frame_ref.r#type as u8;
    buf[1] = frame_ref.version;
    buf[2] = frame_ref.length;
    buf[3] = 0; /* checksum */

    hdmi_audio_infoframe_pack_payload(frame_ref, &mut buf[HDMI_INFOFRAME_HEADER_SIZE..]);
    hdmi_infoframe_set_checksum(buf);

    length as isize
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_pack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiAudioInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_audio_infoframe_pack(
    frame: *mut HdmiAudioInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `frame` is validated by `hdmi_audio_infoframe_check`.
    let ret = unsafe { hdmi_audio_infoframe_check(frame) };
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Forwarded to `hdmi_audio_infoframe_pack_only`.
    unsafe { hdmi_audio_infoframe_pack_only(frame, buffer, size) }
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_pack_for_dp()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiAudioInfoframe`.
/// - If `sdp` is non-null, it must point to an allocated, writable `DpSdp`.
pub unsafe extern "C" fn hdmi_audio_infoframe_pack_for_dp(
    frame: *const HdmiAudioInfoframe,
    sdp: *mut DpSdp,
    dp_version: u8,
) -> isize {
    if frame.is_null() || sdp.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null and is dereferenceable.
    let frame_ref = unsafe { &*frame };
    let ret = hdmi_audio_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret as isize;
    }

    // SAFETY: `sdp` was checked non-null and is writable.
    let sdp_ref = unsafe { &mut *sdp };
    sdp_ref.db.fill(0);

    sdp_ref.sdp_header.HB0 = 0;
    sdp_ref.sdp_header.HB1 = frame_ref.r#type as u8;
    sdp_ref.sdp_header.HB2 = DP_SDP_AUDIO_INFOFRAME_HB2;
    sdp_ref.sdp_header.HB3 = (dp_version & 0x3f) << 2;

    hdmi_audio_infoframe_pack_payload(frame_ref, &mut sdp_ref.db);

    (frame_ref.length as isize) + 4
}

// ============================================================================
// Vendor Infoframe (HDMI VSIF)
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_vendor_infoframe_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an allocated, writable `HdmiVendorInfoframe`.
pub unsafe extern "C" fn hdmi_vendor_infoframe_init(frame: *mut HdmiVendorInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` was checked non-null and is writable.
    unsafe {
        core::ptr::write_bytes(frame, 0, 1);
        (*frame).r#type = HDMI_INFOFRAME_TYPE_VENDOR;
        (*frame).version = 1;
        (*frame).oui = HDMI_IEEE_OUI;
        (*frame).s3d_struct = HDMI_3D_STRUCTURE_INVALID;
        (*frame).length = HDMI_VENDOR_INFOFRAME_SIZE as u8;
    }
    0
}

// upstream: drivers/video/hdmi.c hdmi_vendor_infoframe_length()
fn hdmi_vendor_infoframe_length(frame: &HdmiVendorInfoframe) -> usize {
    if frame.s3d_struct >= HDMI_3D_STRUCTURE_SIDE_BY_SIDE_HALF {
        6
    } else if frame.vic != 0 || frame.s3d_struct != HDMI_3D_STRUCTURE_INVALID {
        5
    } else {
        4
    }
}

// upstream: drivers/video/hdmi.c hdmi_vendor_infoframe_check_only()
fn hdmi_vendor_infoframe_check_only(frame: &HdmiVendorInfoframe) -> i32 {
    if frame.r#type != HDMI_INFOFRAME_TYPE_VENDOR
        || frame.version != 1
        || frame.oui != HDMI_IEEE_OUI
    {
        return -EINVAL;
    }

    if frame.vic != 0 && frame.s3d_struct != HDMI_3D_STRUCTURE_INVALID {
        return -EINVAL;
    }

    if frame.length as usize != hdmi_vendor_infoframe_length(frame) {
        return -EINVAL;
    }

    0
}

// upstream: drivers/video/hdmi.c hdmi_vendor_infoframe_check()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an initialized `HdmiVendorInfoframe`.
pub unsafe extern "C" fn hdmi_vendor_infoframe_check(frame: *mut HdmiVendorInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` was checked non-null and is mutable.
    let frame_ref = unsafe { &mut *frame };
    frame_ref.length = hdmi_vendor_infoframe_length(frame_ref) as u8;
    hdmi_vendor_infoframe_check_only(frame_ref)
}

// upstream: drivers/video/hdmi.c hdmi_vendor_infoframe_pack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiVendorInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_vendor_infoframe_pack_only(
    frame: *const HdmiVendorInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null and is dereferenceable.
    let frame_ref = unsafe { &*frame };
    let ret = hdmi_vendor_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret as isize;
    }

    let length = HDMI_INFOFRAME_HEADER_SIZE + frame_ref.length as usize;
    if size < length {
        return -ENOSPC as isize;
    }

    // SAFETY: `buffer` was checked non-null and `size >= length`.
    let buf = unsafe { slice::from_raw_parts_mut(buffer as *mut u8, length) };
    buf.fill(0);

    buf[0] = frame_ref.r#type as u8;
    buf[1] = frame_ref.version;
    buf[2] = frame_ref.length;
    buf[3] = 0; /* checksum */

    // HDMI OUI 0x000c03 in 24-bit little endian order
    buf[4] = 0x03;
    buf[5] = 0x0c;
    buf[6] = 0x00;

    if frame_ref.s3d_struct != HDMI_3D_STRUCTURE_INVALID {
        buf[7] = 0x2 << 5; /* video format 3D */
        buf[8] = ((frame_ref.s3d_struct as u8) & 0xf) << 4;
        if frame_ref.s3d_struct >= HDMI_3D_STRUCTURE_SIDE_BY_SIDE_HALF {
            buf[9] = ((frame_ref.s3d_ext_data as u8) & 0xf) << 4;
        }
    } else if frame_ref.vic != 0 {
        buf[7] = 0x1 << 5; /* video format HDMI_VIC */
        buf[8] = frame_ref.vic;
    } else {
        buf[7] = 0x0 << 5; /* video format standard */
    }

    hdmi_infoframe_set_checksum(buf);

    length as isize
}

// upstream: drivers/video/hdmi.c hdmi_vendor_infoframe_pack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiVendorInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_vendor_infoframe_pack(
    frame: *mut HdmiVendorInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `frame` is validated by `hdmi_vendor_infoframe_check`.
    let ret = unsafe { hdmi_vendor_infoframe_check(frame) };
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Forwarded to `hdmi_vendor_infoframe_pack_only`.
    unsafe { hdmi_vendor_infoframe_pack_only(frame, buffer, size) }
}

fn hdmi_vendor_any_infoframe_check_only(frame: &HdmiVendorAnyInfoframe) -> i32 {
    // SAFETY: Accessing union field `any` of initialized `HdmiVendorAnyInfoframe`.
    let any = unsafe { &frame.any };
    if any.r#type != HDMI_INFOFRAME_TYPE_VENDOR || any.version != 1 {
        return -EINVAL;
    }
    0
}

/// # Safety
///
/// `frame` must point to an initialized `HdmiVendorAnyInfoframe` union.
unsafe fn hdmi_vendor_any_infoframe_check(frame: *mut HdmiVendorAnyInfoframe) -> i32 {
    // SAFETY: Caller guarantees `frame` points to an initialized `HdmiVendorAnyInfoframe` union.
    let frame_ref = unsafe { &mut *frame };
    let ret = hdmi_vendor_any_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret;
    }
    // SAFETY: Accessing union field `any`.
    let any = unsafe { &frame_ref.any };
    if any.oui != HDMI_IEEE_OUI {
        return -EINVAL;
    }
    // SAFETY: Getting pointer to union field `hdmi`.
    let hdmi_ptr = unsafe { core::ptr::addr_of_mut!((*frame).hdmi) as *mut HdmiVendorInfoframe };
    // SAFETY: Passing `hdmi_ptr` to `hdmi_vendor_infoframe_check`.
    unsafe { hdmi_vendor_infoframe_check(hdmi_ptr) }
}

/// # Safety
///
/// - `frame` must point to an initialized `HdmiVendorAnyInfoframe` union.
/// - `buffer` must point to writable memory of at least `size` bytes.
unsafe fn hdmi_vendor_any_infoframe_pack_only(
    frame: *const HdmiVendorAnyInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: Caller guarantees `frame` is initialized and valid.
    let frame_ref = unsafe { &*frame };
    let ret = hdmi_vendor_any_infoframe_check_only(frame_ref);
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Accessing union field `any`.
    let any = unsafe { &frame_ref.any };
    if any.oui != HDMI_IEEE_OUI {
        return -EINVAL as isize;
    }
    // SAFETY: Getting pointer to union field `hdmi`.
    let hdmi_ptr = unsafe { core::ptr::addr_of!((*frame).hdmi) as *const HdmiVendorInfoframe };
    // SAFETY: Passing `hdmi_ptr` to `hdmi_vendor_infoframe_pack_only`.
    unsafe { hdmi_vendor_infoframe_pack_only(hdmi_ptr, buffer, size) }
}

/// # Safety
///
/// - `frame` must point to an initialized `HdmiVendorAnyInfoframe` union.
/// - `buffer` must point to writable memory of at least `size` bytes.
unsafe fn hdmi_vendor_any_infoframe_pack(
    frame: *mut HdmiVendorAnyInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `frame` is validated by `hdmi_vendor_any_infoframe_check`.
    let ret = unsafe { hdmi_vendor_any_infoframe_check(frame) };
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Forwarded to `hdmi_vendor_any_infoframe_pack_only`.
    unsafe { hdmi_vendor_any_infoframe_pack_only(frame, buffer, size) }
}

// ============================================================================
// DRM Infoframe (Dynamic Range and Mastering)
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an allocated, writable `HdmiDrmInfoframe`.
pub unsafe extern "C" fn hdmi_drm_infoframe_init(frame: *mut HdmiDrmInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` was checked non-null and is writable.
    unsafe {
        core::ptr::write_bytes(frame, 0, 1);
        (*frame).r#type = HDMI_INFOFRAME_TYPE_DRM;
        (*frame).version = 1;
        (*frame).length = HDMI_DRM_INFOFRAME_SIZE as u8;
    }
    0
}

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_check_only()
fn hdmi_drm_infoframe_check_only(frame: &HdmiDrmInfoframe) -> i32 {
    if frame.r#type != HDMI_INFOFRAME_TYPE_DRM || frame.version != 1 {
        return -EINVAL;
    }
    if frame.length as usize != HDMI_DRM_INFOFRAME_SIZE {
        return -EINVAL;
    }
    0
}

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_check()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `frame` is non-null, it must point to an initialized `HdmiDrmInfoframe`.
pub unsafe extern "C" fn hdmi_drm_infoframe_check(frame: *mut HdmiDrmInfoframe) -> i32 {
    if frame.is_null() {
        return -EINVAL;
    }
    // SAFETY: `frame` was checked non-null and is dereferenceable.
    hdmi_drm_infoframe_check_only(unsafe { &*frame })
}

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_pack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiDrmInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_drm_infoframe_pack_only(
    frame: *const HdmiDrmInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null and is dereferenceable.
    let frame_ref = unsafe { &*frame };
    let length = HDMI_INFOFRAME_HEADER_SIZE + frame_ref.length as usize;
    if size < length {
        return -ENOSPC as isize;
    }

    // SAFETY: `buffer` was checked non-null and `size >= length`.
    let buf = unsafe { slice::from_raw_parts_mut(buffer as *mut u8, length) };
    buf.fill(0);

    buf[0] = frame_ref.r#type as u8;
    buf[1] = frame_ref.version;
    buf[2] = frame_ref.length;
    buf[3] = 0; /* checksum */

    let ptr = &mut buf[HDMI_INFOFRAME_HEADER_SIZE..];

    ptr[0] = frame_ref.eotf as u8;
    ptr[1] = frame_ref.metadata_type as u8;

    let mut idx = 2;
    for primary in &frame_ref.display_primaries {
        let x = primary.x;
        let y = primary.y;
        ptr[idx] = (x & 0xff) as u8;
        ptr[idx + 1] = ((x >> 8) & 0xff) as u8;
        ptr[idx + 2] = (y & 0xff) as u8;
        ptr[idx + 3] = ((y >> 8) & 0xff) as u8;
        idx += 4;
    }

    ptr[14] = (frame_ref.white_point.x & 0xff) as u8;
    ptr[15] = ((frame_ref.white_point.x >> 8) & 0xff) as u8;

    ptr[16] = (frame_ref.white_point.y & 0xff) as u8;
    ptr[17] = ((frame_ref.white_point.y >> 8) & 0xff) as u8;

    ptr[18] = (frame_ref.max_display_mastering_luminance & 0xff) as u8;
    ptr[19] = ((frame_ref.max_display_mastering_luminance >> 8) & 0xff) as u8;

    ptr[20] = (frame_ref.min_display_mastering_luminance & 0xff) as u8;
    ptr[21] = ((frame_ref.min_display_mastering_luminance >> 8) & 0xff) as u8;

    ptr[22] = (frame_ref.max_cll & 0xff) as u8;
    ptr[23] = ((frame_ref.max_cll >> 8) & 0xff) as u8;

    ptr[24] = (frame_ref.max_fall & 0xff) as u8;
    ptr[25] = ((frame_ref.max_fall >> 8) & 0xff) as u8;

    hdmi_infoframe_set_checksum(buf);

    length as isize
}

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_pack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiDrmInfoframe`.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_drm_infoframe_pack(
    frame: *mut HdmiDrmInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `frame` is validated by `hdmi_drm_infoframe_check`.
    let ret = unsafe { hdmi_drm_infoframe_check(frame) };
    if ret != 0 {
        return ret as isize;
    }
    // SAFETY: Forwarded to `hdmi_drm_infoframe_pack_only`.
    unsafe { hdmi_drm_infoframe_pack_only(frame, buffer, size) }
}

// ============================================================================
// Generic Infoframe pack / unpack
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_infoframe_pack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiInfoframe` union.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_infoframe_pack_only(
    frame: *const HdmiInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null and is dereferenceable.
    let frame_ref = unsafe { &*frame };
    // SAFETY: Accessing union field `any`.
    let any_type = unsafe { (*frame_ref.any).r#type };
    match any_type {
        HDMI_INFOFRAME_TYPE_AVI => {
            // SAFETY: Accessing union field `avi`.
            let p = unsafe { core::ptr::addr_of!((*frame).avi) as *const HdmiAviInfoframe };
            // SAFETY: Forwarded to `hdmi_avi_infoframe_pack_only`.
            unsafe { hdmi_avi_infoframe_pack_only(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_DRM => {
            // SAFETY: Accessing union field `drm`.
            let p = unsafe { core::ptr::addr_of!((*frame).drm) as *const HdmiDrmInfoframe };
            // SAFETY: Forwarded to `hdmi_drm_infoframe_pack_only`.
            unsafe { hdmi_drm_infoframe_pack_only(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_SPD => {
            // SAFETY: Accessing union field `spd`.
            let p = unsafe { core::ptr::addr_of!((*frame).spd) as *const HdmiSpdInfoframe };
            // SAFETY: Forwarded to `hdmi_spd_infoframe_pack_only`.
            unsafe { hdmi_spd_infoframe_pack_only(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_AUDIO => {
            // SAFETY: Accessing union field `audio`.
            let p = unsafe { core::ptr::addr_of!((*frame).audio) as *const HdmiAudioInfoframe };
            // SAFETY: Forwarded to `hdmi_audio_infoframe_pack_only`.
            unsafe { hdmi_audio_infoframe_pack_only(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_VENDOR => {
            let p =
                // SAFETY: `frame` is non-null (checked at the top of this function); `vendor` is a union view of it, so `addr_of!` forms the field pointer without reading the union.
                unsafe { core::ptr::addr_of!((*frame).vendor) as *const HdmiVendorAnyInfoframe };
            // SAFETY: Forwarded to `hdmi_vendor_any_infoframe_pack_only`.
            unsafe { hdmi_vendor_any_infoframe_pack_only(p, buffer, size) }
        }
        _ => -EINVAL as isize,
    }
}

// upstream: drivers/video/hdmi.c hdmi_infoframe_pack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an initialized `HdmiInfoframe` union.
/// - If `buffer` is non-null, it must point to writable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_infoframe_pack(
    frame: *mut HdmiInfoframe,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `frame` was checked non-null and is dereferenceable.
    let frame_ref = unsafe { &*frame };
    // SAFETY: Accessing union field `any`.
    let any_type = unsafe { (*frame_ref.any).r#type };
    match any_type {
        HDMI_INFOFRAME_TYPE_AVI => {
            // SAFETY: Accessing union field `avi`.
            let p = unsafe { core::ptr::addr_of_mut!((*frame).avi) as *mut HdmiAviInfoframe };
            // SAFETY: Forwarded to `hdmi_avi_infoframe_pack`.
            unsafe { hdmi_avi_infoframe_pack(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_DRM => {
            // SAFETY: Accessing union field `drm`.
            let p = unsafe { core::ptr::addr_of_mut!((*frame).drm) as *mut HdmiDrmInfoframe };
            // SAFETY: Forwarded to `hdmi_drm_infoframe_pack`.
            unsafe { hdmi_drm_infoframe_pack(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_SPD => {
            // SAFETY: Accessing union field `spd`.
            let p = unsafe { core::ptr::addr_of_mut!((*frame).spd) as *mut HdmiSpdInfoframe };
            // SAFETY: Forwarded to `hdmi_spd_infoframe_pack`.
            unsafe { hdmi_spd_infoframe_pack(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_AUDIO => {
            // SAFETY: Accessing union field `audio`.
            let p = unsafe { core::ptr::addr_of_mut!((*frame).audio) as *mut HdmiAudioInfoframe };
            // SAFETY: Forwarded to `hdmi_audio_infoframe_pack`.
            unsafe { hdmi_audio_infoframe_pack(p, buffer, size) }
        }
        HDMI_INFOFRAME_TYPE_VENDOR => {
            let p =
                // SAFETY: `frame` is non-null (checked at the top of this function); `vendor` is a union view of it, so `addr_of_mut!` forms the field pointer without reading the union.
                unsafe { core::ptr::addr_of_mut!((*frame).vendor) as *mut HdmiVendorAnyInfoframe };
            // SAFETY: Forwarded to `hdmi_vendor_any_infoframe_pack`.
            unsafe { hdmi_vendor_any_infoframe_pack(p, buffer, size) }
        }
        _ => -EINVAL as isize,
    }
}

// ============================================================================
// Unpack helpers
// ============================================================================

// upstream: drivers/video/hdmi.c hdmi_avi_infoframe_unpack()
fn hdmi_avi_infoframe_unpack(frame: &mut HdmiAviInfoframe, buffer: &[u8]) -> i32 {
    let size = buffer.len();
    if size < HDMI_INFOFRAME_HEADER_SIZE + HDMI_AVI_INFOFRAME_SIZE {
        return -EINVAL;
    }

    if buffer[0] != HDMI_INFOFRAME_TYPE_AVI as u8
        || buffer[1] != 2
        || buffer[2] as usize != HDMI_AVI_INFOFRAME_SIZE
    {
        return -EINVAL;
    }

    if hdmi_infoframe_checksum(&buffer[..HDMI_INFOFRAME_HEADER_SIZE + HDMI_AVI_INFOFRAME_SIZE]) != 0
    {
        return -EINVAL;
    }

    // SAFETY: `frame` is a valid, writable reference passed by caller.
    unsafe { hdmi_avi_infoframe_init(frame) };

    let ptr = &buffer[HDMI_INFOFRAME_HEADER_SIZE..];

    frame.colorspace = ((ptr[0] >> 5) & 0x3) as u32;
    if (ptr[0] & 0x10) != 0 {
        frame.active_aspect = (ptr[1] & 0xf) as u32;
    }
    if (ptr[0] & 0x8) != 0 {
        frame.top_bar = (ptr[5] as u16) | ((ptr[6] as u16) << 8);
        frame.bottom_bar = (ptr[7] as u16) | ((ptr[8] as u16) << 8);
    }
    if (ptr[0] & 0x4) != 0 {
        frame.left_bar = (ptr[9] as u16) | ((ptr[10] as u16) << 8);
        frame.right_bar = (ptr[11] as u16) | ((ptr[12] as u16) << 8);
    }
    frame.scan_mode = (ptr[0] & 0x3) as u32;

    frame.colorimetry = ((ptr[1] >> 6) & 0x3) as u32;
    frame.picture_aspect = ((ptr[1] >> 4) & 0x3) as u32;
    frame.active_aspect = (ptr[1] & 0xf) as u32;

    frame.itc = (ptr[2] & 0x80) != 0;
    frame.extended_colorimetry = ((ptr[2] >> 4) & 0x7) as u32;
    frame.quantization_range = ((ptr[2] >> 2) & 0x3) as u32;
    frame.nups = (ptr[2] & 0x3) as u32;

    frame.video_code = ptr[3] & 0x7f;
    frame.ycc_quantization_range = ((ptr[4] >> 6) & 0x3) as u32;
    frame.content_type = ((ptr[4] >> 4) & 0x3) as u32;
    frame.pixel_repeat = ptr[4] & 0xf;

    0
}

// upstream: drivers/video/hdmi.c hdmi_spd_infoframe_unpack()
fn hdmi_spd_infoframe_unpack(frame: &mut HdmiSpdInfoframe, buffer: &[u8]) -> i32 {
    let size = buffer.len();
    if size < HDMI_INFOFRAME_HEADER_SIZE + HDMI_SPD_INFOFRAME_SIZE {
        return -EINVAL;
    }

    if buffer[0] != HDMI_INFOFRAME_TYPE_SPD as u8
        || buffer[1] != 1
        || buffer[2] as usize != HDMI_SPD_INFOFRAME_SIZE
    {
        return -EINVAL;
    }

    if hdmi_infoframe_checksum(&buffer[..HDMI_INFOFRAME_HEADER_SIZE + HDMI_SPD_INFOFRAME_SIZE]) != 0
    {
        return -EINVAL;
    }

    let ptr = &buffer[HDMI_INFOFRAME_HEADER_SIZE..];
    // SAFETY: buffer length was verified above (size >= HEADER + SPD_SIZE), so ptr has at least
    // 24 bytes valid for reading vendor (8 bytes) and product (16 bytes) strings.
    let ret = unsafe {
        hdmi_spd_infoframe_init(
            frame,
            ptr.as_ptr() as *const c_char,
            ptr.as_ptr().add(8) as *const c_char,
        )
    };
    if ret != 0 {
        return ret;
    }

    frame.sdi = ptr[24] as u32;
    0
}

// upstream: drivers/video/hdmi.c hdmi_audio_infoframe_unpack()
fn hdmi_audio_infoframe_unpack(frame: &mut HdmiAudioInfoframe, buffer: &[u8]) -> i32 {
    let size = buffer.len();
    if size < HDMI_INFOFRAME_HEADER_SIZE + HDMI_AUDIO_INFOFRAME_SIZE {
        return -EINVAL;
    }

    if buffer[0] != HDMI_INFOFRAME_TYPE_AUDIO as u8
        || buffer[1] != 1
        || buffer[2] as usize != HDMI_AUDIO_INFOFRAME_SIZE
    {
        return -EINVAL;
    }

    if hdmi_infoframe_checksum(&buffer[..HDMI_INFOFRAME_HEADER_SIZE + HDMI_AUDIO_INFOFRAME_SIZE])
        != 0
    {
        return -EINVAL;
    }

    // SAFETY: frame is a valid mutable reference to HdmiAudioInfoframe.
    let ret = unsafe { hdmi_audio_infoframe_init(frame) };
    if ret != 0 {
        return ret;
    }

    let ptr = &buffer[HDMI_INFOFRAME_HEADER_SIZE..];

    frame.channels = ptr[0] & 0x7;
    frame.coding_type = ((ptr[0] >> 4) & 0xf) as u32;
    frame.sample_size = (ptr[1] & 0x3) as u32;
    frame.sample_frequency = ((ptr[1] >> 2) & 0x7) as u32;
    frame.coding_type_ext = (ptr[2] & 0x1f) as u32;
    frame.channel_allocation = ptr[3];
    frame.level_shift_value = (ptr[4] >> 3) & 0xf;
    frame.downmix_inhibit = (ptr[4] & 0x80) != 0;

    0
}

// upstream: drivers/video/hdmi.c hdmi_vendor_any_infoframe_unpack()
fn hdmi_vendor_any_infoframe_unpack(frame: &mut HdmiVendorAnyInfoframe, buffer: &[u8]) -> i32 {
    let size = buffer.len();
    if size < HDMI_INFOFRAME_HEADER_SIZE {
        return -EINVAL;
    }

    if buffer[0] != HDMI_INFOFRAME_TYPE_VENDOR as u8
        || buffer[1] != 1
        || (buffer[2] != 4 && buffer[2] != 5 && buffer[2] != 6)
    {
        return -EINVAL;
    }

    let length = buffer[2] as usize;
    if size < HDMI_INFOFRAME_HEADER_SIZE + length {
        return -EINVAL;
    }

    if hdmi_infoframe_checksum(&buffer[..HDMI_INFOFRAME_HEADER_SIZE + length]) != 0 {
        return -EINVAL;
    }

    let ptr = &buffer[HDMI_INFOFRAME_HEADER_SIZE..];
    if ptr[0] != 0x03 || ptr[1] != 0x0c || ptr[2] != 0x00 {
        return -EINVAL;
    }

    let hdmi_video_format = ptr[3] >> 5;
    if hdmi_video_format > 0x2 {
        return -EINVAL;
    }

    // SAFETY: frame.hdmi is a valid pointer to HdmiVendorInfoframe inside the union.
    let hvf = unsafe { &mut *frame.hdmi };
    // SAFETY: hvf is a valid exclusive reference to HdmiVendorInfoframe.
    let ret = unsafe { hdmi_vendor_infoframe_init(hvf as *mut HdmiVendorInfoframe) };
    if ret != 0 {
        return ret;
    }

    hvf.length = length as u8;

    if hdmi_video_format == 0x2 {
        if length != 5 && length != 6 {
            return -EINVAL;
        }
        hvf.s3d_struct = (ptr[4] >> 4) as i32;
        if hvf.s3d_struct >= HDMI_3D_STRUCTURE_SIDE_BY_SIDE_HALF {
            if length != 6 {
                return -EINVAL;
            }
            hvf.s3d_ext_data = (ptr[5] >> 4) as u32;
        }
    } else if hdmi_video_format == 0x1 {
        if length != 5 {
            return -EINVAL;
        }
        hvf.vic = ptr[4];
    } else if length != 4 {
        return -EINVAL;
    }

    0
}

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_unpack_only()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an allocated, writable `HdmiDrmInfoframe`.
/// - If `buffer` is non-null, it must point to readable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_drm_infoframe_unpack_only(
    frame: *mut HdmiDrmInfoframe,
    buffer: *const c_void,
    size: usize,
) -> i32 {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL;
    }
    if size < HDMI_DRM_INFOFRAME_SIZE {
        return -EINVAL;
    }

    // SAFETY: `frame` is non-null and writable.
    let ret = unsafe { hdmi_drm_infoframe_init(frame) };
    if ret != 0 {
        return ret;
    }

    // SAFETY: `buffer` is non-null and readable for at least `size` bytes.
    let ptr = unsafe { slice::from_raw_parts(buffer as *const u8, size) };
    // SAFETY: `frame` is non-null and writable.
    let frame_ref = unsafe { &mut *frame };

    frame_ref.eotf = (ptr[0] & 0x7) as u32;
    frame_ref.metadata_type = (ptr[1] & 0x7) as u32;

    let mut temp = 2;
    for primary in &mut frame_ref.display_primaries {
        let x_lsb = ptr[temp];
        let x_msb = ptr[temp + 1];
        temp += 2;
        primary.x = ((x_msb as u16) << 8) | (x_lsb as u16);

        let y_lsb = ptr[temp];
        let y_msb = ptr[temp + 1];
        temp += 2;
        primary.y = ((y_msb as u16) << 8) | (y_lsb as u16);
    }

    frame_ref.white_point.x = ((ptr[15] as u16) << 8) | (ptr[14] as u16);
    frame_ref.white_point.y = ((ptr[17] as u16) << 8) | (ptr[16] as u16);

    frame_ref.max_display_mastering_luminance = ((ptr[19] as u16) << 8) | (ptr[18] as u16);
    frame_ref.min_display_mastering_luminance = ((ptr[21] as u16) << 8) | (ptr[20] as u16);
    frame_ref.max_cll = ((ptr[23] as u16) << 8) | (ptr[22] as u16);
    frame_ref.max_fall = ((ptr[25] as u16) << 8) | (ptr[24] as u16);

    0
}

// upstream: drivers/video/hdmi.c hdmi_drm_infoframe_unpack()
fn hdmi_drm_infoframe_unpack(frame: &mut HdmiDrmInfoframe, buffer: &[u8]) -> i32 {
    let size = buffer.len();
    if size < HDMI_INFOFRAME_HEADER_SIZE + HDMI_DRM_INFOFRAME_SIZE {
        return -EINVAL;
    }

    if buffer[0] != HDMI_INFOFRAME_TYPE_DRM as u8
        || buffer[1] != 1
        || buffer[2] as usize != HDMI_DRM_INFOFRAME_SIZE
    {
        return -EINVAL;
    }

    if hdmi_infoframe_checksum(&buffer[..HDMI_INFOFRAME_HEADER_SIZE + HDMI_DRM_INFOFRAME_SIZE]) != 0
    {
        return -EINVAL;
    }

    // SAFETY: `buffer` has at least `HDMI_INFOFRAME_HEADER_SIZE + HDMI_DRM_INFOFRAME_SIZE` bytes; `frame` is a valid mutable reference.
    unsafe {
        hdmi_drm_infoframe_unpack_only(
            frame,
            buffer[HDMI_INFOFRAME_HEADER_SIZE..].as_ptr() as *const c_void,
            size - HDMI_INFOFRAME_HEADER_SIZE,
        )
    }
}

// upstream: drivers/video/hdmi.c hdmi_infoframe_unpack()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `frame` is non-null, it must point to an allocated, writable `HdmiInfoframe` union.
/// - If `buffer` is non-null, it must point to readable memory of at least `size` bytes.
pub unsafe extern "C" fn hdmi_infoframe_unpack(
    frame: *mut HdmiInfoframe,
    buffer: *const c_void,
    size: usize,
) -> i32 {
    if frame.is_null() || buffer.is_null() {
        return -EINVAL;
    }
    if size < HDMI_INFOFRAME_HEADER_SIZE {
        return -EINVAL;
    }

    // SAFETY: `buffer` is non-null and `size >= HDMI_INFOFRAME_HEADER_SIZE`.
    let buf = unsafe { slice::from_raw_parts(buffer as *const u8, size) };
    match buf[0] as u32 {
        HDMI_INFOFRAME_TYPE_AVI => {
            // SAFETY: Accessing union field `avi`.
            let avi = unsafe { &mut (*frame).avi };
            hdmi_avi_infoframe_unpack(avi, buf)
        }
        HDMI_INFOFRAME_TYPE_DRM => {
            // SAFETY: Accessing union field `drm`.
            let drm = unsafe { &mut (*frame).drm };
            hdmi_drm_infoframe_unpack(drm, buf)
        }
        HDMI_INFOFRAME_TYPE_SPD => {
            // SAFETY: Accessing union field `spd`.
            let spd = unsafe { &mut (*frame).spd };
            hdmi_spd_infoframe_unpack(spd, buf)
        }
        HDMI_INFOFRAME_TYPE_AUDIO => {
            // SAFETY: Accessing union field `audio`.
            let audio = unsafe { &mut (*frame).audio };
            hdmi_audio_infoframe_unpack(audio, buf)
        }
        HDMI_INFOFRAME_TYPE_VENDOR => {
            // SAFETY: Accessing union field `vendor`.
            let vendor = unsafe { &mut (*frame).vendor };
            hdmi_vendor_any_infoframe_unpack(vendor, buf)
        }
        _ => -EINVAL,
    }
}

// ============================================================================
// Logging functions
// ============================================================================

fn hdmi_infoframe_type_get_name(r#type: HdmiInfoframeType) -> &'static str {
    match r#type {
        HDMI_INFOFRAME_TYPE_VENDOR => "Vendor",
        HDMI_INFOFRAME_TYPE_AVI => "Auxiliary Video Information (AVI)",
        HDMI_INFOFRAME_TYPE_SPD => "Source Product Description (SPD)",
        HDMI_INFOFRAME_TYPE_AUDIO => "Audio",
        HDMI_INFOFRAME_TYPE_DRM => "Dynamic Range and Mastering",
        _ => {
            if !(0x80..=0x9f).contains(&r#type) {
                "Invalid"
            } else {
                "Reserved"
            }
        }
    }
}

fn hdmi_infoframe_log_header(level: DrmLogLevel, frame: &HdmiAnyInfoframe) {
    hdmi_log!(
        level,
        "HDMI infoframe: {}, version {}, length {}\n",
        hdmi_infoframe_type_get_name(frame.r#type),
        frame.version,
        frame.length
    );
}

fn hdmi_colorspace_get_name(colorspace: HdmiColorspace) -> &'static str {
    match colorspace {
        HDMI_COLORSPACE_RGB => "RGB",
        HDMI_COLORSPACE_YUV422 => "YCbCr 4:2:2",
        HDMI_COLORSPACE_YUV444 => "YCbCr 4:4:4",
        HDMI_COLORSPACE_YUV420 => "YCbCr 4:2:0",
        HDMI_COLORSPACE_RESERVED4 => "Reserved (4)",
        HDMI_COLORSPACE_RESERVED5 => "Reserved (5)",
        HDMI_COLORSPACE_RESERVED6 => "Reserved (6)",
        HDMI_COLORSPACE_IDO_DEFINED => "IDO Defined",
        _ => "Invalid",
    }
}

fn hdmi_scan_mode_get_name(scan_mode: HdmiScanMode) -> &'static str {
    match scan_mode {
        HDMI_SCAN_MODE_NONE => "No Data",
        HDMI_SCAN_MODE_OVERSCAN => "Overscan",
        HDMI_SCAN_MODE_UNDERSCAN => "Underscan",
        HDMI_SCAN_MODE_RESERVED => "Reserved",
        _ => "Invalid",
    }
}

fn hdmi_colorimetry_get_name(colorimetry: HdmiColorimetry) -> &'static str {
    match colorimetry {
        HDMI_COLORIMETRY_NONE => "No Data",
        HDMI_COLORIMETRY_ITU_601 => "ITU601",
        HDMI_COLORIMETRY_ITU_709 => "ITU709",
        HDMI_COLORIMETRY_EXTENDED => "Extended",
        _ => "Invalid",
    }
}

fn hdmi_picture_aspect_get_name(aspect: HdmiPictureAspect) -> &'static str {
    match aspect {
        HDMI_PICTURE_ASPECT_NONE => "No Data",
        HDMI_PICTURE_ASPECT_4_3 => "4:3",
        HDMI_PICTURE_ASPECT_16_9 => "16:9",
        HDMI_PICTURE_ASPECT_64_27 => "64:27",
        HDMI_PICTURE_ASPECT_256_135 => "256:135",
        HDMI_PICTURE_ASPECT_RESERVED => "Reserved",
        _ => "Invalid",
    }
}

fn hdmi_active_aspect_get_name(aspect: HdmiActiveAspect) -> &'static str {
    match aspect {
        HDMI_ACTIVE_ASPECT_16_9_TOP => "16:9 Top",
        HDMI_ACTIVE_ASPECT_14_9_TOP => "14:9 Top",
        HDMI_ACTIVE_ASPECT_16_9_CENTER => "16:9 Center",
        HDMI_ACTIVE_ASPECT_PICTURE => "Same as Picture",
        HDMI_ACTIVE_ASPECT_4_3 => "4:3",
        HDMI_ACTIVE_ASPECT_16_9 => "16:9",
        HDMI_ACTIVE_ASPECT_14_9 => "14:9",
        HDMI_ACTIVE_ASPECT_4_3_SP_14_9 => "4:3 SP 14:9",
        HDMI_ACTIVE_ASPECT_16_9_SP_14_9 => "16:9 SP 14:9",
        HDMI_ACTIVE_ASPECT_16_9_SP_4_3 => "16:9 SP 4:3",
        _ => {
            if aspect > 0xf {
                "Invalid"
            } else {
                "Reserved"
            }
        }
    }
}

fn hdmi_extended_colorimetry_get_name(ec: HdmiExtendedColorimetry) -> &'static str {
    match ec {
        HDMI_EXTENDED_COLORIMETRY_XV_YCC_601 => "xvYCC 601",
        HDMI_EXTENDED_COLORIMETRY_XV_YCC_709 => "xvYCC 709",
        HDMI_EXTENDED_COLORIMETRY_S_YCC_601 => "sYCC 601",
        HDMI_EXTENDED_COLORIMETRY_OPYCC_601 => "opYCC 601",
        HDMI_EXTENDED_COLORIMETRY_OPRGB => "opRGB",
        HDMI_EXTENDED_COLORIMETRY_BT2020_CONST_LUM => "BT.2020 Constant Luminance",
        HDMI_EXTENDED_COLORIMETRY_BT2020 => "BT.2020",
        HDMI_EXTENDED_COLORIMETRY_RESERVED => "Reserved",
        _ => "Invalid",
    }
}

fn hdmi_quantization_range_get_name(qr: HdmiQuantizationRange) -> &'static str {
    match qr {
        HDMI_QUANTIZATION_RANGE_DEFAULT => "Default",
        HDMI_QUANTIZATION_RANGE_LIMITED => "Limited",
        HDMI_QUANTIZATION_RANGE_FULL => "Full",
        HDMI_QUANTIZATION_RANGE_RESERVED => "Reserved",
        _ => "Invalid",
    }
}

fn hdmi_nups_get_name(nups: HdmiNups) -> &'static str {
    match nups {
        HDMI_NUPS_UNKNOWN => "Unknown Non-uniform Scaling",
        HDMI_NUPS_HORIZONTAL => "Horizontally Scaled",
        HDMI_NUPS_VERTICAL => "Vertically Scaled",
        HDMI_NUPS_BOTH => "Horizontally and Vertically Scaled",
        _ => "Invalid",
    }
}

fn hdmi_ycc_quantization_range_get_name(qr: HdmiYccQuantizationRange) -> &'static str {
    match qr {
        HDMI_YCC_QUANTIZATION_RANGE_LIMITED => "Limited",
        HDMI_YCC_QUANTIZATION_RANGE_FULL => "Full",
        _ => "Invalid",
    }
}

fn hdmi_content_type_get_name(ct: HdmiContentType) -> &'static str {
    match ct {
        HDMI_CONTENT_TYPE_GRAPHICS => "Graphics",
        HDMI_CONTENT_TYPE_PHOTO => "Photo",
        HDMI_CONTENT_TYPE_CINEMA => "Cinema",
        HDMI_CONTENT_TYPE_GAME => "Game",
        _ => "Invalid",
    }
}

fn hdmi_avi_infoframe_log(level: DrmLogLevel, frame: &HdmiAviInfoframe) {
    // SAFETY: `HdmiAviInfoframe` begins with an `HdmiAnyInfoframe` header prefix matching C layout.
    let any = unsafe { &*(frame as *const HdmiAviInfoframe as *const HdmiAnyInfoframe) };
    hdmi_infoframe_log_header(level, any);

    hdmi_log!(
        level,
        "    colorspace: {}\n",
        hdmi_colorspace_get_name(frame.colorspace)
    );
    hdmi_log!(
        level,
        "    scan mode: {}\n",
        hdmi_scan_mode_get_name(frame.scan_mode)
    );
    hdmi_log!(
        level,
        "    colorimetry: {}\n",
        hdmi_colorimetry_get_name(frame.colorimetry)
    );
    hdmi_log!(
        level,
        "    picture aspect: {}\n",
        hdmi_picture_aspect_get_name(frame.picture_aspect)
    );
    hdmi_log!(
        level,
        "    active aspect: {}\n",
        hdmi_active_aspect_get_name(frame.active_aspect)
    );
    hdmi_log!(
        level,
        "    itc: {}\n",
        if frame.itc { "IT Content" } else { "No Data" }
    );
    hdmi_log!(
        level,
        "    extended colorimetry: {}\n",
        hdmi_extended_colorimetry_get_name(frame.extended_colorimetry)
    );
    hdmi_log!(
        level,
        "    quantization range: {}\n",
        hdmi_quantization_range_get_name(frame.quantization_range)
    );
    hdmi_log!(level, "    nups: {}\n", hdmi_nups_get_name(frame.nups));
    hdmi_log!(level, "    video code: {}\n", frame.video_code);
    hdmi_log!(
        level,
        "    ycc quantization range: {}\n",
        hdmi_ycc_quantization_range_get_name(frame.ycc_quantization_range)
    );
    hdmi_log!(
        level,
        "    hdmi content type: {}\n",
        hdmi_content_type_get_name(frame.content_type)
    );
    hdmi_log!(level, "    pixel repeat: {}\n", frame.pixel_repeat);
    hdmi_log!(
        level,
        "    bar top {}, bottom {}, left {}, right {}\n",
        frame.top_bar,
        frame.bottom_bar,
        frame.left_bar,
        frame.right_bar
    );
}

fn hdmi_spd_sdi_get_name(sdi: HdmiSpdSdi) -> &'static str {
    match sdi {
        HDMI_SPD_SDI_UNKNOWN => "Unknown",
        HDMI_SPD_SDI_DSTB => "Digital STB",
        HDMI_SPD_SDI_DVDP => "DVD Player",
        HDMI_SPD_SDI_DVHS => "D-VHS",
        HDMI_SPD_SDI_HDDVR => "HDD Videorecorder",
        HDMI_SPD_SDI_DVC => "DVC",
        HDMI_SPD_SDI_DSC => "DSC",
        HDMI_SPD_SDI_VCD => "Video CD",
        HDMI_SPD_SDI_GAME => "Game",
        HDMI_SPD_SDI_PC => "PC General",
        HDMI_SPD_SDI_BD => "Blu-Ray Disc (BD)",
        HDMI_SPD_SDI_SACD => "Super Audio CD",
        HDMI_SPD_SDI_HDDVD => "HD DVD",
        HDMI_SPD_SDI_PMP => "PMP",
        _ => {
            if sdi > 0xff {
                "Invalid"
            } else {
                "Reserved"
            }
        }
    }
}

fn hdmi_spd_infoframe_log(level: DrmLogLevel, frame: &HdmiSpdInfoframe) {
    // SAFETY: `HdmiSpdInfoframe` begins with an `HdmiAnyInfoframe` header prefix matching C layout.
    let any = unsafe { &*(frame as *const HdmiSpdInfoframe as *const HdmiAnyInfoframe) };
    hdmi_infoframe_log_header(level, any);

    // SAFETY: frame.vendor is an array of 8 c_char, casting to u8 byte slice is valid.
    let v_slice = unsafe { slice::from_raw_parts(frame.vendor.as_ptr().cast::<u8>(), 8) };
    // SAFETY: frame.product is an array of 16 c_char, casting to u8 byte slice is valid.
    let p_slice = unsafe { slice::from_raw_parts(frame.product.as_ptr().cast::<u8>(), 16) };
    let v_str = core::str::from_utf8(v_slice).unwrap_or("<invalid utf8>");
    let p_str = core::str::from_utf8(p_slice).unwrap_or("<invalid utf8>");

    hdmi_log!(level, "    vendor: {}\n", v_str.trim_matches('\0'));
    hdmi_log!(level, "    product: {}\n", p_str.trim_matches('\0'));
    hdmi_log!(
        level,
        "    source device information: {} ({:#x})\n",
        hdmi_spd_sdi_get_name(frame.sdi),
        frame.sdi
    );
}

fn hdmi_audio_coding_type_get_name(ct: HdmiAudioCodingType) -> &'static str {
    match ct {
        HDMI_AUDIO_CODING_TYPE_STREAM => "Refer to Stream Header",
        HDMI_AUDIO_CODING_TYPE_PCM => "PCM",
        HDMI_AUDIO_CODING_TYPE_AC3 => "AC-3",
        HDMI_AUDIO_CODING_TYPE_MPEG1 => "MPEG1",
        HDMI_AUDIO_CODING_TYPE_MP3 => "MP3",
        HDMI_AUDIO_CODING_TYPE_MPEG2 => "MPEG2",
        HDMI_AUDIO_CODING_TYPE_AAC_LC => "AAC",
        HDMI_AUDIO_CODING_TYPE_DTS => "DTS",
        HDMI_AUDIO_CODING_TYPE_ATRAC => "ATRAC",
        HDMI_AUDIO_CODING_TYPE_DSD => "One Bit Audio",
        HDMI_AUDIO_CODING_TYPE_EAC3 => "Dolby Digital +",
        HDMI_AUDIO_CODING_TYPE_DTS_HD => "DTS-HD",
        HDMI_AUDIO_CODING_TYPE_MLP => "MAT (MLP)",
        HDMI_AUDIO_CODING_TYPE_DST => "DST",
        HDMI_AUDIO_CODING_TYPE_WMA_PRO => "WMA PRO",
        HDMI_AUDIO_CODING_TYPE_CXT => "Refer to CXT",
        _ => "Invalid",
    }
}

fn hdmi_audio_sample_size_get_name(ss: HdmiAudioSampleSize) -> &'static str {
    match ss {
        HDMI_AUDIO_SAMPLE_SIZE_STREAM => "Refer to Stream Header",
        HDMI_AUDIO_SAMPLE_SIZE_16 => "16 bit",
        HDMI_AUDIO_SAMPLE_SIZE_20 => "20 bit",
        HDMI_AUDIO_SAMPLE_SIZE_24 => "24 bit",
        _ => "Invalid",
    }
}

fn hdmi_audio_sample_frequency_get_name(sf: HdmiAudioSampleFrequency) -> &'static str {
    match sf {
        HDMI_AUDIO_SAMPLE_FREQUENCY_STREAM => "Refer to Stream Header",
        HDMI_AUDIO_SAMPLE_FREQUENCY_32000 => "32 kHz",
        HDMI_AUDIO_SAMPLE_FREQUENCY_44100 => "44.1 kHz (CD)",
        HDMI_AUDIO_SAMPLE_FREQUENCY_48000 => "48 kHz",
        HDMI_AUDIO_SAMPLE_FREQUENCY_88200 => "88.2 kHz",
        HDMI_AUDIO_SAMPLE_FREQUENCY_96000 => "96 kHz",
        HDMI_AUDIO_SAMPLE_FREQUENCY_176400 => "176.4 kHz",
        HDMI_AUDIO_SAMPLE_FREQUENCY_192000 => "192 kHz",
        _ => "Invalid",
    }
}

fn hdmi_audio_coding_type_ext_get_name(ctx: HdmiAudioCodingTypeExt) -> &'static str {
    match ctx {
        HDMI_AUDIO_CODING_TYPE_EXT_CT => "Refer to CT",
        HDMI_AUDIO_CODING_TYPE_EXT_HE_AAC => "HE AAC",
        HDMI_AUDIO_CODING_TYPE_EXT_HE_AAC_V2 => "HE AAC v2",
        HDMI_AUDIO_CODING_TYPE_EXT_MPEG_SURROUND => "MPEG SURROUND",
        HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_HE_AAC => "MPEG-4 HE AAC",
        HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_HE_AAC_V2 => "MPEG-4 HE AAC v2",
        HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_AAC_LC => "MPEG-4 AAC LC",
        HDMI_AUDIO_CODING_TYPE_EXT_DRA => "DRA",
        HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_HE_AAC_SURROUND => "MPEG-4 HE AAC + MPEG Surround",
        HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_AAC_LC_SURROUND => "MPEG-4 AAC LC + MPEG Surround",
        _ => {
            if ctx > 0x1f {
                "Invalid"
            } else {
                "Reserved"
            }
        }
    }
}

fn hdmi_audio_infoframe_log(level: DrmLogLevel, frame: &HdmiAudioInfoframe) {
    // SAFETY: `HdmiAudioInfoframe` begins with an `HdmiAnyInfoframe` header prefix matching C layout.
    let any = unsafe { &*(frame as *const HdmiAudioInfoframe as *const HdmiAnyInfoframe) };
    hdmi_infoframe_log_header(level, any);

    if frame.channels != 0 {
        hdmi_log!(level, "    channels: {}\n", frame.channels - 1);
    } else {
        hdmi_log!(level, "    channels: Refer to stream header\n");
    }
    hdmi_log!(
        level,
        "    coding type: {}\n",
        hdmi_audio_coding_type_get_name(frame.coding_type)
    );
    hdmi_log!(
        level,
        "    sample size: {}\n",
        hdmi_audio_sample_size_get_name(frame.sample_size)
    );
    hdmi_log!(
        level,
        "    sample frequency: {}\n",
        hdmi_audio_sample_frequency_get_name(frame.sample_frequency)
    );
    hdmi_log!(
        level,
        "    coding type ext: {}\n",
        hdmi_audio_coding_type_ext_get_name(frame.coding_type_ext)
    );
    hdmi_log!(
        level,
        "    channel allocation: {:#x}\n",
        frame.channel_allocation
    );
    hdmi_log!(
        level,
        "    level shift value: {} dB\n",
        frame.level_shift_value
    );
    hdmi_log!(
        level,
        "    downmix inhibit: {}\n",
        if frame.downmix_inhibit { "Yes" } else { "No" }
    );
}

fn hdmi_drm_infoframe_log(level: DrmLogLevel, frame: &HdmiDrmInfoframe) {
    // SAFETY: `HdmiDrmInfoframe` begins with an `HdmiAnyInfoframe` header prefix matching C layout.
    let any = unsafe { &*(frame as *const HdmiDrmInfoframe as *const HdmiAnyInfoframe) };
    hdmi_infoframe_log_header(level, any);

    hdmi_log!(level, "length: {}\n", frame.length);
    hdmi_log!(level, "metadata type: {}\n", frame.metadata_type);
    hdmi_log!(level, "eotf: {}\n", frame.eotf);
    for (i, primary) in frame.display_primaries.iter().enumerate() {
        hdmi_log!(level, "x[{}]: {}\n", i, primary.x);
        hdmi_log!(level, "y[{}]: {}\n", i, primary.y);
    }
    hdmi_log!(level, "white point x: {}\n", frame.white_point.x);
    hdmi_log!(level, "white point y: {}\n", frame.white_point.y);
    hdmi_log!(
        level,
        "max_display_mastering_luminance: {}\n",
        frame.max_display_mastering_luminance
    );
    hdmi_log!(
        level,
        "min_display_mastering_luminance: {}\n",
        frame.min_display_mastering_luminance
    );
    hdmi_log!(level, "max_cll: {}\n", frame.max_cll);
    hdmi_log!(level, "max_fall: {}\n", frame.max_fall);
}

fn hdmi_3d_structure_get_name(s3d: Hdmi3dStructure) -> &'static str {
    match s3d {
        HDMI_3D_STRUCTURE_FRAME_PACKING => "Frame Packing",
        HDMI_3D_STRUCTURE_FIELD_ALTERNATIVE => "Field Alternative",
        HDMI_3D_STRUCTURE_LINE_ALTERNATIVE => "Line Alternative",
        HDMI_3D_STRUCTURE_SIDE_BY_SIDE_FULL => "Side-by-side (Full)",
        HDMI_3D_STRUCTURE_L_DEPTH => "L + Depth",
        HDMI_3D_STRUCTURE_L_DEPTH_GFX_GFX_DEPTH => "L + Depth + Graphics + Graphics-depth",
        HDMI_3D_STRUCTURE_TOP_AND_BOTTOM => "Top-and-Bottom",
        HDMI_3D_STRUCTURE_SIDE_BY_SIDE_HALF => "Side-by-side (Half)",
        _ => {
            if !(0..=0xf).contains(&s3d) {
                "Invalid"
            } else {
                "Reserved"
            }
        }
    }
}

fn hdmi_vendor_any_infoframe_log(level: DrmLogLevel, frame: &HdmiVendorAnyInfoframe) {
    // SAFETY: `frame` is a live shared reference, and `any` is a field of its union, so the borrow stays valid for the duration of this log call.
    let any = unsafe { &*frame.any };
    let hdmi_any =
        // SAFETY: `HdmiVendorAnyInfoframeAny` begins with the common `HdmiAnyInfoframe` header prefix per C layout; this cast relies on that prefix, as `hdmi_drm_infoframe_log` does.
        unsafe { &*(any as *const HdmiVendorAnyInfoframeAny as *const HdmiAnyInfoframe) };
    hdmi_infoframe_log_header(level, hdmi_any);

    if any.oui != HDMI_IEEE_OUI {
        hdmi_log!(level, "    not a HDMI vendor infoframe\n");
        return;
    }

    // SAFETY: Accessing union field `hdmi`.
    let hvf = unsafe { &frame.hdmi };
    if hvf.vic == 0 && hvf.s3d_struct == HDMI_3D_STRUCTURE_INVALID {
        hdmi_log!(level, "    empty frame\n");
        return;
    }

    if hvf.vic != 0 {
        hdmi_log!(level, "    HDMI VIC: {}\n", hvf.vic);
    }
    if hvf.s3d_struct != HDMI_3D_STRUCTURE_INVALID {
        hdmi_log!(
            level,
            "    3D structure: {}\n",
            hdmi_3d_structure_get_name(hvf.s3d_struct)
        );
        if hvf.s3d_struct >= HDMI_3D_STRUCTURE_SIDE_BY_SIDE_HALF {
            hdmi_log!(level, "    3D extension data: {}\n", hvf.s3d_ext_data);
        }
    }
}

// upstream: drivers/video/hdmi.c hdmi_infoframe_log()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `level` must be null or point to a valid null-terminated C string.
/// - `frame` must be null or point to an initialized `HdmiInfoframe` union.
pub unsafe extern "C" fn hdmi_infoframe_log(
    level: *const c_char,
    _dev: *mut c_void,
    frame: *const HdmiInfoframe,
) {
    if frame.is_null() {
        return;
    }
    // SAFETY: `level` is checked non-null and dereferenceable.
    let log_level = if !level.is_null() && unsafe { *level } == b'3' as c_char {
        DrmLogLevel::Error
    // SAFETY: `level` is checked non-null and dereferenceable.
    } else if !level.is_null() && unsafe { *level } == b'4' as c_char {
        DrmLogLevel::Warn
    // SAFETY: `level` is checked non-null and dereferenceable.
    } else if !level.is_null() && unsafe { *level } == b'6' as c_char {
        DrmLogLevel::Info
    } else {
        DrmLogLevel::Debug
    };

    // SAFETY: `frame` was checked non-null and is dereferenceable.
    let frame_ref = unsafe { &*frame };
    // SAFETY: Accessing union field `any`.
    let any_type = unsafe { (*frame_ref.any).r#type };
    match any_type {
        HDMI_INFOFRAME_TYPE_AVI => {
            // SAFETY: Accessing union field `avi`.
            let avi = unsafe { &(*frame).avi };
            hdmi_avi_infoframe_log(log_level, avi);
        }
        HDMI_INFOFRAME_TYPE_SPD => {
            // SAFETY: Accessing union field `spd`.
            let spd = unsafe { &(*frame).spd };
            hdmi_spd_infoframe_log(log_level, spd);
        }
        HDMI_INFOFRAME_TYPE_AUDIO => {
            // SAFETY: Accessing union field `audio`.
            let audio = unsafe { &(*frame).audio };
            hdmi_audio_infoframe_log(log_level, audio);
        }
        HDMI_INFOFRAME_TYPE_VENDOR => {
            // SAFETY: Accessing union field `vendor`.
            let vendor = unsafe { &(*frame).vendor };
            hdmi_vendor_any_infoframe_log(log_level, vendor);
        }
        HDMI_INFOFRAME_TYPE_DRM => {
            // SAFETY: Accessing union field `drm`.
            let drm = unsafe { &(*frame).drm };
            hdmi_drm_infoframe_log(log_level, drm);
        }
        _ => {}
    }
}

// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_eld.c`.
//!
//! Source-faithful transcription of Linux 7.2.3. The ELD header inlines from
//! `include/drm/drm_eld.h` are provided as private helpers. The CTA SAD
//! conversion is owned by the EDID module and is declared `extern "C"` here.

use drm_kapi::config::EINVAL;

use crate::drm_edid_types::CeaSad;

// upstream: include/drm/drm_eld.h
const DRM_ELD_CEA_EDID_VER_MNL: usize = 4;
const DRM_ELD_MNL_SHIFT: u32 = 0;
const DRM_ELD_MNL_MASK: u8 = 0x1f;
const DRM_ELD_SAD_COUNT_CONN_TYPE: usize = 5;
const DRM_ELD_SAD_COUNT_SHIFT: u32 = 4;
const DRM_ELD_SAD_COUNT_MASK: u8 = 0xf << 4;

// upstream: include/drm/drm_eld.h DRM_ELD_CEA_SAD(mnl, sad)
const fn drm_eld_cea_sad(mnl: usize, sad: usize) -> usize {
    20 + mnl + 3 * sad
}

// upstream: include/drm/drm_eld.h drm_eld_mnl()
#[inline]
fn drm_eld_mnl(eld: *const u8) -> usize {
    // SAFETY: `eld` is expected to point to a valid ELD buffer of at least 5 bytes.
    // Offset 4 is within the ELD baseline header (bytes 0..20) and reading 1 byte has no alignment requirement.
    ((unsafe { *eld.add(DRM_ELD_CEA_EDID_VER_MNL) } & DRM_ELD_MNL_MASK) >> DRM_ELD_MNL_SHIFT)
        as usize
}

// upstream: include/drm/drm_eld.h drm_eld_sad_count()
#[inline]
fn drm_eld_sad_count(eld: *const u8) -> i32 {
    // SAFETY: `eld` is expected to point to a valid ELD buffer of at least 6 bytes.
    // Offset 5 is within the ELD baseline header (bytes 0..20) and reading 1 byte has no alignment requirement.
    ((unsafe { *eld.add(DRM_ELD_SAD_COUNT_CONN_TYPE) } & DRM_ELD_SAD_COUNT_MASK)
        >> DRM_ELD_SAD_COUNT_SHIFT) as i32
}

// CTA SAD serialization is implemented by the sibling EDID module.
unsafe extern "C" {
    fn drm_edid_cta_sad_get(cta_sad: *const CeaSad, sad: *mut u8);
    fn drm_edid_cta_sad_set(cta_sad: *mut CeaSad, sad: *const u8);
}

// upstream: drm_eld.c drm_eld_sad_get()
#[unsafe(no_mangle)]
/// # Safety
/// - `eld` must be non-null, properly aligned for `u8`, and point to a readable ELD buffer of at least
///   `20 + mnl + 3 * (sad_index + 1)` bytes, where `mnl = eld[4] & 0x1f`.
/// - `sad_index` must be non-negative and strictly less than `drm_eld_sad_count(eld)`.
/// - `cta_sad` must be non-null, properly aligned for `CeaSad`, and point to unaliased writable memory
///   with capacity for at least `size_of::<CeaSad>()` bytes.
pub unsafe extern "C" fn drm_eld_sad_get(
    eld: *const u8,
    sad_index: i32,
    cta_sad: *mut CeaSad,
) -> i32 {
    if sad_index >= drm_eld_sad_count(eld) {
        return -EINVAL;
    }
    // SAFETY: `eld` points to a valid ELD buffer whose size covers the baseline header and
    // the CTA SAD block at `sad_index`. The computed offset `20 + mnl + 3 * sad_index` remains
    // within the allocated buffer, producing a valid pointer to a 3-byte CTA SAD entry.
    let sad = unsafe { eld.add(drm_eld_cea_sad(drm_eld_mnl(eld), sad_index as usize)) };
    // SAFETY: `cta_sad` is non-null, properly aligned, and writable for `CeaSad`; `sad` points
    // to a readable 3-byte CTA SAD block in the ELD buffer. `drm_edid_cta_sad_set` unpacks the SAD
    // bytes into the destination struct.
    unsafe { drm_edid_cta_sad_set(cta_sad, sad) };
    0
}

// upstream: drm_eld.c drm_eld_sad_set()
#[unsafe(no_mangle)]
/// # Safety
/// - `eld` must be non-null, properly aligned for `u8`, and point to a writable ELD buffer of at least
///   `20 + mnl + 3 * (sad_index + 1)` bytes, where `mnl = eld[4] & 0x1f`.
/// - `sad_index` must be non-negative and strictly less than `drm_eld_sad_count(eld)`.
/// - `cta_sad` must be non-null, properly aligned for `CeaSad`, and point to an initialized `CeaSad`.
pub unsafe extern "C" fn drm_eld_sad_set(
    eld: *mut u8,
    sad_index: i32,
    cta_sad: *const CeaSad,
) -> i32 {
    if sad_index >= drm_eld_sad_count(eld) {
        return -EINVAL;
    }
    // SAFETY: `eld` points to a valid, writable ELD buffer whose length accommodates the baseline header
    // and the SAD entry at `sad_index`. The computed offset `20 + mnl + 3 * sad_index` remains within
    // the buffer, yielding a valid, writable 3-byte destination pointer.
    let sad = unsafe { eld.add(drm_eld_cea_sad(drm_eld_mnl(eld), sad_index as usize)) };
    // SAFETY: `cta_sad` is non-null, aligned, and points to a valid, initialized `CeaSad`; `sad` points
    // to 3 writable bytes within the ELD buffer. `drm_edid_cta_sad_get` serializes the SAD into the buffer.
    unsafe { drm_edid_cta_sad_get(cta_sad, sad) };
    0
}

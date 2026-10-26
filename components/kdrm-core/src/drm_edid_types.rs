// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_edid.h`

use core::{
    ffi::c_char,
    mem::{ManuallyDrop, size_of},
};

#[repr(C)]
pub struct EstTimings {
    pub t1: u8,
    pub t2: u8,
    pub mfg_rsvd: u8,
}

#[repr(C)]
pub struct StdTiming {
    pub hsize: u8,
    pub vfreq_aspect: u8,
}

#[repr(C)]
pub struct DetailedPixelTiming {
    pub hactive_lo: u8,
    pub hblank_lo: u8,
    pub hactive_hblank_hi: u8,
    pub vactive_lo: u8,
    pub vblank_lo: u8,
    pub vactive_vblank_hi: u8,
    pub hsync_offset_lo: u8,
    pub hsync_pulse_width_lo: u8,
    pub vsync_offset_pulse_width_lo: u8,
    pub hsync_vsync_offset_pulse_width_hi: u8,
    pub width_mm_lo: u8,
    pub height_mm_lo: u8,
    pub width_height_mm_hi: u8,
    pub hborder: u8,
    pub vborder: u8,
    pub misc: u8,
}

#[repr(C)]
pub struct DetailedDataString {
    pub str: [u8; 13],
}

#[repr(C, packed)]
pub struct DetailedDataMonitorRangeFormulaGtf2 {
    pub reserved: u8,
    pub hfreq_start_khz: u8,
    pub c: u8,
    pub m: u16,
    pub k: u8,
    pub j: u8,
}

#[repr(C)]
pub struct DetailedDataMonitorRangeFormulaCvt {
    pub version: u8,
    pub data1: u8,
    pub data2: u8,
    pub supported_aspects: u8,
    pub flags: u8,
    pub supported_scalings: u8,
    pub preferred_refresh: u8,
}

#[repr(C)]
pub union DetailedDataMonitorRangeFormula {
    pub gtf2: ManuallyDrop<DetailedDataMonitorRangeFormulaGtf2>,
    pub cvt: ManuallyDrop<DetailedDataMonitorRangeFormulaCvt>,
}

#[repr(C, packed)]
pub struct DetailedDataMonitorRange {
    pub min_vfreq: u8,
    pub max_vfreq: u8,
    pub min_hfreq_khz: u8,
    pub max_hfreq_khz: u8,
    pub pixel_clock_mhz: u8,
    pub flags: u8,
    pub formula: DetailedDataMonitorRangeFormula,
}

#[repr(C)]
pub struct DetailedDataWpindex {
    pub white_yx_lo: u8,
    pub white_x_hi: u8,
    pub white_y_hi: u8,
    pub gamma: u8,
}

#[repr(C)]
pub struct DetailedDataColorPoint {
    pub windex1: u8,
    pub wpindex1: [u8; 3],
    pub windex2: u8,
    pub wpindex2: [u8; 3],
}

#[repr(C)]
pub struct CvtTiming {
    pub code: [u8; 3],
}

#[repr(C)]
pub union DetailedNonPixelData {
    pub str: ManuallyDrop<DetailedDataString>,
    pub range: ManuallyDrop<DetailedDataMonitorRange>,
    pub color: ManuallyDrop<DetailedDataWpindex>,
    pub timings: [ManuallyDrop<StdTiming>; 6],
    pub cvt: [ManuallyDrop<CvtTiming>; 4],
}

#[repr(C, packed)]
pub struct DetailedNonPixel {
    pub pad1: u8,
    pub r#type: u8,
    pub pad2: u8,
    pub data: DetailedNonPixelData,
}

#[repr(C)]
pub union DetailedTimingData {
    pub pixel_data: ManuallyDrop<DetailedPixelTiming>,
    pub other_data: ManuallyDrop<DetailedNonPixel>,
}

#[repr(C)]
pub struct DetailedTiming {
    pub pixel_clock: u16,
    pub data: DetailedTimingData,
}

#[repr(C, packed)]
pub struct DrmEdidProductId {
    pub manufacturer_name: u16,
    pub product_code: u16,
    pub serial_number: u32,
    pub week_of_manufacture: u8,
    pub year_of_manufacture: u8,
}

#[repr(C, packed)]
pub struct EdidAnon8Anon8 {
    pub mfg_id: [u8; 2],
    pub prod_code: [u8; 2],
    pub serial: u32,
    pub mfg_week: u8,
    pub mfg_year: u8,
}

#[repr(C)]
pub union EdidAnon8 {
    pub product_id: ManuallyDrop<DrmEdidProductId>,
    pub _anon_8: ManuallyDrop<EdidAnon8Anon8>,
}

#[repr(C)]
pub struct Edid {
    pub header: [u8; 8],
    pub _anon_8: EdidAnon8,
    pub version: u8,
    pub revision: u8,
    pub input: u8,
    pub width_cm: u8,
    pub height_cm: u8,
    pub gamma: u8,
    pub features: u8,
    pub red_green_lo: u8,
    pub blue_white_lo: u8,
    pub red_x: u8,
    pub red_y: u8,
    pub green_x: u8,
    pub green_y: u8,
    pub blue_x: u8,
    pub blue_y: u8,
    pub white_x: u8,
    pub white_y: u8,
    pub established_timings: EstTimings,
    pub standard_timings: [StdTiming; 8],
    pub detailed_timings: [DetailedTiming; 4],
    pub extensions: u8,
    pub checksum: u8,
}

#[repr(C)]
pub struct DrmEdidIdent {
    pub panel_id: u32,
    pub name: *mut c_char,
}

#[repr(C)]
pub struct CeaSad {
    pub format: u8,
    pub channels: u8,
    pub freq: u8,
    pub byte2: u8,
}

const _: () = assert!(size_of::<EstTimings>() == 3);
const _: () = assert!(core::mem::offset_of!(EstTimings, t1) == 0);
const _: () = assert!(core::mem::offset_of!(EstTimings, t2) == 1);
const _: () = assert!(core::mem::offset_of!(EstTimings, mfg_rsvd) == 2);
const _: () = assert!(size_of::<StdTiming>() == 2);
const _: () = assert!(core::mem::offset_of!(StdTiming, hsize) == 0);
const _: () = assert!(core::mem::offset_of!(StdTiming, vfreq_aspect) == 1);
const _: () = assert!(size_of::<DetailedPixelTiming>() == 16);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, hactive_lo) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, hblank_lo) == 1);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, hactive_hblank_hi) == 2);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, vactive_lo) == 3);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, vblank_lo) == 4);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, vactive_vblank_hi) == 5);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, hsync_offset_lo) == 6);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, hsync_pulse_width_lo) == 7);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, vsync_offset_pulse_width_lo) == 8);
const _: () =
    assert!(core::mem::offset_of!(DetailedPixelTiming, hsync_vsync_offset_pulse_width_hi) == 9);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, width_mm_lo) == 10);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, height_mm_lo) == 11);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, width_height_mm_hi) == 12);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, hborder) == 13);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, vborder) == 14);
const _: () = assert!(core::mem::offset_of!(DetailedPixelTiming, misc) == 15);
const _: () = assert!(size_of::<DetailedDataString>() == 13);
const _: () = assert!(core::mem::offset_of!(DetailedDataString, str) == 0);
const _: () = assert!(size_of::<[u8; 13]>() == 13);
const _: () = assert!(size_of::<DetailedDataMonitorRangeFormulaGtf2>() == 7);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaGtf2, reserved) == 0);
const _: () =
    assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaGtf2, hfreq_start_khz) == 1);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaGtf2, c) == 2);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaGtf2, m) == 3);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaGtf2, k) == 5);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaGtf2, j) == 6);
const _: () = assert!(size_of::<DetailedDataMonitorRangeFormulaCvt>() == 7);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, version) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, data1) == 1);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, data2) == 2);
const _: () =
    assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, supported_aspects) == 3);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, flags) == 4);
const _: () =
    assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, supported_scalings) == 5);
const _: () =
    assert!(core::mem::offset_of!(DetailedDataMonitorRangeFormulaCvt, preferred_refresh) == 6);
const _: () = assert!(size_of::<DetailedDataMonitorRangeFormula>() == 7);
const _: () = assert!(size_of::<DetailedDataMonitorRange>() == 13);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, min_vfreq) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, max_vfreq) == 1);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, min_hfreq_khz) == 2);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, max_hfreq_khz) == 3);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, pixel_clock_mhz) == 4);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, flags) == 5);
const _: () = assert!(core::mem::offset_of!(DetailedDataMonitorRange, formula) == 6);
const _: () = assert!(size_of::<DetailedDataWpindex>() == 4);
const _: () = assert!(core::mem::offset_of!(DetailedDataWpindex, white_yx_lo) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedDataWpindex, white_x_hi) == 1);
const _: () = assert!(core::mem::offset_of!(DetailedDataWpindex, white_y_hi) == 2);
const _: () = assert!(core::mem::offset_of!(DetailedDataWpindex, gamma) == 3);
const _: () = assert!(size_of::<DetailedDataColorPoint>() == 8);
const _: () = assert!(core::mem::offset_of!(DetailedDataColorPoint, windex1) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedDataColorPoint, wpindex1) == 1);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () = assert!(core::mem::offset_of!(DetailedDataColorPoint, windex2) == 4);
const _: () = assert!(core::mem::offset_of!(DetailedDataColorPoint, wpindex2) == 5);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () = assert!(size_of::<CvtTiming>() == 3);
const _: () = assert!(core::mem::offset_of!(CvtTiming, code) == 0);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () = assert!(size_of::<DetailedNonPixelData>() == 13);
const _: () = assert!(size_of::<DetailedNonPixel>() == 16);
const _: () = assert!(core::mem::offset_of!(DetailedNonPixel, pad1) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedNonPixel, r#type) == 1);
const _: () = assert!(core::mem::offset_of!(DetailedNonPixel, pad2) == 2);
const _: () = assert!(core::mem::offset_of!(DetailedNonPixel, data) == 3);
const _: () = assert!(size_of::<DetailedTimingData>() == 16);
const _: () = assert!(size_of::<DetailedTiming>() == 18);
const _: () = assert!(core::mem::offset_of!(DetailedTiming, pixel_clock) == 0);
const _: () = assert!(core::mem::offset_of!(DetailedTiming, data) == 2);
const _: () = assert!(size_of::<DrmEdidProductId>() == 10);
const _: () = assert!(core::mem::offset_of!(DrmEdidProductId, manufacturer_name) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEdidProductId, product_code) == 2);
const _: () = assert!(core::mem::offset_of!(DrmEdidProductId, serial_number) == 4);
const _: () = assert!(core::mem::offset_of!(DrmEdidProductId, week_of_manufacture) == 8);
const _: () = assert!(core::mem::offset_of!(DrmEdidProductId, year_of_manufacture) == 9);
const _: () = assert!(size_of::<EdidAnon8Anon8>() == 10);
const _: () = assert!(core::mem::offset_of!(EdidAnon8Anon8, mfg_id) == 0);
const _: () = assert!(size_of::<[u8; 2]>() == 2);
const _: () = assert!(core::mem::offset_of!(EdidAnon8Anon8, prod_code) == 2);
const _: () = assert!(size_of::<[u8; 2]>() == 2);
const _: () = assert!(core::mem::offset_of!(EdidAnon8Anon8, serial) == 4);
const _: () = assert!(core::mem::offset_of!(EdidAnon8Anon8, mfg_week) == 8);
const _: () = assert!(core::mem::offset_of!(EdidAnon8Anon8, mfg_year) == 9);
const _: () = assert!(size_of::<EdidAnon8>() == 10);
const _: () = assert!(size_of::<Edid>() == 128);
const _: () = assert!(core::mem::offset_of!(Edid, header) == 0);
const _: () = assert!(size_of::<[u8; 8]>() == 8);
const _: () = assert!(core::mem::offset_of!(Edid, _anon_8) == 8);
const _: () = assert!(core::mem::offset_of!(Edid, version) == 18);
const _: () = assert!(core::mem::offset_of!(Edid, revision) == 19);
const _: () = assert!(core::mem::offset_of!(Edid, input) == 20);
const _: () = assert!(core::mem::offset_of!(Edid, width_cm) == 21);
const _: () = assert!(core::mem::offset_of!(Edid, height_cm) == 22);
const _: () = assert!(core::mem::offset_of!(Edid, gamma) == 23);
const _: () = assert!(core::mem::offset_of!(Edid, features) == 24);
const _: () = assert!(core::mem::offset_of!(Edid, red_green_lo) == 25);
const _: () = assert!(core::mem::offset_of!(Edid, blue_white_lo) == 26);
const _: () = assert!(core::mem::offset_of!(Edid, red_x) == 27);
const _: () = assert!(core::mem::offset_of!(Edid, red_y) == 28);
const _: () = assert!(core::mem::offset_of!(Edid, green_x) == 29);
const _: () = assert!(core::mem::offset_of!(Edid, green_y) == 30);
const _: () = assert!(core::mem::offset_of!(Edid, blue_x) == 31);
const _: () = assert!(core::mem::offset_of!(Edid, blue_y) == 32);
const _: () = assert!(core::mem::offset_of!(Edid, white_x) == 33);
const _: () = assert!(core::mem::offset_of!(Edid, white_y) == 34);
const _: () = assert!(core::mem::offset_of!(Edid, established_timings) == 35);
const _: () = assert!(size_of::<EstTimings>() == 3);
const _: () = assert!(core::mem::offset_of!(Edid, standard_timings) == 38);
const _: () = assert!(size_of::<[StdTiming; 8]>() == 16);
const _: () = assert!(core::mem::offset_of!(Edid, detailed_timings) == 54);
const _: () = assert!(size_of::<[DetailedTiming; 4]>() == 72);
const _: () = assert!(core::mem::offset_of!(Edid, extensions) == 126);
const _: () = assert!(core::mem::offset_of!(Edid, checksum) == 127);
const _: () = assert!(size_of::<DrmEdidIdent>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmEdidIdent, panel_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEdidIdent, name) == 8);
const _: () = assert!(size_of::<CeaSad>() == 4);
const _: () = assert!(core::mem::offset_of!(CeaSad, format) == 0);
const _: () = assert!(core::mem::offset_of!(CeaSad, channels) == 1);
const _: () = assert!(core::mem::offset_of!(CeaSad, freq) == 2);
const _: () = assert!(core::mem::offset_of!(CeaSad, byte2) == 3);

// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/display/drm_dp_helper.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::types::{DelayedWork, Mutex, WorkStruct};

use crate::{
    drm_connector_types::DrmConnector,
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_dp_types::{DpColorimetry, DpContentType, DpDynamicRange, DpPixelformat, OperationMode},
    kernel_placeholder_types::I2cAdapter,
};

#[repr(C)]
pub struct DrmDpVscSdp {
    pub sdp_type: u8,
    pub revision: u8,
    pub length: u8,
    pub pixelformat: DpPixelformat,
    pub colorimetry: DpColorimetry,
    pub bpc: i32,
    pub dynamic_range: DpDynamicRange,
    pub content_type: DpContentType,
}

#[repr(C)]
pub struct DrmDpAsSdp {
    pub sdp_type: u8,
    pub revision: u8,
    pub length: u8,
    pub vtotal: i32,
    pub target_rr: i32,
    pub duration_incr_ms: i32,
    pub duration_decr_ms: i32,
    pub target_rr_divider: bool,
    pub mode: OperationMode,
    pub coasting_vtotal: i32,
}

#[repr(C)]
pub struct DrmDpAuxMsg {
    pub address: u32,
    pub request: u8,
    pub reply: u8,
    pub buffer: *mut c_void,
    pub size: usize,
}

#[repr(C, align(8))]
pub struct DrmDpAuxCec {
    pub lock: Mutex,
    pub adap: *mut c_void,
    pub connector: *mut DrmConnector,
    pub unregister_work: DelayedWork,
}

#[repr(C, align(8))]
pub struct DrmDpAux {
    pub name: *mut c_char,
    pub ddc: I2cAdapter,
    pub dev: *mut c_void,
    pub drm_dev: *mut DrmDevice,
    pub crtc: *mut DrmCrtc,
    pub hw_mutex: Mutex,
    pub crc_work: WorkStruct,
    pub crc_count: u8,
    pub transfer: Option<unsafe extern "C" fn(*mut DrmDpAux, *mut DrmDpAuxMsg) -> isize>,
    pub wait_hpd_asserted: Option<unsafe extern "C" fn(*mut DrmDpAux, u64) -> i32>,
    pub i2c_nack_count: u32,
    pub i2c_defer_count: u32,
    pub cec: DrmDpAuxCec,
    pub is_remote: bool,
    pub powered_down: bool,
    pub no_zero_sized: bool,
    pub dpcd_probe_disabled: bool,
}

#[repr(C)]
pub struct DrmDpDpcdIdent {
    pub oui: [u8; 3],
    pub device_id: [u8; 6],
    pub hw_rev: u8,
    pub sw_major_rev: u8,
    pub sw_minor_rev: u8,
}

#[repr(C)]
pub struct DrmDpDesc {
    pub ident: DrmDpDpcdIdent,
    pub quirks: u32,
}

#[repr(C)]
pub struct DrmEdpBacklightInfo {
    pub pwmgen_bit_count: u8,
    pub pwm_freq_pre_divider: u8,
    pub max: u32,
    /// bit-fields lsb_reg_used:1, aux_enable:1, aux_set:1, luminance_set:1
    pub _bitfields_8: [u8; 1],
}

#[repr(C)]
pub struct DrmDpPhyTestParams {
    pub link_rate: i32,
    pub num_lanes: u8,
    pub phy_pattern: u8,
    pub hbr2_reset: [u8; 2],
    pub custom80: [u8; 10],
    pub enhanced_frame_cap: bool,
}

const _: () = assert!(size_of::<DrmDpVscSdp>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, sdp_type) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, revision) == 1);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, length) == 2);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, pixelformat) == 4);
const _: () = assert!(size_of::<DpPixelformat>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, colorimetry) == 8);
const _: () = assert!(size_of::<DpColorimetry>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, bpc) == 12);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, dynamic_range) == 16);
const _: () = assert!(size_of::<DpDynamicRange>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpVscSdp, content_type) == 20);
const _: () = assert!(size_of::<DpContentType>() == 4);
const _: () = assert!(size_of::<DrmDpAsSdp>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, sdp_type) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, revision) == 1);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, length) == 2);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, vtotal) == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, target_rr) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, duration_incr_ms) == 12);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, duration_decr_ms) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, target_rr_divider) == 20);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, mode) == 24);
const _: () = assert!(size_of::<OperationMode>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpAsSdp, coasting_vtotal) == 28);
const _: () = assert!(size_of::<DrmDpAuxMsg>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxMsg, address) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxMsg, request) == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxMsg, reply) == 5);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxMsg, buffer) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxMsg, size) == 16);
const _: () = assert!(size_of::<DrmDpAuxCec>() == 128);
const _: () = assert!(align_of::<DrmDpAuxCec>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxCec, lock) == 0);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxCec, adap) == 24);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxCec, connector) == 32);
const _: () = assert!(core::mem::offset_of!(DrmDpAuxCec, unregister_work) == 40);
const _: () = assert!(size_of::<DelayedWork>() == 88);
const _: () = assert!(size_of::<DrmDpAux>() == 1320);
const _: () = assert!(align_of::<DrmDpAux>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, name) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, ddc) == 8);
const _: () = assert!(size_of::<I2cAdapter>() == 1064);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, dev) == 1072);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, drm_dev) == 1080);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, crtc) == 1088);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, hw_mutex) == 1096);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, crc_work) == 1120);
const _: () = assert!(size_of::<WorkStruct>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, crc_count) == 1152);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, transfer) == 1160);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, wait_hpd_asserted) == 1168);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, i2c_nack_count) == 1176);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, i2c_defer_count) == 1180);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, cec) == 1184);
const _: () = assert!(size_of::<DrmDpAuxCec>() == 128);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, is_remote) == 1312);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, powered_down) == 1313);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, no_zero_sized) == 1314);
const _: () = assert!(core::mem::offset_of!(DrmDpAux, dpcd_probe_disabled) == 1315);
const _: () = assert!(size_of::<DrmDpDpcdIdent>() == 12);
const _: () = assert!(core::mem::offset_of!(DrmDpDpcdIdent, oui) == 0);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () = assert!(core::mem::offset_of!(DrmDpDpcdIdent, device_id) == 3);
const _: () = assert!(size_of::<[u8; 6]>() == 6);
const _: () = assert!(core::mem::offset_of!(DrmDpDpcdIdent, hw_rev) == 9);
const _: () = assert!(core::mem::offset_of!(DrmDpDpcdIdent, sw_major_rev) == 10);
const _: () = assert!(core::mem::offset_of!(DrmDpDpcdIdent, sw_minor_rev) == 11);
const _: () = assert!(size_of::<DrmDpDesc>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDpDesc, ident) == 0);
const _: () = assert!(size_of::<DrmDpDpcdIdent>() == 12);
const _: () = assert!(core::mem::offset_of!(DrmDpDesc, quirks) == 12);
const _: () = assert!(size_of::<DrmEdpBacklightInfo>() == 12);
const _: () = assert!(core::mem::offset_of!(DrmEdpBacklightInfo, pwmgen_bit_count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEdpBacklightInfo, pwm_freq_pre_divider) == 1);
const _: () = assert!(core::mem::offset_of!(DrmEdpBacklightInfo, max) == 4);
const _: () = assert!(core::mem::offset_of!(DrmEdpBacklightInfo, _bitfields_8) == 8);
const _: () = assert!(size_of::<DrmDpPhyTestParams>() == 20);
const _: () = assert!(core::mem::offset_of!(DrmDpPhyTestParams, link_rate) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDpPhyTestParams, num_lanes) == 4);
const _: () = assert!(core::mem::offset_of!(DrmDpPhyTestParams, phy_pattern) == 5);
const _: () = assert!(core::mem::offset_of!(DrmDpPhyTestParams, hbr2_reset) == 6);
const _: () = assert!(size_of::<[u8; 2]>() == 2);
const _: () = assert!(core::mem::offset_of!(DrmDpPhyTestParams, custom80) == 8);
const _: () = assert!(size_of::<[u8; 10]>() == 10);
const _: () = assert!(core::mem::offset_of!(DrmDpPhyTestParams, enhanced_frame_cap) == 18);

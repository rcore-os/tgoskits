// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/uapi/drm/drm_mode.h`

use core::{
    ffi::c_char,
    mem::{ManuallyDrop, size_of},
};

#[repr(C)]
pub struct DrmModeModeinfo {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub r#type: u32,
    pub name: [c_char; 32],
}

#[repr(C)]
pub struct DrmModeCardRes {
    pub fb_id_ptr: u64,
    pub crtc_id_ptr: u64,
    pub connector_id_ptr: u64,
    pub encoder_id_ptr: u64,
    pub count_fbs: u32,
    pub count_crtcs: u32,
    pub count_connectors: u32,
    pub count_encoders: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
}

#[repr(C)]
pub struct DrmModeCrtc {
    pub set_connectors_ptr: u64,
    pub count_connectors: u32,
    pub crtc_id: u32,
    pub fb_id: u32,
    pub x: u32,
    pub y: u32,
    pub gamma_size: u32,
    pub mode_valid: u32,
    pub mode: DrmModeModeinfo,
}

#[repr(C)]
pub struct DrmModeSetPlane {
    pub plane_id: u32,
    pub crtc_id: u32,
    pub fb_id: u32,
    pub flags: u32,
    pub crtc_x: i32,
    pub crtc_y: i32,
    pub crtc_w: u32,
    pub crtc_h: u32,
    pub src_x: u32,
    pub src_y: u32,
    pub src_h: u32,
    pub src_w: u32,
}

#[repr(C)]
pub struct DrmModeGetPlane {
    pub plane_id: u32,
    pub crtc_id: u32,
    pub fb_id: u32,
    pub possible_crtcs: u32,
    pub gamma_size: u32,
    pub count_format_types: u32,
    pub format_type_ptr: u64,
}

#[repr(C)]
pub struct DrmModeGetPlaneRes {
    pub plane_id_ptr: u64,
    pub count_planes: u32,
}

#[repr(C)]
pub struct DrmModeGetEncoder {
    pub encoder_id: u32,
    pub encoder_type: u32,
    pub crtc_id: u32,
    pub possible_crtcs: u32,
    pub possible_clones: u32,
}

#[repr(C)]
pub struct DrmModeGetConnector {
    pub encoders_ptr: u64,
    pub modes_ptr: u64,
    pub props_ptr: u64,
    pub prop_values_ptr: u64,
    pub count_modes: u32,
    pub count_props: u32,
    pub count_encoders: u32,
    pub encoder_id: u32,
    pub connector_id: u32,
    pub connector_type: u32,
    pub connector_type_id: u32,
    pub connection: u32,
    pub mm_width: u32,
    pub mm_height: u32,
    pub subpixel: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct DrmModePropertyEnum {
    pub value: u64,
    pub name: [c_char; 32],
}

#[repr(C)]
pub struct DrmModeGetProperty {
    pub values_ptr: u64,
    pub enum_blob_ptr: u64,
    pub prop_id: u32,
    pub flags: u32,
    pub name: [c_char; 32],
    pub count_values: u32,
    pub count_enum_blobs: u32,
}

#[repr(C)]
pub struct DrmModeConnectorSetProperty {
    pub value: u64,
    pub prop_id: u32,
    pub connector_id: u32,
}

#[repr(C)]
pub struct DrmModeObjGetProperties {
    pub props_ptr: u64,
    pub prop_values_ptr: u64,
    pub count_props: u32,
    pub obj_id: u32,
    pub obj_type: u32,
}

#[repr(C)]
pub struct DrmModeObjSetProperty {
    pub value: u64,
    pub prop_id: u32,
    pub obj_id: u32,
    pub obj_type: u32,
}

#[repr(C)]
pub struct DrmModeGetBlob {
    pub blob_id: u32,
    pub length: u32,
    pub data: u64,
}

#[repr(C)]
pub struct DrmModeFbCmd {
    pub fb_id: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u32,
    pub depth: u32,
    pub handle: u32,
}

#[repr(C)]
pub struct DrmModeFbCmd2 {
    pub fb_id: u32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub flags: u32,
    pub handles: [u32; 4],
    pub pitches: [u32; 4],
    pub offsets: [u32; 4],
    pub modifier: [u64; 4],
}

#[repr(C)]
pub struct DrmModeFbDirtyCmd {
    pub fb_id: u32,
    pub flags: u32,
    pub color: u32,
    pub num_clips: u32,
    pub clips_ptr: u64,
}

#[repr(C)]
pub struct DrmModeModeCmd {
    pub connector_id: u32,
    pub mode: DrmModeModeinfo,
}

#[repr(C)]
pub struct DrmModeCursor {
    pub flags: u32,
    pub crtc_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub handle: u32,
}

#[repr(C)]
pub struct DrmModeCursor2 {
    pub flags: u32,
    pub crtc_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub handle: u32,
    pub hot_x: i32,
    pub hot_y: i32,
}

#[repr(C)]
pub struct DrmModeCrtcLut {
    pub crtc_id: u32,
    pub gamma_size: u32,
    pub red: u64,
    pub green: u64,
    pub blue: u64,
}

#[repr(C)]
pub struct DrmColorCtm {
    pub matrix: [u64; 9],
}

#[repr(C)]
pub struct DrmColorCtm3x4 {
    pub matrix: [u64; 12],
}

#[repr(C)]
pub struct DrmColorLut {
    pub red: u16,
    pub green: u16,
    pub blue: u16,
    pub reserved: u16,
}

#[repr(C)]
pub struct DrmColorLut32 {
    pub red: u32,
    pub green: u32,
    pub blue: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct DrmPlaneSizeHint {
    pub width: u16,
    pub height: u16,
}

#[repr(C)]
pub struct HdrMetadataInfoframeDisplayPrimaries {
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
pub struct HdrMetadataInfoframeWhitePoint {
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
pub struct HdrMetadataInfoframe {
    pub eotf: u8,
    pub metadata_type: u8,
    pub display_primaries: [HdrMetadataInfoframeDisplayPrimaries; 3],
    pub white_point: HdrMetadataInfoframeWhitePoint,
    pub max_display_mastering_luminance: u16,
    pub min_display_mastering_luminance: u16,
    pub max_cll: u16,
    pub max_fall: u16,
}

#[repr(C)]
pub union HdrOutputMetadataAnon4 {
    pub hdmi_metadata_type1: ManuallyDrop<HdrMetadataInfoframe>,
}

#[repr(C)]
pub struct HdrOutputMetadata {
    pub metadata_type: u32,
    pub _anon_4: HdrOutputMetadataAnon4,
}

#[repr(C)]
pub struct DrmModeCrtcPageFlip {
    pub crtc_id: u32,
    pub fb_id: u32,
    pub flags: u32,
    pub reserved: u32,
    pub user_data: u64,
}

#[repr(C)]
pub struct DrmModeCrtcPageFlipTarget {
    pub crtc_id: u32,
    pub fb_id: u32,
    pub flags: u32,
    pub sequence: u32,
    pub user_data: u64,
}

#[repr(C)]
pub struct DrmModeCreateDumb {
    pub height: u32,
    pub width: u32,
    pub bpp: u32,
    pub flags: u32,
    pub handle: u32,
    pub pitch: u32,
    pub size: u64,
}

#[repr(C)]
pub struct DrmModeMapDumb {
    pub handle: u32,
    pub pad: u32,
    pub offset: u64,
}

#[repr(C)]
pub struct DrmModeDestroyDumb {
    pub handle: u32,
}

#[repr(C)]
pub struct DrmModeAtomic {
    pub flags: u32,
    pub count_objs: u32,
    pub objs_ptr: u64,
    pub count_props_ptr: u64,
    pub props_ptr: u64,
    pub prop_values_ptr: u64,
    pub reserved: u64,
    pub user_data: u64,
}

#[repr(C)]
pub struct DrmFormatModifierBlob {
    pub version: u32,
    pub flags: u32,
    pub count_formats: u32,
    pub formats_offset: u32,
    pub count_modifiers: u32,
    pub modifiers_offset: u32,
}

#[repr(C)]
pub struct DrmFormatModifier {
    pub formats: u64,
    pub offset: u32,
    pub pad: u32,
    pub modifier: u64,
}

#[repr(C)]
pub struct DrmModeCreateBlob {
    pub data: u64,
    pub length: u32,
    pub blob_id: u32,
}

#[repr(C)]
pub struct DrmModeDestroyBlob {
    pub blob_id: u32,
}

#[repr(C)]
pub struct DrmModeCreateLease {
    pub object_ids: u64,
    pub object_count: u32,
    pub flags: u32,
    pub lessee_id: u32,
    pub fd: u32,
}

#[repr(C)]
pub struct DrmModeListLessees {
    pub count_lessees: u32,
    pub pad: u32,
    pub lessees_ptr: u64,
}

#[repr(C)]
pub struct DrmModeGetLease {
    pub count_objects: u32,
    pub pad: u32,
    pub objects_ptr: u64,
}

#[repr(C)]
pub struct DrmModeRevokeLease {
    pub lessee_id: u32,
}

#[repr(C)]
pub struct DrmModeRect {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

#[repr(C)]
pub struct DrmModeClosefb {
    pub fb_id: u32,
    pub pad: u32,
}

pub type DrmModeSubconnector = u32;
pub const DRM_MODE_SUBCONNECTOR_AUTOMATIC: DrmModeSubconnector = 0;
pub const DRM_MODE_SUBCONNECTOR_UNKNOWN: DrmModeSubconnector = 0;
pub const DRM_MODE_SUBCONNECTOR_VGA: DrmModeSubconnector = 1;
pub const DRM_MODE_SUBCONNECTOR_DVID: DrmModeSubconnector = 3;
pub const DRM_MODE_SUBCONNECTOR_DVIA: DrmModeSubconnector = 4;
pub const DRM_MODE_SUBCONNECTOR_COMPOSITE: DrmModeSubconnector = 5;
pub const DRM_MODE_SUBCONNECTOR_SVIDEO: DrmModeSubconnector = 6;
pub const DRM_MODE_SUBCONNECTOR_COMPONENT: DrmModeSubconnector = 8;
pub const DRM_MODE_SUBCONNECTOR_SCART: DrmModeSubconnector = 9;
pub const DRM_MODE_SUBCONNECTOR_DISPLAYPORT: DrmModeSubconnector = 10;
pub const DRM_MODE_SUBCONNECTOR_HDMIA: DrmModeSubconnector = 11;
pub const DRM_MODE_SUBCONNECTOR_NATIVE: DrmModeSubconnector = 15;
pub const DRM_MODE_SUBCONNECTOR_WIRELESS: DrmModeSubconnector = 18;

const _: () = assert!(size_of::<DrmModeModeinfo>() == 68);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, clock) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, hdisplay) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, hsync_start) == 6);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, hsync_end) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, htotal) == 10);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, hskew) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, vdisplay) == 14);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, vsync_start) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, vsync_end) == 18);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, vtotal) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, vscan) == 22);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, vrefresh) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, flags) == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, r#type) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeModeinfo, name) == 36);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(size_of::<DrmModeCardRes>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, fb_id_ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, crtc_id_ptr) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, connector_id_ptr) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, encoder_id_ptr) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, count_fbs) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, count_crtcs) == 36);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, count_connectors) == 40);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, count_encoders) == 44);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, min_width) == 48);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, max_width) == 52);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, min_height) == 56);
const _: () = assert!(core::mem::offset_of!(DrmModeCardRes, max_height) == 60);
const _: () = assert!(size_of::<DrmModeCrtc>() == 104);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, set_connectors_ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, count_connectors) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, crtc_id) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, fb_id) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, x) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, y) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, gamma_size) == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, mode_valid) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtc, mode) == 36);
const _: () = assert!(size_of::<DrmModeModeinfo>() == 68);
const _: () = assert!(size_of::<DrmModeSetPlane>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, plane_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, crtc_id) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, fb_id) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, flags) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, crtc_x) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, crtc_y) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, crtc_w) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, crtc_h) == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, src_x) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, src_y) == 36);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, src_h) == 40);
const _: () = assert!(core::mem::offset_of!(DrmModeSetPlane, src_w) == 44);
const _: () = assert!(size_of::<DrmModeGetPlane>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, plane_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, crtc_id) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, fb_id) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, possible_crtcs) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, gamma_size) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, count_format_types) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlane, format_type_ptr) == 24);
const _: () = assert!(size_of::<DrmModeGetPlaneRes>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlaneRes, plane_id_ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetPlaneRes, count_planes) == 8);
const _: () = assert!(size_of::<DrmModeGetEncoder>() == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeGetEncoder, encoder_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetEncoder, encoder_type) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeGetEncoder, crtc_id) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeGetEncoder, possible_crtcs) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeGetEncoder, possible_clones) == 16);
const _: () = assert!(size_of::<DrmModeGetConnector>() == 80);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, encoders_ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, modes_ptr) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, props_ptr) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, prop_values_ptr) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, count_modes) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, count_props) == 36);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, count_encoders) == 40);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, encoder_id) == 44);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, connector_id) == 48);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, connector_type) == 52);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, connector_type_id) == 56);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, connection) == 60);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, mm_width) == 64);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, mm_height) == 68);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, subpixel) == 72);
const _: () = assert!(core::mem::offset_of!(DrmModeGetConnector, pad) == 76);
const _: () = assert!(size_of::<DrmModePropertyEnum>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmModePropertyEnum, value) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModePropertyEnum, name) == 8);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(size_of::<DrmModeGetProperty>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, values_ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, enum_blob_ptr) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, prop_id) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, flags) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, name) == 24);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, count_values) == 56);
const _: () = assert!(core::mem::offset_of!(DrmModeGetProperty, count_enum_blobs) == 60);
const _: () = assert!(size_of::<DrmModeConnectorSetProperty>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConnectorSetProperty, value) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeConnectorSetProperty, prop_id) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeConnectorSetProperty, connector_id) == 12);
const _: () = assert!(size_of::<DrmModeObjGetProperties>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeObjGetProperties, props_ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeObjGetProperties, prop_values_ptr) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeObjGetProperties, count_props) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeObjGetProperties, obj_id) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeObjGetProperties, obj_type) == 24);
const _: () = assert!(size_of::<DrmModeObjSetProperty>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeObjSetProperty, value) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeObjSetProperty, prop_id) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeObjSetProperty, obj_id) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeObjSetProperty, obj_type) == 16);
const _: () = assert!(size_of::<DrmModeGetBlob>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeGetBlob, blob_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetBlob, length) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeGetBlob, data) == 8);
const _: () = assert!(size_of::<DrmModeFbCmd>() == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, fb_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, width) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, height) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, pitch) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, bpp) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, depth) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd, handle) == 24);
const _: () = assert!(size_of::<DrmModeFbCmd2>() == 104);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, fb_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, width) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, height) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, pixel_format) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, flags) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, handles) == 20);
const _: () = assert!(size_of::<[u32; 4]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, pitches) == 36);
const _: () = assert!(size_of::<[u32; 4]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, offsets) == 52);
const _: () = assert!(size_of::<[u32; 4]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeFbCmd2, modifier) == 72);
const _: () = assert!(size_of::<[u64; 4]>() == 32);
const _: () = assert!(size_of::<DrmModeFbDirtyCmd>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeFbDirtyCmd, fb_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeFbDirtyCmd, flags) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeFbDirtyCmd, color) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeFbDirtyCmd, num_clips) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeFbDirtyCmd, clips_ptr) == 16);
const _: () = assert!(size_of::<DrmModeModeCmd>() == 72);
const _: () = assert!(core::mem::offset_of!(DrmModeModeCmd, connector_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeModeCmd, mode) == 4);
const _: () = assert!(size_of::<DrmModeModeinfo>() == 68);
const _: () = assert!(size_of::<DrmModeCursor>() == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, flags) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, crtc_id) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, x) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, y) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, width) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, height) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor, handle) == 24);
const _: () = assert!(size_of::<DrmModeCursor2>() == 36);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, flags) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, crtc_id) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, x) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, y) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, width) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, height) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, handle) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, hot_x) == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeCursor2, hot_y) == 32);
const _: () = assert!(size_of::<DrmModeCrtcLut>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcLut, crtc_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcLut, gamma_size) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcLut, red) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcLut, green) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcLut, blue) == 24);
const _: () = assert!(size_of::<DrmColorCtm>() == 72);
const _: () = assert!(core::mem::offset_of!(DrmColorCtm, matrix) == 0);
const _: () = assert!(size_of::<[u64; 9]>() == 72);
const _: () = assert!(size_of::<DrmColorCtm3x4>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmColorCtm3x4, matrix) == 0);
const _: () = assert!(size_of::<[u64; 12]>() == 96);
const _: () = assert!(size_of::<DrmColorLut>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmColorLut, red) == 0);
const _: () = assert!(core::mem::offset_of!(DrmColorLut, green) == 2);
const _: () = assert!(core::mem::offset_of!(DrmColorLut, blue) == 4);
const _: () = assert!(core::mem::offset_of!(DrmColorLut, reserved) == 6);
const _: () = assert!(size_of::<DrmColorLut32>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmColorLut32, red) == 0);
const _: () = assert!(core::mem::offset_of!(DrmColorLut32, green) == 4);
const _: () = assert!(core::mem::offset_of!(DrmColorLut32, blue) == 8);
const _: () = assert!(core::mem::offset_of!(DrmColorLut32, reserved) == 12);
const _: () = assert!(size_of::<DrmPlaneSizeHint>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmPlaneSizeHint, width) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPlaneSizeHint, height) == 2);
const _: () = assert!(size_of::<HdrMetadataInfoframeDisplayPrimaries>() == 4);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframeDisplayPrimaries, x) == 0);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframeDisplayPrimaries, y) == 2);
const _: () = assert!(size_of::<HdrMetadataInfoframeWhitePoint>() == 4);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframeWhitePoint, x) == 0);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframeWhitePoint, y) == 2);
const _: () = assert!(size_of::<HdrMetadataInfoframe>() == 26);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframe, eotf) == 0);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframe, metadata_type) == 1);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframe, display_primaries) == 2);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframe, white_point) == 14);
const _: () =
    assert!(core::mem::offset_of!(HdrMetadataInfoframe, max_display_mastering_luminance) == 18);
const _: () =
    assert!(core::mem::offset_of!(HdrMetadataInfoframe, min_display_mastering_luminance) == 20);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframe, max_cll) == 22);
const _: () = assert!(core::mem::offset_of!(HdrMetadataInfoframe, max_fall) == 24);
const _: () = assert!(size_of::<HdrOutputMetadataAnon4>() == 26);
const _: () = assert!(size_of::<HdrOutputMetadata>() == 32);
const _: () = assert!(core::mem::offset_of!(HdrOutputMetadata, metadata_type) == 0);
const _: () = assert!(core::mem::offset_of!(HdrOutputMetadata, _anon_4) == 4);
const _: () = assert!(size_of::<DrmModeCrtcPageFlip>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlip, crtc_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlip, fb_id) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlip, flags) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlip, reserved) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlip, user_data) == 16);
const _: () = assert!(size_of::<DrmModeCrtcPageFlipTarget>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlipTarget, crtc_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlipTarget, fb_id) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlipTarget, flags) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlipTarget, sequence) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCrtcPageFlipTarget, user_data) == 16);
const _: () = assert!(size_of::<DrmModeCreateDumb>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, height) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, width) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, bpp) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, flags) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, handle) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, pitch) == 20);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateDumb, size) == 24);
const _: () = assert!(size_of::<DrmModeMapDumb>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeMapDumb, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeMapDumb, pad) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeMapDumb, offset) == 8);
const _: () = assert!(size_of::<DrmModeDestroyDumb>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeDestroyDumb, handle) == 0);
const _: () = assert!(size_of::<DrmModeAtomic>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, flags) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, count_objs) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, objs_ptr) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, count_props_ptr) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, props_ptr) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, prop_values_ptr) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, reserved) == 40);
const _: () = assert!(core::mem::offset_of!(DrmModeAtomic, user_data) == 48);
const _: () = assert!(size_of::<DrmFormatModifierBlob>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifierBlob, version) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifierBlob, flags) == 4);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifierBlob, count_formats) == 8);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifierBlob, formats_offset) == 12);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifierBlob, count_modifiers) == 16);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifierBlob, modifiers_offset) == 20);
const _: () = assert!(size_of::<DrmFormatModifier>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifier, formats) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifier, offset) == 8);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifier, pad) == 12);
const _: () = assert!(core::mem::offset_of!(DrmFormatModifier, modifier) == 16);
const _: () = assert!(size_of::<DrmModeCreateBlob>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateBlob, data) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateBlob, length) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateBlob, blob_id) == 12);
const _: () = assert!(size_of::<DrmModeDestroyBlob>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeDestroyBlob, blob_id) == 0);
const _: () = assert!(size_of::<DrmModeCreateLease>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateLease, object_ids) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateLease, object_count) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateLease, flags) == 12);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateLease, lessee_id) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeCreateLease, fd) == 20);
const _: () = assert!(size_of::<DrmModeListLessees>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeListLessees, count_lessees) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeListLessees, pad) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeListLessees, lessees_ptr) == 8);
const _: () = assert!(size_of::<DrmModeGetLease>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeGetLease, count_objects) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeGetLease, pad) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeGetLease, objects_ptr) == 8);
const _: () = assert!(size_of::<DrmModeRevokeLease>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeRevokeLease, lessee_id) == 0);
const _: () = assert!(size_of::<DrmModeRect>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeRect, x1) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeRect, y1) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeRect, x2) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeRect, y2) == 12);
const _: () = assert!(size_of::<DrmModeClosefb>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeClosefb, fb_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeClosefb, pad) == 4);

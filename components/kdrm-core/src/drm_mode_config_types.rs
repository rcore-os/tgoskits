// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_mode_config.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::mem::{align_of, size_of};

use drm_kapi::{
    idr::{Ida, Idr},
    types::{DelayedWork, ListHead, LlistHead, Mutex, Spinlock, WorkStruct},
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_fourcc_types::DrmFormatInfo,
    drm_framebuffer_types::DrmFramebuffer,
    drm_modes_types::{DrmDisplayMode, DrmModeStatus},
    drm_modeset_helper_vtables_types::DrmModeConfigHelperFuncs,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_property_types::DrmProperty,
    uapi_drm_mode_types::DrmModeFbCmd2,
};

#[repr(C)]
pub struct DrmModeConfigFuncs {
    pub fb_create: Option<
        unsafe extern "C" fn(
            *mut DrmDevice,
            *mut DrmFile,
            *mut DrmFormatInfo,
            *mut DrmModeFbCmd2,
        ) -> *mut DrmFramebuffer,
    >,
    pub get_format_info: Option<unsafe extern "C" fn(u32, u64) -> *mut DrmFormatInfo>,
    pub mode_valid:
        Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmDisplayMode) -> DrmModeStatus>,
    pub atomic_check: Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmAtomicCommit) -> i32>,
    pub atomic_commit:
        Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmAtomicCommit, bool) -> i32>,
    pub atomic_state_alloc: Option<unsafe extern "C" fn(*mut DrmDevice) -> *mut DrmAtomicCommit>,
    pub atomic_state_clear: Option<unsafe extern "C" fn(*mut DrmAtomicCommit)>,
    pub atomic_state_free: Option<unsafe extern "C" fn(*mut DrmAtomicCommit)>,
}

#[repr(C, align(8))]
pub struct DrmModeConfig {
    pub mutex: Mutex,
    pub connection_mutex: DrmModesetLock,
    pub acquire_ctx: *mut DrmModesetAcquireCtx,
    pub idr_mutex: Mutex,
    pub object_idr: Idr,
    pub tile_idr: Idr,
    pub fb_lock: Mutex,
    pub num_fb: i32,
    pub fb_list: ListHead,
    pub connector_list_lock: Spinlock,
    pub num_connector: i32,
    pub connector_ida: Ida,
    pub connector_list: ListHead,
    pub connector_free_list: LlistHead,
    pub connector_free_work: WorkStruct,
    pub num_encoder: i32,
    pub encoder_list: ListHead,
    pub num_total_plane: i32,
    pub plane_list: ListHead,
    pub panic_lock: Spinlock,
    pub num_colorop: i32,
    pub colorop_list: ListHead,
    pub num_crtc: i32,
    pub crtc_list: ListHead,
    pub property_list: ListHead,
    pub privobj_list: ListHead,
    pub min_width: u32,
    pub min_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub funcs: *mut DrmModeConfigFuncs,
    pub poll_enabled: bool,
    pub poll_running: bool,
    pub delayed_event: bool,
    pub output_poll_work: DelayedWork,
    pub blob_lock: Mutex,
    pub property_blob_list: ListHead,
    pub edid_property: *mut DrmProperty,
    pub dpms_property: *mut DrmProperty,
    pub path_property: *mut DrmProperty,
    pub tile_property: *mut DrmProperty,
    pub panel_type_property: *mut DrmProperty,
    pub link_status_property: *mut DrmProperty,
    pub plane_type_property: *mut DrmProperty,
    pub prop_src_x: *mut DrmProperty,
    pub prop_src_y: *mut DrmProperty,
    pub prop_src_w: *mut DrmProperty,
    pub prop_src_h: *mut DrmProperty,
    pub prop_crtc_x: *mut DrmProperty,
    pub prop_crtc_y: *mut DrmProperty,
    pub prop_crtc_w: *mut DrmProperty,
    pub prop_crtc_h: *mut DrmProperty,
    pub prop_fb_id: *mut DrmProperty,
    pub prop_in_fence_fd: *mut DrmProperty,
    pub prop_out_fence_ptr: *mut DrmProperty,
    pub prop_crtc_id: *mut DrmProperty,
    pub prop_fb_damage_clips: *mut DrmProperty,
    pub prop_active: *mut DrmProperty,
    pub prop_mode_id: *mut DrmProperty,
    pub prop_vrr_enabled: *mut DrmProperty,
    pub dvi_i_subconnector_property: *mut DrmProperty,
    pub dvi_i_select_subconnector_property: *mut DrmProperty,
    pub dp_subconnector_property: *mut DrmProperty,
    pub tv_subconnector_property: *mut DrmProperty,
    pub tv_select_subconnector_property: *mut DrmProperty,
    pub legacy_tv_mode_property: *mut DrmProperty,
    pub tv_mode_property: *mut DrmProperty,
    pub tv_left_margin_property: *mut DrmProperty,
    pub tv_right_margin_property: *mut DrmProperty,
    pub tv_top_margin_property: *mut DrmProperty,
    pub tv_bottom_margin_property: *mut DrmProperty,
    pub tv_brightness_property: *mut DrmProperty,
    pub tv_contrast_property: *mut DrmProperty,
    pub tv_flicker_reduction_property: *mut DrmProperty,
    pub tv_overscan_property: *mut DrmProperty,
    pub tv_saturation_property: *mut DrmProperty,
    pub tv_hue_property: *mut DrmProperty,
    pub scaling_mode_property: *mut DrmProperty,
    pub aspect_ratio_property: *mut DrmProperty,
    pub content_type_property: *mut DrmProperty,
    pub degamma_lut_property: *mut DrmProperty,
    pub degamma_lut_size_property: *mut DrmProperty,
    pub ctm_property: *mut DrmProperty,
    pub gamma_lut_property: *mut DrmProperty,
    pub gamma_lut_size_property: *mut DrmProperty,
    pub background_color_property: *mut DrmProperty,
    pub suggested_x_property: *mut DrmProperty,
    pub suggested_y_property: *mut DrmProperty,
    pub non_desktop_property: *mut DrmProperty,
    pub panel_orientation_property: *mut DrmProperty,
    pub writeback_fb_id_property: *mut DrmProperty,
    pub writeback_pixel_formats_property: *mut DrmProperty,
    pub writeback_out_fence_ptr_property: *mut DrmProperty,
    pub hdr_output_metadata_property: *mut DrmProperty,
    pub content_protection_property: *mut DrmProperty,
    pub hdcp_content_type_property: *mut DrmProperty,
    pub preferred_depth: u32,
    pub prefer_shadow: u32,
    pub quirk_addfb_prefer_xbgr_30bpp: bool,
    pub quirk_addfb_prefer_host_byte_order: bool,
    pub async_page_flip: bool,
    pub fb_modifiers_not_supported: bool,
    pub normalize_zpos: bool,
    pub modifiers_property: *mut DrmProperty,
    pub async_modifiers_property: *mut DrmProperty,
    pub size_hints_property: *mut DrmProperty,
    pub cursor_width: u32,
    pub cursor_height: u32,
    pub suspend_state: *mut DrmAtomicCommit,
    pub helper_private: *mut DrmModeConfigHelperFuncs,
}

const _: () = assert!(size_of::<DrmModeConfigFuncs>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, fb_create) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, get_format_info) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, mode_valid) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, atomic_check) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, atomic_commit) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, atomic_state_alloc) == 40);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, atomic_state_clear) == 48);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigFuncs, atomic_state_free) == 56);
const _: () = assert!(size_of::<DrmModeConfig>() == 1104);
const _: () = assert!(align_of::<DrmModeConfig>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, mutex) == 0);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, connection_mutex) == 24);
const _: () = assert!(size_of::<DrmModesetLock>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, acquire_ctx) == 72);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, idr_mutex) == 80);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, object_idr) == 104);
const _: () = assert!(size_of::<Idr>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tile_idr) == 128);
const _: () = assert!(size_of::<Idr>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, fb_lock) == 152);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, num_fb) == 176);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, fb_list) == 184);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, connector_list_lock) == 200);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, num_connector) == 204);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, connector_ida) == 208);
const _: () = assert!(size_of::<Ida>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, connector_list) == 224);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, connector_free_list) == 240);
const _: () = assert!(size_of::<LlistHead>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, connector_free_work) == 248);
const _: () = assert!(size_of::<WorkStruct>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, num_encoder) == 280);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, encoder_list) == 288);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, num_total_plane) == 304);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, plane_list) == 312);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, panic_lock) == 328);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, num_colorop) == 332);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, colorop_list) == 336);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, num_crtc) == 352);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, crtc_list) == 360);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, property_list) == 376);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, privobj_list) == 392);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, min_width) == 408);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, min_height) == 412);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, max_width) == 416);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, max_height) == 420);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, funcs) == 424);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, poll_enabled) == 432);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, poll_running) == 433);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, delayed_event) == 434);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, output_poll_work) == 440);
const _: () = assert!(size_of::<DelayedWork>() == 88);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, blob_lock) == 528);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, property_blob_list) == 552);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, edid_property) == 568);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, dpms_property) == 576);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, path_property) == 584);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tile_property) == 592);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, panel_type_property) == 600);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, link_status_property) == 608);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, plane_type_property) == 616);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_src_x) == 624);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_src_y) == 632);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_src_w) == 640);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_src_h) == 648);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_crtc_x) == 656);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_crtc_y) == 664);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_crtc_w) == 672);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_crtc_h) == 680);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_fb_id) == 688);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_in_fence_fd) == 696);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_out_fence_ptr) == 704);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_crtc_id) == 712);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_fb_damage_clips) == 720);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_active) == 728);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_mode_id) == 736);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prop_vrr_enabled) == 744);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, dvi_i_subconnector_property) == 752);
const _: () =
    assert!(core::mem::offset_of!(DrmModeConfig, dvi_i_select_subconnector_property) == 760);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, dp_subconnector_property) == 768);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_subconnector_property) == 776);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_select_subconnector_property) == 784);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, legacy_tv_mode_property) == 792);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_mode_property) == 800);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_left_margin_property) == 808);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_right_margin_property) == 816);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_top_margin_property) == 824);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_bottom_margin_property) == 832);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_brightness_property) == 840);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_contrast_property) == 848);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_flicker_reduction_property) == 856);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_overscan_property) == 864);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_saturation_property) == 872);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, tv_hue_property) == 880);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, scaling_mode_property) == 888);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, aspect_ratio_property) == 896);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, content_type_property) == 904);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, degamma_lut_property) == 912);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, degamma_lut_size_property) == 920);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, ctm_property) == 928);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, gamma_lut_property) == 936);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, gamma_lut_size_property) == 944);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, background_color_property) == 952);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, suggested_x_property) == 960);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, suggested_y_property) == 968);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, non_desktop_property) == 976);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, panel_orientation_property) == 984);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, writeback_fb_id_property) == 992);
const _: () =
    assert!(core::mem::offset_of!(DrmModeConfig, writeback_pixel_formats_property) == 1000);
const _: () =
    assert!(core::mem::offset_of!(DrmModeConfig, writeback_out_fence_ptr_property) == 1008);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, hdr_output_metadata_property) == 1016);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, content_protection_property) == 1024);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, hdcp_content_type_property) == 1032);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, preferred_depth) == 1040);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, prefer_shadow) == 1044);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, quirk_addfb_prefer_xbgr_30bpp) == 1048);
const _: () =
    assert!(core::mem::offset_of!(DrmModeConfig, quirk_addfb_prefer_host_byte_order) == 1049);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, async_page_flip) == 1050);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, fb_modifiers_not_supported) == 1051);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, normalize_zpos) == 1052);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, modifiers_property) == 1056);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, async_modifiers_property) == 1064);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, size_hints_property) == 1072);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, cursor_width) == 1080);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, cursor_height) == 1084);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, suspend_state) == 1088);
const _: () = assert!(core::mem::offset_of!(DrmModeConfig, helper_private) == 1096);

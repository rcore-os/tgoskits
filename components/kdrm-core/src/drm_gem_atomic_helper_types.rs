// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_gem_atomic_helper.h`

use core::{ffi::c_void, mem::size_of};

use crate::{
    drm_format_helper_types::DrmFormatConvState, drm_plane_types::DrmPlaneState,
    iosys_map_types::IosysMap,
};

#[repr(C)]
pub struct DrmShadowPlaneState {
    pub base: DrmPlaneState,
    pub fmtcnv_state: DrmFormatConvState,
    pub map: [IosysMap; 4],
    pub data: [IosysMap; 4],
}

const _: () = assert!(size_of::<DrmShadowPlaneState>() == 336);
const _: () = assert!(core::mem::offset_of!(DrmShadowPlaneState, base) == 0);
const _: () = assert!(size_of::<DrmPlaneState>() == 184);
const _: () = assert!(core::mem::offset_of!(DrmShadowPlaneState, fmtcnv_state) == 184);
const _: () = assert!(size_of::<DrmFormatConvState>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmShadowPlaneState, map) == 208);
const _: () = assert!(size_of::<[IosysMap; 4]>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmShadowPlaneState, data) == 272);
const _: () = assert!(size_of::<[IosysMap; 4]>() == 64);

use crate::{
    drm_connector_types::DrmConnector, drm_crtc_types::DrmCrtc, drm_encoder_types::DrmEncoder,
    drm_plane_types::DrmPlane,
};

#[repr(C)]
pub struct DrmSimpleDisplayPipe {
    pub crtc: DrmCrtc,
    pub plane: DrmPlane,
    pub encoder: DrmEncoder,
    pub connector: *mut DrmConnector,
    pub funcs: *const c_void,
}

const _: () = assert!(size_of::<DrmSimpleDisplayPipe>() == 3168);
const _: () = assert!(core::mem::offset_of!(DrmSimpleDisplayPipe, crtc) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSimpleDisplayPipe, plane) == 1648);
const _: () = assert!(core::mem::offset_of!(DrmSimpleDisplayPipe, encoder) == 3000);
const _: () = assert!(core::mem::offset_of!(DrmSimpleDisplayPipe, connector) == 3152);
const _: () = assert!(core::mem::offset_of!(DrmSimpleDisplayPipe, funcs) == 3160);

// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux v7.2.3 include/drm/drm_simple_kms_helper.h. Field order and callbacks
// are transcribed from the source header rather than represented as opaque
// storage, so driver callback ABI remains inspectable and type checked.
//! Linux simple-KMS display-pipe callback and object definitions.

use core::{
    ffi::c_void,
    mem::{align_of, offset_of, size_of},
};

use crate::{
    drm_connector_types::DrmConnector,
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_encoder_types::DrmEncoder,
    drm_modes_types::{DrmDisplayMode, DrmModeStatus},
    drm_plane_types::{DrmPlane, DrmPlaneState},
};

#[repr(C)]
pub struct DrmSimpleDisplayPipeFuncs {
    pub mode_valid: Option<
        unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *const DrmDisplayMode) -> DrmModeStatus,
    >,
    pub enable: Option<
        unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmCrtcState, *mut DrmPlaneState),
    >,
    pub disable: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe)>,
    pub check: Option<
        unsafe extern "C" fn(
            *mut DrmSimpleDisplayPipe,
            *mut DrmPlaneState,
            *mut DrmCrtcState,
        ) -> i32,
    >,
    pub update: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmPlaneState)>,
    pub prepare_fb:
        Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmPlaneState) -> i32>,
    pub cleanup_fb: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmPlaneState)>,
    pub begin_fb_access:
        Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmPlaneState) -> i32>,
    pub end_fb_access: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmPlaneState)>,
    pub enable_vblank: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe) -> i32>,
    pub disable_vblank: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe)>,
    pub reset_crtc: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe)>,
    pub duplicate_crtc_state:
        Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe) -> *mut DrmCrtcState>,
    pub destroy_crtc_state:
        Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmCrtcState)>,
    pub reset_plane: Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe)>,
    pub duplicate_plane_state:
        Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe) -> *mut DrmPlaneState>,
    pub destroy_plane_state:
        Option<unsafe extern "C" fn(*mut DrmSimpleDisplayPipe, *mut DrmPlaneState)>,
}

#[repr(C, align(8))]
pub struct DrmSimpleDisplayPipe {
    pub crtc: DrmCrtc,
    pub plane: DrmPlane,
    pub encoder: DrmEncoder,
    pub connector: *mut DrmConnector,
    pub funcs: *const DrmSimpleDisplayPipeFuncs,
}

const _: () = assert!(size_of::<DrmSimpleDisplayPipeFuncs>() == 17 * size_of::<usize>());
const _: () = assert!(align_of::<DrmSimpleDisplayPipe>() == 8);
const _: () = assert!(offset_of!(DrmSimpleDisplayPipe, crtc) == 0);
const _: () = assert!(offset_of!(DrmSimpleDisplayPipe, plane) == size_of::<DrmCrtc>());
const _: () = assert!(
    offset_of!(DrmSimpleDisplayPipe, encoder) == size_of::<DrmCrtc>() + size_of::<DrmPlane>()
);
const _: () = assert!(
    offset_of!(DrmSimpleDisplayPipe, connector)
        == size_of::<DrmCrtc>() + size_of::<DrmPlane>() + size_of::<DrmEncoder>()
);
const _: () = assert!(
    offset_of!(DrmSimpleDisplayPipe, funcs)
        == offset_of!(DrmSimpleDisplayPipe, connector) + size_of::<*mut c_void>()
);

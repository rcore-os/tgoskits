// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_connector.h`

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::{
    gem_memory::NotifierBlock,
    types::{Kref, ListHead, LlistNode, Mutex},
};

use crate::{
    drm_atomic_types::{DrmAtomicCommit, DrmCrtcCommit},
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_mode_object_types::{DrmModeObject, DrmObjectProperties},
    drm_modes_types::{DrmDisplayMode, DrmModeStatus},
    drm_modeset_helper_vtables_types::DrmConnectorHelperFuncs,
    drm_print_types::DrmPrinter,
    drm_property_types::{DrmProperty, DrmPropertyBlob},
    hdmi_types::{HdmiInfoframe, HdmiPictureAspect, HdrSinkMetadata},
    uapi_drm_mode_types::DrmModeSubconnector,
};

#[repr(C)]
pub struct DrmScrambling {
    pub supported: bool,
    pub low_rates: bool,
}

#[repr(C)]
pub struct DrmScdc {
    pub supported: bool,
    pub read_request: bool,
    pub scrambling: DrmScrambling,
}

#[repr(C)]
pub struct DrmHdmiDscCap {
    pub v_1p2: bool,
    pub native_420: bool,
    pub all_bpp: bool,
    pub bpc_supported: u8,
    pub max_slices: u8,
    pub clk_per_slice: i32,
    pub max_lanes: u8,
    pub max_frl_rate_per_lane: u8,
    pub total_chunk_kbytes: u8,
}

#[repr(C)]
pub struct DrmHdmiInfo {
    pub scdc: DrmScdc,
    pub y420_vdb_modes: [u64; 4],
    pub y420_cmdb_modes: [u64; 4],
    pub y420_dc_modes: u8,
    pub max_frl_rate_per_lane: u8,
    pub max_lanes: u8,
    pub dsc_cap: DrmHdmiDscCap,
}

#[repr(C)]
pub struct DrmMonitorRangeInfo {
    pub min_vfreq: u16,
    pub max_vfreq: u16,
}

#[repr(C)]
pub struct DrmLuminanceRangeInfo {
    pub min_luminance: u32,
    pub max_luminance: u32,
}

#[repr(C)]
pub struct DrmAmdVsdbInfo {
    pub version: u8,
    pub replay_mode: bool,
    pub panel_type: u8,
    pub luminance_range1: DrmLuminanceRangeInfo,
    pub luminance_range2: DrmLuminanceRangeInfo,
}

#[repr(C)]
pub struct DrmDisplayInfo {
    pub width_mm: u32,
    pub height_mm: u32,
    pub bpc: u32,
    pub subpixel_order: SubpixelOrder,
    pub panel_orientation: i32,
    pub color_formats: u32,
    pub bus_formats: *mut u32,
    pub num_bus_formats: u32,
    pub bus_flags: u32,
    pub max_tmds_clock: i32,
    pub dvi_dual: bool,
    pub is_hdmi: bool,
    pub has_audio: bool,
    pub has_hdmi_infoframe: bool,
    pub rgb_quant_range_selectable: bool,
    pub edid_hdmi_rgb444_dc_modes: u8,
    pub edid_hdmi_ycbcr444_dc_modes: u8,
    pub cea_rev: u8,
    pub hdmi: DrmHdmiInfo,
    pub hdr_sink_metadata: HdrSinkMetadata,
    pub non_desktop: bool,
    pub monitor_range: DrmMonitorRangeInfo,
    pub luminance_range: DrmLuminanceRangeInfo,
    pub mso_stream_count: u8,
    pub mso_pixel_overlap: u8,
    pub max_dsc_bpp: u32,
    pub vics: *mut u8,
    pub vics_len: i32,
    pub quirks: u32,
    pub source_physical_address: u16,
    pub amd_vsdb: DrmAmdVsdbInfo,
}

#[repr(C)]
pub struct DrmConnectorTvMargins {
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
    pub top: u32,
}

#[repr(C)]
pub struct DrmTvConnectorState {
    pub select_subconnector: DrmModeSubconnector,
    pub subconnector: DrmModeSubconnector,
    pub margins: DrmConnectorTvMargins,
    pub legacy_mode: u32,
    pub mode: u32,
    pub brightness: u32,
    pub contrast: u32,
    pub flicker_reduction: u32,
    pub overscan: u32,
    pub saturation: u32,
    pub hue: u32,
}

#[repr(C)]
pub struct DrmConnectorHdmiInfoframe {
    pub data: HdmiInfoframe,
    pub set: bool,
}

#[repr(C)]
pub struct DrmConnectorHdmiStateInfoframes {
    pub avi: DrmConnectorHdmiInfoframe,
    pub hdr_drm: DrmConnectorHdmiInfoframe,
    pub spd: DrmConnectorHdmiInfoframe,
    pub hdmi: DrmConnectorHdmiInfoframe,
}

#[repr(C)]
pub struct DrmConnectorHdmiState {
    pub broadcast_rgb: DrmHdmiBroadcastRgb,
    pub infoframes: DrmConnectorHdmiStateInfoframes,
    pub is_limited_range: bool,
    pub output_bpc: u32,
    pub output_format: DrmOutputColorFormat,
    pub tmds_char_rate: u64,
}

#[repr(C)]
pub struct DrmConnectorState {
    pub connector: *mut DrmConnector,
    pub crtc: *mut DrmCrtc,
    pub best_encoder: *mut DrmEncoder,
    pub link_status: DrmLinkStatus,
    pub state: *mut DrmAtomicCommit,
    pub commit: *mut DrmCrtcCommit,
    pub tv: DrmTvConnectorState,
    pub self_refresh_aware: bool,
    pub picture_aspect_ratio: HdmiPictureAspect,
    pub content_type: u32,
    pub hdcp_content_type: u32,
    pub scaling_mode: u32,
    pub content_protection: u32,
    pub colorspace: DrmColorspace,
    pub writeback_job: *mut c_void,
    pub max_requested_bpc: u8,
    pub max_bpc: u8,
    pub privacy_screen_sw_state: DrmPrivacyScreenStatus,
    pub hdr_output_metadata: *mut DrmPropertyBlob,
    pub hdmi: DrmConnectorHdmiState,
}

#[repr(C)]
pub struct DrmConnectorHdmiAudioFuncs {
    pub startup: Option<unsafe extern "C" fn(*mut DrmConnector) -> i32>,
    pub prepare: Option<unsafe extern "C" fn(*mut DrmConnector, *mut c_void, *mut c_void) -> i32>,
    pub shutdown: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub mute_stream: Option<unsafe extern "C" fn(*mut DrmConnector, bool, i32) -> i32>,
}

#[repr(C)]
pub struct DrmConnectorCecFuncs {
    pub phys_addr_invalidate: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub phys_addr_set: Option<unsafe extern "C" fn(*mut DrmConnector, u16)>,
}

#[repr(C)]
pub struct DrmConnectorInfoframeFuncs {
    pub clear_infoframe: Option<unsafe extern "C" fn(*mut DrmConnector) -> i32>,
    pub write_infoframe: Option<unsafe extern "C" fn(*mut DrmConnector, *mut u8, usize) -> i32>,
}

#[repr(C)]
pub struct DrmConnectorHdmiFuncs {
    pub tmds_char_rate_valid:
        Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmDisplayMode, u64) -> DrmModeStatus>,
    pub read_edid: Option<unsafe extern "C" fn(*mut DrmConnector) -> *mut c_void>,
    pub avi: DrmConnectorInfoframeFuncs,
    pub hdmi: DrmConnectorInfoframeFuncs,
    pub audio: DrmConnectorInfoframeFuncs,
    pub hdr_drm: DrmConnectorInfoframeFuncs,
    pub spd: DrmConnectorInfoframeFuncs,
}

#[repr(C)]
pub struct DrmConnectorFuncs {
    pub dpms: Option<unsafe extern "C" fn(*mut DrmConnector, i32) -> i32>,
    pub reset: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub detect: Option<unsafe extern "C" fn(*mut DrmConnector, bool) -> DrmConnectorStatus>,
    pub force: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub fill_modes: Option<unsafe extern "C" fn(*mut DrmConnector, u32, u32) -> i32>,
    pub set_property: Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmProperty, u64) -> i32>,
    pub late_register: Option<unsafe extern "C" fn(*mut DrmConnector) -> i32>,
    pub early_unregister: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub destroy: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub atomic_duplicate_state:
        Option<unsafe extern "C" fn(*mut DrmConnector) -> *mut DrmConnectorState>,
    pub atomic_destroy_state:
        Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmConnectorState)>,
    pub atomic_set_property: Option<
        unsafe extern "C" fn(
            *mut DrmConnector,
            *mut DrmConnectorState,
            *mut DrmProperty,
            u64,
        ) -> i32,
    >,
    pub atomic_get_property: Option<
        unsafe extern "C" fn(
            *mut DrmConnector,
            *mut DrmConnectorState,
            *mut DrmProperty,
            *mut u64,
        ) -> i32,
    >,
    pub atomic_print_state: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut DrmConnectorState)>,
    pub oob_hotplug_event: Option<unsafe extern "C" fn(*mut DrmConnector, DrmConnectorStatus)>,
    pub debugfs_init: Option<unsafe extern "C" fn(*mut DrmConnector, *mut c_void)>,
}

#[repr(C)]
pub struct DrmCmdlineMode {
    pub name: [c_char; 32],
    pub specified: bool,
    pub refresh_specified: bool,
    pub bpp_specified: bool,
    pub pixel_clock: u32,
    pub xres: i32,
    pub yres: i32,
    pub bpp: i32,
    pub refresh: i32,
    pub rb: bool,
    pub interlace: bool,
    pub cvt: bool,
    pub margins: bool,
    pub force: DrmConnectorForce,
    pub rotation_reflection: u32,
    pub panel_orientation: DrmPanelOrientation,
    pub tv_margins: DrmConnectorTvMargins,
    pub tv_mode: DrmConnectorTvMode,
    pub tv_mode_specified: bool,
}

#[repr(C, align(8))]
pub struct DrmConnectorHdmiAudio {
    pub funcs: *mut DrmConnectorHdmiAudioFuncs,
    pub codec_pdev: *mut c_void,
    pub lock: Mutex,
    pub plugged_cb: Option<unsafe extern "C" fn(*mut c_void, bool)>,
    pub plugged_cb_dev: *mut c_void,
    pub last_state: bool,
    pub dai_port: i32,
}

#[repr(C, align(8))]
pub struct DrmConnectorHdmiInfoframes {
    pub lock: Mutex,
    pub audio: DrmConnectorHdmiInfoframe,
}

#[repr(C, align(8))]
pub struct DrmConnectorHdmi {
    pub vendor: [u8; 8],
    pub product: [u8; 16],
    pub supported_formats: u64,
    pub funcs: *mut DrmConnectorHdmiFuncs,
    pub infoframes: DrmConnectorHdmiInfoframes,
}

#[repr(C, align(8))]
pub struct DrmConnectorCec {
    pub mutex: Mutex,
    pub funcs: *mut DrmConnectorCecFuncs,
    pub data: *mut c_void,
}

#[repr(C, align(8))]
pub struct DrmConnector {
    pub dev: *mut DrmDevice,
    pub kdev: *mut c_void,
    pub attr: *mut c_void,
    pub fwnode: *mut c_void,
    pub head: ListHead,
    pub global_connector_list_entry: ListHead,
    pub base: DrmModeObject,
    pub name: *mut c_char,
    pub mutex: Mutex,
    pub index: u32,
    pub connector_type: i32,
    pub connector_type_id: i32,
    pub interlace_allowed: bool,
    pub doublescan_allowed: bool,
    pub stereo_allowed: bool,
    pub ycbcr_420_allowed: bool,
    pub registration_state: DrmConnectorRegistrationState,
    pub modes: ListHead,
    pub status: DrmConnectorStatus,
    pub probed_modes: ListHead,
    pub display_info: DrmDisplayInfo,
    pub funcs: *mut DrmConnectorFuncs,
    pub edid_blob_ptr: *mut DrmPropertyBlob,
    pub properties: DrmObjectProperties,
    pub scaling_mode_property: *mut DrmProperty,
    pub vrr_capable_property: *mut DrmProperty,
    pub colorspace_property: *mut DrmProperty,
    pub path_blob_ptr: *mut DrmPropertyBlob,
    pub max_bpc: u32,
    pub max_bpc_property: *mut DrmProperty,
    pub privacy_screen: *mut c_void,
    pub privacy_screen_notifier: NotifierBlock,
    pub privacy_screen_sw_state_property: *mut DrmProperty,
    pub privacy_screen_hw_state_property: *mut DrmProperty,
    pub broadcast_rgb_property: *mut DrmProperty,
    pub polled: u8,
    pub dpms: i32,
    pub helper_private: *mut DrmConnectorHelperFuncs,
    pub cmdline_mode: DrmCmdlineMode,
    pub force: DrmConnectorForce,
    pub edid_override: *mut c_void,
    pub edid_override_mutex: Mutex,
    pub epoch_counter: u64,
    pub possible_encoders: u32,
    pub encoder: *mut DrmEncoder,
    pub eld: [u8; 128],
    pub eld_mutex: Mutex,
    pub latency_present: [bool; 2],
    pub video_latency: [i32; 2],
    pub audio_latency: [i32; 2],
    pub ddc: *mut c_void,
    pub null_edid_counter: i32,
    pub bad_edid_counter: u32,
    pub edid_corrupt: bool,
    pub real_edid_checksum: u8,
    pub debugfs_entry: *mut c_void,
    pub state: *mut DrmConnectorState,
    pub tile_blob_ptr: *mut DrmPropertyBlob,
    pub has_tile: bool,
    pub tile_group: *mut DrmTileGroup,
    pub tile_is_single_monitor: bool,
    pub num_h_tile: u8,
    pub num_v_tile: u8,
    pub tile_h_loc: u8,
    pub tile_v_loc: u8,
    pub tile_h_size: u16,
    pub tile_v_size: u16,
    pub free_node: LlistNode,
    pub hdmi: DrmConnectorHdmi,
    pub hdmi_audio: DrmConnectorHdmiAudio,
    pub cec: DrmConnectorCec,
}

#[repr(C, align(8))]
pub struct DrmTileGroup {
    pub refcount: Kref,
    pub dev: *mut DrmDevice,
    pub id: i32,
    pub group_data: [u8; 9],
}

#[repr(C)]
pub struct DrmConnectorListIter {
    pub dev: *mut DrmDevice,
    pub conn: *mut DrmConnector,
}

pub type SubpixelOrder = u32;
pub const SUBPIXELUNKNOWN: SubpixelOrder = 0;
pub const SUBPIXELHORIZONTALRGB: SubpixelOrder = 1;
pub const SUBPIXELHORIZONTALBGR: SubpixelOrder = 2;
pub const SUBPIXELVERTICALRGB: SubpixelOrder = 3;
pub const SUBPIXELVERTICALBGR: SubpixelOrder = 4;
pub const SUBPIXELNONE: SubpixelOrder = 5;

pub type DrmHdmiBroadcastRgb = u32;
pub const DRM_HDMI_BROADCAST_RGB_AUTO: DrmHdmiBroadcastRgb = 0;
pub const DRM_HDMI_BROADCAST_RGB_FULL: DrmHdmiBroadcastRgb = 1;
pub const DRM_HDMI_BROADCAST_RGB_LIMITED: DrmHdmiBroadcastRgb = 2;

pub type DrmOutputColorFormat = u32;
pub const DRM_OUTPUT_COLOR_FORMAT_RGB444: DrmOutputColorFormat = 0;
pub const DRM_OUTPUT_COLOR_FORMAT_YCBCR444: DrmOutputColorFormat = 1;
pub const DRM_OUTPUT_COLOR_FORMAT_YCBCR422: DrmOutputColorFormat = 2;
pub const DRM_OUTPUT_COLOR_FORMAT_YCBCR420: DrmOutputColorFormat = 3;

pub type DrmLinkStatus = u32;
pub const DRM_LINK_STATUS_GOOD: DrmLinkStatus = 0;
pub const DRM_LINK_STATUS_BAD: DrmLinkStatus = 1;

pub type DrmColorspace = u32;
pub const DRM_MODE_COLORIMETRY_DEFAULT: DrmColorspace = 0;
pub const DRM_MODE_COLORIMETRY_NO_DATA: DrmColorspace = 0;
pub const DRM_MODE_COLORIMETRY_SMPTE_170M_YCC: DrmColorspace = 1;
pub const DRM_MODE_COLORIMETRY_BT709_YCC: DrmColorspace = 2;
pub const DRM_MODE_COLORIMETRY_XVYCC_601: DrmColorspace = 3;
pub const DRM_MODE_COLORIMETRY_XVYCC_709: DrmColorspace = 4;
pub const DRM_MODE_COLORIMETRY_SYCC_601: DrmColorspace = 5;
pub const DRM_MODE_COLORIMETRY_OPYCC_601: DrmColorspace = 6;
pub const DRM_MODE_COLORIMETRY_OPRGB: DrmColorspace = 7;
pub const DRM_MODE_COLORIMETRY_BT2020_CYCC: DrmColorspace = 8;
pub const DRM_MODE_COLORIMETRY_BT2020_RGB: DrmColorspace = 9;
pub const DRM_MODE_COLORIMETRY_BT2020_YCC: DrmColorspace = 10;
pub const DRM_MODE_COLORIMETRY_DCI_P3_RGB_D65: DrmColorspace = 11;
pub const DRM_MODE_COLORIMETRY_DCI_P3_RGB_THEATER: DrmColorspace = 12;
pub const DRM_MODE_COLORIMETRY_RGB_WIDE_FIXED: DrmColorspace = 13;
pub const DRM_MODE_COLORIMETRY_RGB_WIDE_FLOAT: DrmColorspace = 14;
pub const DRM_MODE_COLORIMETRY_BT601_YCC: DrmColorspace = 15;
pub const DRM_MODE_COLORIMETRY_COUNT: DrmColorspace = 16;

pub type DrmPrivacyScreenStatus = u32;
pub const PRIVACY_SCREEN_DISABLED: DrmPrivacyScreenStatus = 0;
pub const PRIVACY_SCREEN_ENABLED: DrmPrivacyScreenStatus = 1;
pub const PRIVACY_SCREEN_DISABLED_LOCKED: DrmPrivacyScreenStatus = 2;
pub const PRIVACY_SCREEN_ENABLED_LOCKED: DrmPrivacyScreenStatus = 3;

pub type DrmConnectorStatus = u32;
pub const CONNECTOR_STATUS_CONNECTED: DrmConnectorStatus = 1;
pub const CONNECTOR_STATUS_DISCONNECTED: DrmConnectorStatus = 2;
pub const CONNECTOR_STATUS_UNKNOWN: DrmConnectorStatus = 3;

pub type DrmConnectorForce = u32;
pub const DRM_FORCE_UNSPECIFIED: DrmConnectorForce = 0;
pub const DRM_FORCE_OFF: DrmConnectorForce = 1;
pub const DRM_FORCE_ON: DrmConnectorForce = 2;
pub const DRM_FORCE_ON_DIGITAL: DrmConnectorForce = 3;

pub type DrmPanelOrientation = i32;
pub const DRM_MODE_PANEL_ORIENTATION_UNKNOWN: DrmPanelOrientation = -1;
pub const DRM_MODE_PANEL_ORIENTATION_NORMAL: DrmPanelOrientation = 0;
pub const DRM_MODE_PANEL_ORIENTATION_BOTTOM_UP: DrmPanelOrientation = 1;
pub const DRM_MODE_PANEL_ORIENTATION_LEFT_UP: DrmPanelOrientation = 2;
pub const DRM_MODE_PANEL_ORIENTATION_RIGHT_UP: DrmPanelOrientation = 3;

pub type DrmConnectorTvMode = u32;
pub const DRM_MODE_TV_MODE_NTSC: DrmConnectorTvMode = 0;
pub const DRM_MODE_TV_MODE_NTSC_443: DrmConnectorTvMode = 1;
pub const DRM_MODE_TV_MODE_NTSC_J: DrmConnectorTvMode = 2;
pub const DRM_MODE_TV_MODE_PAL: DrmConnectorTvMode = 3;
pub const DRM_MODE_TV_MODE_PAL_M: DrmConnectorTvMode = 4;
pub const DRM_MODE_TV_MODE_PAL_N: DrmConnectorTvMode = 5;
pub const DRM_MODE_TV_MODE_SECAM: DrmConnectorTvMode = 6;
pub const DRM_MODE_TV_MODE_MONOCHROME: DrmConnectorTvMode = 7;
pub const DRM_MODE_TV_MODE_MAX: DrmConnectorTvMode = 8;

pub type DrmConnectorRegistrationState = u32;
pub const DRM_CONNECTOR_INITIALIZING: DrmConnectorRegistrationState = 0;
pub const DRM_CONNECTOR_REGISTERED: DrmConnectorRegistrationState = 1;
pub const DRM_CONNECTOR_UNREGISTERED: DrmConnectorRegistrationState = 2;

const _: () = assert!(size_of::<DrmScrambling>() == 2);
const _: () = assert!(core::mem::offset_of!(DrmScrambling, supported) == 0);
const _: () = assert!(core::mem::offset_of!(DrmScrambling, low_rates) == 1);
const _: () = assert!(size_of::<DrmScdc>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmScdc, supported) == 0);
const _: () = assert!(core::mem::offset_of!(DrmScdc, read_request) == 1);
const _: () = assert!(core::mem::offset_of!(DrmScdc, scrambling) == 2);
const _: () = assert!(size_of::<DrmScrambling>() == 2);
const _: () = assert!(size_of::<DrmHdmiDscCap>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, v_1p2) == 0);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, native_420) == 1);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, all_bpp) == 2);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, bpc_supported) == 3);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, max_slices) == 4);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, clk_per_slice) == 8);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, max_lanes) == 12);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, max_frl_rate_per_lane) == 13);
const _: () = assert!(core::mem::offset_of!(DrmHdmiDscCap, total_chunk_kbytes) == 14);
const _: () = assert!(size_of::<DrmHdmiInfo>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, scdc) == 0);
const _: () = assert!(size_of::<DrmScdc>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, y420_vdb_modes) == 8);
const _: () = assert!(size_of::<[u64; 4]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, y420_cmdb_modes) == 40);
const _: () = assert!(size_of::<[u64; 4]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, y420_dc_modes) == 72);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, max_frl_rate_per_lane) == 73);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, max_lanes) == 74);
const _: () = assert!(core::mem::offset_of!(DrmHdmiInfo, dsc_cap) == 76);
const _: () = assert!(size_of::<DrmHdmiDscCap>() == 16);
const _: () = assert!(size_of::<DrmMonitorRangeInfo>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmMonitorRangeInfo, min_vfreq) == 0);
const _: () = assert!(core::mem::offset_of!(DrmMonitorRangeInfo, max_vfreq) == 2);
const _: () = assert!(size_of::<DrmLuminanceRangeInfo>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmLuminanceRangeInfo, min_luminance) == 0);
const _: () = assert!(core::mem::offset_of!(DrmLuminanceRangeInfo, max_luminance) == 4);
const _: () = assert!(size_of::<DrmAmdVsdbInfo>() == 20);
const _: () = assert!(core::mem::offset_of!(DrmAmdVsdbInfo, version) == 0);
const _: () = assert!(core::mem::offset_of!(DrmAmdVsdbInfo, replay_mode) == 1);
const _: () = assert!(core::mem::offset_of!(DrmAmdVsdbInfo, panel_type) == 2);
const _: () = assert!(core::mem::offset_of!(DrmAmdVsdbInfo, luminance_range1) == 4);
const _: () = assert!(size_of::<DrmLuminanceRangeInfo>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmAmdVsdbInfo, luminance_range2) == 12);
const _: () = assert!(size_of::<DrmLuminanceRangeInfo>() == 8);
const _: () = assert!(size_of::<DrmDisplayInfo>() == 232);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, width_mm) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, height_mm) == 4);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, bpc) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, subpixel_order) == 12);
const _: () = assert!(size_of::<SubpixelOrder>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, panel_orientation) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, color_formats) == 20);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, bus_formats) == 24);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, num_bus_formats) == 32);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, bus_flags) == 36);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, max_tmds_clock) == 40);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, dvi_dual) == 44);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, is_hdmi) == 45);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, has_audio) == 46);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, has_hdmi_infoframe) == 47);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, rgb_quant_range_selectable) == 48);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, edid_hdmi_rgb444_dc_modes) == 49);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, edid_hdmi_ycbcr444_dc_modes) == 50);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, cea_rev) == 51);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, hdmi) == 56);
const _: () = assert!(size_of::<DrmHdmiInfo>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, hdr_sink_metadata) == 152);
const _: () = assert!(size_of::<HdrSinkMetadata>() == 12);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, non_desktop) == 164);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, monitor_range) == 166);
const _: () = assert!(size_of::<DrmMonitorRangeInfo>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, luminance_range) == 172);
const _: () = assert!(size_of::<DrmLuminanceRangeInfo>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, mso_stream_count) == 180);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, mso_pixel_overlap) == 181);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, max_dsc_bpp) == 184);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, vics) == 192);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, vics_len) == 200);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, quirks) == 204);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, source_physical_address) == 208);
const _: () = assert!(core::mem::offset_of!(DrmDisplayInfo, amd_vsdb) == 212);
const _: () = assert!(size_of::<DrmAmdVsdbInfo>() == 20);
const _: () = assert!(size_of::<DrmConnectorTvMargins>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorTvMargins, bottom) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorTvMargins, left) == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorTvMargins, right) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorTvMargins, top) == 12);
const _: () = assert!(size_of::<DrmTvConnectorState>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, select_subconnector) == 0);
const _: () = assert!(size_of::<DrmModeSubconnector>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, subconnector) == 4);
const _: () = assert!(size_of::<DrmModeSubconnector>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, margins) == 8);
const _: () = assert!(size_of::<DrmConnectorTvMargins>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, legacy_mode) == 24);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, mode) == 28);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, brightness) == 32);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, contrast) == 36);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, flicker_reduction) == 40);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, overscan) == 44);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, saturation) == 48);
const _: () = assert!(core::mem::offset_of!(DrmTvConnectorState, hue) == 52);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframe>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiInfoframe, data) == 0);
const _: () = assert!(size_of::<HdmiInfoframe>() == 60);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiInfoframe, set) == 60);
const _: () = assert!(size_of::<DrmConnectorHdmiStateInfoframes>() == 256);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiStateInfoframes, avi) == 0);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframe>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiStateInfoframes, hdr_drm) == 64);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframe>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiStateInfoframes, spd) == 128);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframe>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiStateInfoframes, hdmi) == 192);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframe>() == 64);
const _: () = assert!(size_of::<DrmConnectorHdmiState>() == 280);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiState, broadcast_rgb) == 0);
const _: () = assert!(size_of::<DrmHdmiBroadcastRgb>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiState, infoframes) == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiState, is_limited_range) == 260);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiState, output_bpc) == 264);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiState, output_format) == 268);
const _: () = assert!(size_of::<DrmOutputColorFormat>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiState, tmds_char_rate) == 272);
const _: () = assert!(size_of::<DrmConnectorState>() == 440);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, connector) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, crtc) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, best_encoder) == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, link_status) == 24);
const _: () = assert!(size_of::<DrmLinkStatus>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, state) == 32);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, commit) == 40);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, tv) == 48);
const _: () = assert!(size_of::<DrmTvConnectorState>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, self_refresh_aware) == 104);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, picture_aspect_ratio) == 108);
const _: () = assert!(size_of::<HdmiPictureAspect>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, content_type) == 112);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, hdcp_content_type) == 116);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, scaling_mode) == 120);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, content_protection) == 124);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, colorspace) == 128);
const _: () = assert!(size_of::<DrmColorspace>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, writeback_job) == 136);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, max_requested_bpc) == 144);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, max_bpc) == 145);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, privacy_screen_sw_state) == 148);
const _: () = assert!(size_of::<DrmPrivacyScreenStatus>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, hdr_output_metadata) == 152);
const _: () = assert!(core::mem::offset_of!(DrmConnectorState, hdmi) == 160);
const _: () = assert!(size_of::<DrmConnectorHdmiState>() == 280);
const _: () = assert!(size_of::<DrmConnectorHdmiAudioFuncs>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudioFuncs, startup) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudioFuncs, prepare) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudioFuncs, shutdown) == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudioFuncs, mute_stream) == 24);
const _: () = assert!(size_of::<DrmConnectorCecFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorCecFuncs, phys_addr_invalidate) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorCecFuncs, phys_addr_set) == 8);
const _: () = assert!(size_of::<DrmConnectorInfoframeFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorInfoframeFuncs, clear_infoframe) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorInfoframeFuncs, write_infoframe) == 8);
const _: () = assert!(size_of::<DrmConnectorHdmiFuncs>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, tmds_char_rate_valid) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, read_edid) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, avi) == 16);
const _: () = assert!(size_of::<DrmConnectorInfoframeFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, hdmi) == 32);
const _: () = assert!(size_of::<DrmConnectorInfoframeFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, audio) == 48);
const _: () = assert!(size_of::<DrmConnectorInfoframeFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, hdr_drm) == 64);
const _: () = assert!(size_of::<DrmConnectorInfoframeFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiFuncs, spd) == 80);
const _: () = assert!(size_of::<DrmConnectorInfoframeFuncs>() == 16);
const _: () = assert!(size_of::<DrmConnectorFuncs>() == 128);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, dpms) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, reset) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, detect) == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, force) == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, fill_modes) == 32);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, set_property) == 40);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, late_register) == 48);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, early_unregister) == 56);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, destroy) == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, atomic_duplicate_state) == 72);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, atomic_destroy_state) == 80);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, atomic_set_property) == 88);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, atomic_get_property) == 96);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, atomic_print_state) == 104);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, oob_hotplug_event) == 112);
const _: () = assert!(core::mem::offset_of!(DrmConnectorFuncs, debugfs_init) == 120);
const _: () = assert!(size_of::<DrmCmdlineMode>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, name) == 0);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, specified) == 32);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, refresh_specified) == 33);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, bpp_specified) == 34);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, pixel_clock) == 36);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, xres) == 40);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, yres) == 44);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, bpp) == 48);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, refresh) == 52);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, rb) == 56);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, interlace) == 57);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, cvt) == 58);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, margins) == 59);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, force) == 60);
const _: () = assert!(size_of::<DrmConnectorForce>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, rotation_reflection) == 64);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, panel_orientation) == 68);
const _: () = assert!(size_of::<DrmPanelOrientation>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, tv_margins) == 72);
const _: () = assert!(size_of::<DrmConnectorTvMargins>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, tv_mode) == 88);
const _: () = assert!(size_of::<DrmConnectorTvMode>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCmdlineMode, tv_mode_specified) == 92);
const _: () = assert!(size_of::<DrmConnectorHdmiAudio>() == 64);
const _: () = assert!(align_of::<DrmConnectorHdmiAudio>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, funcs) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, codec_pdev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, lock) == 16);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, plugged_cb) == 40);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, plugged_cb_dev) == 48);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, last_state) == 56);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiAudio, dai_port) == 60);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframes>() == 88);
const _: () = assert!(align_of::<DrmConnectorHdmiInfoframes>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiInfoframes, lock) == 0);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmiInfoframes, audio) == 24);
const _: () = assert!(size_of::<DrmConnectorHdmiInfoframe>() == 64);
const _: () = assert!(size_of::<DrmConnectorHdmi>() == 128);
const _: () = assert!(align_of::<DrmConnectorHdmi>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmi, vendor) == 0);
const _: () = assert!(size_of::<[u8; 8]>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmi, product) == 8);
const _: () = assert!(size_of::<[u8; 16]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmi, supported_formats) == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmi, funcs) == 32);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHdmi, infoframes) == 40);
const _: () = assert!(size_of::<DrmConnectorCec>() == 40);
const _: () = assert!(align_of::<DrmConnectorCec>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorCec, mutex) == 0);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorCec, funcs) == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorCec, data) == 32);
const _: () = assert!(size_of::<DrmConnector>() == 2248);
const _: () = assert!(align_of::<DrmConnector>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnector, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnector, kdev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnector, attr) == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnector, fwnode) == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnector, head) == 32);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnector, global_connector_list_entry) == 48);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnector, base) == 64);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmConnector, name) == 96);
const _: () = assert!(core::mem::offset_of!(DrmConnector, mutex) == 104);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnector, index) == 128);
const _: () = assert!(core::mem::offset_of!(DrmConnector, connector_type) == 132);
const _: () = assert!(core::mem::offset_of!(DrmConnector, connector_type_id) == 136);
const _: () = assert!(core::mem::offset_of!(DrmConnector, interlace_allowed) == 140);
const _: () = assert!(core::mem::offset_of!(DrmConnector, doublescan_allowed) == 141);
const _: () = assert!(core::mem::offset_of!(DrmConnector, stereo_allowed) == 142);
const _: () = assert!(core::mem::offset_of!(DrmConnector, ycbcr_420_allowed) == 143);
const _: () = assert!(core::mem::offset_of!(DrmConnector, registration_state) == 144);
const _: () = assert!(size_of::<DrmConnectorRegistrationState>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnector, modes) == 152);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnector, status) == 168);
const _: () = assert!(size_of::<DrmConnectorStatus>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnector, probed_modes) == 176);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnector, display_info) == 192);
const _: () = assert!(size_of::<DrmDisplayInfo>() == 232);
const _: () = assert!(core::mem::offset_of!(DrmConnector, funcs) == 424);
const _: () = assert!(core::mem::offset_of!(DrmConnector, edid_blob_ptr) == 432);
const _: () = assert!(core::mem::offset_of!(DrmConnector, properties) == 440);
const _: () = assert!(size_of::<DrmObjectProperties>() == 1032);
const _: () = assert!(core::mem::offset_of!(DrmConnector, scaling_mode_property) == 1472);
const _: () = assert!(core::mem::offset_of!(DrmConnector, vrr_capable_property) == 1480);
const _: () = assert!(core::mem::offset_of!(DrmConnector, colorspace_property) == 1488);
const _: () = assert!(core::mem::offset_of!(DrmConnector, path_blob_ptr) == 1496);
const _: () = assert!(core::mem::offset_of!(DrmConnector, max_bpc) == 1504);
const _: () = assert!(core::mem::offset_of!(DrmConnector, max_bpc_property) == 1512);
const _: () = assert!(core::mem::offset_of!(DrmConnector, privacy_screen) == 1520);
const _: () = assert!(core::mem::offset_of!(DrmConnector, privacy_screen_notifier) == 1528);
const _: () = assert!(size_of::<NotifierBlock>() == 24);
const _: () =
    assert!(core::mem::offset_of!(DrmConnector, privacy_screen_sw_state_property) == 1552);
const _: () =
    assert!(core::mem::offset_of!(DrmConnector, privacy_screen_hw_state_property) == 1560);
const _: () = assert!(core::mem::offset_of!(DrmConnector, broadcast_rgb_property) == 1568);
const _: () = assert!(core::mem::offset_of!(DrmConnector, polled) == 1576);
const _: () = assert!(core::mem::offset_of!(DrmConnector, dpms) == 1580);
const _: () = assert!(core::mem::offset_of!(DrmConnector, helper_private) == 1584);
const _: () = assert!(core::mem::offset_of!(DrmConnector, cmdline_mode) == 1592);
const _: () = assert!(size_of::<DrmCmdlineMode>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmConnector, force) == 1688);
const _: () = assert!(size_of::<DrmConnectorForce>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmConnector, edid_override) == 1696);
const _: () = assert!(core::mem::offset_of!(DrmConnector, edid_override_mutex) == 1704);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnector, epoch_counter) == 1728);
const _: () = assert!(core::mem::offset_of!(DrmConnector, possible_encoders) == 1736);
const _: () = assert!(core::mem::offset_of!(DrmConnector, encoder) == 1744);
const _: () = assert!(core::mem::offset_of!(DrmConnector, eld) == 1752);
const _: () = assert!(size_of::<[u8; 128]>() == 128);
const _: () = assert!(core::mem::offset_of!(DrmConnector, eld_mutex) == 1880);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnector, latency_present) == 1904);
const _: () = assert!(size_of::<[bool; 2]>() == 2);
const _: () = assert!(core::mem::offset_of!(DrmConnector, video_latency) == 1908);
const _: () = assert!(size_of::<[i32; 2]>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnector, audio_latency) == 1916);
const _: () = assert!(size_of::<[i32; 2]>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnector, ddc) == 1928);
const _: () = assert!(core::mem::offset_of!(DrmConnector, null_edid_counter) == 1936);
const _: () = assert!(core::mem::offset_of!(DrmConnector, bad_edid_counter) == 1940);
const _: () = assert!(core::mem::offset_of!(DrmConnector, edid_corrupt) == 1944);
const _: () = assert!(core::mem::offset_of!(DrmConnector, real_edid_checksum) == 1945);
const _: () = assert!(core::mem::offset_of!(DrmConnector, debugfs_entry) == 1952);
const _: () = assert!(core::mem::offset_of!(DrmConnector, state) == 1960);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_blob_ptr) == 1968);
const _: () = assert!(core::mem::offset_of!(DrmConnector, has_tile) == 1976);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_group) == 1984);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_is_single_monitor) == 1992);
const _: () = assert!(core::mem::offset_of!(DrmConnector, num_h_tile) == 1993);
const _: () = assert!(core::mem::offset_of!(DrmConnector, num_v_tile) == 1994);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_h_loc) == 1995);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_v_loc) == 1996);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_h_size) == 1998);
const _: () = assert!(core::mem::offset_of!(DrmConnector, tile_v_size) == 2000);
const _: () = assert!(core::mem::offset_of!(DrmConnector, free_node) == 2008);
const _: () = assert!(size_of::<LlistNode>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnector, hdmi) == 2016);
const _: () = assert!(size_of::<DrmConnectorHdmi>() == 128);
const _: () = assert!(core::mem::offset_of!(DrmConnector, hdmi_audio) == 2144);
const _: () = assert!(size_of::<DrmConnectorHdmiAudio>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnector, cec) == 2208);
const _: () = assert!(size_of::<DrmConnectorCec>() == 40);
const _: () = assert!(size_of::<DrmTileGroup>() == 32);
const _: () = assert!(align_of::<DrmTileGroup>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmTileGroup, refcount) == 0);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmTileGroup, dev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmTileGroup, id) == 16);
const _: () = assert!(core::mem::offset_of!(DrmTileGroup, group_data) == 20);
const _: () = assert!(size_of::<[u8; 9]>() == 9);
const _: () = assert!(size_of::<DrmConnectorListIter>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorListIter, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorListIter, conn) == 8);

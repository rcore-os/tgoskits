// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/linux/hdmi.h`

use core::{
    ffi::c_char,
    mem::{ManuallyDrop, size_of},
};

#[repr(C)]
pub struct HdmiAnyInfoframe {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
}

#[repr(C)]
pub struct HdmiAviInfoframe {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
    pub itc: bool,
    pub pixel_repeat: u8,
    pub colorspace: HdmiColorspace,
    pub scan_mode: HdmiScanMode,
    pub colorimetry: HdmiColorimetry,
    pub picture_aspect: HdmiPictureAspect,
    pub active_aspect: HdmiActiveAspect,
    pub extended_colorimetry: HdmiExtendedColorimetry,
    pub quantization_range: HdmiQuantizationRange,
    pub nups: HdmiNups,
    pub video_code: u8,
    pub ycc_quantization_range: HdmiYccQuantizationRange,
    pub content_type: HdmiContentType,
    pub top_bar: u16,
    pub bottom_bar: u16,
    pub left_bar: u16,
    pub right_bar: u16,
}

#[repr(C)]
pub struct HdmiDrmInfoframeDisplayPrimaries {
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
pub struct HdmiDrmInfoframeWhitePoint {
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
pub struct HdmiDrmInfoframe {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
    pub eotf: HdmiEotf,
    pub metadata_type: HdmiMetadataType,
    pub display_primaries: [HdmiDrmInfoframeDisplayPrimaries; 3],
    pub white_point: HdmiDrmInfoframeWhitePoint,
    pub max_display_mastering_luminance: u16,
    pub min_display_mastering_luminance: u16,
    pub max_cll: u16,
    pub max_fall: u16,
}

#[repr(C)]
pub struct HdmiSpdInfoframe {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
    pub vendor: [c_char; 8],
    pub product: [c_char; 16],
    pub sdi: HdmiSpdSdi,
}

#[repr(C)]
pub struct HdmiAudioInfoframe {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
    pub channels: u8,
    pub coding_type: HdmiAudioCodingType,
    pub sample_size: HdmiAudioSampleSize,
    pub sample_frequency: HdmiAudioSampleFrequency,
    pub coding_type_ext: HdmiAudioCodingTypeExt,
    pub channel_allocation: u8,
    pub level_shift_value: u8,
    pub downmix_inhibit: bool,
}

#[repr(C)]
pub struct HdmiVendorInfoframe {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
    pub oui: u32,
    pub vic: u8,
    pub s3d_struct: Hdmi3dStructure,
    pub s3d_ext_data: u32,
}

#[repr(C)]
pub struct HdrStaticMetadata {
    pub eotf: u8,
    pub metadata_type: u8,
    pub max_cll: u16,
    pub max_fall: u16,
    pub min_cll: u16,
}

#[repr(C)]
pub union HdrSinkMetadataAnon4 {
    pub hdmi_type1: ManuallyDrop<HdrStaticMetadata>,
}

#[repr(C)]
pub struct HdrSinkMetadata {
    pub metadata_type: u32,
    pub _anon_4: HdrSinkMetadataAnon4,
}

#[repr(C)]
pub struct HdmiVendorAnyInfoframeAny {
    pub r#type: HdmiInfoframeType,
    pub version: u8,
    pub length: u8,
    pub oui: u32,
}

#[repr(C)]
pub union HdmiVendorAnyInfoframe {
    pub any: ManuallyDrop<HdmiVendorAnyInfoframeAny>,
    pub hdmi: ManuallyDrop<HdmiVendorInfoframe>,
}

#[repr(C)]
pub union HdmiInfoframe {
    pub any: ManuallyDrop<HdmiAnyInfoframe>,
    pub avi: ManuallyDrop<HdmiAviInfoframe>,
    pub spd: ManuallyDrop<HdmiSpdInfoframe>,
    pub vendor: ManuallyDrop<HdmiVendorAnyInfoframe>,
    pub audio: ManuallyDrop<HdmiAudioInfoframe>,
    pub drm: ManuallyDrop<HdmiDrmInfoframe>,
}

pub type HdmiPictureAspect = u32;
pub const HDMI_PICTURE_ASPECT_NONE: HdmiPictureAspect = 0;
pub const HDMI_PICTURE_ASPECT_4_3: HdmiPictureAspect = 1;
pub const HDMI_PICTURE_ASPECT_16_9: HdmiPictureAspect = 2;
pub const HDMI_PICTURE_ASPECT_64_27: HdmiPictureAspect = 3;
pub const HDMI_PICTURE_ASPECT_256_135: HdmiPictureAspect = 4;
pub const HDMI_PICTURE_ASPECT_RESERVED: HdmiPictureAspect = 5;

pub type HdmiInfoframeType = u32;
pub const HDMI_INFOFRAME_TYPE_VENDOR: HdmiInfoframeType = 129;
pub const HDMI_INFOFRAME_TYPE_AVI: HdmiInfoframeType = 130;
pub const HDMI_INFOFRAME_TYPE_SPD: HdmiInfoframeType = 131;
pub const HDMI_INFOFRAME_TYPE_AUDIO: HdmiInfoframeType = 132;
pub const HDMI_INFOFRAME_TYPE_DRM: HdmiInfoframeType = 135;

pub type HdmiColorspace = u32;
pub const HDMI_COLORSPACE_RGB: HdmiColorspace = 0;
pub const HDMI_COLORSPACE_YUV422: HdmiColorspace = 1;
pub const HDMI_COLORSPACE_YUV444: HdmiColorspace = 2;
pub const HDMI_COLORSPACE_YUV420: HdmiColorspace = 3;
pub const HDMI_COLORSPACE_RESERVED4: HdmiColorspace = 4;
pub const HDMI_COLORSPACE_RESERVED5: HdmiColorspace = 5;
pub const HDMI_COLORSPACE_RESERVED6: HdmiColorspace = 6;
pub const HDMI_COLORSPACE_IDO_DEFINED: HdmiColorspace = 7;

pub type HdmiScanMode = u32;
pub const HDMI_SCAN_MODE_NONE: HdmiScanMode = 0;
pub const HDMI_SCAN_MODE_OVERSCAN: HdmiScanMode = 1;
pub const HDMI_SCAN_MODE_UNDERSCAN: HdmiScanMode = 2;
pub const HDMI_SCAN_MODE_RESERVED: HdmiScanMode = 3;

pub type HdmiColorimetry = u32;
pub const HDMI_COLORIMETRY_NONE: HdmiColorimetry = 0;
pub const HDMI_COLORIMETRY_ITU_601: HdmiColorimetry = 1;
pub const HDMI_COLORIMETRY_ITU_709: HdmiColorimetry = 2;
pub const HDMI_COLORIMETRY_EXTENDED: HdmiColorimetry = 3;

pub type HdmiActiveAspect = u32;
pub const HDMI_ACTIVE_ASPECT_16_9_TOP: HdmiActiveAspect = 2;
pub const HDMI_ACTIVE_ASPECT_14_9_TOP: HdmiActiveAspect = 3;
pub const HDMI_ACTIVE_ASPECT_16_9_CENTER: HdmiActiveAspect = 4;
pub const HDMI_ACTIVE_ASPECT_PICTURE: HdmiActiveAspect = 8;
pub const HDMI_ACTIVE_ASPECT_4_3: HdmiActiveAspect = 9;
pub const HDMI_ACTIVE_ASPECT_16_9: HdmiActiveAspect = 10;
pub const HDMI_ACTIVE_ASPECT_14_9: HdmiActiveAspect = 11;
pub const HDMI_ACTIVE_ASPECT_4_3_SP_14_9: HdmiActiveAspect = 13;
pub const HDMI_ACTIVE_ASPECT_16_9_SP_14_9: HdmiActiveAspect = 14;
pub const HDMI_ACTIVE_ASPECT_16_9_SP_4_3: HdmiActiveAspect = 15;

pub type HdmiExtendedColorimetry = u32;
pub const HDMI_EXTENDED_COLORIMETRY_XV_YCC_601: HdmiExtendedColorimetry = 0;
pub const HDMI_EXTENDED_COLORIMETRY_XV_YCC_709: HdmiExtendedColorimetry = 1;
pub const HDMI_EXTENDED_COLORIMETRY_S_YCC_601: HdmiExtendedColorimetry = 2;
pub const HDMI_EXTENDED_COLORIMETRY_OPYCC_601: HdmiExtendedColorimetry = 3;
pub const HDMI_EXTENDED_COLORIMETRY_OPRGB: HdmiExtendedColorimetry = 4;
pub const HDMI_EXTENDED_COLORIMETRY_BT2020_CONST_LUM: HdmiExtendedColorimetry = 5;
pub const HDMI_EXTENDED_COLORIMETRY_BT2020: HdmiExtendedColorimetry = 6;
pub const HDMI_EXTENDED_COLORIMETRY_RESERVED: HdmiExtendedColorimetry = 7;

pub type HdmiQuantizationRange = u32;
pub const HDMI_QUANTIZATION_RANGE_DEFAULT: HdmiQuantizationRange = 0;
pub const HDMI_QUANTIZATION_RANGE_LIMITED: HdmiQuantizationRange = 1;
pub const HDMI_QUANTIZATION_RANGE_FULL: HdmiQuantizationRange = 2;
pub const HDMI_QUANTIZATION_RANGE_RESERVED: HdmiQuantizationRange = 3;

pub type HdmiNups = u32;
pub const HDMI_NUPS_UNKNOWN: HdmiNups = 0;
pub const HDMI_NUPS_HORIZONTAL: HdmiNups = 1;
pub const HDMI_NUPS_VERTICAL: HdmiNups = 2;
pub const HDMI_NUPS_BOTH: HdmiNups = 3;

pub type HdmiYccQuantizationRange = u32;
pub const HDMI_YCC_QUANTIZATION_RANGE_LIMITED: HdmiYccQuantizationRange = 0;
pub const HDMI_YCC_QUANTIZATION_RANGE_FULL: HdmiYccQuantizationRange = 1;

pub type HdmiContentType = u32;
pub const HDMI_CONTENT_TYPE_GRAPHICS: HdmiContentType = 0;
pub const HDMI_CONTENT_TYPE_PHOTO: HdmiContentType = 1;
pub const HDMI_CONTENT_TYPE_CINEMA: HdmiContentType = 2;
pub const HDMI_CONTENT_TYPE_GAME: HdmiContentType = 3;

pub type HdmiEotf = u32;
pub const HDMI_EOTF_TRADITIONAL_GAMMA_SDR: HdmiEotf = 0;
pub const HDMI_EOTF_TRADITIONAL_GAMMA_HDR: HdmiEotf = 1;
pub const HDMI_EOTF_SMPTE_ST2084: HdmiEotf = 2;
pub const HDMI_EOTF_BT_2100_HLG: HdmiEotf = 3;

pub type HdmiMetadataType = u32;
pub const HDMI_STATIC_METADATA_TYPE1: HdmiMetadataType = 0;

pub type HdmiSpdSdi = u32;
pub const HDMI_SPD_SDI_UNKNOWN: HdmiSpdSdi = 0;
pub const HDMI_SPD_SDI_DSTB: HdmiSpdSdi = 1;
pub const HDMI_SPD_SDI_DVDP: HdmiSpdSdi = 2;
pub const HDMI_SPD_SDI_DVHS: HdmiSpdSdi = 3;
pub const HDMI_SPD_SDI_HDDVR: HdmiSpdSdi = 4;
pub const HDMI_SPD_SDI_DVC: HdmiSpdSdi = 5;
pub const HDMI_SPD_SDI_DSC: HdmiSpdSdi = 6;
pub const HDMI_SPD_SDI_VCD: HdmiSpdSdi = 7;
pub const HDMI_SPD_SDI_GAME: HdmiSpdSdi = 8;
pub const HDMI_SPD_SDI_PC: HdmiSpdSdi = 9;
pub const HDMI_SPD_SDI_BD: HdmiSpdSdi = 10;
pub const HDMI_SPD_SDI_SACD: HdmiSpdSdi = 11;
pub const HDMI_SPD_SDI_HDDVD: HdmiSpdSdi = 12;
pub const HDMI_SPD_SDI_PMP: HdmiSpdSdi = 13;

pub type HdmiAudioCodingType = u32;
pub const HDMI_AUDIO_CODING_TYPE_STREAM: HdmiAudioCodingType = 0;
pub const HDMI_AUDIO_CODING_TYPE_PCM: HdmiAudioCodingType = 1;
pub const HDMI_AUDIO_CODING_TYPE_AC3: HdmiAudioCodingType = 2;
pub const HDMI_AUDIO_CODING_TYPE_MPEG1: HdmiAudioCodingType = 3;
pub const HDMI_AUDIO_CODING_TYPE_MP3: HdmiAudioCodingType = 4;
pub const HDMI_AUDIO_CODING_TYPE_MPEG2: HdmiAudioCodingType = 5;
pub const HDMI_AUDIO_CODING_TYPE_AAC_LC: HdmiAudioCodingType = 6;
pub const HDMI_AUDIO_CODING_TYPE_DTS: HdmiAudioCodingType = 7;
pub const HDMI_AUDIO_CODING_TYPE_ATRAC: HdmiAudioCodingType = 8;
pub const HDMI_AUDIO_CODING_TYPE_DSD: HdmiAudioCodingType = 9;
pub const HDMI_AUDIO_CODING_TYPE_EAC3: HdmiAudioCodingType = 10;
pub const HDMI_AUDIO_CODING_TYPE_DTS_HD: HdmiAudioCodingType = 11;
pub const HDMI_AUDIO_CODING_TYPE_MLP: HdmiAudioCodingType = 12;
pub const HDMI_AUDIO_CODING_TYPE_DST: HdmiAudioCodingType = 13;
pub const HDMI_AUDIO_CODING_TYPE_WMA_PRO: HdmiAudioCodingType = 14;
pub const HDMI_AUDIO_CODING_TYPE_CXT: HdmiAudioCodingType = 15;

pub type HdmiAudioSampleSize = u32;
pub const HDMI_AUDIO_SAMPLE_SIZE_STREAM: HdmiAudioSampleSize = 0;
pub const HDMI_AUDIO_SAMPLE_SIZE_16: HdmiAudioSampleSize = 1;
pub const HDMI_AUDIO_SAMPLE_SIZE_20: HdmiAudioSampleSize = 2;
pub const HDMI_AUDIO_SAMPLE_SIZE_24: HdmiAudioSampleSize = 3;

pub type HdmiAudioSampleFrequency = u32;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_STREAM: HdmiAudioSampleFrequency = 0;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_32000: HdmiAudioSampleFrequency = 1;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_44100: HdmiAudioSampleFrequency = 2;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_48000: HdmiAudioSampleFrequency = 3;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_88200: HdmiAudioSampleFrequency = 4;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_96000: HdmiAudioSampleFrequency = 5;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_176400: HdmiAudioSampleFrequency = 6;
pub const HDMI_AUDIO_SAMPLE_FREQUENCY_192000: HdmiAudioSampleFrequency = 7;

pub type HdmiAudioCodingTypeExt = u32;
pub const HDMI_AUDIO_CODING_TYPE_EXT_CT: HdmiAudioCodingTypeExt = 0;
pub const HDMI_AUDIO_CODING_TYPE_EXT_HE_AAC: HdmiAudioCodingTypeExt = 1;
pub const HDMI_AUDIO_CODING_TYPE_EXT_HE_AAC_V2: HdmiAudioCodingTypeExt = 2;
pub const HDMI_AUDIO_CODING_TYPE_EXT_MPEG_SURROUND: HdmiAudioCodingTypeExt = 3;
pub const HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_HE_AAC: HdmiAudioCodingTypeExt = 4;
pub const HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_HE_AAC_V2: HdmiAudioCodingTypeExt = 5;
pub const HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_AAC_LC: HdmiAudioCodingTypeExt = 6;
pub const HDMI_AUDIO_CODING_TYPE_EXT_DRA: HdmiAudioCodingTypeExt = 7;
pub const HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_HE_AAC_SURROUND: HdmiAudioCodingTypeExt = 8;
pub const HDMI_AUDIO_CODING_TYPE_EXT_MPEG4_AAC_LC_SURROUND: HdmiAudioCodingTypeExt = 10;

pub type Hdmi3dStructure = i32;
pub const HDMI_3D_STRUCTURE_INVALID: Hdmi3dStructure = -1;
pub const HDMI_3D_STRUCTURE_FRAME_PACKING: Hdmi3dStructure = 0;
pub const HDMI_3D_STRUCTURE_FIELD_ALTERNATIVE: Hdmi3dStructure = 1;
pub const HDMI_3D_STRUCTURE_LINE_ALTERNATIVE: Hdmi3dStructure = 2;
pub const HDMI_3D_STRUCTURE_SIDE_BY_SIDE_FULL: Hdmi3dStructure = 3;
pub const HDMI_3D_STRUCTURE_L_DEPTH: Hdmi3dStructure = 4;
pub const HDMI_3D_STRUCTURE_L_DEPTH_GFX_GFX_DEPTH: Hdmi3dStructure = 5;
pub const HDMI_3D_STRUCTURE_TOP_AND_BOTTOM: Hdmi3dStructure = 6;
pub const HDMI_3D_STRUCTURE_SIDE_BY_SIDE_HALF: Hdmi3dStructure = 8;

const _: () = assert!(size_of::<HdmiAnyInfoframe>() == 8);
const _: () = assert!(core::mem::offset_of!(HdmiAnyInfoframe, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAnyInfoframe, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAnyInfoframe, length) == 5);
const _: () = assert!(size_of::<HdmiAviInfoframe>() == 60);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, length) == 5);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, itc) == 6);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, pixel_repeat) == 7);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, colorspace) == 8);
const _: () = assert!(size_of::<HdmiColorspace>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, scan_mode) == 12);
const _: () = assert!(size_of::<HdmiScanMode>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, colorimetry) == 16);
const _: () = assert!(size_of::<HdmiColorimetry>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, picture_aspect) == 20);
const _: () = assert!(size_of::<HdmiPictureAspect>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, active_aspect) == 24);
const _: () = assert!(size_of::<HdmiActiveAspect>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, extended_colorimetry) == 28);
const _: () = assert!(size_of::<HdmiExtendedColorimetry>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, quantization_range) == 32);
const _: () = assert!(size_of::<HdmiQuantizationRange>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, nups) == 36);
const _: () = assert!(size_of::<HdmiNups>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, video_code) == 40);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, ycc_quantization_range) == 44);
const _: () = assert!(size_of::<HdmiYccQuantizationRange>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, content_type) == 48);
const _: () = assert!(size_of::<HdmiContentType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, top_bar) == 52);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, bottom_bar) == 54);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, left_bar) == 56);
const _: () = assert!(core::mem::offset_of!(HdmiAviInfoframe, right_bar) == 58);
const _: () = assert!(size_of::<HdmiDrmInfoframeDisplayPrimaries>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframeDisplayPrimaries, x) == 0);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframeDisplayPrimaries, y) == 2);
const _: () = assert!(size_of::<HdmiDrmInfoframeWhitePoint>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframeWhitePoint, x) == 0);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframeWhitePoint, y) == 2);
const _: () = assert!(size_of::<HdmiDrmInfoframe>() == 40);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, length) == 5);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, eotf) == 8);
const _: () = assert!(size_of::<HdmiEotf>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, metadata_type) == 12);
const _: () = assert!(size_of::<HdmiMetadataType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, display_primaries) == 16);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, white_point) == 28);
const _: () =
    assert!(core::mem::offset_of!(HdmiDrmInfoframe, max_display_mastering_luminance) == 32);
const _: () =
    assert!(core::mem::offset_of!(HdmiDrmInfoframe, min_display_mastering_luminance) == 34);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, max_cll) == 36);
const _: () = assert!(core::mem::offset_of!(HdmiDrmInfoframe, max_fall) == 38);
const _: () = assert!(size_of::<HdmiSpdInfoframe>() == 36);
const _: () = assert!(core::mem::offset_of!(HdmiSpdInfoframe, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiSpdInfoframe, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiSpdInfoframe, length) == 5);
const _: () = assert!(core::mem::offset_of!(HdmiSpdInfoframe, vendor) == 6);
const _: () = assert!(size_of::<[c_char; 8]>() == 8);
const _: () = assert!(core::mem::offset_of!(HdmiSpdInfoframe, product) == 14);
const _: () = assert!(size_of::<[c_char; 16]>() == 16);
const _: () = assert!(core::mem::offset_of!(HdmiSpdInfoframe, sdi) == 32);
const _: () = assert!(size_of::<HdmiSpdSdi>() == 4);
const _: () = assert!(size_of::<HdmiAudioInfoframe>() == 28);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, length) == 5);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, channels) == 6);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, coding_type) == 8);
const _: () = assert!(size_of::<HdmiAudioCodingType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, sample_size) == 12);
const _: () = assert!(size_of::<HdmiAudioSampleSize>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, sample_frequency) == 16);
const _: () = assert!(size_of::<HdmiAudioSampleFrequency>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, coding_type_ext) == 20);
const _: () = assert!(size_of::<HdmiAudioCodingTypeExt>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, channel_allocation) == 24);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, level_shift_value) == 25);
const _: () = assert!(core::mem::offset_of!(HdmiAudioInfoframe, downmix_inhibit) == 26);
const _: () = assert!(size_of::<HdmiVendorInfoframe>() == 24);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, length) == 5);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, oui) == 8);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, vic) == 12);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, s3d_struct) == 16);
const _: () = assert!(size_of::<Hdmi3dStructure>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiVendorInfoframe, s3d_ext_data) == 20);
const _: () = assert!(size_of::<HdrStaticMetadata>() == 8);
const _: () = assert!(core::mem::offset_of!(HdrStaticMetadata, eotf) == 0);
const _: () = assert!(core::mem::offset_of!(HdrStaticMetadata, metadata_type) == 1);
const _: () = assert!(core::mem::offset_of!(HdrStaticMetadata, max_cll) == 2);
const _: () = assert!(core::mem::offset_of!(HdrStaticMetadata, max_fall) == 4);
const _: () = assert!(core::mem::offset_of!(HdrStaticMetadata, min_cll) == 6);
const _: () = assert!(size_of::<HdrSinkMetadataAnon4>() == 8);
const _: () = assert!(size_of::<HdrSinkMetadata>() == 12);
const _: () = assert!(core::mem::offset_of!(HdrSinkMetadata, metadata_type) == 0);
const _: () = assert!(core::mem::offset_of!(HdrSinkMetadata, _anon_4) == 4);
const _: () = assert!(size_of::<HdmiVendorAnyInfoframeAny>() == 12);
const _: () = assert!(core::mem::offset_of!(HdmiVendorAnyInfoframeAny, r#type) == 0);
const _: () = assert!(size_of::<HdmiInfoframeType>() == 4);
const _: () = assert!(core::mem::offset_of!(HdmiVendorAnyInfoframeAny, version) == 4);
const _: () = assert!(core::mem::offset_of!(HdmiVendorAnyInfoframeAny, length) == 5);
const _: () = assert!(core::mem::offset_of!(HdmiVendorAnyInfoframeAny, oui) == 8);
const _: () = assert!(size_of::<HdmiVendorAnyInfoframe>() == 24);
const _: () = assert!(size_of::<HdmiInfoframe>() == 60);

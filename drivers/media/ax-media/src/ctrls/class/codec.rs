//! 有状态编解码控件。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_CODEC` —— 有状态编解码控件。
pub const CLASS_ID: u32 = CtrlClass::Codec as u32;

/// `V4L2_CID_CODEC_CLASS = (V4L2_CTRL_CLASS_CODEC | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_CODEC_BASE = (V4L2_CTRL_CLASS_CODEC | 0x900) = 0x00990900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `V4L2_CID_CODEC_CX2341X_BASE = (V4L2_CTRL_CLASS_CODEC | 0x1000) = 0x00991000`。
pub const CX2341X_CID_BASE: u32 = CLASS_ID | 0x1000;

/// `V4L2_CID_CODEC_MFC51_BASE = (V4L2_CTRL_CLASS_CODEC | 0x1100) = 0x00991100`。
pub const MFC51_CID_BASE: u32 = CLASS_ID | 0x1100;

// ── 菜单枚举 ─────────────────────────────────────────────────

/// `enum v4l2_mpeg_stream_type` —— `V4L2_CID_MPEG_STREAM_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegStreamType {
    Mpeg2Ps   = 0,
    Mpeg2Ts   = 1,
    Mpeg1Ss   = 2,
    Mpeg2Dvd  = 3,
    Mpeg1Vcd  = 4,
    Mpeg2Svcd = 5,
}

/// `enum v4l2_mpeg_stream_vbi_fmt` —— `V4L2_CID_MPEG_STREAM_VBI_FMT` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegStreamVbiFmt {
    None = 0,
    Ivtv = 1,
}

/// `enum v4l2_mpeg_audio_sampling_freq` —— `V4L2_CID_MPEG_AUDIO_SAMPLING_FREQ` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioSamplingFreq {
    Hz44100 = 0,
    Hz48000 = 1,
    Hz32000 = 2,
}

/// `enum v4l2_mpeg_audio_encoding` —— `V4L2_CID_MPEG_AUDIO_ENCODING` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioEncoding {
    Layer1 = 0,
    Layer2 = 1,
    Layer3 = 2,
    Aac    = 3,
    Ac3    = 4,
}

/// `enum v4l2_mpeg_audio_l1_bitrate` —— `V4L2_CID_MPEG_AUDIO_L1_BITRATE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioL1Bitrate {
    Kbps32  = 0,
    Kbps64  = 1,
    Kbps96  = 2,
    Kbps128 = 3,
    Kbps160 = 4,
    Kbps192 = 5,
    Kbps224 = 6,
    Kbps256 = 7,
    Kbps288 = 8,
    Kbps320 = 9,
    Kbps352 = 10,
    Kbps384 = 11,
    Kbps416 = 12,
    Kbps448 = 13,
}

/// `enum v4l2_mpeg_audio_l2_bitrate` —— `V4L2_CID_MPEG_AUDIO_L2_BITRATE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioL2Bitrate {
    Kbps32  = 0,
    Kbps48  = 1,
    Kbps56  = 2,
    Kbps64  = 3,
    Kbps80  = 4,
    Kbps96  = 5,
    Kbps112 = 6,
    Kbps128 = 7,
    Kbps160 = 8,
    Kbps192 = 9,
    Kbps224 = 10,
    Kbps256 = 11,
    Kbps320 = 12,
    Kbps384 = 13,
}

/// `enum v4l2_mpeg_audio_l3_bitrate` —— `V4L2_CID_MPEG_AUDIO_L3_BITRATE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioL3Bitrate {
    Kbps32  = 0,
    Kbps40  = 1,
    Kbps48  = 2,
    Kbps56  = 3,
    Kbps64  = 4,
    Kbps80  = 5,
    Kbps96  = 6,
    Kbps112 = 7,
    Kbps128 = 8,
    Kbps160 = 9,
    Kbps192 = 10,
    Kbps224 = 11,
    Kbps256 = 12,
    Kbps320 = 13,
}

/// `enum v4l2_mpeg_audio_mode` —— `V4L2_CID_MPEG_AUDIO_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioMode {
    Stereo      = 0,
    JointStereo = 1,
    Dual        = 2,
    Mono        = 3,
}

/// `enum v4l2_mpeg_audio_mode_extension` —— `V4L2_CID_MPEG_AUDIO_MODE_EXTENSION` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioModeExtension {
    Bound4  = 0,
    Bound8  = 1,
    Bound12 = 2,
    Bound16 = 3,
}

/// `enum v4l2_mpeg_audio_emphasis` —— `V4L2_CID_MPEG_AUDIO_EMPHASIS` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioEmphasis {
    None     = 0,
    Div50Us  = 1,
    CcittJ17 = 2,
}

/// `enum v4l2_mpeg_audio_crc` —— `V4L2_CID_MPEG_AUDIO_CRC` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioCrc {
    None  = 0,
    Crc16 = 1,
}

/// `enum v4l2_mpeg_audio_ac3_bitrate` —— `V4L2_CID_MPEG_AUDIO_AC3_BITRATE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioAc3Bitrate {
    Kbps32  = 0,
    Kbps40  = 1,
    Kbps48  = 2,
    Kbps56  = 3,
    Kbps64  = 4,
    Kbps80  = 5,
    Kbps96  = 6,
    Kbps112 = 7,
    Kbps128 = 8,
    Kbps160 = 9,
    Kbps192 = 10,
    Kbps224 = 11,
    Kbps256 = 12,
    Kbps320 = 13,
    Kbps384 = 14,
    Kbps448 = 15,
    Kbps512 = 16,
    Kbps576 = 17,
    Kbps640 = 18,
}

/// `enum v4l2_mpeg_audio_dec_playback` —— `V4L2_CID_MPEG_AUDIO_DEC_PLAYBACK` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegAudioDecPlayback {
    Auto          = 0,
    Stereo        = 1,
    Left          = 2,
    Right         = 3,
    Mono          = 4,
    SwappedStereo = 5,
}

/// `enum v4l2_mpeg_video_encoding` —— `V4L2_CID_MPEG_VIDEO_ENCODING` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoEncoding {
    Mpeg1    = 0,
    Mpeg2    = 1,
    Mpeg4Avc = 2,
}

/// `enum v4l2_mpeg_video_aspect` —— `V4L2_CID_MPEG_VIDEO_ASPECT` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoAspect {
    Aspect1x1     = 0,
    Aspect4x3     = 1,
    Aspect16x9    = 2,
    Aspect221x100 = 3,
}

/// `enum v4l2_mpeg_video_bitrate_mode` —— `V4L2_CID_MPEG_VIDEO_BITRATE_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoBitrateMode {
    Vbr = 0,
    Cbr = 1,
    Cq  = 2,
}

/// `enum v4l2_mpeg_video_header_mode` —— `V4L2_CID_MPEG_VIDEO_HEADER_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHeaderMode {
    Separate           = 0,
    JoinedWith1stFrame = 1,
}

/// `enum v4l2_mpeg_video_multi_slice_mode` —— `V4L2_CID_MPEG_VIDEO_MULTI_SLICE_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoMultiSliceMode {
    Single   = 0,
    MaxMb    = 1,
    MaxBytes = 2,
}

/// `enum v4l2_mpeg_video_intra_refresh_period_type` —— `V4L2_CID_MPEG_VIDEO_INTRA_REFRESH_PERIOD_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoIntraRefreshPeriodType {
    Random = 0,
    Cyclic = 1,
}

/// `enum v4l2_mpeg_video_mpeg2_level` —— `V4L2_CID_MPEG_VIDEO_MPEG2_LEVEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoMpeg2Level {
    Low      = 0,
    Main     = 1,
    High1440 = 2,
    High     = 3,
}

/// `enum v4l2_mpeg_video_mpeg2_profile` —— `V4L2_CID_MPEG_VIDEO_MPEG2_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoMpeg2Profile {
    Simple            = 0,
    Main              = 1,
    SnrScalable       = 2,
    SpatiallyScalable = 3,
    High              = 4,
    Multiview         = 5,
}

/// `enum v4l2_mpeg_video_h264_entropy_mode` —— `V4L2_CID_MPEG_VIDEO_H264_ENTROPY_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264EntropyMode {
    Cavlc = 0,
    Cabac = 1,
}

/// `enum v4l2_mpeg_video_h264_level` —— `V4L2_CID_MPEG_VIDEO_H264_LEVEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264Level {
    Level1_0 = 0,
    Level1B  = 1,
    Level1_1 = 2,
    Level1_2 = 3,
    Level1_3 = 4,
    Level2_0 = 5,
    Level2_1 = 6,
    Level2_2 = 7,
    Level3_0 = 8,
    Level3_1 = 9,
    Level3_2 = 10,
    Level4_0 = 11,
    Level4_1 = 12,
    Level4_2 = 13,
    Level5_0 = 14,
    Level5_1 = 15,
    Level5_2 = 16,
    Level6_0 = 17,
    Level6_1 = 18,
    Level6_2 = 19,
}

/// `enum v4l2_mpeg_video_h264_loop_filter_mode` —— `V4L2_CID_MPEG_VIDEO_H264_LOOP_FILTER_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264LoopFilterMode {
    Enabled  = 0,
    Disabled = 1,
    DisabledAtSliceBoundary = 2,
}

/// `enum v4l2_mpeg_video_h264_profile` —— `V4L2_CID_MPEG_VIDEO_H264_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264Profile {
    Baseline            = 0,
    ConstrainedBaseline = 1,
    Main                = 2,
    Extended            = 3,
    High                = 4,
    High10              = 5,
    High422             = 6,
    High444Predictive   = 7,
    High10Intra         = 8,
    High422Intra        = 9,
    High444Intra        = 10,
    Cavlc444Intra       = 11,
    ScalableBaseline    = 12,
    ScalableHigh        = 13,
    ScalableHighIntra   = 14,
    StereoHigh          = 15,
    MultiviewHigh       = 16,
    ConstrainedHigh     = 17,
}

/// `enum v4l2_mpeg_video_h264_vui_sar_idc` —— `V4L2_CID_MPEG_VIDEO_H264_VUI_SAR_IDC` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264VuiSarIdc {
    Unspecified = 0,
    Idc1x1      = 1,
    Idc12x11    = 2,
    Idc10x11    = 3,
    Idc16x11    = 4,
    Idc40x33    = 5,
    Idc24x11    = 6,
    Idc20x11    = 7,
    Idc32x11    = 8,
    Idc80x33    = 9,
    Idc18x11    = 10,
    Idc15x11    = 11,
    Idc64x33    = 12,
    Idc160x99   = 13,
    Idc4x3      = 14,
    Idc3x2      = 15,
    Idc2x1      = 16,
    Extended    = 17,
}

/// `enum v4l2_mpeg_video_h264_sei_fp_arrangement_type` —— `V4L2_CID_MPEG_VIDEO_H264_SEI_FP_ARRANGEMENT_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264SeiFpArrangementType {
    Checkerboard = 0,
    Column       = 1,
    Row          = 2,
    SideBySide   = 3,
    TopBottom    = 4,
    Temporal     = 5,
}

/// `enum v4l2_mpeg_video_h264_fmo_map_type` —— `V4L2_CID_MPEG_VIDEO_H264_FMO_MAP_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264FmoMapType {
    InterleavedSlices = 0,
    ScatteredSlices   = 1,
    ForegroundWithLeftOver = 2,
    BoxOut            = 3,
    RasterScan        = 4,
    WipeScan          = 5,
    Explicit          = 6,
}

/// `enum v4l2_mpeg_video_h264_fmo_change_dir` —— `V4L2_CID_MPEG_VIDEO_H264_FMO_CHANGE_DIRECTION` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264FmoChangeDir {
    Right = 0,
    Left  = 1,
}

/// `enum v4l2_mpeg_video_h264_hierarchical_coding_type` —— `V4L2_CID_MPEG_VIDEO_H264_HIERARCHICAL_CODING_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoH264HierarchicalCodingType {
    HierCodingB = 0,
    HierCodingP = 1,
}

/// `enum v4l2_mpeg_video_mpeg4_level` —— `V4L2_CID_MPEG_VIDEO_MPEG4_LEVEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoMpeg4Level {
    Level0  = 0,
    Level0B = 1,
    Level1  = 2,
    Level2  = 3,
    Level3  = 4,
    Level3B = 5,
    Level4  = 6,
    Level5  = 7,
}

/// `enum v4l2_mpeg_video_mpeg4_profile` —— `V4L2_CID_MPEG_VIDEO_MPEG4_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoMpeg4Profile {
    Simple         = 0,
    AdvancedSimple = 1,
    Core           = 2,
    SimpleScalable = 3,
    AdvancedCodingEfficiency = 4,
}

/// `enum v4l2_vp8_num_partitions` —— `V4L2_CID_MPEG_VIDEO_VPX_NUM_PARTITIONS` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp8NumPartitions {
    Partitions1 = 0,
    Partitions2 = 1,
    Partitions4 = 2,
    Partitions8 = 3,
}

/// `enum v4l2_vp8_num_ref_frames` —— `V4L2_CID_MPEG_VIDEO_VPX_NUM_REF_FRAMES` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp8NumRefFrames {
    RefFrame1 = 0,
    RefFrame2 = 1,
    RefFrame3 = 2,
}

/// `enum v4l2_vp8_golden_frame_sel` —— `V4L2_CID_MPEG_VIDEO_VPX_GOLDEN_FRAME_SEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp8GoldenFrameSel {
    UsePrev      = 0,
    UseRefPeriod = 1,
}

/// `enum v4l2_mpeg_video_vp8_profile` —— `V4L2_CID_MPEG_VIDEO_VP8_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoVp8Profile {
    Profile0 = 0,
    Profile1 = 1,
    Profile2 = 2,
    Profile3 = 3,
}

/// `enum v4l2_mpeg_video_vp9_profile` —— `V4L2_CID_MPEG_VIDEO_VP9_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoVp9Profile {
    Profile0 = 0,
    Profile1 = 1,
    Profile2 = 2,
    Profile3 = 3,
}

/// `enum v4l2_mpeg_video_vp9_level` —— `V4L2_CID_MPEG_VIDEO_VP9_LEVEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoVp9Level {
    Level1_0 = 0,
    Level1_1 = 1,
    Level2_0 = 2,
    Level2_1 = 3,
    Level3_0 = 4,
    Level3_1 = 5,
    Level4_0 = 6,
    Level4_1 = 7,
    Level5_0 = 8,
    Level5_1 = 9,
    Level5_2 = 10,
    Level6_0 = 11,
    Level6_1 = 12,
    Level6_2 = 13,
}

/// `enum v4l2_mpeg_video_hevc_profile` —— `V4L2_CID_MPEG_VIDEO_HEVC_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHevcProfile {
    Main             = 0,
    MainStillPicture = 1,
    Main10           = 2,
}

/// `enum v4l2_mpeg_video_hevc_level` —— `V4L2_CID_MPEG_VIDEO_HEVC_LEVEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHevcLevel {
    Level1   = 0,
    Level2   = 1,
    Level2_1 = 2,
    Level3   = 3,
    Level3_1 = 4,
    Level4   = 5,
    Level4_1 = 6,
    Level5   = 7,
    Level5_1 = 8,
    Level5_2 = 9,
    Level6   = 10,
    Level6_1 = 11,
    Level6_2 = 12,
}

/// `enum v4l2_mpeg_video_hevc_tier` —— `V4L2_CID_MPEG_VIDEO_HEVC_TIER` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHevcTier {
    Main = 0,
    High = 1,
}

/// `enum v4l2_cid_mpeg_video_hevc_loop_filter_mode` —— `V4L2_CID_MPEG_VIDEO_HEVC_LOOP_FILTER_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHevcLoopFilterMode {
    Disabled = 0,
    Enabled  = 1,
    DisabledAtSliceBoundary = 2,
}

/// `enum v4l2_cid_mpeg_video_hevc_refresh_type` —— `V4L2_CID_MPEG_VIDEO_HEVC_REFRESH_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHevcRefreshType {
    None = 0,
    Cra  = 1,
    Idr  = 2,
}

/// `enum v4l2_cid_mpeg_video_hevc_size_of_length_field` —— `V4L2_CID_MPEG_VIDEO_HEVC_SIZE_OF_LENGTH_FIELD` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoHevcSizeOfLengthField {
    Size0 = 0,
    Size1 = 1,
    Size2 = 2,
    Size4 = 3,
}

/// `enum v4l2_mpeg_video_frame_skip_mode` —— `V4L2_CID_MPEG_VIDEO_FRAME_SKIP_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoFrameSkipMode {
    Disabled   = 0,
    LevelLimit = 1,
    BufLimit   = 2,
}

/// `enum v4l2_mpeg_video_av1_profile` —— `V4L2_CID_MPEG_VIDEO_AV1_PROFILE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoAv1Profile {
    Main         = 0,
    High         = 1,
    Professional = 2,
}

/// `enum v4l2_mpeg_video_av1_level` —— `V4L2_CID_MPEG_VIDEO_AV1_LEVEL` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVideoAv1Level {
    Level2_0 = 0,
    Level2_1 = 1,
    Level2_2 = 2,
    Level2_3 = 3,
    Level3_0 = 4,
    Level3_1 = 5,
    Level3_2 = 6,
    Level3_3 = 7,
    Level4_0 = 8,
    Level4_1 = 9,
    Level4_2 = 10,
    Level4_3 = 11,
    Level5_0 = 12,
    Level5_1 = 13,
    Level5_2 = 14,
    Level5_3 = 15,
    Level6_0 = 16,
    Level6_1 = 17,
    Level6_2 = 18,
    Level6_3 = 19,
    Level7_0 = 20,
    Level7_1 = 21,
    Level7_2 = 22,
    Level7_3 = 23,
}

/// `enum v4l2_mpeg_cx2341x_video_spatial_filter_mode` —— CX2341x 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegCx2341xVideoSpatialFilterMode {
    Manual = 0,
    Auto   = 1,
}

/// `enum v4l2_mpeg_cx2341x_video_luma_spatial_filter_type` —— CX2341x 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegCx2341xVideoLumaSpatialFilterType {
    Off               = 0,
    Hor1D             = 1,
    Vert1D            = 2,
    HvSeparable2D     = 3,
    SymNonSeparable2D = 4,
}

/// `enum v4l2_mpeg_cx2341x_video_chroma_spatial_filter_type` —— CX2341x 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegCx2341xVideoChromaSpatialFilterType {
    Off   = 0,
    Hor1D = 1,
}

/// `enum v4l2_mpeg_cx2341x_video_temporal_filter_mode` —— CX2341x 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegCx2341xVideoTemporalFilterMode {
    Manual = 0,
    Auto   = 1,
}

/// `enum v4l2_mpeg_cx2341x_video_median_filter_type` —— CX2341x 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegCx2341xVideoMedianFilterType {
    Off     = 0,
    Hor     = 1,
    Vert    = 2,
    HorVert = 3,
    Diag    = 4,
}

/// `enum v4l2_mpeg_mfc51_video_frame_skip_mode` —— MFC51 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegMfc51VideoFrameSkipMode {
    Disabled   = 0,
    LevelLimit = 1,
    BufLimit   = 2,
}

/// `enum v4l2_mpeg_mfc51_video_force_frame_type` —— MFC51 控件菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegMfc51VideoForceFrameType {
    Disabled = 0,
    IFrame   = 1,
    NotCoded = 2,
}

// ── 编解码类控制 ID ───────────────────────────────────────

/// V4L2 编解码类控制 ID（`V4L2_CID_CODEC_BASE` + 偏移）。
///
/// 设计：`V4L2_CID_MPEG_STREAM_TYPE = (V4L2_CTRL_CLASS_CODEC | 0x900) + 0`。
///
/// CX2341X 与 MFC51 驱动控件使用各自的基址（`CX2341X_CID_BASE` / `MFC51_CID_BASE`）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecClassCtrl {
    // MPEG 流，特定于复用流
    MpegStreamType       = CID_BASE,
    MpegStreamPidPmt     = CID_BASE + 1,
    MpegStreamPidAudio   = CID_BASE + 2,
    MpegStreamPidVideo   = CID_BASE + 3,
    MpegStreamPidPcr     = CID_BASE + 4,
    MpegStreamPesIdAudio = CID_BASE + 5,
    MpegStreamPesIdVideo = CID_BASE + 6,
    MpegStreamVbiFmt     = CID_BASE + 7,

    // 特定于复用流的 MPEG 音频控件
    MpegAudioSamplingFreq = CID_BASE + 100,
    MpegAudioEncoding    = CID_BASE + 101,
    MpegAudioL1Bitrate   = CID_BASE + 102,
    MpegAudioL2Bitrate   = CID_BASE + 103,
    MpegAudioL3Bitrate   = CID_BASE + 104,
    MpegAudioMode        = CID_BASE + 105,
    MpegAudioModeExtension = CID_BASE + 106,
    MpegAudioEmphasis    = CID_BASE + 107,
    MpegAudioCrc         = CID_BASE + 108,
    MpegAudioMute        = CID_BASE + 109,
    MpegAudioAacBitrate  = CID_BASE + 110,
    MpegAudioAc3Bitrate  = CID_BASE + 111,
    MpegAudioDecPlayback = CID_BASE + 112,
    MpegAudioDecMultilingualPlayback = CID_BASE + 113,

    // 特定于复用流的 MPEG 视频控件
    MpegVideoEncoding    = CID_BASE + 200,
    MpegVideoAspect      = CID_BASE + 201,
    MpegVideoBFrames     = CID_BASE + 202,
    MpegVideoGopSize     = CID_BASE + 203,
    MpegVideoGopClosure  = CID_BASE + 204,
    MpegVideoPulldown    = CID_BASE + 205,
    MpegVideoBitrateMode = CID_BASE + 206,
    MpegVideoBitrate     = CID_BASE + 207,
    MpegVideoBitratePeak = CID_BASE + 208,
    MpegVideoTemporalDecimation = CID_BASE + 209,
    MpegVideoMute        = CID_BASE + 210,
    MpegVideoMuteYuv     = CID_BASE + 211,
    MpegVideoDecoderSliceInterface = CID_BASE + 212,
    MpegVideoDecoderMpeg4DeblockFilter = CID_BASE + 213,
    MpegVideoCyclicIntraRefreshMb = CID_BASE + 214,
    MpegVideoFrameRcEnable = CID_BASE + 215,
    MpegVideoHeaderMode  = CID_BASE + 216,
    MpegVideoMaxRefPic   = CID_BASE + 217,
    MpegVideoMbRcEnable  = CID_BASE + 218,
    MpegVideoMultiSliceMaxBytes = CID_BASE + 219,
    MpegVideoMultiSliceMaxMb = CID_BASE + 220,
    MpegVideoMultiSliceMode = CID_BASE + 221,
    MpegVideoVbvSize     = CID_BASE + 222,
    MpegVideoDecPts      = CID_BASE + 223,
    MpegVideoDecFrame    = CID_BASE + 224,
    MpegVideoVbvDelay    = CID_BASE + 225,
    MpegVideoRepeatSeqHeader = CID_BASE + 226,
    MpegVideoMvHSearchRange = CID_BASE + 227,
    MpegVideoMvVSearchRange = CID_BASE + 228,
    MpegVideoForceKeyFrame = CID_BASE + 229,
    MpegVideoBaselayerPriorityId = CID_BASE + 230,
    MpegVideoAuDelimiter = CID_BASE + 231,
    MpegVideoLtrCount    = CID_BASE + 232,
    MpegVideoFrameLtrIndex = CID_BASE + 233,
    MpegVideoUseLtrFrames = CID_BASE + 234,
    MpegVideoDecConcealColor = CID_BASE + 235,
    MpegVideoIntraRefreshPeriod = CID_BASE + 236,
    MpegVideoIntraRefreshPeriodType = CID_BASE + 237,
    MpegVideoBackgroundDetection = CID_BASE + 238,

    // MPEG-2 Part 2 (H.262) 编解码器控件
    MpegVideoMpeg2Level  = CID_BASE + 270,
    MpegVideoMpeg2Profile = CID_BASE + 271,

    // FWHT 编解码器控件（vicodec 驱动）
    FwhtIFrameQp         = CID_BASE + 290,
    FwhtPFrameQp         = CID_BASE + 291,

    // H.263
    MpegVideoH263IFrameQp = CID_BASE + 300,
    MpegVideoH263PFrameQp = CID_BASE + 301,
    MpegVideoH263BFrameQp = CID_BASE + 302,
    MpegVideoH263MinQp   = CID_BASE + 303,
    MpegVideoH263MaxQp   = CID_BASE + 304,

    // H.264
    MpegVideoH264IFrameQp = CID_BASE + 350,
    MpegVideoH264PFrameQp = CID_BASE + 351,
    MpegVideoH264BFrameQp = CID_BASE + 352,
    MpegVideoH264MinQp   = CID_BASE + 353,
    MpegVideoH264MaxQp   = CID_BASE + 354,
    MpegVideoH264X8Transform = CID_BASE + 355,
    MpegVideoH264CpbSize = CID_BASE + 356,
    MpegVideoH264EntropyMode = CID_BASE + 357,
    MpegVideoH264IPeriod = CID_BASE + 358,
    MpegVideoH264Level   = CID_BASE + 359,
    MpegVideoH264LoopFilterAlpha = CID_BASE + 360,
    MpegVideoH264LoopFilterBeta = CID_BASE + 361,
    MpegVideoH264LoopFilterMode = CID_BASE + 362,
    MpegVideoH264Profile = CID_BASE + 363,
    MpegVideoH264VuiExtSarHeight = CID_BASE + 364,
    MpegVideoH264VuiExtSarWidth = CID_BASE + 365,
    MpegVideoH264VuiSarEnable = CID_BASE + 366,
    MpegVideoH264VuiSarIdc = CID_BASE + 367,
    MpegVideoH264SeiFramePacking = CID_BASE + 368,
    MpegVideoH264SeiFpCurrentFrame0 = CID_BASE + 369,
    MpegVideoH264SeiFpArrangementType = CID_BASE + 370,
    MpegVideoH264Fmo     = CID_BASE + 371,
    MpegVideoH264FmoMapType = CID_BASE + 372,
    MpegVideoH264FmoSliceGroup = CID_BASE + 373,
    MpegVideoH264FmoChangeDirection = CID_BASE + 374,
    MpegVideoH264FmoChangeRate = CID_BASE + 375,
    MpegVideoH264FmoRunLength = CID_BASE + 376,
    MpegVideoH264Aso     = CID_BASE + 377,
    MpegVideoH264AsoSliceOrder = CID_BASE + 378,
    MpegVideoH264HierarchicalCoding = CID_BASE + 379,
    MpegVideoH264HierarchicalCodingType = CID_BASE + 380,
    MpegVideoH264HierarchicalCodingLayer = CID_BASE + 381,
    MpegVideoH264HierarchicalCodingLayerQp = CID_BASE + 382,
    MpegVideoH264ConstrainedIntraPrediction = CID_BASE + 383,
    MpegVideoH264ChromaQpIndexOffset = CID_BASE + 384,
    MpegVideoH264IFrameMinQp = CID_BASE + 385,
    MpegVideoH264IFrameMaxQp = CID_BASE + 386,
    MpegVideoH264PFrameMinQp = CID_BASE + 387,
    MpegVideoH264PFrameMaxQp = CID_BASE + 388,
    MpegVideoH264BFrameMinQp = CID_BASE + 389,
    MpegVideoH264BFrameMaxQp = CID_BASE + 390,
    MpegVideoH264HierCodingL0Br = CID_BASE + 391,
    MpegVideoH264HierCodingL1Br = CID_BASE + 392,
    MpegVideoH264HierCodingL2Br = CID_BASE + 393,
    MpegVideoH264HierCodingL3Br = CID_BASE + 394,
    MpegVideoH264HierCodingL4Br = CID_BASE + 395,
    MpegVideoH264HierCodingL5Br = CID_BASE + 396,
    MpegVideoH264HierCodingL6Br = CID_BASE + 397,

    // MPEG-4 Part 2
    MpegVideoMpeg4IFrameQp = CID_BASE + 400,
    MpegVideoMpeg4PFrameQp = CID_BASE + 401,
    MpegVideoMpeg4BFrameQp = CID_BASE + 402,
    MpegVideoMpeg4MinQp  = CID_BASE + 403,
    MpegVideoMpeg4MaxQp  = CID_BASE + 404,
    MpegVideoMpeg4Level  = CID_BASE + 405,
    MpegVideoMpeg4Profile = CID_BASE + 406,
    MpegVideoMpeg4Qpel   = CID_BASE + 407,

    // VP8 / VP9 流（虽非 MPEG，但归入此类）
    MpegVideoVpxNumPartitions = CID_BASE + 500,
    MpegVideoVpxImdDisable4x4 = CID_BASE + 501,
    MpegVideoVpxNumRefFrames = CID_BASE + 502,
    MpegVideoVpxFilterLevel = CID_BASE + 503,
    MpegVideoVpxFilterSharpness = CID_BASE + 504,
    MpegVideoVpxGoldenFrameRefPeriod = CID_BASE + 505,
    MpegVideoVpxGoldenFrameSel = CID_BASE + 506,
    MpegVideoVpxMinQp    = CID_BASE + 507,
    MpegVideoVpxMaxQp    = CID_BASE + 508,
    MpegVideoVpxIFrameQp = CID_BASE + 509,
    MpegVideoVpxPFrameQp = CID_BASE + 510,
    MpegVideoVp8Profile  = CID_BASE + 511,
    MpegVideoVp9Profile  = CID_BASE + 512,
    MpegVideoVp9Level    = CID_BASE + 513,

    // HEVC 编码
    MpegVideoHevcMinQp   = CID_BASE + 600,
    MpegVideoHevcMaxQp   = CID_BASE + 601,
    MpegVideoHevcIFrameQp = CID_BASE + 602,
    MpegVideoHevcPFrameQp = CID_BASE + 603,
    MpegVideoHevcBFrameQp = CID_BASE + 604,
    MpegVideoHevcHierQp  = CID_BASE + 605,
    MpegVideoHevcHierCodingType = CID_BASE + 606,
    MpegVideoHevcHierCodingLayer = CID_BASE + 607,
    MpegVideoHevcHierCodingL0Qp = CID_BASE + 608,
    MpegVideoHevcHierCodingL1Qp = CID_BASE + 609,
    MpegVideoHevcHierCodingL2Qp = CID_BASE + 610,
    MpegVideoHevcHierCodingL3Qp = CID_BASE + 611,
    MpegVideoHevcHierCodingL4Qp = CID_BASE + 612,
    MpegVideoHevcHierCodingL5Qp = CID_BASE + 613,
    MpegVideoHevcHierCodingL6Qp = CID_BASE + 614,
    MpegVideoHevcProfile = CID_BASE + 615,
    MpegVideoHevcLevel   = CID_BASE + 616,
    MpegVideoHevcFrameRateResolution = CID_BASE + 617,
    MpegVideoHevcTier    = CID_BASE + 618,
    MpegVideoHevcMaxPartitionDepth = CID_BASE + 619,
    MpegVideoHevcLoopFilterMode = CID_BASE + 620,
    MpegVideoHevcLfBetaOffsetDiv2 = CID_BASE + 621,
    MpegVideoHevcLfTcOffsetDiv2 = CID_BASE + 622,
    MpegVideoHevcRefreshType = CID_BASE + 623,
    MpegVideoHevcRefreshPeriod = CID_BASE + 624,
    MpegVideoHevcLosslessCu = CID_BASE + 625,
    MpegVideoHevcConstIntraPred = CID_BASE + 626,
    MpegVideoHevcWavefront = CID_BASE + 627,
    MpegVideoHevcGeneralPb = CID_BASE + 628,
    MpegVideoHevcTemporalId = CID_BASE + 629,
    MpegVideoHevcStrongSmoothing = CID_BASE + 630,
    MpegVideoHevcMaxNumMergeMvMinus1 = CID_BASE + 631,
    MpegVideoHevcIntraPuSplit = CID_BASE + 632,
    MpegVideoHevcTmvPrediction = CID_BASE + 633,
    MpegVideoHevcWithoutStartcode = CID_BASE + 634,
    MpegVideoHevcSizeOfLengthField = CID_BASE + 635,
    MpegVideoHevcHierCodingL0Br = CID_BASE + 636,
    MpegVideoHevcHierCodingL1Br = CID_BASE + 637,
    MpegVideoHevcHierCodingL2Br = CID_BASE + 638,
    MpegVideoHevcHierCodingL3Br = CID_BASE + 639,
    MpegVideoHevcHierCodingL4Br = CID_BASE + 640,
    MpegVideoHevcHierCodingL5Br = CID_BASE + 641,
    MpegVideoHevcHierCodingL6Br = CID_BASE + 642,
    MpegVideoRefNumberForPFrames = CID_BASE + 643,
    MpegVideoPrependSpsppsToIdr = CID_BASE + 644,
    MpegVideoConstantQuality = CID_BASE + 645,
    MpegVideoFrameSkipMode = CID_BASE + 646,
    MpegVideoHevcIFrameMinQp = CID_BASE + 647,
    MpegVideoHevcIFrameMaxQp = CID_BASE + 648,
    MpegVideoHevcPFrameMinQp = CID_BASE + 649,
    MpegVideoHevcPFrameMaxQp = CID_BASE + 650,
    MpegVideoHevcBFrameMinQp = CID_BASE + 651,
    MpegVideoHevcBFrameMaxQp = CID_BASE + 652,
    MpegVideoDecDisplayDelay = CID_BASE + 653,
    MpegVideoDecDisplayDelayEnable = CID_BASE + 654,
    MpegVideoAv1Profile  = CID_BASE + 655,
    MpegVideoAv1Level    = CID_BASE + 656,
    MpegVideoAverageQp   = CID_BASE + 657,

    // CX2341x 驱动控件
    MpegCx2341xVideoSpatialFilterMode = CX2341X_CID_BASE,
    MpegCx2341xVideoSpatialFilter = CX2341X_CID_BASE + 1,
    MpegCx2341xVideoLumaSpatialFilterType = CX2341X_CID_BASE + 2,
    MpegCx2341xVideoChromaSpatialFilterType = CX2341X_CID_BASE + 3,
    MpegCx2341xVideoTemporalFilterMode = CX2341X_CID_BASE + 4,
    MpegCx2341xVideoTemporalFilter = CX2341X_CID_BASE + 5,
    MpegCx2341xVideoMedianFilterType = CX2341X_CID_BASE + 6,
    MpegCx2341xVideoLumaMedianFilterBottom = CX2341X_CID_BASE + 7,
    MpegCx2341xVideoLumaMedianFilterTop = CX2341X_CID_BASE + 8,
    MpegCx2341xVideoChromaMedianFilterBottom = CX2341X_CID_BASE + 9,
    MpegCx2341xVideoChromaMedianFilterTop = CX2341X_CID_BASE + 10,
    MpegCx2341xStreamInsertNavPackets = CX2341X_CID_BASE + 11,

    // Samsung MFC 5.1 驱动控件
    MpegMfc51VideoDecoderH264DisplayDelay = MFC51_CID_BASE,
    MpegMfc51VideoDecoderH264DisplayDelayEnable = MFC51_CID_BASE + 1,
    MpegMfc51VideoFrameSkipMode = MFC51_CID_BASE + 2,
    MpegMfc51VideoForceFrameType = MFC51_CID_BASE + 3,
    MpegMfc51VideoPadding = MFC51_CID_BASE + 4,
    MpegMfc51VideoPaddingYuv = MFC51_CID_BASE + 5,
    MpegMfc51VideoRcFixedTargetBit = MFC51_CID_BASE + 6,
    MpegMfc51VideoRcReactionCoeff = MFC51_CID_BASE + 7,
    MpegMfc51VideoH264AdaptiveRcActivity = MFC51_CID_BASE + 50,
    MpegMfc51VideoH264AdaptiveRcDark = MFC51_CID_BASE + 51,
    MpegMfc51VideoH264AdaptiveRcSmooth = MFC51_CID_BASE + 52,
    MpegMfc51VideoH264AdaptiveRcStatic = MFC51_CID_BASE + 53,
    MpegMfc51VideoH264NumRefPicForP = MFC51_CID_BASE + 54,
}

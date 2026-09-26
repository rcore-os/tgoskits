//! 无状态编解码控件。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_CODEC_STATELESS` —— 无状态编解码控件。
pub const CLASS_ID: u32 = CtrlClass::CodecStateless as u32;

/// `V4L2_CID_CODEC_STATELESS_CLASS = (V4L2_CTRL_CLASS_CODEC_STATELESS | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_CODEC_STATELESS_BASE = (V4L2_CTRL_CLASS_CODEC_STATELESS | 0x900) = 0x00a40900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

// ── 无状态编解码控制 ID ───────────────────────────────────

/// V4L2 无状态编解码类控制 ID（`V4L2_CID_CODEC_STATELESS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecStatelessClassCtrl {
    // H.264
    StatelessH264DecodeMode = CID_BASE,
    StatelessH264StartCode = CID_BASE + 1,
    StatelessH264Sps     = CID_BASE + 2,
    StatelessH264Pps     = CID_BASE + 3,
    StatelessH264ScalingMatrix = CID_BASE + 4,
    StatelessH264PredWeights = CID_BASE + 5,
    StatelessH264SliceParams = CID_BASE + 6,
    StatelessH264DecodeParams = CID_BASE + 7,

    // FWHT（vicodec 驱动）
    StatelessFwhtParams  = CID_BASE + 100,

    // VP8
    StatelessVp8Frame    = CID_BASE + 200,

    // MPEG-2
    StatelessMpeg2Sequence = CID_BASE + 220,
    StatelessMpeg2Picture = CID_BASE + 221,
    StatelessMpeg2Quantisation = CID_BASE + 222,

    // VP9
    StatelessVp9Frame    = CID_BASE + 300,
    StatelessVp9CompressedHdr = CID_BASE + 301,

    // HEVC
    StatelessHevcSps     = CID_BASE + 400,
    StatelessHevcPps     = CID_BASE + 401,
    StatelessHevcSliceParams = CID_BASE + 402,
    StatelessHevcScalingMatrix = CID_BASE + 403,
    StatelessHevcDecodeParams = CID_BASE + 404,
    StatelessHevcDecodeMode = CID_BASE + 405,
    StatelessHevcStartCode = CID_BASE + 406,
    StatelessHevcEntryPointOffsets = CID_BASE + 407,
    StatelessHevcExtSpsStRps = CID_BASE + 408,
    StatelessHevcExtSpsLtRps = CID_BASE + 409,

    // AV1
    StatelessAv1Sequence = CID_BASE + 500,
    StatelessAv1TileGroupEntry = CID_BASE + 501,
    StatelessAv1Frame    = CID_BASE + 502,
    StatelessAv1FilmGrain = CID_BASE + 505,
}

// ── H.264 ────────────────────────────────────────────────

/// `enum v4l2_stateless_h264_decode_mode` —— `V4L2_CID_STATELESS_H264_DECODE_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatelessH264DecodeMode {
    SliceBased = 0,
    FrameBased = 1,
}

/// `enum v4l2_stateless_h264_start_code` —— `V4L2_CID_STATELESS_H264_START_CODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatelessH264StartCode {
    None   = 0,
    AnnexB = 1,
}

/// `V4L2_H264_SPS_CONSTRAINT_SET*_FLAG`。
pub mod h264_sps_constraint_set {
    pub const SET0: u32 = 0x01;
    pub const SET1: u32 = 0x02;
    pub const SET2: u32 = 0x04;
    pub const SET3: u32 = 0x08;
    pub const SET4: u32 = 0x10;
    pub const SET5: u32 = 0x20;
}

/// `V4L2_H264_SPS_FLAG_*`。
pub mod h264_sps_flag {
    pub const SEPARATE_COLOUR_PLANE: u32 = 0x01;
    pub const QPPRIME_Y_ZERO_TRANSFORM_BYPASS: u32 = 0x02;
    pub const DELTA_PIC_ORDER_ALWAYS_ZERO: u32 = 0x04;
    pub const GAPS_IN_FRAME_NUM_VALUE_ALLOWED: u32 = 0x08;
    pub const FRAME_MBS_ONLY: u32 = 0x10;
    pub const MB_ADAPTIVE_FRAME_FIELD: u32 = 0x20;
    pub const DIRECT_8X8_INFERENCE: u32 = 0x40;
}

/// `V4L2_H264_PPS_FLAG_*`。
pub mod h264_pps_flag {
    pub const ENTROPY_CODING_MODE: u32 = 0x0001;
    pub const BOTTOM_FIELD_PIC_ORDER_IN_FRAME_PRESENT: u32 = 0x0002;
    pub const WEIGHTED_PRED: u32 = 0x0004;
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u32 = 0x0008;
    pub const CONSTRAINED_INTRA_PRED: u32 = 0x0010;
    pub const REDUNDANT_PIC_CNT_PRESENT: u32 = 0x0020;
    pub const TRANSFORM_8X8_MODE: u32 = 0x0040;
    pub const SCALING_MATRIX_PRESENT: u32 = 0x0080;
}

/// `V4L2_H264_TOP_FIELD_REF` / `V4L2_H264_BOTTOM_FIELD_REF` / `V4L2_H264_FRAME_REF`。
pub mod h264_field_ref {
    pub const TOP: u8 = 0x1;
    pub const BOTTOM: u8 = 0x2;
    pub const FRAME: u8 = 0x3;
}

/// `V4L2_H264_NUM_DPB_ENTRIES` —— 最大 DPB 条目数。
pub const H264_NUM_DPB_ENTRIES: usize = 16;
/// `V4L2_H264_REF_LIST_LEN = (2 * V4L2_H264_NUM_DPB_ENTRIES)`。
pub const H264_REF_LIST_LEN: usize = 2 * H264_NUM_DPB_ENTRIES;

/// `V4L2_H264_SLICE_TYPE_*`。
pub mod h264_slice_type {
    pub const P: u8 = 0;
    pub const B: u8 = 1;
    pub const I: u8 = 2;
    pub const SP: u8 = 3;
    pub const SI: u8 = 4;
}

/// `V4L2_H264_SLICE_FLAG_*`。
pub mod h264_slice_flag {
    pub const DIRECT_SPATIAL_MV_PRED: u32 = 0x01;
    pub const SP_FOR_SWITCH: u32 = 0x02;
}

/// `V4L2_H264_DPB_ENTRY_FLAG_*`。
pub mod h264_dpb_entry_flag {
    pub const VALID: u8 = 0x01;
    pub const ACTIVE: u8 = 0x02;
    pub const LONG_TERM: u8 = 0x04;
    pub const FIELD: u8 = 0x08;
}

/// `V4L2_H264_DECODE_PARAM_FLAG_*`。
pub mod h264_decode_param_flag {
    pub const IDR_PIC: u32 = 0x01;
    pub const FIELD_PIC: u32 = 0x02;
    pub const BOTTOM_FIELD: u32 = 0x04;
    pub const PFRAME: u32 = 0x08;
    pub const BFRAME: u32 = 0x10;
}

/// `struct v4l2_ctrl_h264_sps` —— H.264 序列参数集。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlH264Sps {
    pub profile_idc: u8,
    pub constraint_set_flags: u8,
    pub level_idc: u8,
    pub seq_parameter_set_id: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub log2_max_frame_num_minus4: u8,
    pub pic_order_cnt_type: u8,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub max_num_ref_frames: u8,
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    pub offset_for_ref_frame: [i32; 255],
    pub offset_for_non_ref_pic: i32,
    pub offset_for_top_to_bottom_field: i32,
    pub pic_width_in_mbs_minus1: u16,
    pub pic_height_in_map_units_minus1: u16,
    pub flags: u32,
}

/// `struct v4l2_ctrl_h264_pps` —— H.264 图像参数集。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlH264Pps {
    pub pic_parameter_set_id: u8,
    pub seq_parameter_set_id: u8,
    pub num_slice_groups_minus1: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub weighted_bipred_idc: u8,
    pub pic_init_qp_minus26: i8,
    pub pic_init_qs_minus26: i8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub flags: u16,
}

/// `struct v4l2_ctrl_h264_scaling_matrix` —— H.264 缩放矩阵。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlH264ScalingMatrix {
    pub scaling_list_4x4: [[u8; 16]; 6],
    pub scaling_list_8x8: [[u8; 64]; 6],
}

/// `struct v4l2_h264_weight_factors` —— H.264 加权预测因子。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct H264WeightFactors {
    pub luma_weight: [i16; 32],
    pub luma_offset: [i16; 32],
    pub chroma_weight: [[i16; 2]; 32],
    pub chroma_offset: [[i16; 2]; 32],
}

/// `struct v4l2_ctrl_h264_pred_weights` —— H.264 加权预测表。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlH264PredWeights {
    pub luma_log2_weight_denom: u16,
    pub chroma_log2_weight_denom: u16,
    pub weight_factors: [H264WeightFactors; 2],
}

/// `struct v4l2_h264_reference` —— H.264 图像参考。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct H264Reference {
    pub fields: u8,
    pub index: u8,
}

/// `struct v4l2_ctrl_h264_slice_params` —— H.264 条带参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlH264SliceParams {
    pub header_bit_size: u32,
    pub first_mb_in_slice: u32,
    pub slice_type: u8,
    pub colour_plane_id: u8,
    pub redundant_pic_cnt: u8,
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub slice_qs_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub reserved: u8,
    pub ref_pic_list0: [H264Reference; H264_REF_LIST_LEN],
    pub ref_pic_list1: [H264Reference; H264_REF_LIST_LEN],
    pub flags: u32,
}

/// `struct v4l2_h264_dpb_entry` —— H.264 解码图像缓冲条目。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct H264DpbEntry {
    pub reference_ts: u64,
    pub pic_num: u32,
    pub frame_num: u16,
    pub fields: u8,
    pub reserved: [u8; 5],
    pub top_field_order_cnt: i32,
    pub bottom_field_order_cnt: i32,
    pub flags: u32,
}

/// `struct v4l2_ctrl_h264_decode_params` —— H.264 解码参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlH264DecodeParams {
    pub dpb: [H264DpbEntry; H264_NUM_DPB_ENTRIES],
    pub nal_ref_idc: u16,
    pub frame_num: u16,
    pub top_field_order_cnt: i32,
    pub bottom_field_order_cnt: i32,
    pub idr_pic_id: u16,
    pub pic_order_cnt_lsb: u16,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt0: i32,
    pub delta_pic_order_cnt1: i32,
    pub dec_ref_pic_marking_bit_size: u32,
    pub pic_order_cnt_bit_size: u32,
    pub slice_group_change_cycle: u32,
    pub reserved: u32,
    pub flags: u32,
}

// ── FWHT ─────────────────────────────────────────────────

/// `V4L2_FWHT_VERSION` —— 当前 FWHT 版本。
pub const FWHT_VERSION: u32 = 3;

/// `V4L2_FWHT_FL_*` 标志（`_BITUL`，64 位）。
pub mod fwht_flag {
    pub const IS_INTERLACED: u64 = 1 << 0;
    pub const IS_BOTTOM_FIRST: u64 = 1 << 1;
    pub const IS_ALTERNATE: u64 = 1 << 2;
    pub const IS_BOTTOM_FIELD: u64 = 1 << 3;
    pub const LUMA_IS_UNCOMPRESSED: u64 = 1 << 4;
    pub const CB_IS_UNCOMPRESSED: u64 = 1 << 5;
    pub const CR_IS_UNCOMPRESSED: u64 = 1 << 6;
    pub const CHROMA_FULL_HEIGHT: u64 = 1 << 7;
    pub const CHROMA_FULL_WIDTH: u64 = 1 << 8;
    pub const ALPHA_IS_UNCOMPRESSED: u64 = 1 << 9;
    pub const I_FRAME: u64 = 1 << 10;
    pub const COMPONENTS_NUM_MSK: u64 = 0x0007_0000;
    pub const COMPONENTS_NUM_OFFSET: u64 = 16;
    pub const PIXENC_MSK: u64 = 0x0018_0000;
    pub const PIXENC_OFFSET: u64 = 19;
    pub const PIXENC_YUV: u64 = 1 << 19;
    pub const PIXENC_RGB: u64 = 2 << 19;
    pub const PIXENC_HSV: u64 = 3 << 19;
}

/// `struct v4l2_ctrl_fwht_params` —— FWHT 参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlFwhtParams {
    pub backward_ref_ts: u64,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub flags: u32,
    pub colorspace: u32,
    pub xfer_func: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
}

// ── VP8 ─────────────────────────────────────────────────

/// `V4L2_VP8_SEGMENT_FLAG_*`。
pub mod vp8_segment_flag {
    pub const ENABLED: u32 = 0x01;
    pub const UPDATE_MAP: u32 = 0x02;
    pub const UPDATE_FEATURE_DATA: u32 = 0x04;
    pub const DELTA_VALUE_MODE: u32 = 0x08;
}

/// `struct v4l2_vp8_segment` —— VP8 分段调整参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp8Segment {
    pub quant_update: [i8; 4],
    pub lf_update: [i8; 4],
    pub segment_probs: [u8; 3],
    pub padding: u8,
    pub flags: u32,
}

/// `V4L2_VP8_LF_*` 标志。
pub mod vp8_lf_flag {
    pub const ADJ_ENABLE: u32 = 0x01;
    pub const DELTA_UPDATE: u32 = 0x02;
    pub const FILTER_TYPE_SIMPLE: u32 = 0x04;
}

/// `struct v4l2_vp8_loop_filter` —— VP8 环路滤波参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp8LoopFilter {
    pub ref_frm_delta: [i8; 4],
    pub mb_mode_delta: [i8; 4],
    pub sharpness_level: u8,
    pub level: u8,
    pub padding: u16,
    pub flags: u32,
}

/// `struct v4l2_vp8_quantization` —— VP8 量化索引。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp8Quantization {
    pub y_ac_qi: u8,
    pub y_dc_delta: i8,
    pub y2_dc_delta: i8,
    pub y2_ac_delta: i8,
    pub uv_dc_delta: i8,
    pub uv_ac_delta: i8,
    pub padding: u16,
}

/// `V4L2_VP8_COEFF_PROB_CNT` / `V4L2_VP8_MV_PROB_CNT`。
pub const VP8_COEFF_PROB_CNT: usize = 11;
pub const VP8_MV_PROB_CNT: usize = 19;

/// `struct v4l2_vp8_entropy` —— VP8 概率更新。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp8Entropy {
    pub coeff_probs: [[[[u8; VP8_COEFF_PROB_CNT]; 3]; 8]; 4],
    pub y_mode_probs: [u8; 4],
    pub uv_mode_probs: [u8; 3],
    pub mv_probs: [[u8; VP8_MV_PROB_CNT]; 2],
    pub padding: [u8; 3],
}

/// `struct v4l2_vp8_entropy_coder_state` —— VP8 布尔熵编码器状态。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp8EntropyCoderState {
    pub range: u8,
    pub value: u8,
    pub bit_count: u8,
    pub padding: u8,
}

/// `V4L2_VP8_FRAME_FLAG_*`。
pub mod vp8_frame_flag {
    pub const KEY_FRAME: u32 = 0x01;
    pub const EXPERIMENTAL: u32 = 0x02;
    pub const SHOW_FRAME: u32 = 0x04;
    pub const MB_NO_SKIP_COEFF: u32 = 0x08;
    pub const SIGN_BIAS_GOLDEN: u32 = 0x10;
    pub const SIGN_BIAS_ALT: u32 = 0x20;
}

/// `struct v4l2_ctrl_vp8_frame` —— VP8 帧参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlVp8Frame {
    pub segment: Vp8Segment,
    pub lf: Vp8LoopFilter,
    pub quant: Vp8Quantization,
    pub entropy: Vp8Entropy,
    pub coder_state: Vp8EntropyCoderState,
    pub width: u16,
    pub height: u16,
    pub horizontal_scale: u8,
    pub vertical_scale: u8,
    pub version: u8,
    pub prob_skip_false: u8,
    pub prob_intra: u8,
    pub prob_last: u8,
    pub prob_gf: u8,
    pub num_dct_parts: u8,
    pub first_part_size: u32,
    pub first_part_header_bits: u32,
    pub dct_part_sizes: [u32; 8],
    pub last_frame_ts: u64,
    pub golden_frame_ts: u64,
    pub alt_frame_ts: u64,
    pub flags: u64,
}

// ── MPEG-2 ───────────────────────────────────────────────

/// `V4L2_MPEG2_SEQ_FLAG_PROGRESSIVE`。
pub const MPEG2_SEQ_FLAG_PROGRESSIVE: u8 = 0x01;

/// `V4L2_MPEG2_PIC_CODING_TYPE_*`。
pub mod mpeg2_pic_coding_type {
    pub const I: u8 = 1;
    pub const P: u8 = 2;
    pub const B: u8 = 3;
    pub const D: u8 = 4;
}

/// `V4L2_MPEG2_PIC_TOP_FIELD` / `V4L2_MPEG2_PIC_BOTTOM_FIELD` / `V4L2_MPEG2_PIC_FRAME`。
pub mod mpeg2_pic_field {
    pub const TOP: u8 = 0x1;
    pub const BOTTOM: u8 = 0x2;
    pub const FRAME: u8 = 0x3;
}

/// `V4L2_MPEG2_PIC_FLAG_*`。
pub mod mpeg2_pic_flag {
    pub const TOP_FIELD_FIRST: u32 = 0x0001;
    pub const FRAME_PRED_DCT: u32 = 0x0002;
    pub const CONCEALMENT_MV: u32 = 0x0004;
    pub const Q_SCALE_TYPE: u32 = 0x0008;
    pub const INTRA_VLC: u32 = 0x0010;
    pub const ALT_SCAN: u32 = 0x0020;
    pub const REPEAT_FIRST: u32 = 0x0040;
    pub const PROGRESSIVE: u32 = 0x0080;
}

/// `struct v4l2_ctrl_mpeg2_sequence` —— MPEG-2 序列头。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlMpeg2Sequence {
    pub horizontal_size: u16,
    pub vertical_size: u16,
    pub vbv_buffer_size: u32,
    pub profile_and_level_indication: u16,
    pub chroma_format: u8,
    pub flags: u8,
}

/// `struct v4l2_ctrl_mpeg2_picture` —— MPEG-2 图像头。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlMpeg2Picture {
    pub backward_ref_ts: u64,
    pub forward_ref_ts: u64,
    pub flags: u32,
    pub f_code: [[u8; 2]; 2],
    pub picture_coding_type: u8,
    pub picture_structure: u8,
    pub intra_dc_precision: u8,
    pub reserved: [u8; 5],
}

/// `struct v4l2_ctrl_mpeg2_quantisation` —— MPEG-2 量化矩阵。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlMpeg2Quantisation {
    pub intra_quantiser_matrix: [u8; 64],
    pub non_intra_quantiser_matrix: [u8; 64],
    pub chroma_intra_quantiser_matrix: [u8; 64],
    pub chroma_non_intra_quantiser_matrix: [u8; 64],
}

// ── HEVC ─────────────────────────────────────────────────

/// `enum v4l2_stateless_hevc_decode_mode` —— `V4L2_CID_STATELESS_HEVC_DECODE_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatelessHevcDecodeMode {
    SliceBased = 0,
    FrameBased = 1,
}

/// `enum v4l2_stateless_hevc_start_code` —— `V4L2_CID_STATELESS_HEVC_START_CODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatelessHevcStartCode {
    None   = 0,
    AnnexB = 1,
}

/// `V4L2_HEVC_SLICE_TYPE_*`。
pub mod hevc_slice_type {
    pub const B: u8 = 0;
    pub const P: u8 = 1;
    pub const I: u8 = 2;
}

/// `V4L2_HEVC_SPS_FLAG_*`（64 位）。
pub mod hevc_sps_flag {
    pub const SEPARATE_COLOUR_PLANE: u64 = 1 << 0;
    pub const SCALING_LIST_ENABLED: u64 = 1 << 1;
    pub const AMP_ENABLED: u64 = 1 << 2;
    pub const SAMPLE_ADAPTIVE_OFFSET: u64 = 1 << 3;
    pub const PCM_ENABLED: u64 = 1 << 4;
    pub const PCM_LOOP_FILTER_DISABLED: u64 = 1 << 5;
    pub const LONG_TERM_REF_PICS_PRESENT: u64 = 1 << 6;
    pub const SPS_TEMPORAL_MVP_ENABLED: u64 = 1 << 7;
    pub const STRONG_INTRA_SMOOTHING_ENABLED: u64 = 1 << 8;
}

/// `struct v4l2_ctrl_hevc_sps` —— HEVC 序列参数集。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcSps {
    pub video_parameter_set_id: u8,
    pub seq_parameter_set_id: u8,
    pub pic_width_in_luma_samples: u16,
    pub pic_height_in_luma_samples: u16,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub sps_max_dec_pic_buffering_minus1: u8,
    pub sps_max_num_reorder_pics: u8,
    pub sps_max_latency_increase_plus1: u8,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_luma_transform_block_size_minus2: u8,
    pub log2_diff_max_min_luma_transform_block_size: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub pcm_sample_bit_depth_luma_minus1: u8,
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    pub num_short_term_ref_pic_sets: u8,
    pub num_long_term_ref_pics_sps: u8,
    pub chroma_format_idc: u8,
    pub sps_max_sub_layers_minus1: u8,
    pub reserved: [u8; 6],
    pub flags: u64,
}

/// `V4L2_HEVC_PPS_FLAG_*`（64 位）。
pub mod hevc_pps_flag {
    pub const DEPENDENT_SLICE_SEGMENT_ENABLED: u64 = 1 << 0;
    pub const OUTPUT_FLAG_PRESENT: u64 = 1 << 1;
    pub const SIGN_DATA_HIDING_ENABLED: u64 = 1 << 2;
    pub const CABAC_INIT_PRESENT: u64 = 1 << 3;
    pub const CONSTRAINED_INTRA_PRED: u64 = 1 << 4;
    pub const TRANSFORM_SKIP_ENABLED: u64 = 1 << 5;
    pub const CU_QP_DELTA_ENABLED: u64 = 1 << 6;
    pub const PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT: u64 = 1 << 7;
    pub const WEIGHTED_PRED: u64 = 1 << 8;
    pub const WEIGHTED_BIPRED: u64 = 1 << 9;
    pub const TRANSQUANT_BYPASS_ENABLED: u64 = 1 << 10;
    pub const TILES_ENABLED: u64 = 1 << 11;
    pub const ENTROPY_CODING_SYNC_ENABLED: u64 = 1 << 12;
    pub const LOOP_FILTER_ACROSS_TILES_ENABLED: u64 = 1 << 13;
    pub const PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED: u64 = 1 << 14;
    pub const DEBLOCKING_FILTER_OVERRIDE_ENABLED: u64 = 1 << 15;
    pub const PPS_DISABLE_DEBLOCKING_FILTER: u64 = 1 << 16;
    pub const LISTS_MODIFICATION_PRESENT: u64 = 1 << 17;
    pub const SLICE_SEGMENT_HEADER_EXTENSION_PRESENT: u64 = 1 << 18;
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u64 = 1 << 19;
    pub const UNIFORM_SPACING: u64 = 1 << 20;
}

/// `struct v4l2_ctrl_hevc_pps` —— HEVC 图像参数集。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcPps {
    pub pic_parameter_set_id: u8,
    pub num_extra_slice_header_bits: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub init_qp_minus26: i8,
    pub diff_cu_qp_delta_depth: u8,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u8; 20],
    pub row_height_minus1: [u8; 22],
    pub pps_beta_offset_div2: i8,
    pub pps_tc_offset_div2: i8,
    pub log2_parallel_merge_level_minus2: u8,
    pub reserved: u8,
    pub flags: u64,
}

/// `V4L2_HEVC_DPB_ENTRY_LONG_TERM_REFERENCE`。
pub const HEVC_DPB_ENTRY_LONG_TERM_REFERENCE: u8 = 0x01;

/// `V4L2_HEVC_SEI_PIC_STRUCT_*`。
pub mod hevc_sei_pic_struct {
    pub const FRAME: u8 = 0;
    pub const TOP_FIELD: u8 = 1;
    pub const BOTTOM_FIELD: u8 = 2;
    pub const TOP_BOTTOM: u8 = 3;
    pub const BOTTOM_TOP: u8 = 4;
    pub const TOP_BOTTOM_TOP: u8 = 5;
    pub const BOTTOM_TOP_BOTTOM: u8 = 6;
    pub const FRAME_DOUBLING: u8 = 7;
    pub const FRAME_TRIPLING: u8 = 8;
    pub const TOP_PAIRED_PREVIOUS_BOTTOM: u8 = 9;
    pub const BOTTOM_PAIRED_PREVIOUS_TOP: u8 = 10;
    pub const TOP_PAIRED_NEXT_BOTTOM: u8 = 11;
    pub const BOTTOM_PAIRED_NEXT_TOP: u8 = 12;
}

/// `V4L2_HEVC_DPB_ENTRIES_NUM_MAX` —— 最大 DPB 条目数。
pub const HEVC_DPB_ENTRIES_NUM_MAX: usize = 16;

/// `struct v4l2_hevc_dpb_entry` —— HEVC 解码图像缓冲条目。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HevcDpbEntry {
    pub timestamp: u64,
    pub flags: u8,
    pub field_pic: u8,
    pub reserved: u16,
    pub pic_order_cnt_val: i32,
}

/// `struct v4l2_hevc_pred_weight_table` —— HEVC 加权预测参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HevcPredWeightTable {
    pub delta_luma_weight_l0: [i8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub luma_offset_l0: [i8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub delta_chroma_weight_l0: [[i8; 2]; HEVC_DPB_ENTRIES_NUM_MAX],
    pub chroma_offset_l0: [[i8; 2]; HEVC_DPB_ENTRIES_NUM_MAX],
    pub delta_luma_weight_l1: [i8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub luma_offset_l1: [i8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub delta_chroma_weight_l1: [[i8; 2]; HEVC_DPB_ENTRIES_NUM_MAX],
    pub chroma_offset_l1: [[i8; 2]; HEVC_DPB_ENTRIES_NUM_MAX],
    pub luma_log2_weight_denom: u8,
    pub delta_chroma_log2_weight_denom: i8,
}

/// `V4L2_HEVC_SLICE_PARAMS_FLAG_*`（64 位）。
pub mod hevc_slice_params_flag {
    pub const SLICE_SAO_LUMA: u64 = 1 << 0;
    pub const SLICE_SAO_CHROMA: u64 = 1 << 1;
    pub const SLICE_TEMPORAL_MVP_ENABLED: u64 = 1 << 2;
    pub const MVD_L1_ZERO: u64 = 1 << 3;
    pub const CABAC_INIT: u64 = 1 << 4;
    pub const COLLOCATED_FROM_L0: u64 = 1 << 5;
    pub const USE_INTEGER_MV: u64 = 1 << 6;
    pub const SLICE_DEBLOCKING_FILTER_DISABLED: u64 = 1 << 7;
    pub const SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED: u64 = 1 << 8;
    pub const DEPENDENT_SLICE_SEGMENT: u64 = 1 << 9;
}

/// `struct v4l2_ctrl_hevc_slice_params` —— HEVC 条带参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcSliceParams {
    pub bit_size: u32,
    pub data_byte_offset: u32,
    pub num_entry_point_offsets: u32,
    pub nal_unit_type: u8,
    pub nuh_temporal_id_plus1: u8,
    pub slice_type: u8,
    pub colour_plane_id: u8,
    pub slice_pic_order_cnt: i32,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub collocated_ref_idx: u8,
    pub five_minus_max_num_merge_cand: u8,
    pub slice_qp_delta: i8,
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
    pub slice_act_y_qp_offset: i8,
    pub slice_act_cb_qp_offset: i8,
    pub slice_act_cr_qp_offset: i8,
    pub slice_beta_offset_div2: i8,
    pub slice_tc_offset_div2: i8,
    pub pic_struct: u8,
    pub reserved0: [u8; 3],
    pub slice_segment_addr: u32,
    pub ref_idx_l0: [u8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub ref_idx_l1: [u8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub short_term_ref_pic_set_size: u16,
    pub long_term_ref_pic_set_size: u16,
    pub pred_weight_table: HevcPredWeightTable,
    pub reserved1: [u8; 2],
    pub flags: u64,
}

/// `V4L2_HEVC_DECODE_PARAM_FLAG_*`。
pub mod hevc_decode_param_flag {
    pub const IRAP_PIC: u32 = 0x1;
    pub const IDR_PIC: u32 = 0x2;
    pub const NO_OUTPUT_OF_PRIOR: u32 = 0x4;
}

/// `struct v4l2_ctrl_hevc_decode_params` —— HEVC 解码参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcDecodeParams {
    pub pic_order_cnt_val: i32,
    pub short_term_ref_pic_set_size: u16,
    pub long_term_ref_pic_set_size: u16,
    pub num_active_dpb_entries: u8,
    pub num_poc_st_curr_before: u8,
    pub num_poc_st_curr_after: u8,
    pub num_poc_lt_curr: u8,
    pub poc_st_curr_before: [u8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub poc_st_curr_after: [u8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub poc_lt_curr: [u8; HEVC_DPB_ENTRIES_NUM_MAX],
    pub num_delta_pocs_of_ref_rps_idx: u8,
    pub reserved: [u8; 3],
    pub dpb: [HevcDpbEntry; HEVC_DPB_ENTRIES_NUM_MAX],
    pub flags: u64,
}

/// `struct v4l2_ctrl_hevc_scaling_matrix` —— HEVC 缩放列表参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcScalingMatrix {
    pub scaling_list_4x4: [[u8; 16]; 6],
    pub scaling_list_8x8: [[u8; 64]; 6],
    pub scaling_list_16x16: [[u8; 64]; 6],
    pub scaling_list_32x32: [[u8; 64]; 2],
    pub scaling_list_dc_coef_16x16: [u8; 6],
    pub scaling_list_dc_coef_32x32: [u8; 2],
}

/// `V4L2_HEVC_EXT_SPS_ST_RPS_FLAG_INTER_REF_PIC_SET_PRED`。
pub const HEVC_EXT_SPS_ST_RPS_FLAG_INTER_REF_PIC_SET_PRED: u16 = 0x1;

/// `struct v4l2_ctrl_hevc_ext_sps_st_rps` —— HEVC 短期参考图像集参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcExtSpsStRps {
    pub delta_idx_minus1: u8,
    pub delta_rps_sign: u8,
    pub num_negative_pics: u8,
    pub num_positive_pics: u8,
    pub used_by_curr_pic: u32,
    pub use_delta_flag: u32,
    pub abs_delta_rps_minus1: u16,
    pub delta_poc_s0_minus1: [u16; 16],
    pub delta_poc_s1_minus1: [u16; 16],
    pub flags: u16,
}

/// `V4L2_HEVC_EXT_SPS_LT_RPS_FLAG_USED_LT`。
pub const HEVC_EXT_SPS_LT_RPS_FLAG_USED_LT: u16 = 0x1;

/// `struct v4l2_ctrl_hevc_ext_sps_lt_rps` —— HEVC 长期参考图像集参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHevcExtSpsLtRps {
    pub lt_ref_pic_poc_lsb_sps: u16,
    pub flags: u16,
}

// ── VP9 ─────────────────────────────────────────────────

/// `V4L2_VP9_LOOP_FILTER_FLAG_*`。
pub mod vp9_loop_filter_flag {
    pub const DELTA_ENABLED: u8 = 0x1;
    pub const DELTA_UPDATE: u8 = 0x2;
}

/// `struct v4l2_vp9_loop_filter` —— VP9 环路滤波参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp9LoopFilter {
    pub ref_deltas: [i8; 4],
    pub mode_deltas: [i8; 2],
    pub level: u8,
    pub sharpness: u8,
    pub flags: u8,
    pub reserved: [u8; 7],
}

/// `struct v4l2_vp9_quantization` —— VP9 量化参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp9Quantization {
    pub base_q_idx: u8,
    pub delta_q_y_dc: i8,
    pub delta_q_uv_dc: i8,
    pub delta_q_uv_ac: i8,
    pub reserved: [u8; 4],
}

/// `V4L2_VP9_SEGMENTATION_FLAG_*`。
pub mod vp9_segmentation_flag {
    pub const ENABLED: u8 = 0x01;
    pub const UPDATE_MAP: u8 = 0x02;
    pub const TEMPORAL_UPDATE: u8 = 0x04;
    pub const UPDATE_DATA: u8 = 0x08;
    pub const ABS_OR_DELTA_UPDATE: u8 = 0x10;
}

/// `V4L2_VP9_SEG_LVL_*`。
pub mod vp9_seg_lvl {
    pub const ALT_Q: usize = 0;
    pub const ALT_L: usize = 1;
    pub const REF_FRAME: usize = 2;
    pub const SKIP: usize = 3;
    pub const MAX: usize = 4;
}

/// `V4L2_VP9_SEGMENT_FEATURE_ENABLED_MASK`。
pub const VP9_SEGMENT_FEATURE_ENABLED_MASK: u8 = 0xf;

/// `V4L2_VP9_SEGMENT_FEATURE_ENABLED(id) = 1 << (id)`。
pub const fn vp9_segment_feature_enabled(id: usize) -> u8 {
    1 << id
}

/// `struct v4l2_vp9_segmentation` —— VP9 分段参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp9Segmentation {
    pub feature_data: [[i16; 4]; 8],
    pub feature_enabled: [u8; 8],
    pub tree_probs: [u8; 7],
    pub pred_probs: [u8; 3],
    pub flags: u8,
    pub reserved: [u8; 5],
}

/// `V4L2_VP9_FRAME_FLAG_*`。
pub mod vp9_frame_flag {
    pub const KEY_FRAME: u32 = 0x001;
    pub const SHOW_FRAME: u32 = 0x002;
    pub const ERROR_RESILIENT: u32 = 0x004;
    pub const INTRA_ONLY: u32 = 0x008;
    pub const ALLOW_HIGH_PREC_MV: u32 = 0x010;
    pub const REFRESH_FRAME_CTX: u32 = 0x020;
    pub const PARALLEL_DEC_MODE: u32 = 0x040;
    pub const X_SUBSAMPLING: u32 = 0x080;
    pub const Y_SUBSAMPLING: u32 = 0x100;
    pub const COLOR_RANGE_FULL_SWING: u32 = 0x200;
}

/// `V4L2_VP9_SIGN_BIAS_*`。
pub mod vp9_sign_bias {
    pub const LAST: u8 = 0x1;
    pub const GOLDEN: u8 = 0x2;
    pub const ALT: u8 = 0x4;
}

/// `V4L2_VP9_RESET_FRAME_CTX_*`。
pub mod vp9_reset_frame_ctx {
    pub const NONE: u8 = 0;
    pub const SPEC: u8 = 1;
    pub const ALL: u8 = 2;
}

/// `V4L2_VP9_INTERP_FILTER_*`。
pub mod vp9_interp_filter {
    pub const EIGHTTAP: u8 = 0;
    pub const EIGHTTAP_SMOOTH: u8 = 1;
    pub const EIGHTTAP_SHARP: u8 = 2;
    pub const BILINEAR: u8 = 3;
    pub const SWITCHABLE: u8 = 4;
}

/// `V4L2_VP9_REFERENCE_MODE_*`。
pub mod vp9_reference_mode {
    pub const SINGLE_REFERENCE: u8 = 0;
    pub const COMPOUND_REFERENCE: u8 = 1;
    pub const SELECT: u8 = 2;
}

/// `V4L2_VP9_PROFILE_MAX`。
pub const VP9_PROFILE_MAX: u8 = 3;

/// `V4L2_VP9_NUM_FRAME_CTX`。
pub const VP9_NUM_FRAME_CTX: usize = 4;

/// `V4L2_VP9_TX_MODE_*`。
pub mod vp9_tx_mode {
    pub const ONLY_4X4: u8 = 0;
    pub const ALLOW_8X8: u8 = 1;
    pub const ALLOW_16X16: u8 = 2;
    pub const ALLOW_32X32: u8 = 3;
    pub const SELECT: u8 = 4;
}

/// `struct v4l2_ctrl_vp9_frame` —— VP9 帧解码控制。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlVp9Frame {
    pub lf: Vp9LoopFilter,
    pub quant: Vp9Quantization,
    pub seg: Vp9Segmentation,
    pub flags: u32,
    pub compressed_header_size: u16,
    pub uncompressed_header_size: u16,
    pub frame_width_minus_1: u16,
    pub frame_height_minus_1: u16,
    pub render_width_minus_1: u16,
    pub render_height_minus_1: u16,
    pub last_frame_ts: u64,
    pub golden_frame_ts: u64,
    pub alt_frame_ts: u64,
    pub ref_frame_sign_bias: u8,
    pub reset_frame_context: u8,
    pub frame_context_idx: u8,
    pub profile: u8,
    pub bit_depth: u8,
    pub interpolation_filter: u8,
    pub tile_cols_log2: u8,
    pub tile_rows_log2: u8,
    pub reference_mode: u8,
    pub reserved: [u8; 7],
}

/// `struct v4l2_vp9_mv_probs` —— VP9 运动矢量概率更新。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vp9MvProbs {
    pub joint: [u8; 3],
    pub sign: [u8; 2],
    pub classes: [[u8; 10]; 2],
    pub class0_bit: [u8; 2],
    pub bits: [[u8; 10]; 2],
    pub class0_fr: [[[u8; 3]; 2]; 2],
    pub fr: [[u8; 3]; 2],
    pub class0_hp: [u8; 2],
    pub hp: [u8; 2],
}

/// `struct v4l2_ctrl_vp9_compressed_hdr` —— VP9 压缩头概率更新控制。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlVp9CompressedHdr {
    pub tx_mode: u8,
    pub tx8: [[u8; 1]; 2],
    pub tx16: [[u8; 2]; 2],
    pub tx32: [[u8; 3]; 2],
    pub coef: [[[[[u8; 3]; 6]; 6]; 2]; 4],
    pub skip: [u8; 3],
    pub inter_mode: [[u8; 3]; 7],
    pub interp_filter: [[u8; 2]; 4],
    pub is_inter: [u8; 4],
    pub comp_mode: [u8; 5],
    pub single_ref: [[u8; 2]; 5],
    pub comp_ref: [u8; 5],
    pub y_mode: [[u8; 9]; 4],
    pub uv_mode: [[u8; 9]; 10],
    pub partition: [[u8; 3]; 16],
    pub mv: Vp9MvProbs,
}

// ── AV1 ─────────────────────────────────────────────────

/// AV1 常量。
pub mod av1 {
    pub const TOTAL_REFS_PER_FRAME: usize = 8;
    pub const CDEF_MAX: usize = 8;
    pub const NUM_PLANES_MAX: usize = 3;
    pub const MAX_SEGMENTS: usize = 8;
    pub const MAX_OPERATING_POINTS: usize = 32;
    pub const REFS_PER_FRAME: usize = 7;
    pub const MAX_NUM_Y_POINTS: usize = 16;
    pub const MAX_NUM_CB_POINTS: usize = 16;
    pub const MAX_NUM_CR_POINTS: usize = 16;
    pub const AR_COEFFS_SIZE: usize = 25;
    pub const MAX_NUM_PLANES: usize = 3;
    pub const MAX_TILE_COLS: usize = 64;
    pub const MAX_TILE_ROWS: usize = 64;
    pub const MAX_TILE_COUNT: usize = 512;
}

/// `V4L2_AV1_SEQUENCE_FLAG_*`。
pub mod av1_sequence_flag {
    pub const STILL_PICTURE: u32 = 0x0000_0001;
    pub const USE_128X128_SUPERBLOCK: u32 = 0x0000_0002;
    pub const ENABLE_FILTER_INTRA: u32 = 0x0000_0004;
    pub const ENABLE_INTRA_EDGE_FILTER: u32 = 0x0000_0008;
    pub const ENABLE_INTERINTRA_COMPOUND: u32 = 0x0000_0010;
    pub const ENABLE_MASKED_COMPOUND: u32 = 0x0000_0020;
    pub const ENABLE_WARPED_MOTION: u32 = 0x0000_0040;
    pub const ENABLE_DUAL_FILTER: u32 = 0x0000_0080;
    pub const ENABLE_ORDER_HINT: u32 = 0x0000_0100;
    pub const ENABLE_JNT_COMP: u32 = 0x0000_0200;
    pub const ENABLE_REF_FRAME_MVS: u32 = 0x0000_0400;
    pub const ENABLE_SUPERRES: u32 = 0x0000_0800;
    pub const ENABLE_CDEF: u32 = 0x0000_1000;
    pub const ENABLE_RESTORATION: u32 = 0x0000_2000;
    pub const MONO_CHROME: u32 = 0x0000_4000;
    pub const COLOR_RANGE: u32 = 0x0000_8000;
    pub const SUBSAMPLING_X: u32 = 0x0001_0000;
    pub const SUBSAMPLING_Y: u32 = 0x0002_0000;
    pub const FILM_GRAIN_PARAMS_PRESENT: u32 = 0x0004_0000;
    pub const SEPARATE_UV_DELTA_Q: u32 = 0x0008_0000;
}

/// `struct v4l2_ctrl_av1_sequence` —— AV1 序列头 OBU。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlAv1Sequence {
    pub flags: u32,
    pub seq_profile: u8,
    pub order_hint_bits: u8,
    pub bit_depth: u8,
    pub reserved: u8,
    pub max_frame_width_minus_1: u16,
    pub max_frame_height_minus_1: u16,
}

/// `struct v4l2_ctrl_av1_tile_group_entry` —— AV1 tile group 条目。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlAv1TileGroupEntry {
    pub tile_offset: u32,
    pub tile_size: u32,
    pub tile_row: u32,
    pub tile_col: u32,
}

/// `enum v4l2_av1_warp_model` —— AV1 warp 模型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1WarpModel {
    Identity    = 0,
    Translation = 1,
    Rotzoom     = 2,
    Affine      = 3,
}

/// `enum v4l2_av1_reference_frame` —— AV1 参考帧。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1ReferenceFrame {
    IntraFrame   = 0,
    LastFrame    = 1,
    Last2Frame   = 2,
    Last3Frame   = 3,
    GoldenFrame  = 4,
    BwdrefFrame  = 5,
    Altref2Frame = 6,
    AltrefFrame  = 7,
}

/// `V4L2_AV1_GLOBAL_MOTION_FLAG_*`。
pub mod av1_global_motion_flag {
    pub const IS_GLOBAL: u8 = 0x1;
    pub const IS_ROT_ZOOM: u8 = 0x2;
    pub const IS_TRANSLATION: u8 = 0x4;
}

/// `V4L2_AV1_GLOBAL_MOTION_IS_INVALID(ref) = 1 << (ref)`。
pub const fn av1_global_motion_is_invalid(reference: usize) -> u32 {
    1 << reference
}

/// `struct v4l2_av1_global_motion` —— AV1 全局运动参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1GlobalMotion {
    pub flags: [u8; av1::TOTAL_REFS_PER_FRAME],
    pub type_: [Av1WarpModel; av1::TOTAL_REFS_PER_FRAME],
    pub params: [[i32; 6]; av1::TOTAL_REFS_PER_FRAME],
    pub invalid: u8,
    pub reserved: [u8; 3],
}

/// `enum v4l2_av1_frame_restoration_type` —— AV1 帧恢复类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1FrameRestorationType {
    None       = 0,
    Wiener     = 1,
    Sgrproj    = 2,
    Switchable = 3,
}

/// `V4L2_AV1_LOOP_RESTORATION_FLAG_*`。
pub mod av1_loop_restoration_flag {
    pub const USES_LR: u8 = 0x1;
    pub const USES_CHROMA_LR: u8 = 0x2;
}

/// `struct v4l2_av1_loop_restoration` —— AV1 环路恢复参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1LoopRestoration {
    pub flags: u8,
    pub lr_unit_shift: u8,
    pub lr_uv_shift: u8,
    pub reserved: u8,
    pub frame_restoration_type: [Av1FrameRestorationType; av1::NUM_PLANES_MAX],
    pub loop_restoration_size: [u32; av1::MAX_NUM_PLANES],
}

/// `struct v4l2_av1_cdef` —— AV1 CDEF 参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1Cdef {
    pub damping_minus_3: u8,
    pub bits: u8,
    pub y_pri_strength: [u8; av1::CDEF_MAX],
    pub y_sec_strength: [u8; av1::CDEF_MAX],
    pub uv_pri_strength: [u8; av1::CDEF_MAX],
    pub uv_sec_strength: [u8; av1::CDEF_MAX],
}

/// `V4L2_AV1_SEGMENTATION_FLAG_*`。
pub mod av1_segmentation_flag {
    pub const ENABLED: u8 = 0x1;
    pub const UPDATE_MAP: u8 = 0x2;
    pub const TEMPORAL_UPDATE: u8 = 0x4;
    pub const UPDATE_DATA: u8 = 0x8;
    pub const SEG_ID_PRE_SKIP: u8 = 0x10;
}

/// `enum v4l2_av1_segment_feature` —— AV1 段特性索引。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1SegmentFeature {
    AltQ        = 0,
    AltLfYV     = 1,
    RefFrame    = 5,
    RefSkip     = 6,
    RefGlobalmv = 7,
    Max         = 8,
}

/// `V4L2_AV1_SEGMENT_FEATURE_ENABLED(id) = 1 << (id)`。
pub const fn av1_segment_feature_enabled(id: usize) -> u32 {
    1 << id
}

/// `struct v4l2_av1_segmentation` —— AV1 分段参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1Segmentation {
    pub flags: u8,
    pub last_active_seg_id: u8,
    pub feature_enabled: [u8; av1::MAX_SEGMENTS],
    pub feature_data: [[i16; av1::MAX_SEGMENTS]; av1::MAX_SEGMENTS],
}

/// `V4L2_AV1_LOOP_FILTER_FLAG_*`。
pub mod av1_loop_filter_flag {
    pub const DELTA_ENABLED: u8 = 0x1;
    pub const DELTA_UPDATE: u8 = 0x2;
    pub const DELTA_LF_PRESENT: u8 = 0x4;
    pub const DELTA_LF_MULTI: u8 = 0x8;
}

/// `struct v4l2_av1_loop_filter` —— AV1 环路滤波参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1LoopFilter {
    pub flags: u8,
    pub level: [u8; 4],
    pub sharpness: u8,
    pub ref_deltas: [i8; av1::TOTAL_REFS_PER_FRAME],
    pub mode_deltas: [i8; 2],
    pub delta_lf_res: u8,
}

/// `V4L2_AV1_QUANTIZATION_FLAG_*`。
pub mod av1_quantization_flag {
    pub const DIFF_UV_DELTA: u8 = 0x1;
    pub const USING_QMATRIX: u8 = 0x2;
    pub const DELTA_Q_PRESENT: u8 = 0x4;
}

/// `struct v4l2_av1_quantization` —— AV1 量化参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1Quantization {
    pub flags: u8,
    pub base_q_idx: u8,
    pub delta_q_y_dc: i8,
    pub delta_q_u_dc: i8,
    pub delta_q_u_ac: i8,
    pub delta_q_v_dc: i8,
    pub delta_q_v_ac: i8,
    pub qm_y: u8,
    pub qm_u: u8,
    pub qm_v: u8,
    pub delta_q_res: u8,
}

/// `V4L2_AV1_TILE_INFO_FLAG_UNIFORM_TILE_SPACING`。
pub const AV1_TILE_INFO_FLAG_UNIFORM_TILE_SPACING: u8 = 0x1;

/// `struct v4l2_av1_tile_info` —— AV1 tile 信息。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Av1TileInfo {
    pub flags: u8,
    pub context_update_tile_id: u8,
    pub tile_cols: u8,
    pub tile_rows: u8,
    pub mi_col_starts: [u32; av1::MAX_TILE_COLS + 1],
    pub mi_row_starts: [u32; av1::MAX_TILE_ROWS + 1],
    pub width_in_sbs_minus_1: [u32; av1::MAX_TILE_COLS],
    pub height_in_sbs_minus_1: [u32; av1::MAX_TILE_ROWS],
    pub tile_size_bytes: u8,
    pub reserved: [u8; 3],
}

/// `enum v4l2_av1_frame_type` —— AV1 帧类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1FrameType {
    KeyFrame       = 0,
    InterFrame     = 1,
    IntraOnlyFrame = 2,
    SwitchFrame    = 3,
}

/// `enum v4l2_av1_interpolation_filter` —— AV1 插值滤波器类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1InterpolationFilter {
    Eighttap       = 0,
    EighttapSmooth = 1,
    EighttapSharp  = 2,
    Bilinear       = 3,
    Switchable     = 4,
}

/// `enum v4l2_av1_tx_mode` —— AV1 变换模式。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Av1TxMode {
    Only4x4 = 0,
    Largest = 1,
    Select  = 2,
}

/// `V4L2_AV1_FRAME_FLAG_*`。
pub mod av1_frame_flag {
    pub const SHOW_FRAME: u32 = 0x0000_0001;
    pub const SHOWABLE_FRAME: u32 = 0x0000_0002;
    pub const ERROR_RESILIENT_MODE: u32 = 0x0000_0004;
    pub const DISABLE_CDF_UPDATE: u32 = 0x0000_0008;
    pub const ALLOW_SCREEN_CONTENT_TOOLS: u32 = 0x0000_0010;
    pub const FORCE_INTEGER_MV: u32 = 0x0000_0020;
    pub const ALLOW_INTRABC: u32 = 0x0000_0040;
    pub const USE_SUPERRES: u32 = 0x0000_0080;
    pub const ALLOW_HIGH_PRECISION_MV: u32 = 0x0000_0100;
    pub const IS_MOTION_MODE_SWITCHABLE: u32 = 0x0000_0200;
    pub const USE_REF_FRAME_MVS: u32 = 0x0000_0400;
    pub const DISABLE_FRAME_END_UPDATE_CDF: u32 = 0x0000_0800;
    pub const ALLOW_WARPED_MOTION: u32 = 0x0000_1000;
    pub const REFERENCE_SELECT: u32 = 0x0000_2000;
    pub const REDUCED_TX_SET: u32 = 0x0000_4000;
    pub const SKIP_MODE_ALLOWED: u32 = 0x0000_8000;
    pub const SKIP_MODE_PRESENT: u32 = 0x0001_0000;
    pub const FRAME_SIZE_OVERRIDE: u32 = 0x0002_0000;
    pub const BUFFER_REMOVAL_TIME_PRESENT: u32 = 0x0004_0000;
    pub const FRAME_REFS_SHORT_SIGNALING: u32 = 0x0008_0000;
}

/// `struct v4l2_ctrl_av1_frame` —— AV1 帧头 OBU。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlAv1Frame {
    pub tile_info: Av1TileInfo,
    pub quantization: Av1Quantization,
    pub superres_denom: u8,
    pub segmentation: Av1Segmentation,
    pub loop_filter: Av1LoopFilter,
    pub cdef: Av1Cdef,
    pub skip_mode_frame: [u8; 2],
    pub primary_ref_frame: u8,
    pub loop_restoration: Av1LoopRestoration,
    pub global_motion: Av1GlobalMotion,
    pub flags: u32,
    pub frame_type: Av1FrameType,
    pub order_hint: u32,
    pub upscaled_width: u32,
    pub interpolation_filter: Av1InterpolationFilter,
    pub tx_mode: Av1TxMode,
    pub frame_width_minus_1: u32,
    pub frame_height_minus_1: u32,
    pub render_width_minus_1: u16,
    pub render_height_minus_1: u16,
    pub current_frame_id: u32,
    pub buffer_removal_time: [u32; av1::MAX_OPERATING_POINTS],
    pub reserved: [u8; 4],
    pub order_hints: [u32; av1::TOTAL_REFS_PER_FRAME],
    pub reference_frame_ts: [u64; av1::TOTAL_REFS_PER_FRAME],
    pub ref_frame_idx: [i8; av1::REFS_PER_FRAME],
    pub refresh_frame_flags: u8,
}

/// `V4L2_AV1_FILM_GRAIN_FLAG_*`。
pub mod av1_film_grain_flag {
    pub const APPLY_GRAIN: u8 = 0x1;
    pub const UPDATE_GRAIN: u8 = 0x2;
    pub const CHROMA_SCALING_FROM_LUMA: u8 = 0x4;
    pub const OVERLAP: u8 = 0x8;
    pub const CLIP_TO_RESTRICTED_RANGE: u8 = 0x10;
}

/// `struct v4l2_ctrl_av1_film_grain` —— AV1 胶片颗粒参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlAv1FilmGrain {
    pub flags: u8,
    pub cr_mult: u8,
    pub grain_seed: u16,
    pub film_grain_params_ref_idx: u8,
    pub num_y_points: u8,
    pub point_y_value: [u8; av1::MAX_NUM_Y_POINTS],
    pub point_y_scaling: [u8; av1::MAX_NUM_Y_POINTS],
    pub num_cb_points: u8,
    pub point_cb_value: [u8; av1::MAX_NUM_CB_POINTS],
    pub point_cb_scaling: [u8; av1::MAX_NUM_CB_POINTS],
    pub num_cr_points: u8,
    pub point_cr_value: [u8; av1::MAX_NUM_CR_POINTS],
    pub point_cr_scaling: [u8; av1::MAX_NUM_CR_POINTS],
    pub grain_scaling_minus_8: u8,
    pub ar_coeff_lag: u8,
    pub ar_coeffs_y_plus_128: [u8; av1::AR_COEFFS_SIZE],
    pub ar_coeffs_cb_plus_128: [u8; av1::AR_COEFFS_SIZE],
    pub ar_coeffs_cr_plus_128: [u8; av1::AR_COEFFS_SIZE],
    pub ar_coeff_shift_minus_6: u8,
    pub grain_scale_shift: u8,
    pub cb_mult: u8,
    pub cb_luma_mult: u8,
    pub cr_luma_mult: u8,
    pub cb_offset: u16,
    pub cr_offset: u16,
    pub reserved: [u8; 4],
}

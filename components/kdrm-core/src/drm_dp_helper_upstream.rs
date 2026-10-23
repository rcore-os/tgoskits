// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/display/drm_dp_helper.c`.
//!
//! Local AUX/I2C and protocol helpers only. Remote MST AUX requests are
//! rejected with EOPNOTSUPP. CRC/debugfs collection and AUX character-device
//! registration are outside this component and have no exported entry points.

extern crate alloc;

use alloc::format;
use core::{
    ffi::{c_char, c_int, c_uint, c_ulong, c_void},
    ptr,
};

use drm_kapi::config::{EBUSY, EINVAL, EIO, ENODEV, EOPNOTSUPP, EPROTO, ERANGE, ETIMEDOUT};
pub const EREMOTEIO: i32 = 121;

use drm_kapi::{
    i2c::{
        I2C_FUNC_10BIT_ADDR, I2C_FUNC_I2C, I2C_FUNC_SMBUS_BLOCK_PROC_CALL, I2C_FUNC_SMBUS_EMUL,
        I2C_FUNC_SMBUS_READ_BLOCK_DATA, I2C_M_RD, I2C_M_STOP, I2cAdapter, I2cAlgorithm, I2cMsg,
    },
    mutex::mutex_init,
};

use crate::{
    drm_connector_types::{CONNECTOR_STATUS_CONNECTED, DrmConnector, DrmConnectorStatus},
    drm_device_types::DrmDevice,
    drm_dp_helper_types::{
        DrmDpAsSdp, DrmDpAux, DrmDpAuxMsg, DrmDpDesc, DrmDpDpcdIdent, DrmDpPhyTestParams,
        DrmDpVscSdp,
    },
    drm_dp_types::{
        DP_COLORIMETRY_BT2020_RGB, DP_COLORIMETRY_BT2020_YCC, DP_COLORIMETRY_DCI_P3_RGB,
        DP_COLORIMETRY_DEFAULT, DP_COLORIMETRY_OPRGB, DP_COLORIMETRY_RGB_CUSTOM,
        DP_COLORIMETRY_RGB_WIDE_FIXED, DP_COLORIMETRY_RGB_WIDE_FLOAT, DP_CONTENT_TYPE_GAME,
        DP_CONTENT_TYPE_GRAPHICS, DP_CONTENT_TYPE_NOT_DEFINED, DP_CONTENT_TYPE_PHOTO,
        DP_CONTENT_TYPE_VIDEO, DP_DYNAMIC_RANGE_CTA, DP_DYNAMIC_RANGE_VESA, DP_PIXELFORMAT_RAW,
        DP_PIXELFORMAT_RGB, DP_PIXELFORMAT_Y_ONLY, DP_PIXELFORMAT_YUV420, DP_PIXELFORMAT_YUV422,
        DP_PIXELFORMAT_YUV444, DpColorimetry, DpContentType, DpDynamicRange, DpPixelformat, DpSdp,
    },
    drm_edid_types::Edid,
    drm_edid_upstream::DrmEdid,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{mutex_lock, mutex_unlock},
    drm_modes_types::DrmDisplayMode,
    drm_print_types::DrmPrinter,
    drm_property_types::DrmProperty,
    uapi_drm_consts::*,
    uapi_drm_mode_types::*,
};

#[inline]
fn usleep_range(min_us: u64, max_us: u64) {
    #[cfg(not(test))]
    drm_kapi::iopoll::usleep_range(min_us, max_us);
    #[cfg(test)]
    let _ = (min_us, max_us);
}

// ---------------------------------------------------------------------------
// Constants from include/drm/display/drm_dp.h & drm_dp_helper.h
// ---------------------------------------------------------------------------

pub const DP_AUX_MAX_PAYLOAD_BYTES: usize = 16;

pub const DP_AUX_I2C_WRITE: u8 = 0x0;
pub const DP_AUX_I2C_READ: u8 = 0x1;
pub const DP_AUX_I2C_WRITE_STATUS_UPDATE: u8 = 0x2;
pub const DP_AUX_I2C_MOT: u8 = 0x4;
pub const DP_AUX_NATIVE_WRITE: u8 = 0x8;
pub const DP_AUX_NATIVE_READ: u8 = 0x9;

pub const DP_AUX_NATIVE_REPLY_ACK: u8 = 0x0;
pub const DP_AUX_NATIVE_REPLY_NACK: u8 = 0x1;
pub const DP_AUX_NATIVE_REPLY_DEFER: u8 = 0x2;
pub const DP_AUX_NATIVE_REPLY_MASK: u8 = 0x3;

pub const DP_AUX_I2C_REPLY_ACK: u8 = 0x0;
pub const DP_AUX_I2C_REPLY_NACK: u8 = 0x4;
pub const DP_AUX_I2C_REPLY_DEFER: u8 = 0x8;
pub const DP_AUX_I2C_REPLY_MASK: u8 = 0xc;

pub const DP_DPCD_REV: u32 = 0x000;
pub const DP_DPCD_REV_10: u8 = 0x10;
pub const DP_DPCD_REV_11: u8 = 0x11;
pub const DP_DPCD_REV_12: u8 = 0x12;
pub const DP_DPCD_REV_13: u8 = 0x13;
pub const DP_DPCD_REV_14: u8 = 0x14;

pub const DP_MAX_LINK_RATE: u32 = 0x001;
pub const DP_MAX_LANE_COUNT: u32 = 0x002;
pub const DP_MAX_LANE_COUNT_MASK: u8 = 0x1f;
pub const DP_POST_LT_ADJ_REQ_SUPPORTED: u8 = 1 << 5;
pub const DP_TPS3_SUPPORTED: u8 = 1 << 6;
pub const DP_ENHANCED_FRAME_CAP: u8 = 1 << 7;

pub const DP_MAX_DOWNSPREAD: u32 = 0x003;
pub const DP_MAX_DOWNSPREAD_0_5: u8 = 1 << 0;
pub const DP_NO_AUX_HANDSHAKE_LINK_TRAINING: u8 = 1 << 6;
pub const DP_TPS4_SUPPORTED: u8 = 1 << 7;

pub const DP_DOWNSTREAMPORT_PRESENT: u32 = 0x005;
pub const DP_DWN_STRM_PORT_PRESENT: u8 = 1 << 0;
pub const DP_DWN_STRM_PORT_TYPE_MASK: u8 = 0x06;
pub const DP_DWN_STRM_PORT_TYPE_DP: u8 = 0 << 1;
pub const DP_DWN_STRM_PORT_TYPE_ANALOG: u8 = 1 << 1;
pub const DP_DWN_STRM_PORT_TYPE_TMDS: u8 = 2 << 1;
pub const DP_DWN_STRM_PORT_TYPE_OTHER: u8 = 3 << 1;
pub const DP_FORMAT_CONVERSION: u8 = 1 << 3;
pub const DP_DETAILED_CAP_INFO_AVAILABLE: u8 = 1 << 4;

pub const DP_MAIN_LINK_CHANNEL_CODING: u32 = 0x006;
pub const DP_CAP_ANSI_8B10B: u8 = 1 << 0;
pub const DP_CAP_ANSI_128B132B: u8 = 1 << 1;

pub const DP_DOWN_STREAM_PORT_COUNT: u32 = 0x007;
pub const DP_PORT_COUNT_MASK: u8 = 0x0f;
pub const DP_MSA_TIMING_PAR_IGNORED: u8 = 1 << 6;
pub const DP_OUI_SUPPORT: u8 = 1 << 7;

pub const DP_EDP_CONFIGURATION_CAP: u32 = 0x00d;
pub const DP_ALTERNATE_SCRAMBLER_RESET_CAP: u8 = 1 << 0;

pub const DP_TRAINING_AUX_RD_INTERVAL: u32 = 0x00e;
pub const DP_TRAINING_AUX_RD_MASK: u8 = 0x7f;
pub const DP_EXTENDED_RECEIVER_CAP_FIELD_PRESENT: u8 = 1 << 7;

pub const DP_DSC_SUPPORT: u32 = 0x060;
pub const DP_DSC_DECOMPRESSION_IS_SUPPORTED: u8 = 1 << 0;

pub const DP_DSC_RC_BUF_BLK_SIZE: u32 = 0x062;
pub const DP_DSC_THROUGHPUT_MODE_0_DELTA_SHIFT: u8 = 3;
pub const DP_DSC_THROUGHPUT_MODE_0_DELTA_MASK: u8 = 0x1f << DP_DSC_THROUGHPUT_MODE_0_DELTA_SHIFT;

pub const DP_DSC_SLICE_CAP_1: u32 = 0x064;
pub const DP_DSC_1_PER_DP_DSC_SINK: u8 = 1 << 0;
pub const DP_DSC_2_PER_DP_DSC_SINK: u8 = 1 << 1;
pub const DP_DSC_4_PER_DP_DSC_SINK: u8 = 1 << 3;
pub const DP_DSC_6_PER_DP_DSC_SINK: u8 = 1 << 4;
pub const DP_DSC_8_PER_DP_DSC_SINK: u8 = 1 << 5;
pub const DP_DSC_10_PER_DP_DSC_SINK: u8 = 1 << 6;
pub const DP_DSC_12_PER_DP_DSC_SINK: u8 = 1 << 7;

pub const DP_DSC_LINE_BUF_BIT_DEPTH: u32 = 0x065;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_MASK: u8 = 0xf;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_9: u8 = 0;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_10: u8 = 1;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_11: u8 = 2;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_12: u8 = 3;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_13: u8 = 4;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_14: u8 = 5;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_15: u8 = 6;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_16: u8 = 7;
pub const DP_DSC_LINE_BUF_BIT_DEPTH_8: u8 = 8;

pub const DP_DSC_MAX_BITS_PER_PIXEL_LOW: u32 = 0x067;
pub const DP_DSC_MAX_BITS_PER_PIXEL_HI: u32 = 0x068;
pub const DP_DSC_MAX_BITS_PER_PIXEL_HI_MASK: u8 = 0x3;

pub const DP_DSC_DEC_COLOR_FORMAT_CAP: u32 = 0x069;
pub const DP_DSC_DEC_COLOR_DEPTH_CAP: u32 = 0x06a;
pub const DP_DSC_8_BPC: u8 = 1 << 1;
pub const DP_DSC_10_BPC: u8 = 1 << 2;
pub const DP_DSC_12_BPC: u8 = 1 << 3;

pub const DP_DSC_PEAK_THROUGHPUT: u32 = 0x06b;
pub const DP_DSC_THROUGHPUT_MODE_0_MASK: u8 = 0xf;
pub const DP_DSC_THROUGHPUT_MODE_0_SHIFT: u8 = 0;
pub const DP_DSC_THROUGHPUT_MODE_1_MASK: u8 = 0xf0;
pub const DP_DSC_THROUGHPUT_MODE_1_SHIFT: u8 = 4;

pub const DP_DSC_MAX_SLICE_WIDTH: u32 = 0x06c;
pub const DP_DSC_SLICE_WIDTH_MULTIPLIER: u32 = 320;

pub const DP_DSC_SLICE_CAP_2: u32 = 0x06d;
pub const DP_DSC_16_PER_DP_DSC_SINK: u8 = 1 << 0;
pub const DP_DSC_20_PER_DP_DSC_SINK: u8 = 1 << 1;
pub const DP_DSC_24_PER_DP_DSC_SINK: u8 = 1 << 2;

pub const DP_DSC_BITS_PER_PIXEL_INC: u32 = 0x06f;
pub const DP_DSC_BITS_PER_PIXEL_1_16: u8 = 0;
pub const DP_DSC_BITS_PER_PIXEL_1_8: u8 = 1;
pub const DP_DSC_BITS_PER_PIXEL_1_4: u8 = 2;
pub const DP_DSC_BITS_PER_PIXEL_1_2: u8 = 3;
pub const DP_DSC_BITS_PER_PIXEL_1_1: u8 = 4;
pub const DP_DSC_BITS_PER_PIXEL_MASK: u8 = 0x7;

pub const DP_DOWNSTREAM_PORT_0: u32 = 0x080;
pub const DP_DS_PORT_TYPE_MASK: u8 = 7;
pub const DP_DS_PORT_TYPE_DP: u8 = 0;
pub const DP_DS_PORT_TYPE_VGA: u8 = 1;
pub const DP_DS_PORT_TYPE_DVI: u8 = 2;
pub const DP_DS_PORT_TYPE_HDMI: u8 = 3;
pub const DP_DS_PORT_TYPE_NON_EDID: u8 = 4;
pub const DP_DS_PORT_TYPE_DP_DUALMODE: u8 = 5;
pub const DP_DS_PORT_TYPE_WIRELESS: u8 = 6;

pub const DP_DS_NON_EDID_MASK: u8 = 0xf0;
pub const DP_DS_NON_EDID_720X480I_60: u8 = 1 << 4;
pub const DP_DS_NON_EDID_720X480I_50: u8 = 2 << 4;
pub const DP_DS_NON_EDID_1920X1080I_60: u8 = 3 << 4;
pub const DP_DS_NON_EDID_1920X1080I_50: u8 = 4 << 4;
pub const DP_DS_NON_EDID_1280X720_60: u8 = 5 << 4;
pub const DP_DS_NON_EDID_1280X720_50: u8 = 7 << 4;

pub const DP_DS_MAX_BPC_MASK: u8 = 3;
pub const DP_DS_8BPC: u8 = 0;
pub const DP_DS_10BPC: u8 = 1;
pub const DP_DS_12BPC: u8 = 2;
pub const DP_DS_16BPC: u8 = 3;

pub const DP_PCON_MAX_FRL_BW: u8 = 7 << 2;
pub const DP_PCON_MAX_0GBPS: u8 = 0 << 2;
pub const DP_PCON_MAX_9GBPS: u8 = 1 << 2;
pub const DP_PCON_MAX_18GBPS: u8 = 2 << 2;
pub const DP_PCON_MAX_24GBPS: u8 = 3 << 2;
pub const DP_PCON_MAX_32GBPS: u8 = 4 << 2;
pub const DP_PCON_MAX_40GBPS: u8 = 5 << 2;
pub const DP_PCON_MAX_48GBPS: u8 = 6 << 2;

pub const DP_DS_HDMI_YCBCR420_PASS_THROUGH: u8 = 1 << 2;
pub const DP_DS_HDMI_YCBCR444_TO_420_CONV: u8 = 1 << 4;

pub const DP_FEC_CAPABILITY: u32 = 0x090;
pub const DP_FEC_CAPABLE: u8 = 1 << 0;

pub const DP_PCON_DSC_ENCODER: u32 = 0x092;
pub const DP_PCON_DSC_VERSION: u32 = 0x093;
pub const DP_PCON_DSC_MAJOR_MASK: u8 = 0xf;
pub const DP_PCON_DSC_MINOR_MASK: u8 = 0xf0;
pub const DP_PCON_DSC_MAJOR_SHIFT: u8 = 0;
pub const DP_PCON_DSC_MINOR_SHIFT: u8 = 4;

pub const DP_PCON_DSC_SLICE_CAP_1: u32 = 0x096;
pub const DP_PCON_DSC_1_PER_DSC_ENC: u8 = 1 << 0;
pub const DP_PCON_DSC_2_PER_DSC_ENC: u8 = 1 << 1;
pub const DP_PCON_DSC_4_PER_DSC_ENC: u8 = 1 << 3;
pub const DP_PCON_DSC_6_PER_DSC_ENC: u8 = 1 << 4;
pub const DP_PCON_DSC_8_PER_DSC_ENC: u8 = 1 << 5;
pub const DP_PCON_DSC_10_PER_DSC_ENC: u8 = 1 << 6;
pub const DP_PCON_DSC_12_PER_DSC_ENC: u8 = 1 << 7;

pub const DP_PCON_DSC_MAX_SLICE_WIDTH: u32 = 0x09b;

pub const DP_PCON_DSC_SLICE_CAP_2: u32 = 0x09c;
pub const DP_PCON_DSC_16_PER_DSC_ENC: u8 = 1 << 0;
pub const DP_PCON_DSC_20_PER_DSC_ENC: u8 = 1 << 1;
pub const DP_PCON_DSC_24_PER_DSC_ENC: u8 = 1 << 2;

pub const DP_PCON_DSC_BPP_INCR: u32 = 0x09e;
pub const DP_PCON_DSC_BPP_INCR_MASK: u8 = 0x7;
pub const DP_PCON_DSC_ONE_16TH_BPP: u8 = 0;
pub const DP_PCON_DSC_ONE_8TH_BPP: u8 = 1;
pub const DP_PCON_DSC_ONE_4TH_BPP: u8 = 2;
pub const DP_PCON_DSC_ONE_HALF_BPP: u8 = 3;
pub const DP_PCON_DSC_ONE_BPP: u8 = 4;

pub const DP_DSC_BRANCH_OVERALL_THROUGHPUT_0: u32 = 0x0a0;
pub const DP_DSC_BRANCH_OVERALL_THROUGHPUT_1: u32 = 0x0a1;
pub const DP_DSC_BRANCH_MAX_LINE_WIDTH: u32 = 0x0a2;

pub const DP_LINK_BW_1_62: u8 = 0x06;
pub const DP_LINK_BW_2_7: u8 = 0x0a;
pub const DP_LINK_BW_5_4: u8 = 0x14;
pub const DP_LINK_BW_8_1: u8 = 0x1e;
pub const DP_LINK_BW_10: u8 = 0x01;
pub const DP_LINK_BW_13_5: u8 = 0x04;
pub const DP_LINK_BW_20: u8 = 0x02;

pub const DP_TRAINING_PATTERN_SET: u32 = 0x102;
pub const DP_TRAINING_PATTERN_MASK: u8 = 0x3;
pub const DP_TRAINING_PATTERN_MASK_1_4: u8 = 0xf;
pub const DP_LINK_QUAL_PATTERN_11_MASK: u8 = 3 << 2;

pub const DP_LINK_QUAL_LANE0_SET: u32 = 0x10b;

pub const DP_TRAIN_VOLTAGE_SWING_SHIFT: u8 = 0;
pub const DP_TRAIN_VOLTAGE_SWING_LEVEL_0: u8 = 0;
pub const DP_TRAIN_VOLTAGE_SWING_LEVEL_1: u8 = 1 << 0;
pub const DP_TRAIN_VOLTAGE_SWING_LEVEL_2: u8 = 2;
pub const DP_TRAIN_VOLTAGE_SWING_LEVEL_3: u8 = 3;

pub const DP_TRAIN_PRE_EMPHASIS_SHIFT: u8 = 3;
pub const DP_TRAIN_PRE_EMPH_LEVEL_0: u8 = 0 << 3;
pub const DP_TRAIN_PRE_EMPH_LEVEL_1: u8 = 1 << 3;
pub const DP_TRAIN_PRE_EMPH_LEVEL_2: u8 = 2 << 3;
pub const DP_TRAIN_PRE_EMPH_LEVEL_3: u8 = 3 << 3;

pub const DP_EXTENDED_DPRX_SLEEP_WAKE_TIMEOUT_GRANT: u32 = 0x119;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_GRANTED: u8 = 1 << 0;

pub const DP_PAYLOAD_ALLOCATE_SET: u32 = 0x1c0;

pub const DP_SINK_COUNT: u32 = 0x200;
#[inline]
pub const fn dp_get_sink_count(x: u8) -> i32 {
    (((x & 0x80) >> 1) | (x & 0x3f)) as i32
}

pub const DP_DEVICE_SERVICE_IRQ_VECTOR: u32 = 0x201;
pub const DP_AUTOMATED_TEST_REQUEST: u8 = 1 << 1;

pub const DP_LANE0_1_STATUS: u32 = 0x202;
pub const DP_LANE2_3_STATUS: u32 = 0x203;
pub const DP_LANE_CR_DONE: u8 = 1 << 0;
pub const DP_LANE_CHANNEL_EQ_DONE: u8 = 1 << 1;
pub const DP_LANE_SYMBOL_LOCKED: u8 = 1 << 2;
pub const DP_CHANNEL_EQ_BITS: u8 =
    DP_LANE_CR_DONE | DP_LANE_CHANNEL_EQ_DONE | DP_LANE_SYMBOL_LOCKED;

pub const DP_LANE_ALIGN_STATUS_UPDATED: u32 = 0x204;
pub const DP_INTERLANE_ALIGN_DONE: u8 = 1 << 0;
pub const DP_POST_LT_ADJ_REQ_IN_PROGRESS: u8 = 1 << 1;
pub const DP_128B132B_DPRX_EQ_INTERLANE_ALIGN_DONE: u8 = 1 << 2;
pub const DP_128B132B_DPRX_CDS_INTERLANE_ALIGN_DONE: u8 = 1 << 3;
pub const DP_128B132B_LT_FAILED: u8 = 1 << 4;

pub const DP_SINK_STATUS: u32 = 0x205;

pub const DP_ADJUST_REQUEST_LANE0_1: u32 = 0x206;
pub const DP_ADJUST_REQUEST_LANE2_3: u32 = 0x207;
pub const DP_ADJUST_VOLTAGE_SWING_LANE0_SHIFT: u8 = 0;
pub const DP_ADJUST_VOLTAGE_SWING_LANE1_SHIFT: u8 = 4;
pub const DP_ADJUST_PRE_EMPHASIS_LANE0_SHIFT: u8 = 2;
pub const DP_ADJUST_PRE_EMPHASIS_LANE1_SHIFT: u8 = 6;
pub const DP_ADJUST_TX_FFE_PRESET_LANE0_SHIFT: u8 = 0;
pub const DP_ADJUST_TX_FFE_PRESET_LANE1_SHIFT: u8 = 4;

pub const DP_TEST_REQUEST: u32 = 0x218;
pub const DP_TEST_LINK_EDID_READ: u8 = 1 << 2;
pub const DP_TEST_LINK_RATE: u32 = 0x219;
pub const DP_TEST_LANE_COUNT: u32 = 0x220;

pub const DP_TEST_CRC_R_CR: u32 = 0x240;
pub const DP_TEST_SINK_MISC: u32 = 0x246;
pub const DP_TEST_COUNT_MASK: u8 = 0xf;

pub const DP_PHY_TEST_PATTERN: u32 = 0x248;
pub const DP_PHY_TEST_PATTERN_80BIT_CUSTOM: u8 = 4;
pub const DP_PHY_TEST_PATTERN_CP2520: u8 = 5;

pub const DP_TEST_HBR2_SCRAMBLER_RESET: u32 = 0x24a;
pub const DP_TEST_80BIT_CUSTOM_PATTERN_7_0: u32 = 0x250;

pub const DP_TEST_RESPONSE: u32 = 0x260;
pub const DP_TEST_EDID_CHECKSUM_WRITE: u8 = 1 << 2;
pub const DP_TEST_EDID_CHECKSUM: u32 = 0x261;

pub const DP_TEST_SINK: u32 = 0x270;
pub const DP_TEST_SINK_START: u8 = 1 << 0;

pub const DP_PAYLOAD_TABLE_UPDATE_STATUS: u32 = 0x2c0;
pub const DP_PAYLOAD_TABLE_UPDATED: u8 = 1 << 0;
pub const DP_PAYLOAD_ACT_HANDLED: u8 = 1 << 1;

pub const DP_SINK_OUI: u32 = 0x400;
pub const DP_BRANCH_OUI: u32 = 0x500;
pub const DP_BRANCH_ID: u32 = 0x503;
pub const DP_BRANCH_HW_REV: u32 = 0x509;
pub const DP_BRANCH_SW_REV: u32 = 0x50a;

pub const DP_SET_POWER: u32 = 0x600;
pub const DP_SET_POWER_D0: u8 = 0x1;
pub const DP_SET_POWER_D3: u8 = 0x2;
pub const DP_SET_POWER_MASK: u8 = 0x3;

pub const DP_DP13_DPCD_REV: u32 = 0x2200;
pub const DP_DPRX_FEATURE_ENUMERATION_LIST: u32 = 0x2210;
pub const DP_VSC_SDP_EXT_FOR_COLORIMETRY_SUPPORTED: u8 = 1 << 3;

pub const DP_DPRX_FEATURE_ENUMERATION_LIST_CONT_1: u32 = 0x2214;
pub const DP_ADAPTIVE_SYNC_SDP_SUPPORTED: u8 = 1 << 0;

pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL: u32 = 0x2216;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_1MS_UNIT: u8 = 1 << 7;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_MASK: u8 = 0x7f;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_400_US: u8 = 0x00;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_4_MS: u8 = 0x01;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_8_MS: u8 = 0x02;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_12_MS: u8 = 0x03;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_16_MS: u8 = 0x04;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_32_MS: u8 = 0x05;
pub const DP_128B132B_TRAINING_AUX_RD_INTERVAL_64_MS: u8 = 0x06;

pub const DP_EXTENDED_DPRX_SLEEP_WAKE_TIMEOUT_REQUEST: u32 = 0x2211;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_1_MS: usize = 0;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_20_MS: usize = 1;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_40_MS: usize = 2;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_60_MS: usize = 3;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_80_MS: usize = 4;
pub const DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_100_MS: usize = 5;

pub const DP_PCON_HDMI_POST_FRL_STATUS: u32 = 0x3036;
pub const DP_PCON_HDMI_LINK_MODE: u8 = 1 << 0;
pub const DP_PCON_HDMI_MODE_TMDS: u8 = 0;
pub const DP_PCON_HDMI_MODE_FRL: u8 = 1;
pub const DP_PCON_HDMI_FRL_TRAINED_BW: u8 = 0x3f << 1;

pub const DP_PCON_HDMI_ERROR_STATUS_LN0: u32 = 0x3037;
pub const DP_PCON_HDMI_ERROR_COUNT_MASK: u8 = 0x7;
pub const DP_PCON_HDMI_ERROR_COUNT_THREE_PLUS: u8 = 1 << 0;
pub const DP_PCON_HDMI_ERROR_COUNT_TEN_PLUS: u8 = 1 << 1;
pub const DP_PCON_HDMI_ERROR_COUNT_HUNDRED_PLUS: u8 = 1 << 2;

pub const DP_PCON_HDMI_TX_LINK_STATUS: u32 = 0x303b;
pub const DP_PCON_HDMI_TX_LINK_ACTIVE: u8 = 1 << 0;
pub const DP_PCON_FRL_READY: u8 = 1 << 1;

pub const DP_PROTOCOL_CONVERTER_CONTROL_2: u32 = 0x3052;
pub const DP_PCON_ENABLE_DSC_ENCODER: u8 = 1 << 1;
pub const DP_PCON_ENCODER_PPS_OVERRIDE_MASK: u8 = 0x3 << 2;
pub const DP_PCON_ENC_PPS_OVERRIDE_DISABLED: u8 = 0;
pub const DP_PCON_ENC_PPS_OVERRIDE_EN_PARAMS: u8 = 1;
pub const DP_PCON_ENC_PPS_OVERRIDE_EN_BUFFER: u8 = 2;
pub const DP_CONVERSION_RGB_YCBCR_MASK: u8 = 7 << 4;

pub const DP_PCON_HDMI_LINK_CONFIG_1: u32 = 0x305a;
pub const DP_PCON_ENABLE_MAX_BW_0GBPS: u8 = 0;
pub const DP_PCON_ENABLE_MAX_BW_9GBPS: u8 = 1;
pub const DP_PCON_ENABLE_MAX_BW_18GBPS: u8 = 2;
pub const DP_PCON_ENABLE_MAX_BW_24GBPS: u8 = 3;
pub const DP_PCON_ENABLE_MAX_BW_32GBPS: u8 = 4;
pub const DP_PCON_ENABLE_MAX_BW_40GBPS: u8 = 5;
pub const DP_PCON_ENABLE_MAX_BW_48GBPS: u8 = 6;
pub const DP_PCON_ENABLE_SOURCE_CTL_MODE: u8 = 1 << 3;
pub const DP_PCON_ENABLE_CONCURRENT_LINK: u8 = 1 << 4;
pub const DP_PCON_ENABLE_LINK_FRL_MODE: u8 = 1 << 5;
pub const DP_PCON_ENABLE_HPD_READY: u8 = 1 << 6;
pub const DP_PCON_ENABLE_HDMI_LINK: u8 = 1 << 7;

pub const DP_PCON_HDMI_LINK_CONFIG_2: u32 = 0x305b;
pub const DP_PCON_FRL_LINK_TRAIN_EXTENDED: u8 = 1 << 6;

pub const DP_PCON_HDMI_PPS_OVERRIDE_BASE: u32 = 0x3100;
pub const DP_PCON_HDMI_PPS_OVRD_SLICE_HEIGHT: u32 = 0x3180;
pub const DP_PCON_HDMI_PPS_OVRD_SLICE_WIDTH: u32 = 0x3182;
pub const DP_PCON_HDMI_PPS_OVRD_BPP: u32 = 0x3184;

pub const DP_LT_TUNABLE_PHY_REPEATER_FIELD_DATA_STRUCTURE_REV: u32 = 0xf0000;
pub const DP_MAX_LINK_RATE_PHY_REPEATER: u32 = 0xf0001;
pub const DP_PHY_REPEATER_CNT: u32 = 0xf0002;
pub const DP_PHY_REPEATER_MODE: u32 = 0xf0003;
pub const DP_MAX_LANE_COUNT_PHY_REPEATER: u32 = 0xf0004;
pub const DP_PHY_REPEATER_EXTENDED_WAIT_TIMEOUT: u32 = 0xf0005;
pub const DP_EXTENDED_WAKE_TIMEOUT_REQUEST_MASK: u8 = 0x7f;
pub const DP_EXTENDED_WAKE_TIMEOUT_GRANT: u8 = 1 << 7;

pub const DP_PHY_REPEATER_MODE_TRANSPARENT: u8 = 0x55;
pub const DP_PHY_REPEATER_MODE_NON_TRANSPARENT: u8 = 0xaa;

pub const DP_TRAINING_AUX_RD_INTERVAL_PHY_REPEATER1: u32 = 0xf0020;
pub const DP_TRANSMITTER_CAPABILITY_PHY_REPEATER1: u32 = 0xf0021;
pub const DP_VOLTAGE_SWING_LEVEL_3_SUPPORTED: u8 = 1 << 0;
pub const DP_PRE_EMPHASIS_LEVEL_3_SUPPORTED: u8 = 1 << 1;

pub const DP_LANE0_1_STATUS_PHY_REPEATER1: u32 = 0xf0030;
pub const DP_OUI_PHY_REPEATER1: u32 = 0xf003d;

// Sizes
pub const DP_LINK_STATUS_SIZE: usize = 6;
pub const DP_RECEIVER_CAP_SIZE: usize = 0xf;
pub const DP_DSC_RECEIVER_CAP_SIZE: usize = 0x10;
pub const DP_DSC_BRANCH_CAP_SIZE: usize = 3;
pub const EDP_PSR_RECEIVER_CAP_SIZE: usize = 2;
pub const EDP_DISPLAY_CTL_CAP_SIZE: usize = 5;
pub const DP_LTTPR_COMMON_CAP_SIZE: usize = 8;
pub const DP_LTTPR_PHY_CAP_SIZE: usize = 3;
pub const DP_MAX_DOWNSTREAM_PORTS: usize = 0x10;
pub const DP_PCON_DSC_ENCODER_CAP_SIZE: usize = 0xd;

// SDP types
pub const DP_SDP_AUDIO_TIMESTAMP: u8 = 0x01;
pub const DP_SDP_AUDIO_STREAM: u8 = 0x02;
pub const DP_SDP_EXTENSION: u8 = 0x04;
pub const DP_SDP_AUDIO_COPYMANAGEMENT: u8 = 0x05;
pub const DP_SDP_ISRC: u8 = 0x06;
pub const DP_SDP_VSC: u8 = 0x07;
pub const DP_SDP_PPS: u8 = 0x10;
pub const DP_SDP_VSC_EXT_VESA: u8 = 0x20;
pub const DP_SDP_VSC_EXT_CEA: u8 = 0x21;
pub const DP_SDP_ADAPTIVE_SYNC: u8 = 0x22;

// Quirks
pub const DP_DPCD_QUIRK_CONSTANT_N: u32 = 0;
pub const DP_DPCD_QUIRK_NO_PSR: u32 = 1;
pub const DP_DPCD_QUIRK_NO_SINK_COUNT: u32 = 2;
pub const DP_DPCD_QUIRK_DSC_WITHOUT_VIRTUAL_DPCD: u32 = 3;
pub const DP_DPCD_QUIRK_CAN_DO_MAX_LINK_RATE_3_24_GBPS: u32 = 4;
pub const DP_DPCD_QUIRK_HBLANK_EXPANSION_REQUIRES_DSC: u32 = 5;
pub const DP_DPCD_QUIRK_DSC_THROUGHPUT_BPP_LIMIT: u32 = 6;

// Bandwidth overhead flags
pub const DRM_DP_BW_OVERHEAD_MST: c_ulong = 1 << 0;
pub const DRM_DP_BW_OVERHEAD_UHBR: c_ulong = 1 << 1;
pub const DRM_DP_BW_OVERHEAD_SSC_REF_CLK: c_ulong = 1 << 2;
pub const DRM_DP_BW_OVERHEAD_FEC: c_ulong = 1 << 3;
pub const DRM_DP_BW_OVERHEAD_DSC: c_ulong = 1 << 4;

// EDID input
pub const DRM_EDID_INPUT_DIGITAL: u8 = 1 << 7;
pub const DRM_EDID_DIGITAL_TYPE_MASK: u8 = 7;
pub const DRM_EDID_DIGITAL_TYPE_DP: u8 = 5;

// PHY enum
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DrmDpPhy {
    DpPhyDprx   = 0,
    DpPhyLttpr1 = 1,
    DpPhyLttpr2 = 2,
    DpPhyLttpr3 = 3,
    DpPhyLttpr4 = 4,
    DpPhyLttpr5 = 5,
    DpPhyLttpr6 = 6,
    DpPhyLttpr7 = 7,
    DpPhyLttpr8 = 8,
}
pub use DrmDpPhy::*;
pub const DP_PHY_DPRX: DrmDpPhy = DrmDpPhy::DpPhyDprx;
pub const DP_PHY_LTTPR1: DrmDpPhy = DrmDpPhy::DpPhyLttpr1;
pub const DP_PHY_LTTPR2: DrmDpPhy = DrmDpPhy::DpPhyLttpr2;
pub const DP_PHY_LTTPR3: DrmDpPhy = DrmDpPhy::DpPhyLttpr3;
pub const DP_PHY_LTTPR4: DrmDpPhy = DrmDpPhy::DpPhyLttpr4;
pub const DP_PHY_LTTPR5: DrmDpPhy = DrmDpPhy::DpPhyLttpr5;
pub const DP_PHY_LTTPR6: DrmDpPhy = DrmDpPhy::DpPhyLttpr6;
pub const DP_PHY_LTTPR7: DrmDpPhy = DrmDpPhy::DpPhyLttpr7;
pub const DP_PHY_LTTPR8: DrmDpPhy = DrmDpPhy::DpPhyLttpr8;
pub const DP_MAX_LTTPR_COUNT: DrmDpPhy = DrmDpPhy::DpPhyLttpr8;

#[inline]
pub const fn dp_lttpr_reg(dp_phy: DrmDpPhy, reg1: u32) -> u32 {
    let idx = (dp_phy as u32) - (DpPhyLttpr1 as u32);
    0x50 * idx + reg1
}

#[inline]
pub const fn dp_training_aux_rd_interval_phy_repeater(dp_phy: DrmDpPhy) -> u32 {
    dp_lttpr_reg(dp_phy, DP_TRAINING_AUX_RD_INTERVAL_PHY_REPEATER1)
}

#[inline]
pub const fn dp_128b132b_training_aux_rd_interval_phy_repeater(dp_phy: DrmDpPhy) -> u32 {
    dp_lttpr_reg(dp_phy, 0xf0022)
}

#[inline]
pub const fn dp_lane0_1_status_phy_repeater(dp_phy: DrmDpPhy) -> u32 {
    dp_lttpr_reg(dp_phy, DP_LANE0_1_STATUS_PHY_REPEATER1)
}

#[inline]
pub const fn dp_oui_phy_repeater(dp_phy: DrmDpPhy) -> u32 {
    dp_lttpr_reg(dp_phy, DP_OUI_PHY_REPEATER1)
}

// ---------------------------------------------------------------------------
// External declarations for pending / peer batch symbols
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn drm_display_mode_from_cea_vic(dev: *mut DrmDevice, vic: u8) -> *mut DrmDisplayMode;
    fn drm_edid_raw(drm_edid: *const DrmEdid) -> *const Edid;
    fn drm_object_property_set_value(
        obj: *mut DrmModeObject,
        property: *mut DrmProperty,
        val: u64,
    ) -> i32;

}

// ---------------------------------------------------------------------------
// Internal Link Helpers
// ---------------------------------------------------------------------------

#[inline]
fn dp_link_status(link_status: &[u8], r: usize) -> u8 {
    link_status[r - DP_LANE0_1_STATUS as usize]
}

#[inline]
fn dp_get_lane_status(link_status: &[u8], lane: usize) -> u8 {
    let i = DP_LANE0_1_STATUS as usize + (lane >> 1);
    let s = (lane & 1) * 4;
    let l = dp_link_status(link_status, i);
    (l >> s) & 0xf
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_channel_eq_ok()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes
/// containing DPCD link status registers (0x202..0x207). `lane_count` must not exceed the
/// number of lanes represented in `link_status`.
pub unsafe extern "C" fn drm_dp_channel_eq_ok(link_status: *const u8, lane_count: c_int) -> bool {
    if link_status.is_null() || lane_count <= 0 {
        return false;
    }
    // SAFETY: `link_status` is non-null (checked above) and readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let lane_align = dp_link_status(status, DP_LANE_ALIGN_STATUS_UPDATED as usize);
    if (lane_align & DP_INTERLANE_ALIGN_DONE) == 0 {
        return false;
    }
    for lane in 0..(lane_count as usize) {
        let lane_status = dp_get_lane_status(status, lane);
        if (lane_status & DP_CHANNEL_EQ_BITS) != DP_CHANNEL_EQ_BITS {
            return false;
        }
    }
    true
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_clock_recovery_ok()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
/// `lane_count` must be within `1..=4`.
pub unsafe extern "C" fn drm_dp_clock_recovery_ok(
    link_status: *const u8,
    lane_count: c_int,
) -> bool {
    if link_status.is_null() || lane_count <= 0 {
        return false;
    }
    // SAFETY: `link_status` is non-null (checked above) and readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    for lane in 0..(lane_count as usize) {
        let lane_status = dp_get_lane_status(status, lane);
        if (lane_status & DP_LANE_CR_DONE) == 0 {
            return false;
        }
    }
    true
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_post_lt_adj_req_in_progress()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_post_lt_adj_req_in_progress(link_status: *const u8) -> bool {
    if link_status.is_null() {
        return false;
    }
    // SAFETY: `link_status` is non-null (checked above) and readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let lane_align = dp_link_status(status, DP_LANE_ALIGN_STATUS_UPDATED as usize);
    (lane_align & DP_POST_LT_ADJ_REQ_IN_PROGRESS) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_get_adjust_request_voltage()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
/// `lane` must be within `0..=3`.
pub unsafe extern "C" fn drm_dp_get_adjust_request_voltage(
    link_status: *const u8,
    lane: c_int,
) -> u8 {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let lane = lane as usize;
    let i = DP_ADJUST_REQUEST_LANE0_1 as usize + (lane >> 1);
    let s = if (lane & 1) != 0 {
        DP_ADJUST_VOLTAGE_SWING_LANE1_SHIFT
    } else {
        DP_ADJUST_VOLTAGE_SWING_LANE0_SHIFT
    };
    let l = dp_link_status(status, i);
    ((l >> s) & 0x3) << DP_TRAIN_VOLTAGE_SWING_SHIFT
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_get_adjust_request_pre_emphasis()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
/// `lane` must be within `0..=3`.
pub unsafe extern "C" fn drm_dp_get_adjust_request_pre_emphasis(
    link_status: *const u8,
    lane: c_int,
) -> u8 {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let lane = lane as usize;
    let i = DP_ADJUST_REQUEST_LANE0_1 as usize + (lane >> 1);
    let s = if (lane & 1) != 0 {
        DP_ADJUST_PRE_EMPHASIS_LANE1_SHIFT
    } else {
        DP_ADJUST_PRE_EMPHASIS_LANE0_SHIFT
    };
    let l = dp_link_status(status, i);
    ((l >> s) & 0x3) << DP_TRAIN_PRE_EMPHASIS_SHIFT
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_get_adjust_tx_ffe_preset()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
/// `lane` must be within `0..=3`.
pub unsafe extern "C" fn drm_dp_get_adjust_tx_ffe_preset(
    link_status: *const u8,
    lane: c_int,
) -> u8 {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let lane = lane as usize;
    let i = DP_ADJUST_REQUEST_LANE0_1 as usize + (lane >> 1);
    let s = if (lane & 1) != 0 {
        DP_ADJUST_TX_FFE_PRESET_LANE1_SHIFT
    } else {
        DP_ADJUST_TX_FFE_PRESET_LANE0_SHIFT
    };
    let l = dp_link_status(status, i);
    (l >> s) & 0xf
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_128b132b_lane_channel_eq_done()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
/// `lane_count` must be within `1..=4`.
pub unsafe extern "C" fn drm_dp_128b132b_lane_channel_eq_done(
    link_status: *const u8,
    lane_count: c_int,
) -> bool {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let lane_align = dp_link_status(status, DP_LANE_ALIGN_STATUS_UPDATED as usize);
    if (lane_align & DP_INTERLANE_ALIGN_DONE) == 0 {
        return false;
    }
    for lane in 0..(lane_count as usize) {
        let lane_status = dp_get_lane_status(status, lane);
        if (lane_status & DP_LANE_CHANNEL_EQ_DONE) == 0 {
            return false;
        }
    }
    true
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_128b132b_lane_symbol_locked()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
/// `lane_count` must be within `1..=4`.
pub unsafe extern "C" fn drm_dp_128b132b_lane_symbol_locked(
    link_status: *const u8,
    lane_count: c_int,
) -> bool {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    for lane in 0..(lane_count as usize) {
        let lane_status = dp_get_lane_status(status, lane);
        if (lane_status & DP_LANE_SYMBOL_LOCKED) == 0 {
            return false;
        }
    }
    true
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_128b132b_eq_interlane_align_done()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_128b132b_eq_interlane_align_done(link_status: *const u8) -> bool {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let s = dp_link_status(status, DP_LANE_ALIGN_STATUS_UPDATED as usize);
    (s & DP_128B132B_DPRX_EQ_INTERLANE_ALIGN_DONE) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_128b132b_cds_interlane_align_done()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_128b132b_cds_interlane_align_done(link_status: *const u8) -> bool {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let s = dp_link_status(status, DP_LANE_ALIGN_STATUS_UPDATED as usize);
    (s & DP_128B132B_DPRX_CDS_INTERLANE_ALIGN_DONE) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_128b132b_link_training_failed()
#[unsafe(no_mangle)]
/// # Safety
/// `link_status` must point to a readable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_128b132b_link_training_failed(link_status: *const u8) -> bool {
    // SAFETY: `link_status` is readable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let status = unsafe { core::slice::from_raw_parts(link_status, DP_LINK_STATUS_SIZE) };
    let s = dp_link_status(status, DP_LANE_ALIGN_STATUS_UPDATED as usize);
    (s & DP_128B132B_LT_FAILED) != 0
}

fn __8b10b_clock_recovery_delay_us(aux: *const DrmDpAux, rd_interval: u8) -> i32 {
    if rd_interval > 4 && !aux.is_null() {
        // SAFETY: aux is checked non-null and points to an initialized DrmDpAux with valid drm_dev.
        unsafe {
            drm_kapi::drm_dbg!(
                (*aux).drm_dev,
                "invalid AUX interval 0x{:02x} (max 4)\n",
                rd_interval
            );
        }
    }
    if rd_interval == 0 {
        100
    } else {
        (rd_interval as i32) * 4 * 1000
    }
}

fn __8b10b_channel_eq_delay_us(aux: *const DrmDpAux, rd_interval: u8) -> i32 {
    if rd_interval > 4 && !aux.is_null() {
        // SAFETY: aux is checked non-null and points to an initialized DrmDpAux with valid drm_dev.
        unsafe {
            drm_kapi::drm_dbg!(
                (*aux).drm_dev,
                "invalid AUX interval 0x{:02x} (max 4)\n",
                rd_interval
            );
        }
    }
    if rd_interval == 0 {
        400
    } else {
        (rd_interval as i32) * 4 * 1000
    }
}

fn __128b132b_channel_eq_delay_us(aux: *const DrmDpAux, rd_interval: u8) -> i32 {
    match rd_interval {
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_400_US => 400,
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_4_MS => 4000,
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_8_MS => 8000,
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_12_MS => 12000,
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_16_MS => 16000,
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_32_MS => 32000,
        DP_128B132B_TRAINING_AUX_RD_INTERVAL_64_MS => 64000,
        _ => {
            if !aux.is_null() {
                // SAFETY: aux is checked non-null and points to an initialized DrmDpAux with valid drm_dev.
                unsafe {
                    drm_kapi::drm_dbg!(
                        (*aux).drm_dev,
                        "invalid AUX interval 0x{:02x}\n",
                        rd_interval
                    );
                }
            }
            400
        }
    }
}

/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
/// `aux` must point to an initialized `DrmDpAux` when the interval register lies
/// outside the cached `dpcd` (LTTPR PHYs and 128b/132b DPRX).
unsafe fn __read_delay(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    dp_phy: DrmDpPhy,
    uhbr: bool,
    cr: bool,
) -> i32 {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    let offset: u32;
    let mask: u8;

    let parse: fn(*const DrmDpAux, u8) -> i32;

    if dp_phy == DpPhyDprx {
        if uhbr {
            if cr {
                return 100;
            }
            offset = DP_128B132B_TRAINING_AUX_RD_INTERVAL;
            mask = DP_128B132B_TRAINING_AUX_RD_INTERVAL_MASK;
            parse = __128b132b_channel_eq_delay_us;
        } else {
            if cr && dpcd_slice[DP_DPCD_REV as usize] >= DP_DPCD_REV_14 {
                return 100;
            }
            offset = DP_TRAINING_AUX_RD_INTERVAL;
            mask = DP_TRAINING_AUX_RD_MASK;
            if cr {
                parse = __8b10b_clock_recovery_delay_us;
            } else {
                parse = __8b10b_channel_eq_delay_us;
            }
        }
    } else {
        if uhbr {
            offset = dp_128b132b_training_aux_rd_interval_phy_repeater(dp_phy);
            mask = DP_128B132B_TRAINING_AUX_RD_INTERVAL_MASK;
            parse = __128b132b_channel_eq_delay_us;
        } else {
            if cr {
                return 100;
            }
            offset = dp_training_aux_rd_interval_phy_repeater(dp_phy);
            mask = DP_TRAINING_AUX_RD_MASK;
            parse = __8b10b_channel_eq_delay_us;
        }
    }

    let rd_interval = if (offset as usize) < DP_RECEIVER_CAP_SIZE {
        dpcd_slice[offset as usize]
    } else {
        let mut val = 0u8;
        // SAFETY: the register is outside `dpcd`, so the function's safety contract
        // requires `aux` to be an initialized `DrmDpAux`.
        if unsafe { drm_dp_dpcd_read_byte(aux, offset, &mut val) } < 0 {
            // SAFETY: `aux` was just passed to `drm_dp_dpcd_read_byte`, which requires an initialized `DrmDpAux`.
            drm_kapi::drm_dbg!(unsafe { (*aux).drm_dev }, "failed rd interval read\n");
            return 400;
        }
        val
    };

    parse(aux, rd_interval & mask)
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_clock_recovery_delay()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback is valid.
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_read_clock_recovery_delay(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    dp_phy: DrmDpPhy,
    uhbr: bool,
) -> c_int {
    // SAFETY: `aux` and `dpcd` satisfy `__read_delay`'s safety contract, which matches this function's contract.
    unsafe { __read_delay(aux, dpcd, dp_phy, uhbr, true) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_channel_eq_delay()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback is valid.
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_read_channel_eq_delay(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    dp_phy: DrmDpPhy,
    uhbr: bool,
) -> c_int {
    // SAFETY: `aux` and `dpcd` satisfy `__read_delay`'s safety contract, which matches this function's contract.
    unsafe { __read_delay(aux, dpcd, dp_phy, uhbr, false) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_128b132b_read_aux_rd_interval()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback is valid.
pub unsafe extern "C" fn drm_dp_128b132b_read_aux_rd_interval(aux: *mut DrmDpAux) -> c_int {
    let mut val = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `val` is a live local.
    if unsafe { drm_dp_dpcd_read_byte(aux, DP_128B132B_TRAINING_AUX_RD_INTERVAL, &mut val) } < 0 {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        drm_kapi::drm_err!(unsafe { (*aux).drm_dev }, "failed rd interval read\n");
        val = DP_128B132B_TRAINING_AUX_RD_INTERVAL_MASK;
    }

    let unit = if (val & DP_128B132B_TRAINING_AUX_RD_INTERVAL_1MS_UNIT) != 0 {
        1
    } else {
        2
    };
    val &= DP_128B132B_TRAINING_AUX_RD_INTERVAL_MASK;

    ((val as c_int) + 1) * unit * 1000
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_link_train_clock_recovery_delay()
#[unsafe(no_mangle)]
/// # Safety
/// If `aux` is non-null, it must point to an initialized `DrmDpAux`.
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_link_train_clock_recovery_delay(
    aux: *const DrmDpAux,
    dpcd: *const u8,
) {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    let rd_interval = dpcd_slice[DP_TRAINING_AUX_RD_INTERVAL as usize] & DP_TRAINING_AUX_RD_MASK;
    let delay_us = if dpcd_slice[DP_DPCD_REV as usize] >= DP_DPCD_REV_14 {
        100
    } else {
        __8b10b_clock_recovery_delay_us(aux, rd_interval)
    };
    usleep_range(delay_us as u64, (delay_us * 2) as u64);
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_link_train_channel_eq_delay()
#[unsafe(no_mangle)]
/// # Safety
/// If `aux` is non-null, it must point to an initialized `DrmDpAux`.
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_link_train_channel_eq_delay(aux: *const DrmDpAux, dpcd: *const u8) {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    let rd_interval = dpcd_slice[DP_TRAINING_AUX_RD_INTERVAL as usize] & DP_TRAINING_AUX_RD_MASK;
    let delay_us = __8b10b_channel_eq_delay_us(aux, rd_interval);
    usleep_range(delay_us as u64, (delay_us * 2) as u64);
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_phy_name()
#[unsafe(no_mangle)]
/// # Safety
/// Pure enum lookup; no pointer preconditions. Returns a pointer to a static NUL-terminated string.
pub unsafe extern "C" fn drm_dp_phy_name(dp_phy: DrmDpPhy) -> *const c_char {
    match dp_phy {
        DpPhyDprx => c"DPRX".as_ptr(),
        DpPhyLttpr1 => c"LTTPR 1".as_ptr(),
        DpPhyLttpr2 => c"LTTPR 2".as_ptr(),
        DpPhyLttpr3 => c"LTTPR 3".as_ptr(),
        DpPhyLttpr4 => c"LTTPR 4".as_ptr(),
        DpPhyLttpr5 => c"LTTPR 5".as_ptr(),
        DpPhyLttpr6 => c"LTTPR 6".as_ptr(),
        DpPhyLttpr7 => c"LTTPR 7".as_ptr(),
        DpPhyLttpr8 => c"LTTPR 8".as_ptr(),
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_link_train_clock_recovery_delay()
#[unsafe(no_mangle)]
/// # Safety
/// No parameters; delays execution for the standard LTTPR clock recovery duration.
pub unsafe extern "C" fn drm_dp_lttpr_link_train_clock_recovery_delay() {
    usleep_range(100, 200);
}

#[inline]
fn dp_lttpr_phy_cap(phy_cap: &[u8], r: usize) -> u8 {
    phy_cap[r - DP_TRAINING_AUX_RD_INTERVAL_PHY_REPEATER1 as usize]
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_link_train_channel_eq_delay()
#[unsafe(no_mangle)]
/// # Safety
/// `phy_cap` must point to a readable buffer of at least `DP_LTTPR_PHY_CAP_SIZE` bytes.
/// If `aux` is non-null, it must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_lttpr_link_train_channel_eq_delay(
    aux: *const DrmDpAux,
    phy_cap: *const u8,
) {
    // SAFETY: `phy_cap` is readable for `DP_LTTPR_PHY_CAP_SIZE` bytes per the function's safety contract.
    let phy_cap_slice = unsafe { core::slice::from_raw_parts(phy_cap, DP_LTTPR_PHY_CAP_SIZE) };
    let interval = dp_lttpr_phy_cap(
        phy_cap_slice,
        DP_TRAINING_AUX_RD_INTERVAL_PHY_REPEATER1 as usize,
    ) & DP_TRAINING_AUX_RD_MASK;
    let delay_us = __8b10b_channel_eq_delay_us(aux, interval);
    usleep_range(delay_us as u64, (delay_us * 2) as u64);
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_wake_timeout_setup()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback is valid.
/// `caps` must point to a readable buffer of at least `DP_LTTPR_COMMON_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_lttpr_wake_timeout_setup(
    aux: *mut DrmDpAux,
    transparent_mode: bool,
) {
    let mut val = 1u8;
    if transparent_mode {
        static TIMEOUT_MAPPING: [u8; 6] = [1, 20, 40, 60, 80, 100];
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `val` is a live local.
        let ret = unsafe {
            drm_dp_dpcd_readb(aux, DP_EXTENDED_DPRX_SLEEP_WAKE_TIMEOUT_REQUEST, &mut val)
        };
        if ret != 1 {
            drm_kapi::drm_dbg!(
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).drm_dev },
                "Failed to read Extended sleep wake timeout request\n"
            );
            return;
        }

        let val_usize = val as usize;
        val = if val_usize < TIMEOUT_MAPPING.len() && TIMEOUT_MAPPING[val_usize] != 0 {
            TIMEOUT_MAPPING[val_usize]
        } else {
            1
        };

        if val > 1 {
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            unsafe {
                drm_dp_dpcd_writeb(
                    aux,
                    DP_EXTENDED_DPRX_SLEEP_WAKE_TIMEOUT_GRANT,
                    DP_DPRX_SLEEP_WAKE_TIMEOUT_PERIOD_GRANTED,
                );
            }
        }
    } else {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `val` is a live local.
        let ret =
            unsafe { drm_dp_dpcd_readb(aux, DP_PHY_REPEATER_EXTENDED_WAIT_TIMEOUT, &mut val) };
        if ret != 1 {
            drm_kapi::drm_dbg!(
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).drm_dev },
                "Failed to read Extended sleep wake timeout request\n"
            );
            return;
        }

        val = if (val & DP_EXTENDED_WAKE_TIMEOUT_REQUEST_MASK) != 0 {
            (val & DP_EXTENDED_WAKE_TIMEOUT_REQUEST_MASK) * 10
        } else {
            1
        };

        if val > 1 {
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            unsafe {
                drm_dp_dpcd_writeb(
                    aux,
                    DP_PHY_REPEATER_EXTENDED_WAIT_TIMEOUT,
                    DP_EXTENDED_WAKE_TIMEOUT_GRANT,
                );
            }
        }
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_link_rate_to_bw_code()
#[unsafe(no_mangle)]
/// # Safety
/// Pure numerical conversion from link rate in kHz to DPCD bandwidth code; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_link_rate_to_bw_code(link_rate: c_int) -> u8 {
    match link_rate {
        1000000 => DP_LINK_BW_10,
        1350000 => DP_LINK_BW_13_5,
        2000000 => DP_LINK_BW_20,
        _ => (link_rate / 27000) as u8,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_bw_code_to_link_rate()
#[unsafe(no_mangle)]
/// # Safety
/// Pure numerical conversion from DPCD bandwidth code to link rate in kHz; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_bw_code_to_link_rate(link_bw: u8) -> c_int {
    match link_bw {
        DP_LINK_BW_10 => 1000000,
        DP_LINK_BW_13_5 => 1350000,
        DP_LINK_BW_20 => 2000000,
        _ => (link_bw as c_int) * 27000,
    }
}

const AUX_RETRY_INTERVAL: u64 = 500; // us

/// # Safety
/// `aux` must point to an initialized, valid `DrmDpAux`.
/// `buffer` must point to readable or writable memory of at least `size` bytes depending on `request`.
unsafe fn drm_dp_dpcd_access(
    aux: *mut DrmDpAux,
    request: u8,
    offset: c_uint,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    let mut msg = DrmDpAuxMsg {
        address: offset,
        request,
        reply: 0,
        buffer,
        size,
    };

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { mutex_lock(&mut (*aux).hw_mutex) };

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { (*aux).powered_down } {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        unsafe { mutex_unlock(&mut (*aux).hw_mutex) };
        return -(EBUSY as isize);
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let transfer = match unsafe { (*aux).transfer } {
        Some(f) => f,
        None => {
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            unsafe { mutex_unlock(&mut (*aux).hw_mutex) };
            return -(EIO as isize);
        }
    };

    let mut err: isize = 0;
    let mut ret: isize = 0;

    for _retry in 0..32 {
        if ret != 0 && ret != -(ETIMEDOUT as isize) {
            usleep_range(AUX_RETRY_INTERVAL, AUX_RETRY_INTERVAL + 100);
        }

        // SAFETY: `transfer` is the aux callback and `aux`/`msg` are valid per the function's safety contract.
        ret = unsafe { transfer(aux, &mut msg) };
        if ret >= 0 {
            let native_reply = msg.reply & DP_AUX_NATIVE_REPLY_MASK;
            if native_reply == DP_AUX_NATIVE_REPLY_ACK {
                if ret as usize == size {
                    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                    unsafe { mutex_unlock(&mut (*aux).hw_mutex) };
                    return ret;
                }
                ret = -(EPROTO as isize);
            } else {
                ret = -(EIO as isize);
            }
        }

        if err == 0 {
            err = ret;
        }
    }

    drm_kapi::drm_dbg!(
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        unsafe { (*aux).drm_dev },
        "Too many retries, giving up. First error: {}\n",
        err
    );
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { mutex_unlock(&mut (*aux).hw_mutex) };
    err
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_probe()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback is valid.
pub unsafe extern "C" fn drm_dp_dpcd_probe(aux: *mut DrmDpAux, offset: c_uint) -> c_int {
    let mut buffer = 0u8;
    // SAFETY: `aux` is initialized and valid per the function's safety contract; `buffer` is a live local holding at least 1 byte.
    let ret = unsafe {
        drm_dp_dpcd_access(
            aux,
            DP_AUX_NATIVE_READ,
            offset,
            &mut buffer as *mut u8 as *mut c_void,
            1,
        )
    };
    if ret < 0 { ret as c_int } else { 0 }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_set_powered()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_dpcd_set_powered(aux: *mut DrmDpAux, powered: bool) {
    if aux.is_null() {
        return;
    }
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { mutex_lock(&mut (*aux).hw_mutex) };
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { (*aux).powered_down = !powered };
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { mutex_unlock(&mut (*aux).hw_mutex) };
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_set_probe()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_dpcd_set_probe(aux: *mut DrmDpAux, enable: bool) {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { (*aux).dpcd_probe_disabled = !enable };
}

#[inline]
/// # Safety
/// `aux` must point to an initialized, valid `DrmDpAux`.
unsafe fn dpcd_access_needs_probe(aux: *const DrmDpAux) -> bool {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { !(*aux).is_remote && !(*aux).dpcd_probe_disabled }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_read()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` with an active `transfer` function.
/// `buffer` must point to a writable memory region of at least `size` bytes.
pub unsafe extern "C" fn drm_dp_dpcd_read(
    aux: *mut DrmDpAux,
    offset: c_uint,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { dpcd_access_needs_probe(aux) } {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        let ret = unsafe { drm_dp_dpcd_probe(aux, DP_TRAINING_PATTERN_SET) };
        if ret < 0 {
            return ret as isize;
        }
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { (*aux).is_remote } {
        -(EOPNOTSUPP as isize)
    } else {
        // SAFETY: `aux` is valid and `buffer` covers `size` bytes per the function's safety contract.
        unsafe { drm_dp_dpcd_access(aux, DP_AUX_NATIVE_READ, offset, buffer, size) }
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_write()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` with an active `transfer` function.
/// `buffer` must point to a readable memory region of at least `size` bytes.
pub unsafe extern "C" fn drm_dp_dpcd_write(
    aux: *mut DrmDpAux,
    offset: c_uint,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { (*aux).is_remote } {
        -(EOPNOTSUPP as isize)
    } else {
        // SAFETY: `aux` is valid and `buffer` covers `size` bytes per the function's safety contract.
        unsafe { drm_dp_dpcd_access(aux, DP_AUX_NATIVE_WRITE, offset, buffer, size) }
    }
}

// Inline helper functions mirrored from include/drm/display/drm_dp_helper.h
#[inline]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `valuep` must point to a writable `u8`.
pub unsafe fn drm_dp_dpcd_readb(aux: *mut DrmDpAux, offset: c_uint, valuep: *mut u8) -> isize {
    // SAFETY: `aux` is valid and `valuep` is writable for 1 byte per the function's safety contract.
    unsafe { drm_dp_dpcd_read(aux, offset, valuep as *mut c_void, 1) }
}

#[inline]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe fn drm_dp_dpcd_writeb(aux: *mut DrmDpAux, offset: c_uint, value: u8) -> isize {
    let mut val = value;
    // SAFETY: `aux` is valid per the function's safety contract; `val` is a live local of 1 byte.
    unsafe { drm_dp_dpcd_write(aux, offset, &mut val as *mut u8 as *mut c_void, 1) }
}

#[inline]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `buffer` must point to a writable buffer of at least `size` bytes.
pub unsafe fn drm_dp_dpcd_read_data(
    aux: *mut DrmDpAux,
    offset: c_uint,
    buffer: *mut c_void,
    size: usize,
) -> c_int {
    // SAFETY: `aux` is valid and `buffer` is writable for `size` bytes per the function's safety contract.
    let ret = unsafe { drm_dp_dpcd_read(aux, offset, buffer, size) };
    if ret >= 0 {
        if (ret as usize) < size {
            return -EPROTO;
        }
        return 0;
    }

    let buf = buffer as *mut u8;
    for i in 0..size {
        // SAFETY: `aux` is valid and `buf.add(i)` with `i < size` stays within the `size`-byte buffer per the function's safety contract.
        let single_ret = unsafe { drm_dp_dpcd_readb(aux, offset + (i as c_uint), buf.add(i)) };
        if single_ret < 0 {
            return single_ret as c_int;
        }
    }
    0
}

#[inline]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `buffer` must point to a readable buffer of at least `size` bytes.
pub unsafe fn drm_dp_dpcd_write_data(
    aux: *mut DrmDpAux,
    offset: c_uint,
    buffer: *const c_void,
    size: usize,
) -> c_int {
    // SAFETY: `aux` is valid and `buffer` is readable for `size` bytes per the function's safety contract.
    let ret = unsafe { drm_dp_dpcd_write(aux, offset, buffer as *mut c_void, size) };
    if ret < 0 {
        return ret as c_int;
    }
    if (ret as usize) < size {
        return -EPROTO;
    }
    0
}

#[inline]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `valuep` must point to a writable `u8`.
pub unsafe fn drm_dp_dpcd_read_byte(aux: *mut DrmDpAux, offset: c_uint, valuep: *mut u8) -> c_int {
    // SAFETY: `aux` is valid and `valuep` is writable for 1 byte per the function's safety contract.
    unsafe { drm_dp_dpcd_read_data(aux, offset, valuep as *mut c_void, 1) }
}

#[inline]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe fn drm_dp_dpcd_write_byte(aux: *mut DrmDpAux, offset: c_uint, value: u8) -> c_int {
    let mut val = value;
    // SAFETY: `aux` is valid per the function's safety contract; `val` is a live local of 1 byte.
    unsafe { drm_dp_dpcd_write_data(aux, offset, &mut val as *mut u8 as *const c_void, 1) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_read_link_status()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `status` must point to a writable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dpcd_read_link_status(
    aux: *mut DrmDpAux,
    status: *mut u8,
) -> c_int {
    // SAFETY: `aux` is valid and `status` is writable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    unsafe {
        drm_dp_dpcd_read_data(
            aux,
            DP_LANE0_1_STATUS,
            status as *mut c_void,
            DP_LINK_STATUS_SIZE,
        )
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_read_phy_link_status()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `link_status` must point to a writable buffer of at least `DP_LINK_STATUS_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dpcd_read_phy_link_status(
    aux: *mut DrmDpAux,
    dp_phy: DrmDpPhy,
    link_status: *mut u8,
) -> c_int {
    if dp_phy == DpPhyDprx {
        // SAFETY: `aux` is valid and `link_status` is writable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
        return unsafe {
            drm_dp_dpcd_read_data(
                aux,
                DP_LANE0_1_STATUS,
                link_status as *mut c_void,
                DP_LINK_STATUS_SIZE,
            )
        };
    }

    // SAFETY: `aux` is valid and `link_status` is writable for `DP_LINK_STATUS_SIZE - 1` bytes per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_read_data(
            aux,
            dp_lane0_1_status_phy_repeater(dp_phy),
            link_status as *mut c_void,
            DP_LINK_STATUS_SIZE - 1,
        )
    };
    if ret < 0 {
        return ret;
    }

    // Convert the LTTPR to the sink PHY link status layout: insert 0 at index 3 (DP_SINK_STATUS)
    // SAFETY: `link_status` is writable for `DP_LINK_STATUS_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts_mut(link_status, DP_LINK_STATUS_SIZE) };
    slice[5] = slice[4];
    slice[4] = slice[3];
    slice[3] = 0;

    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_link_power_up()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_link_power_up(aux: *mut DrmDpAux, revision: u8) -> c_int {
    if revision < DP_DPCD_REV_11 {
        return 0;
    }

    let mut value = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract. `value` is a live local.
    let err = unsafe { drm_dp_dpcd_readb(aux, DP_SET_POWER, &mut value) };
    if err < 0 {
        return err as c_int;
    }

    value &= !DP_SET_POWER_MASK;
    value |= DP_SET_POWER_D0;

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let err = unsafe { drm_dp_dpcd_writeb(aux, DP_SET_POWER, value) };
    if err < 0 {
        return err as c_int;
    }

    usleep_range(1000, 2000);
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_link_power_down()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_link_power_down(aux: *mut DrmDpAux, revision: u8) -> c_int {
    if revision < DP_DPCD_REV_11 {
        return 0;
    }

    let mut value = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract. `value` is a live local.
    let err = unsafe { drm_dp_dpcd_readb(aux, DP_SET_POWER, &mut value) };
    if err < 0 {
        return err as c_int;
    }

    value &= !DP_SET_POWER_MASK;
    value |= DP_SET_POWER_D3;

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let err = unsafe { drm_dp_dpcd_writeb(aux, DP_SET_POWER, value) };
    if err < 0 {
        return err as c_int;
    }

    0
}

/// # Safety
/// `aux` must point to an initialized, valid `DrmDpAux`.
unsafe fn read_payload_update_status(aux: *mut DrmDpAux) -> c_int {
    let mut status = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract. `status` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PAYLOAD_TABLE_UPDATE_STATUS, &mut status) };
    if ret < 0 { ret } else { status as c_int }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_write_payload()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// If `payload` is non-null, it must point to an initialized, writable `DrmDpPayload`.
pub unsafe extern "C" fn drm_dp_dpcd_write_payload(
    aux: *mut DrmDpAux,
    vcpid: c_int,
    start_time_slot: u8,
    time_slot_count: u8,
) -> c_int {
    let mut payload_alloc = [0u8; 3];
    let mut status = 0u8;

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe {
        drm_dp_dpcd_write_byte(
            aux,
            DP_PAYLOAD_TABLE_UPDATE_STATUS,
            DP_PAYLOAD_TABLE_UPDATED,
        );
    }

    payload_alloc[0] = vcpid as u8;
    payload_alloc[1] = start_time_slot;
    payload_alloc[2] = time_slot_count;

    // SAFETY: `aux` is valid and `payload_alloc` is readable for 3 bytes per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_write_data(
            aux,
            DP_PAYLOAD_ALLOCATE_SET,
            payload_alloc.as_ptr() as *const c_void,
            3,
        )
    };
    if ret < 0 {
        drm_kapi::drm_dbg!(
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            unsafe { (*aux).drm_dev },
            "failed to write payload allocation {}\n",
            ret
        );
        return ret;
    }

    let mut retries = 0;
    loop {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `status` is a live local.
        let ret =
            unsafe { drm_dp_dpcd_read_byte(aux, DP_PAYLOAD_TABLE_UPDATE_STATUS, &mut status) };
        if ret < 0 {
            drm_kapi::drm_dbg!(
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).drm_dev },
                "failed to read payload table status {}\n",
                ret
            );
            return ret;
        }

        if (status & DP_PAYLOAD_TABLE_UPDATED) != 0 {
            return 0;
        }

        retries += 1;
        if retries < 20 {
            usleep_range(10000, 20000);
            continue;
        }

        drm_kapi::drm_dbg!(
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            unsafe { (*aux).drm_dev },
            "status not set after read payload table status {}\n",
            status
        );
        return -EINVAL;
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_clear_payload()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_dpcd_clear_payload(aux: *mut DrmDpAux) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_payload(aux, 0, 0, 0x3f) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dpcd_poll_act_handled()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_dpcd_poll_act_handled(
    aux: *mut DrmDpAux,
    mut timeout_ms: c_int,
) -> c_int {
    if timeout_ms == 0 {
        timeout_ms = 3000;
    }
    let timeout_ns = (timeout_ms as i64) * 1_000_000;
    let start = drm_kapi::primitives::ktime_get();

    loop {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        let status = unsafe { read_payload_update_status(aux) };
        if status < 0 {
            drm_kapi::drm_dbg!(
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).drm_dev },
                "Failed to read payload table status: {}\n",
                status
            );
            return status;
        }
        if (status & (DP_PAYLOAD_ACT_HANDLED as c_int)) != 0 {
            return 0;
        }

        if drm_kapi::primitives::ktime_get() - start >= timeout_ns {
            drm_kapi::drm_err!(
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).drm_dev },
                "Failed to get ACT after {} ms, last status: {:02x}\n",
                timeout_ms,
                status
            );
            return -EINVAL;
        }

        usleep_range(200, 400);
    }
}

/// # Safety
/// If `drm_edid` is non-null, it must point to a valid `DrmEdid` struct.
unsafe fn is_edid_digital_input_dp(drm_edid: *const DrmEdid) -> bool {
    // SAFETY: `drm_edid` is valid or null per the function's safety contract, as `is_edid_digital_input_dp` requires.
    let edid = unsafe { drm_edid_raw(drm_edid) };
    if edid.is_null() {
        return false;
    }
    // SAFETY: `edid` is non-null (checked above) and points to the EDID returned by `drm_edid_raw`.
    unsafe {
        (*edid).revision >= 4
            && ((*edid).input & DRM_EDID_INPUT_DIGITAL) != 0
            && ((*edid).input & DRM_EDID_DIGITAL_TYPE_MASK) == DRM_EDID_DIGITAL_TYPE_DP
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_is_type()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_downstream_is_type(
    dpcd: *const u8,
    port_cap: *const u8,
    type_: u8,
) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };
    // SAFETY: `dpcd` is readable per the function's safety contract.
    (unsafe { drm_dp_is_branch(dpcd) })
        && dpcd_slice[DP_DPCD_REV as usize] >= 0x11
        && (port_cap_slice[0] & DP_DS_PORT_TYPE_MASK) == type_
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_is_tmds()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes. If `edid` is non-null, it must point to a valid `DrmEdid`.
pub unsafe extern "C" fn drm_dp_downstream_is_tmds(
    dpcd: *const u8,
    port_cap: *const u8,
    drm_edid: *const DrmEdid,
) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    if dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_TYPE_MASK)
            == DP_DWN_STRM_PORT_TYPE_TMDS;
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        // SAFETY: `drm_edid` is valid or null per the function's safety contract, as `is_edid_digital_input_dp` requires.
        DP_DS_PORT_TYPE_DP_DUALMODE => !unsafe { is_edid_digital_input_dp(drm_edid) },
        DP_DS_PORT_TYPE_DVI | DP_DS_PORT_TYPE_HDMI => true,
        _ => false,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_send_real_edid_checksum()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_send_real_edid_checksum(
    aux: *mut DrmDpAux,
    real_edid_checksum: u8,
) -> bool {
    let mut auto_test_req = 0u8;
    let mut link_edid_read = 0u8;

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract. `auto_test_req` is a live local.
    if unsafe { drm_dp_dpcd_read_byte(aux, DP_DEVICE_SERVICE_IRQ_VECTOR, &mut auto_test_req) } < 0 {
        return false;
    }
    auto_test_req &= DP_AUTOMATED_TEST_REQUEST;

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract. `link_edid_read` is a live local.
    if unsafe { drm_dp_dpcd_read_byte(aux, DP_TEST_REQUEST, &mut link_edid_read) } < 0 {
        return false;
    }
    link_edid_read &= DP_TEST_LINK_EDID_READ;

    if auto_test_req == 0 || link_edid_read == 0 {
        return false;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { drm_dp_dpcd_write_byte(aux, DP_DEVICE_SERVICE_IRQ_VECTOR, auto_test_req) } < 0 {
        return false;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { drm_dp_dpcd_write_byte(aux, DP_TEST_EDID_CHECKSUM, real_edid_checksum) } < 0 {
        return false;
    }

    let test_resp = DP_TEST_EDID_CHECKSUM_WRITE;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    (unsafe { drm_dp_dpcd_write_byte(aux, DP_TEST_RESPONSE, test_resp) }) >= 0
}

fn drm_dp_downstream_port_count(dpcd: &[u8]) -> u8 {
    let mut port_count = dpcd[DP_DOWN_STREAM_PORT_COUNT as usize] & DP_PORT_COUNT_MASK;
    if (dpcd[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE) != 0
        && port_count > 4
    {
        port_count = 4;
    }
    port_count
}

/// # Safety
/// `aux` must point to an initialized, valid `DrmDpAux`.
/// `dpcd` must be at least `DP_RECEIVER_CAP_SIZE` bytes.
unsafe fn drm_dp_read_extended_dpcd_caps(aux: *mut DrmDpAux, dpcd: &mut [u8]) -> c_int {
    if (dpcd[DP_TRAINING_AUX_RD_INTERVAL as usize] & DP_EXTENDED_RECEIVER_CAP_FIELD_PRESENT) == 0 {
        return 0;
    }

    let mut dpcd_ext = [0u8; DP_RECEIVER_CAP_SIZE];
    // SAFETY: `aux` is valid and `dpcd_ext` is writable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_read_data(
            aux,
            DP_DP13_DPCD_REV,
            dpcd_ext.as_mut_ptr() as *mut c_void,
            DP_RECEIVER_CAP_SIZE,
        )
    };
    if ret < 0 {
        return ret;
    }

    if dpcd[DP_DPCD_REV as usize] > dpcd_ext[DP_DPCD_REV as usize] {
        return 0;
    }

    if dpcd == dpcd_ext {
        return 0;
    }

    dpcd.copy_from_slice(&dpcd_ext);
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_dpcd_caps()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to a writable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_read_dpcd_caps(aux: *mut DrmDpAux, dpcd: *mut u8) -> c_int {
    // SAFETY: `dpcd` is writable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts_mut(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: `aux` is valid and `dpcd_slice` is writable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_read_data(
            aux,
            DP_DPCD_REV,
            dpcd_slice.as_mut_ptr() as *mut c_void,
            DP_RECEIVER_CAP_SIZE,
        )
    };
    if ret < 0 {
        return ret;
    }
    if dpcd_slice[DP_DPCD_REV as usize] == 0 {
        return -EIO;
    }

    // SAFETY: `aux` is valid and `dpcd_slice` is writable per the function's safety contract.
    let ret = unsafe { drm_dp_read_extended_dpcd_caps(aux, dpcd_slice) };
    if ret < 0 {
        return ret;
    }

    ret
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_downstream_info()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `downstream_ports` must point to a writable buffer of at least `DP_MAX_DOWNSTREAM_PORTS` bytes.
pub unsafe extern "C" fn drm_dp_read_downstream_info(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    downstream_ports: *mut u8,
) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: `downstream_ports` is writable for `DP_MAX_DOWNSTREAM_PORTS` bytes per the function's safety contract.
    let ports_slice =
        unsafe { core::slice::from_raw_parts_mut(downstream_ports, DP_MAX_DOWNSTREAM_PORTS) };
    ports_slice.fill(0);

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] == DP_DPCD_REV_10 {
        return 0;
    }

    let mut len = drm_dp_downstream_port_count(dpcd_slice) as usize;
    if len == 0 {
        return 0;
    }

    if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE) != 0 {
        len *= 4;
    }

    // SAFETY: `aux` is valid and `downstream_ports` is writable for `len` bytes per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_read_data(
            aux,
            DP_DOWNSTREAM_PORT_0,
            downstream_ports as *mut c_void,
            len,
        )
    };
    if ret < 0 {
        return ret;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_max_dotclock()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_downstream_max_dotclock(
    dpcd: *const u8,
    port_cap: *const u8,
) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return 0;
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_VGA => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return 0;
            }
            (port_cap_slice[1] as c_int) * 8000
        }
        _ => 0,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_max_tmds_clock()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
/// If `edid` is non-null, it must point to a valid `DrmEdid`.
pub unsafe extern "C" fn drm_dp_downstream_max_tmds_clock(
    dpcd: *const u8,
    port_cap: *const u8,
    drm_edid: *const DrmEdid,
) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } {
        return 0;
    }

    if dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return match dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_TYPE_MASK {
            DP_DWN_STRM_PORT_TYPE_TMDS => 165000,
            _ => 0,
        };
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_DP_DUALMODE => {
            // SAFETY: `drm_edid` is valid or null per the function's safety contract, as `is_edid_digital_input_dp` requires.
            if unsafe { is_edid_digital_input_dp(drm_edid) } {
                return 0;
            }
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return 300000;
            }
            (port_cap_slice[1] as c_int) * 2500
        }
        DP_DS_PORT_TYPE_HDMI => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return 300000;
            }
            (port_cap_slice[1] as c_int) * 2500
        }
        DP_DS_PORT_TYPE_DVI => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return 165000;
            }
            (port_cap_slice[1] as c_int) * 2500
        }
        _ => 0,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_min_tmds_clock()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
/// If `edid` is non-null, it must point to a valid `DrmEdid`.
pub unsafe extern "C" fn drm_dp_downstream_min_tmds_clock(
    dpcd: *const u8,
    port_cap: *const u8,
    drm_edid: *const DrmEdid,
) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } {
        return 0;
    }

    if dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return match dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_TYPE_MASK {
            DP_DWN_STRM_PORT_TYPE_TMDS => 25000,
            _ => 0,
        };
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_DP_DUALMODE => {
            // SAFETY: `drm_edid` is valid or null per the function's safety contract, as `is_edid_digital_input_dp` requires.
            if unsafe { is_edid_digital_input_dp(drm_edid) } {
                0
            } else {
                25000
            }
        }
        DP_DS_PORT_TYPE_DVI | DP_DS_PORT_TYPE_HDMI => 25000,
        _ => 0,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_max_bpc()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
/// If `edid` is non-null, it must point to a valid `DrmEdid`.
pub unsafe extern "C" fn drm_dp_downstream_max_bpc(
    dpcd: *const u8,
    port_cap: *const u8,
    drm_edid: *const DrmEdid,
) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } {
        return 0;
    }

    if dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return match dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_TYPE_MASK {
            DP_DWN_STRM_PORT_TYPE_DP => 0,
            _ => 8,
        };
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_DP => 0,
        DP_DS_PORT_TYPE_DP_DUALMODE => {
            // SAFETY: `drm_edid` is valid or null per the function's safety contract, as `is_edid_digital_input_dp` requires.
            if unsafe { is_edid_digital_input_dp(drm_edid) } {
                return 0;
            }
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return 8;
            }
            match port_cap_slice[2] & DP_DS_MAX_BPC_MASK {
                DP_DS_8BPC => 8,
                DP_DS_10BPC => 10,
                DP_DS_12BPC => 12,
                DP_DS_16BPC => 16,
                _ => 8,
            }
        }
        DP_DS_PORT_TYPE_HDMI | DP_DS_PORT_TYPE_DVI | DP_DS_PORT_TYPE_VGA => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return 8;
            }
            match port_cap_slice[2] & DP_DS_MAX_BPC_MASK {
                DP_DS_8BPC => 8,
                DP_DS_10BPC => 10,
                DP_DS_12BPC => 12,
                DP_DS_16BPC => 16,
                _ => 8,
            }
        }
        _ => 8,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_420_passthrough()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_downstream_420_passthrough(
    dpcd: *const u8,
    port_cap: *const u8,
) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] < 0x13 {
        return false;
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_DP => true,
        DP_DS_PORT_TYPE_HDMI => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return false;
            }
            (port_cap_slice[3] & DP_DS_HDMI_YCBCR420_PASS_THROUGH) != 0
        }
        _ => false,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_444_to_420_conversion()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_downstream_444_to_420_conversion(
    dpcd: *const u8,
    port_cap: *const u8,
) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] < 0x13 {
        return false;
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_HDMI => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return false;
            }
            (port_cap_slice[3] & DP_DS_HDMI_YCBCR444_TO_420_CONV) != 0
        }
        _ => false,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_rgb_to_ycbcr_conversion()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_downstream_rgb_to_ycbcr_conversion(
    dpcd: *const u8,
    port_cap: *const u8,
    color_spc: u8,
) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] < 0x13 {
        return false;
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_HDMI => {
            if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE)
                == 0
            {
                return false;
            }
            (port_cap_slice[3] & color_spc) != 0
        }
        _ => false,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_mode()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_downstream_mode(
    dev: *mut DrmDevice,
    dpcd: *const u8,
    port_cap: *const u8,
) -> *mut DrmDisplayMode {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return ptr::null_mut();
    }

    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_NON_EDID => {
            let vic: u8 = match port_cap_slice[0] & DP_DS_NON_EDID_MASK {
                DP_DS_NON_EDID_720X480I_60 => 6,
                DP_DS_NON_EDID_720X480I_50 => 21,
                DP_DS_NON_EDID_1920X1080I_60 => 5,
                DP_DS_NON_EDID_1920X1080I_50 => 20,
                DP_DS_NON_EDID_1280X720_60 => 4,
                DP_DS_NON_EDID_1280X720_50 => 19,
                _ => return ptr::null_mut(),
            };
            // SAFETY: `dev` is a valid `DrmDevice` per the function's safety contract.
            unsafe { drm_display_mode_from_cea_vic(dev, vic) }
        }
        _ => ptr::null_mut(),
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_downstream_id()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `id` must point to a writable buffer of at least 6 bytes.
pub unsafe extern "C" fn drm_dp_downstream_id(aux: *mut DrmDpAux, id: *mut c_char) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract. `id` is writable for 6 bytes per the same contract.
    unsafe { drm_dp_dpcd_read_data(aux, DP_BRANCH_ID, id as *mut c_void, 6) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_subconnector_type()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_subconnector_type(
    dpcd: *const u8,
    port_cap: *const u8,
) -> DrmModeSubconnector {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } {
        return DRM_MODE_SUBCONNECTOR_NATIVE;
    }

    if dpcd_slice[DP_DPCD_REV as usize] == DP_DPCD_REV_10 {
        let type_ = dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_TYPE_MASK;
        return match type_ {
            DP_DWN_STRM_PORT_TYPE_TMDS => DRM_MODE_SUBCONNECTOR_DVID,
            DP_DWN_STRM_PORT_TYPE_ANALOG => DRM_MODE_SUBCONNECTOR_VGA,
            DP_DWN_STRM_PORT_TYPE_DP => DRM_MODE_SUBCONNECTOR_DISPLAYPORT,
            _ => DRM_MODE_SUBCONNECTOR_UNKNOWN,
        };
    }

    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };
    match port_cap_slice[0] & DP_DS_PORT_TYPE_MASK {
        DP_DS_PORT_TYPE_DP | DP_DS_PORT_TYPE_DP_DUALMODE => DRM_MODE_SUBCONNECTOR_DISPLAYPORT,
        DP_DS_PORT_TYPE_VGA => DRM_MODE_SUBCONNECTOR_VGA,
        DP_DS_PORT_TYPE_DVI => DRM_MODE_SUBCONNECTOR_DVID,
        DP_DS_PORT_TYPE_HDMI => DRM_MODE_SUBCONNECTOR_HDMIA,
        DP_DS_PORT_TYPE_WIRELESS => DRM_MODE_SUBCONNECTOR_WIRELESS,
        _ => DRM_MODE_SUBCONNECTOR_UNKNOWN,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_set_subconnector_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must point to an initialized `DrmConnector`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_set_subconnector_property(
    connector: *mut DrmConnector,
    status: DrmConnectorStatus,
    dpcd: *const u8,
    port_cap: *const u8,
) {
    let mut subconnector = DRM_MODE_SUBCONNECTOR_UNKNOWN;
    if status == CONNECTOR_STATUS_CONNECTED {
        // SAFETY: `dpcd` and `port_cap` satisfy this function's safety contract, which is that of `drm_dp_subconnector_type`.
        subconnector = unsafe { drm_dp_subconnector_type(dpcd, port_cap) };
    }
    // SAFETY: `connector` is initialized per the function's safety contract; `dev` is checked for null in the same condition.
    if !connector.is_null() && !unsafe { (*connector).dev.is_null() } {
        // SAFETY: `connector` and its `dev` are non-null and live (checked above); `mode_config` points into `dev`.
        let mode_config = unsafe { core::ptr::addr_of_mut!((*(*connector).dev).mode_config) };
        // SAFETY: `connector` and its `dev` are non-null and live (checked above); `mode_config` points into `dev`.
        unsafe {
            drm_object_property_set_value(
                core::ptr::addr_of_mut!((*connector).base),
                (*mode_config).dp_subconnector_property,
                subconnector as u64,
            );
        }
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_sink_count_cap()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must point to an initialized `DrmConnector`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `desc` must point to a valid `DrmDpDesc`.
pub unsafe extern "C" fn drm_dp_read_sink_count_cap(
    connector: *mut DrmConnector,
    dpcd: *const u8,
    desc: *const DrmDpDesc,
) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: `connector` is initialized per the function's safety contract.
    (unsafe { (*connector).connector_type }) != DRM_MODE_CONNECTOR_EDP as i32
        && dpcd_slice[DP_DPCD_REV as usize] >= DP_DPCD_REV_11
        && (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_PRESENT) != 0
        // SAFETY: `desc` is a valid `DrmDpDesc` per the function's safety contract.
        && !unsafe { drm_dp_has_quirk(desc, DP_DPCD_QUIRK_NO_SINK_COUNT) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_sink_count()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_read_sink_count(aux: *mut DrmDpAux) -> c_int {
    let mut count = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `count` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_SINK_COUNT, &mut count) };
    if ret < 0 {
        return ret;
    }
    dp_get_sink_count(count)
}

// ---------------------------------------------------------------------------
// I2C-over-AUX
// ---------------------------------------------------------------------------

fn drm_dp_i2c_functionality() -> u32 {
    I2C_FUNC_I2C
        | I2C_FUNC_SMBUS_EMUL
        | I2C_FUNC_SMBUS_READ_BLOCK_DATA
        | I2C_FUNC_SMBUS_BLOCK_PROC_CALL
        | I2C_FUNC_10BIT_ADDR
}

fn drm_dp_i2c_msg_write_status_update(msg: &mut DrmDpAuxMsg) {
    if (msg.request & !DP_AUX_I2C_MOT) == DP_AUX_I2C_WRITE {
        msg.request &= DP_AUX_I2C_MOT;
        msg.request |= DP_AUX_I2C_WRITE_STATUS_UPDATE;
    }
}

const AUX_PRECHARGE_LEN: i32 = 10;
const AUX_SYNC_LEN: i32 = 20;
const AUX_STOP_LEN: i32 = 4;
const AUX_CMD_LEN: i32 = 4;
const AUX_ADDRESS_LEN: i32 = 20;
const AUX_REPLY_PAD_LEN: i32 = 4;
const AUX_LENGTH_LEN: i32 = 8;

fn drm_dp_aux_req_duration(msg: &DrmDpAuxMsg) -> i32 {
    let mut len = AUX_PRECHARGE_LEN
        + AUX_SYNC_LEN
        + AUX_STOP_LEN
        + AUX_CMD_LEN
        + AUX_ADDRESS_LEN
        + AUX_LENGTH_LEN;
    if (msg.request & DP_AUX_I2C_READ) == 0 {
        len += (msg.size as i32) * 8;
    }
    len
}

fn drm_dp_aux_reply_duration(msg: &DrmDpAuxMsg) -> i32 {
    let mut len = AUX_PRECHARGE_LEN + AUX_SYNC_LEN + AUX_STOP_LEN + AUX_CMD_LEN + AUX_REPLY_PAD_LEN;
    if (msg.request & DP_AUX_I2C_READ) != 0 {
        len += (msg.size as i32) * 8;
    }
    len
}

const I2C_START_LEN: i32 = 1;
const I2C_STOP_LEN: i32 = 1;
const I2C_ADDR_LEN: i32 = 9;
const I2C_DATA_LEN: i32 = 9;

fn drm_dp_i2c_msg_duration(msg: &DrmDpAuxMsg, i2c_speed_khz: i32) -> i32 {
    let bits = I2C_START_LEN + I2C_ADDR_LEN + (msg.size as i32) * I2C_DATA_LEN + I2C_STOP_LEN;
    (bits * 1000 + i2c_speed_khz - 1) / i2c_speed_khz
}

fn drm_dp_i2c_retry_count(msg: &DrmDpAuxMsg, i2c_speed_khz: i32) -> i32 {
    let aux_time_us = drm_dp_aux_req_duration(msg) + drm_dp_aux_reply_duration(msg);
    let i2c_time_us = drm_dp_i2c_msg_duration(msg, i2c_speed_khz);
    let div = aux_time_us + (AUX_RETRY_INTERVAL as i32);
    (i2c_time_us + div - 1) / div
}

/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback and `hw_mutex` are valid.
/// `msg` must contain valid buffer pointers for reading or writing `msg.size` bytes.
unsafe fn drm_dp_i2c_do_msg(aux: *mut DrmDpAux, msg: &mut DrmDpAuxMsg) -> i32 {
    let max_retries = 7.max(drm_dp_i2c_retry_count(msg, 10));
    let mut defer_i2c = 0;
    // SAFETY: `aux` is an initialized `DrmDpAux` with a valid `transfer` per the function's safety contract.
    let transfer = match unsafe { (*aux).transfer } {
        Some(f) => f,
        None => return -EIO,
    };

    let mut retry = 0;
    while retry < (max_retries + defer_i2c) {
        // SAFETY: `transfer` is the aux callback and `aux`/`msg` are valid per the function's safety contract.
        let ret = unsafe { transfer(aux, msg as *mut DrmDpAuxMsg) };
        if ret < 0 {
            if ret == -(EBUSY as isize) {
                retry += 1;
                continue;
            }
            return ret as i32;
        }

        match msg.reply & DP_AUX_NATIVE_REPLY_MASK {
            DP_AUX_NATIVE_REPLY_ACK => {}
            DP_AUX_NATIVE_REPLY_NACK => {
                return -EREMOTEIO;
            }
            DP_AUX_NATIVE_REPLY_DEFER => {
                usleep_range(AUX_RETRY_INTERVAL, AUX_RETRY_INTERVAL + 100);
                retry += 1;
                continue;
            }
            _ => {
                return -EREMOTEIO;
            }
        }

        match msg.reply & DP_AUX_I2C_REPLY_MASK {
            DP_AUX_I2C_REPLY_ACK => {
                if (ret as usize) != msg.size {
                    drm_dp_i2c_msg_write_status_update(msg);
                }
                return ret as i32;
            }
            DP_AUX_I2C_REPLY_NACK => {
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).i2c_nack_count += 1 };
                return -EREMOTEIO;
            }
            DP_AUX_I2C_REPLY_DEFER => {
                // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
                unsafe { (*aux).i2c_defer_count += 1 };
                if defer_i2c < 7 {
                    defer_i2c += 1;
                }
                usleep_range(AUX_RETRY_INTERVAL, AUX_RETRY_INTERVAL + 100);
                drm_dp_i2c_msg_write_status_update(msg);
                retry += 1;
                continue;
            }
            _ => {
                return -EREMOTEIO;
            }
        }
    }

    -EREMOTEIO
}

fn drm_dp_i2c_msg_set_request(msg: &mut DrmDpAuxMsg, flags: u16) {
    msg.request = if (flags & I2C_M_RD) != 0 {
        DP_AUX_I2C_READ
    } else {
        DP_AUX_I2C_WRITE
    };
    if (flags & I2C_M_STOP) == 0 {
        msg.request |= DP_AUX_I2C_MOT;
    }
}

/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `orig_msg` must contain a valid memory buffer of at least `orig_msg.size` bytes.
unsafe fn drm_dp_i2c_drain_msg(aux: *mut DrmDpAux, orig_msg: &mut DrmDpAuxMsg) -> i32 {
    let mut ret = orig_msg.size as i32;
    let mut msg = DrmDpAuxMsg {
        address: orig_msg.address,
        request: orig_msg.request,
        reply: orig_msg.reply,
        buffer: orig_msg.buffer,
        size: orig_msg.size,
    };

    while msg.size > 0 {
        // SAFETY: `aux` is valid per the function's safety contract; `msg` is a live local.
        let err = unsafe { drm_dp_i2c_do_msg(aux, &mut msg) };
        if err <= 0 {
            return if err == 0 { -EPROTO } else { err };
        }

        if err < (msg.size as i32) && err < ret {
            ret = err;
        }

        msg.size -= err as usize;
        msg.buffer = (msg.buffer as usize + err as usize) as *mut c_void;
    }

    ret
}

/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `msgs` elements must contain valid, live data buffers for their respective lengths.
unsafe fn drm_dp_i2c_xfer_impl(aux: *mut DrmDpAux, msgs: &mut [I2cMsg<'_>]) -> i32 {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { (*aux).powered_down } {
        return -EBUSY;
    }

    let num = msgs.len() as i32;

    let mut msg = DrmDpAuxMsg {
        address: 0,
        request: 0,
        reply: 0,
        buffer: ptr::null_mut(),
        size: 0,
    };

    let mut err = 0;
    for message in msgs.iter_mut() {
        msg.address = message.addr as u32;

        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        if !unsafe { (*aux).no_zero_sized } {
            drm_dp_i2c_msg_set_request(&mut msg, message.flags);
            msg.buffer = ptr::null_mut();
            msg.size = 0;
            // SAFETY: `aux` is valid per the function's safety contract; `msg` is a live local.
            err = unsafe { drm_dp_i2c_do_msg(aux, &mut msg) };
        }

        drm_dp_i2c_msg_set_request(&mut msg, message.flags);

        if err < 0 {
            break;
        }

        let mut transfer_size = DP_AUX_MAX_PAYLOAD_BYTES;
        let mut j = 0;
        while j < message.len {
            let chunk_size = transfer_size.min(message.len - j);
            msg.buffer = message.buf[j..].as_mut_ptr() as *mut c_void;
            msg.size = chunk_size;

            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            if j + msg.size == message.len && unsafe { (*aux).no_zero_sized } {
                msg.request &= !DP_AUX_I2C_MOT;
            }
            // SAFETY: `aux` is valid per the function's safety contract; `msg.buffer` covers `chunk_size` bytes of `message.buf`.
            let drain_ret = unsafe { drm_dp_i2c_drain_msg(aux, &mut msg) };
            drm_dp_i2c_msg_set_request(&mut msg, message.flags);

            if drain_ret < 0 {
                err = drain_ret;
                break;
            }
            transfer_size = drain_ret as usize;
            j += chunk_size;
        }
        if err < 0 {
            break;
        }
    }

    if err >= 0 {
        err = num;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if !unsafe { (*aux).no_zero_sized } {
        msg.request &= !DP_AUX_I2C_MOT;
        msg.buffer = ptr::null_mut();
        msg.size = 0;
        // SAFETY: `aux` is valid per the function's safety contract; `msg` is a live local.
        let _ = unsafe { drm_dp_i2c_do_msg(aux, &mut msg) };
    }

    err
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_i2c_xfer()
#[unsafe(no_mangle)]
/// # Safety
/// `adapter` must be the embedded `ddc` member of an active `DrmDpAux`.
/// `msgs` must point to an array of at least `num` initialized `I2cMsg` records with valid buffer pointers.
pub unsafe extern "C" fn drm_dp_i2c_xfer(
    aux: *mut DrmDpAux,
    msgs: *mut I2cMsg<'_>,
    num: c_int,
) -> c_int {
    if msgs.is_null() || num <= 0 {
        return 0;
    }
    // SAFETY: `msgs` is non-null (checked above) and points to `num` initialized `I2cMsg` records per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts_mut(msgs, num as usize) };
    // SAFETY: `aux` is valid per the function's safety contract and `slice` was built from `msgs` above.
    unsafe { drm_dp_i2c_xfer_impl(aux, slice) }
}

struct DrmDpI2cAlgo;

impl I2cAlgorithm for DrmDpI2cAlgo {
    fn functionality(&self) -> u32 {
        drm_dp_i2c_functionality()
    }

    fn master_xfer(&self, adap: &I2cAdapter<'_>, msgs: &mut [I2cMsg<'_>]) -> i32 {
        let aux =
            ((adap as *const _ as usize) - core::mem::offset_of!(DrmDpAux, ddc)) as *mut DrmDpAux;
        // SAFETY: adap is the embedded ddc field within DrmDpAux; container_of arithmetic
        // restores the enclosing valid DrmDpAux pointer; msgs contains valid message buffers.
        unsafe { drm_dp_i2c_xfer_impl(aux, msgs) }
    }
}

static DRM_DP_I2C_ALGO: DrmDpI2cAlgo = DrmDpI2cAlgo;

// ---------------------------------------------------------------------------
// Local AUX Registration
// ---------------------------------------------------------------------------

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_aux_init()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to allocated, writable memory for a `DrmDpAux` struct.
pub unsafe extern "C" fn drm_dp_aux_init(aux: *mut DrmDpAux) {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { mutex_init(&mut (*aux).hw_mutex) };
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { mutex_init(&mut (*aux).cec.lock) };

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let ddc_ptr = unsafe { core::ptr::addr_of_mut!((*aux).ddc) as *mut I2cAdapter<'static> };
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let name_str = if unsafe { (*aux).name.is_null() } {
        "drm_dp_aux"
    } else {
        // SAFETY: `aux` is valid and `name` is non-null (checked above); the contract does not state that `name` is NUL-terminated.
        let name = unsafe { core::ffi::CStr::from_ptr((*aux).name) };
        name.to_str().unwrap_or("drm_dp_aux")
    };
    // SAFETY: `ddc_ptr` points to the `ddc` field of the live `aux` per the function's safety contract, and is writable.
    unsafe { core::ptr::write(ddc_ptr, I2cAdapter::new(name_str, Some(&DRM_DP_I2C_ALGO))) };
    // SAFETY: `ddc_ptr` was initialized by `ptr::write` above.
    unsafe { (*ddc_ptr).retries = 3 };
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_aux_register()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux` whose `transfer` callback and `drm_dev` are valid.
pub unsafe extern "C" fn drm_dp_aux_register(aux: *mut DrmDpAux) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let ddc_ptr = unsafe { core::ptr::addr_of_mut!((*aux).ddc) as *mut I2cAdapter<'static> };
    // SAFETY: `ddc_ptr` points to the `ddc` field of the live `aux` per the function's safety contract.
    if unsafe { (*ddc_ptr).algo.is_none() } {
        // SAFETY: `aux` is valid per the function's safety contract, which is what `drm_dp_aux_init` requires.
        unsafe { drm_dp_aux_init(aux) };
    }

    // This component registers the kernel DDC adapter, not a VFS AUX node.
    // SAFETY: `ddc_ptr` points to the `ddc` field of the live `aux` per the function's safety contract.
    drm_kapi::i2c::i2c_add_adapter(unsafe { &mut *ddc_ptr }, &drm_kapi::i2c::I2C_ADAPTER_IDR)
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_aux_unregister()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to a registered `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_aux_unregister(aux: *mut DrmDpAux) {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let ddc_ptr = unsafe { core::ptr::addr_of_mut!((*aux).ddc) as *mut I2cAdapter<'static> };
    // SAFETY: `ddc_ptr` points to the `ddc` field of the live `aux` per the function's safety contract.
    drm_kapi::i2c::i2c_del_adapter(unsafe { &mut *ddc_ptr }, &drm_kapi::i2c::I2C_ADAPTER_IDR);
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_psr_setup_time()
#[unsafe(no_mangle)]
/// # Safety
/// `psr_cap` must point to a readable array of at least `EDP_PSR_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_psr_setup_time(psr_cap: *const u8) -> c_int {
    static PSR_SETUP_TIME_US: [u16; 7] = [330, 275, 220, 165, 110, 55, 0];
    // SAFETY: `psr_cap` is readable for `EDP_PSR_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let psr_cap_slice = unsafe { core::slice::from_raw_parts(psr_cap, EDP_PSR_RECEIVER_CAP_SIZE) };
    let i = ((psr_cap_slice[1] & (7 << 1)) >> 1) as usize;
    if i >= PSR_SETUP_TIME_US.len() {
        return -EINVAL;
    }
    PSR_SETUP_TIME_US[i] as c_int
}

// ---------------------------------------------------------------------------
// Quirks & Descriptor
// ---------------------------------------------------------------------------

struct DpcdQuirk {
    oui: [u8; 3],
    device_id: [u8; 6],
    is_branch: bool,
    quirks: u32,
}

const DPCD_QUIRK_LIST: &[DpcdQuirk] = &[
    DpcdQuirk {
        oui: [0x00, 0x22, 0xb9],
        device_id: [0; 6],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_CONSTANT_N,
    },
    DpcdQuirk {
        oui: [0x00, 0x22, 0xb9],
        device_id: *b"sivarT",
        is_branch: false,
        quirks: 1 << DP_DPCD_QUIRK_CONSTANT_N,
    },
    DpcdQuirk {
        oui: [0x00, 0x10, 0xfa],
        device_id: [0; 6],
        is_branch: false,
        quirks: 1 << DP_DPCD_QUIRK_NO_PSR,
    },
    DpcdQuirk {
        oui: [0x00, 0x00, 0x00],
        device_id: *b"CH7511",
        is_branch: false,
        quirks: 1 << DP_DPCD_QUIRK_NO_SINK_COUNT,
    },
    DpcdQuirk {
        oui: [0x90, 0xcc, 0x24],
        device_id: [0; 6],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_DSC_WITHOUT_VIRTUAL_DPCD,
    },
    DpcdQuirk {
        oui: [0x00, 0xe0, 0x4c],
        device_id: [b'D', b'p', b'1', b'.', b'4', 0],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_DSC_WITHOUT_VIRTUAL_DPCD,
    },
    DpcdQuirk {
        oui: [0x90, 0xcc, 0x24],
        device_id: [0; 6],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_HBLANK_EXPANSION_REQUIRES_DSC,
    },
    DpcdQuirk {
        oui: [0x00, 0x0c, 0xe7],
        device_id: [0; 6],
        is_branch: false,
        quirks: 1 << DP_DPCD_QUIRK_HBLANK_EXPANSION_REQUIRES_DSC,
    },
    DpcdQuirk {
        oui: [0x00, 0x10, 0xfa],
        device_id: [101, 68, 21, 101, 98, 97],
        is_branch: false,
        quirks: 1 << DP_DPCD_QUIRK_CAN_DO_MAX_LINK_RATE_3_24_GBPS,
    },
    DpcdQuirk {
        oui: [0x90, 0xcc, 0x24],
        device_id: [b'S', b'Y', b'N', b'A', 0x53, 0x22],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_DSC_THROUGHPUT_BPP_LIMIT,
    },
    DpcdQuirk {
        oui: [0x90, 0xcc, 0x24],
        device_id: [b'S', b'Y', b'N', b'A', 0x53, 0x31],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_DSC_THROUGHPUT_BPP_LIMIT,
    },
    DpcdQuirk {
        oui: [0x90, 0xcc, 0x24],
        device_id: [b'S', b'Y', b'N', b'A', 0x53, 0x33],
        is_branch: true,
        quirks: 1 << DP_DPCD_QUIRK_DSC_THROUGHPUT_BPP_LIMIT,
    },
];

/// # Safety
/// `ident` must point to an initialized, readable `DrmDpDpcdIdent` struct.
unsafe fn drm_dp_get_quirks(ident: *const DrmDpDpcdIdent, is_branch: bool) -> u32 {
    let mut quirks = 0;
    let any_device = [0u8; 6];

    for quirk in DPCD_QUIRK_LIST.iter() {
        if quirk.is_branch != is_branch {
            continue;
        }
        // SAFETY: `ident` is an initialized, readable `DrmDpDpcdIdent` per the function's safety contract.
        if quirk.oui != unsafe { (*ident).oui } {
            continue;
        }
        // SAFETY: `ident` is an initialized, readable `DrmDpDpcdIdent` per the function's safety contract.
        if quirk.device_id != any_device && quirk.device_id != unsafe { (*ident).device_id } {
            continue;
        }
        quirks |= quirk.quirks;
    }
    quirks
}

/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `ident` must point to a writable `DrmDpDpcdIdent` struct.
unsafe fn drm_dp_read_ident(
    aux: *mut DrmDpAux,
    offset: c_uint,
    ident: *mut DrmDpDpcdIdent,
) -> c_int {
    // SAFETY: `aux` is valid and `ident` is writable for `DrmDpDpcdIdent` bytes per the function's safety contract.
    unsafe {
        drm_dp_dpcd_read_data(
            aux,
            offset,
            ident as *mut c_void,
            core::mem::size_of::<DrmDpDpcdIdent>(),
        )
    }
}

/// # Safety
/// If `desc` is non-null, it must point to a valid `DrmDpDesc` struct.
unsafe fn drm_dp_dump_desc(
    _aux: *const DrmDpAux,
    _device_name: *const c_char,
    _desc: *const DrmDpDesc,
) {
    // Debug logging disabled in minimal quiet config
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_desc()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `desc` must point to an initialized, writable `DrmDpDesc` struct.
pub unsafe extern "C" fn drm_dp_read_desc(
    aux: *mut DrmDpAux,
    desc: *mut DrmDpDesc,
    is_branch: bool,
) -> c_int {
    let offset = if is_branch {
        DP_BRANCH_OUI
    } else {
        DP_SINK_OUI
    };
    // SAFETY: `desc` is an initialized, writable `DrmDpDesc` per the function's safety contract.
    let ident_ptr = unsafe { core::ptr::addr_of_mut!((*desc).ident) };
    // SAFETY: `aux` is valid and `ident_ptr` is writable per the function's safety contract.
    let ret = unsafe { drm_dp_read_ident(aux, offset, ident_ptr) };
    if ret < 0 {
        return ret;
    }

    // SAFETY: `desc` is writable and `ident_ptr` points to its `ident` field, per the function's safety contract.
    unsafe { (*desc).quirks = drm_dp_get_quirks(ident_ptr, is_branch) };
    // SAFETY: `aux` and `desc` satisfy this function's safety contract; the names passed are static C strings.
    unsafe {
        drm_dp_dump_desc(
            aux,
            if is_branch {
                c"DP branch".as_ptr()
            } else {
                c"DP sink".as_ptr()
            },
            desc,
        );
    }
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dump_lttpr_desc()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_dump_lttpr_desc(aux: *mut DrmDpAux, dp_phy: DrmDpPhy) -> c_int {
    if dp_phy < DpPhyLttpr1 || dp_phy > DpPhyLttpr8 {
        return -EINVAL;
    }

    let mut desc = DrmDpDesc {
        ident: DrmDpDpcdIdent {
            oui: [0; 3],
            device_id: [0; 6],
            hw_rev: 0,
            sw_major_rev: 0,
            sw_minor_rev: 0,
        },
        quirks: 0,
    };
    // SAFETY: `aux` is valid per the function's safety contract; `desc.ident` is a live local.
    let ret = unsafe { drm_dp_read_ident(aux, dp_oui_phy_repeater(dp_phy), &mut desc.ident) };
    if ret < 0 {
        return ret;
    }

    // SAFETY: `aux` is valid per the function's safety contract; `drm_dp_phy_name` returns a static C string; `desc` is a live local.
    unsafe { drm_dp_dump_desc(aux, drm_dp_phy_name(dp_phy), &desc) };
    0
}

// ---------------------------------------------------------------------------
// DSC Capabilities
// ---------------------------------------------------------------------------

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_sink_bpp_incr()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dsc_sink_bpp_incr(dsc_dpcd: *const u8) -> u8 {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dsc_slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    let incr = dsc_slice[(DP_DSC_BITS_PER_PIXEL_INC - DP_DSC_SUPPORT) as usize];
    match incr & DP_DSC_BITS_PER_PIXEL_MASK {
        DP_DSC_BITS_PER_PIXEL_1_16 => 16,
        DP_DSC_BITS_PER_PIXEL_1_8 => 8,
        DP_DSC_BITS_PER_PIXEL_1_4 => 4,
        DP_DSC_BITS_PER_PIXEL_1_2 => 2,
        DP_DSC_BITS_PER_PIXEL_1_1 => 1,
        _ => 0,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_slice_count_to_mask()
#[unsafe(no_mangle)]
/// # Safety
/// Pure integer calculation; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_dsc_slice_count_to_mask(slice_count: c_int) -> u32 {
    1u32 << (slice_count - 1)
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_sink_slice_count_mask()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dsc_sink_slice_count_mask(
    dsc_dpcd: *const u8,
    is_edp: bool,
) -> u32 {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dsc_slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    let slice_cap1 = dsc_slice[(DP_DSC_SLICE_CAP_1 - DP_DSC_SUPPORT) as usize];
    let mut mask = 0u32;

    if !is_edp {
        let slice_cap2 = dsc_slice[(DP_DSC_SLICE_CAP_2 - DP_DSC_SUPPORT) as usize];
        if (slice_cap2 & DP_DSC_24_PER_DP_DSC_SINK) != 0 {
            // SAFETY: this is a pure numeric helper with no pointer preconditions.
            mask |= unsafe { drm_dp_dsc_slice_count_to_mask(24) };
        }
        if (slice_cap2 & DP_DSC_20_PER_DP_DSC_SINK) != 0 {
            // SAFETY: this is a pure numeric helper with no pointer preconditions.
            mask |= unsafe { drm_dp_dsc_slice_count_to_mask(20) };
        }
        if (slice_cap2 & DP_DSC_16_PER_DP_DSC_SINK) != 0 {
            // SAFETY: this is a pure numeric helper with no pointer preconditions.
            mask |= unsafe { drm_dp_dsc_slice_count_to_mask(16) };
        }
    }

    if (slice_cap1 & DP_DSC_12_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(12) };
    }
    if (slice_cap1 & DP_DSC_10_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(10) };
    }
    if (slice_cap1 & DP_DSC_8_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(8) };
    }
    if (slice_cap1 & DP_DSC_6_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(6) };
    }
    if (slice_cap1 & DP_DSC_4_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(4) };
    }
    if (slice_cap1 & DP_DSC_2_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(2) };
    }
    if (slice_cap1 & DP_DSC_1_PER_DP_DSC_SINK) != 0 {
        // SAFETY: this is a pure numeric helper with no pointer preconditions.
        mask |= unsafe { drm_dp_dsc_slice_count_to_mask(1) };
    }

    mask
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_sink_max_slice_count()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dsc_sink_max_slice_count(dsc_dpcd: *const u8, is_edp: bool) -> u8 {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let mask = unsafe { drm_dp_dsc_sink_slice_count_mask(dsc_dpcd, is_edp) };
    if mask == 0 {
        0
    } else {
        (32 - mask.leading_zeros()) as u8
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_sink_line_buf_depth()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dsc_sink_line_buf_depth(dsc_dpcd: *const u8) -> u8 {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dsc_slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    let depth = dsc_slice[(DP_DSC_LINE_BUF_BIT_DEPTH - DP_DSC_SUPPORT) as usize];
    match depth & DP_DSC_LINE_BUF_BIT_DEPTH_MASK {
        DP_DSC_LINE_BUF_BIT_DEPTH_9 => 9,
        DP_DSC_LINE_BUF_BIT_DEPTH_10 => 10,
        DP_DSC_LINE_BUF_BIT_DEPTH_11 => 11,
        DP_DSC_LINE_BUF_BIT_DEPTH_12 => 12,
        DP_DSC_LINE_BUF_BIT_DEPTH_13 => 13,
        DP_DSC_LINE_BUF_BIT_DEPTH_14 => 14,
        DP_DSC_LINE_BUF_BIT_DEPTH_15 => 15,
        DP_DSC_LINE_BUF_BIT_DEPTH_16 => 16,
        DP_DSC_LINE_BUF_BIT_DEPTH_8 => 8,
        _ => 0,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_sink_supported_input_bpcs()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_dpcd` must point to at least `DP_DSC_RECEIVER_CAP_SIZE` readable bytes.
/// `dsc_bpc` must point to a writable buffer of at least 3 bytes.
pub unsafe extern "C" fn drm_dp_dsc_sink_supported_input_bpcs(
    dsc_dpcd: *const u8,
    dsc_bpc: *mut u8,
) -> c_int {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dsc_slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    // SAFETY: `dsc_dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_sink_supports_dsc(dsc_dpcd) } {
        return 0;
    }

    let color_depth = dsc_slice[(DP_DSC_DEC_COLOR_DEPTH_CAP - DP_DSC_SUPPORT) as usize];
    let mut num_bpc = 0;

    if (color_depth & DP_DSC_12_BPC) != 0 {
        // SAFETY: `dsc_bpc` is writable for 3 bytes per the function's safety contract; at most 3 entries are written here.
        unsafe { *dsc_bpc.add(num_bpc) = 12 };
        num_bpc += 1;
    }
    if (color_depth & DP_DSC_10_BPC) != 0 {
        // SAFETY: `dsc_bpc` is writable for 3 bytes per the function's safety contract; at most 3 entries are written here.
        unsafe { *dsc_bpc.add(num_bpc) = 10 };
        num_bpc += 1;
    }
    // SAFETY: `dsc_bpc` is writable for 3 bytes per the function's safety contract; at most 3 entries are written here.
    unsafe { *dsc_bpc.add(num_bpc) = 8 };
    num_bpc += 1;

    num_bpc as c_int
}

fn dsc_sink_min_slice_throughput(peak_pixel_rate: c_int) -> c_int {
    if peak_pixel_rate >= 4800000 {
        600000
    } else if peak_pixel_rate >= 2700000 {
        400000
    } else {
        340000
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_sink_max_slice_throughput()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_dsc_sink_max_slice_throughput(
    dsc_dpcd: *const u8,
    peak_pixel_rate: c_int,
    is_rgb_yuv444: bool,
) -> c_int {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dsc_slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    let mut throughput = dsc_slice[(DP_DSC_PEAK_THROUGHPUT - DP_DSC_SUPPORT) as usize];
    let mut delta = 0;

    if is_rgb_yuv444 {
        let t = (throughput & DP_DSC_THROUGHPUT_MODE_0_MASK) >> DP_DSC_THROUGHPUT_MODE_0_SHIFT;
        let d = (dsc_slice[(DP_DSC_RC_BUF_BLK_SIZE - DP_DSC_SUPPORT) as usize]
            & DP_DSC_THROUGHPUT_MODE_0_DELTA_MASK)
            >> DP_DSC_THROUGHPUT_MODE_0_DELTA_SHIFT;
        delta = (d as c_int) * 2000;
        throughput = t;
    } else {
        throughput = (throughput & DP_DSC_THROUGHPUT_MODE_1_MASK) >> DP_DSC_THROUGHPUT_MODE_1_SHIFT;
    }

    let base = match throughput {
        0 => return dsc_sink_min_slice_throughput(peak_pixel_rate),
        1 => 340000,
        2..=14 => 400000 + 50000 * ((throughput as c_int) - 2),
        15 => 170000,
        _ => 170000,
    };

    base + delta
}

#[inline]
fn dsc_branch_dpcd_cap(dpcd: &[u8], reg: u32) -> u8 {
    dpcd[(reg - DP_DSC_BRANCH_OVERALL_THROUGHPUT_0) as usize]
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_branch_max_overall_throughput()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_branch_dpcd` must point to `DP_DSC_BRANCH_CAP_SIZE` readable DSC branch
/// capability bytes.
pub unsafe extern "C" fn drm_dp_dsc_branch_max_overall_throughput(
    dsc_branch_dpcd: *const u8,
    is_rgb_yuv444: bool,
) -> c_int {
    // SAFETY: `dsc_branch_dpcd` is readable for `DP_DSC_BRANCH_CAP_SIZE` bytes per the
    // function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(dsc_branch_dpcd, DP_DSC_BRANCH_CAP_SIZE) };
    let throughput = if is_rgb_yuv444 {
        dsc_branch_dpcd_cap(slice, DP_DSC_BRANCH_OVERALL_THROUGHPUT_0)
    } else {
        dsc_branch_dpcd_cap(slice, DP_DSC_BRANCH_OVERALL_THROUGHPUT_1)
    };

    match throughput {
        0 => 0,
        1 => 680000,
        _ => 600000 + 50000 * (throughput as c_int),
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_dsc_branch_max_line_width()
#[unsafe(no_mangle)]
/// # Safety
/// `dsc_branch_dpcd` must point to `DP_DSC_BRANCH_CAP_SIZE` readable DSC branch
/// capability bytes.
pub unsafe extern "C" fn drm_dp_dsc_branch_max_line_width(dsc_branch_dpcd: *const u8) -> c_int {
    // SAFETY: `dsc_branch_dpcd` is readable for `DP_DSC_BRANCH_CAP_SIZE` bytes per the
    // function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(dsc_branch_dpcd, DP_DSC_BRANCH_CAP_SIZE) };
    let line_width = dsc_branch_dpcd_cap(slice, DP_DSC_BRANCH_MAX_LINE_WIDTH);

    match line_width {
        0 => 0,
        1..=15 => -EINVAL,
        _ => (line_width as c_int) * 320,
    }
}

// ---------------------------------------------------------------------------
// LTTPR Helpers
// ---------------------------------------------------------------------------

/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `buf` must point to writable memory of at least `buf_size` bytes.
unsafe fn drm_dp_read_lttpr_regs(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    address: u32,
    buf: *mut u8,
    buf_size: usize,
) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    let block_size = if dpcd_slice[DP_DPCD_REV as usize] < 0x14 {
        1
    } else {
        buf_size
    };

    let mut offset = 0;
    while offset < buf_size {
        // SAFETY: `aux` is valid per the function's safety contract; `buf.add(offset)` with `offset < buf_size` and `block_size` bytes stay within the `buf_size`-byte buffer.
        let ret = unsafe {
            drm_dp_dpcd_read_data(
                aux,
                address + (offset as u32),
                buf.add(offset) as *mut c_void,
                block_size,
            )
        };
        if ret < 0 {
            return ret;
        }
        offset += block_size;
    }
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_lttpr_common_caps()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `caps` must point to a writable buffer of at least `DP_LTTPR_COMMON_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_read_lttpr_common_caps(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    caps: *mut u8,
) -> c_int {
    // SAFETY: `aux`, `dpcd` and `caps` satisfy this function's safety contract, which matches `drm_dp_read_lttpr_regs`.
    unsafe {
        drm_dp_read_lttpr_regs(
            aux,
            dpcd,
            DP_LT_TUNABLE_PHY_REPEATER_FIELD_DATA_STRUCTURE_REV,
            caps,
            DP_LTTPR_COMMON_CAP_SIZE,
        )
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_read_lttpr_phy_caps()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `caps` must point to a writable buffer of at least `DP_LTTPR_PHY_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_read_lttpr_phy_caps(
    aux: *mut DrmDpAux,
    dpcd: *const u8,
    dp_phy: DrmDpPhy,
    caps: *mut u8,
) -> c_int {
    // SAFETY: `aux`, `dpcd` and `caps` satisfy this function's safety contract, which matches `drm_dp_read_lttpr_regs`.
    unsafe {
        drm_dp_read_lttpr_regs(
            aux,
            dpcd,
            dp_training_aux_rd_interval_phy_repeater(dp_phy),
            caps,
            DP_LTTPR_PHY_CAP_SIZE,
        )
    }
}

#[inline]
fn dp_lttpr_common_cap(caps: &[u8], r: u32) -> u8 {
    caps[(r - DP_LT_TUNABLE_PHY_REPEATER_FIELD_DATA_STRUCTURE_REV) as usize]
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_count()
#[unsafe(no_mangle)]
/// # Safety
/// `caps` must point to a readable buffer of at least `DP_LTTPR_COMMON_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_lttpr_count(caps: *const u8) -> c_int {
    // SAFETY: `caps` is readable for `DP_LTTPR_COMMON_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(caps, DP_LTTPR_COMMON_CAP_SIZE) };
    let count = dp_lttpr_common_cap(slice, DP_PHY_REPEATER_CNT);

    match count.count_ones() {
        0 => 0,
        1 => (8 - (31 - (count as u32).leading_zeros())) as c_int,
        8 => -ERANGE,
        _ => -EINVAL,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_max_link_rate()
#[unsafe(no_mangle)]
/// # Safety
/// `caps` must point to a readable buffer of at least `DP_LTTPR_COMMON_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_lttpr_max_link_rate(caps: *const u8) -> c_int {
    // SAFETY: `caps` is readable for `DP_LTTPR_COMMON_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(caps, DP_LTTPR_COMMON_CAP_SIZE) };
    let rate = dp_lttpr_common_cap(slice, DP_MAX_LINK_RATE_PHY_REPEATER);
    // SAFETY: this is a pure numeric helper with no pointer preconditions.
    unsafe { drm_dp_bw_code_to_link_rate(rate) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_set_transparent_mode()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_lttpr_set_transparent_mode(
    aux: *mut DrmDpAux,
    enable: bool,
) -> c_int {
    let val = if enable {
        DP_PHY_REPEATER_MODE_TRANSPARENT
    } else {
        DP_PHY_REPEATER_MODE_NON_TRANSPARENT
    };
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let ret = unsafe { drm_dp_dpcd_writeb(aux, DP_PHY_REPEATER_MODE, val) };
    if ret < 0 {
        ret as c_int
    } else if ret == 1 {
        0
    } else {
        -EIO
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_init()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_lttpr_init(aux: *mut DrmDpAux, lttpr_count: c_int) -> c_int {
    if lttpr_count == 0 {
        return 0;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    let ret = unsafe { drm_dp_lttpr_set_transparent_mode(aux, true) };
    if ret != 0 {
        return ret;
    }

    if lttpr_count < 0 {
        return -ENODEV;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    if unsafe { drm_dp_lttpr_set_transparent_mode(aux, false) } != 0 {
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        unsafe { drm_dp_lttpr_set_transparent_mode(aux, true) };
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_max_lane_count()
#[unsafe(no_mangle)]
/// # Safety
/// `caps` must point to a readable buffer of at least `DP_LTTPR_COMMON_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_lttpr_max_lane_count(caps: *const u8) -> c_int {
    // SAFETY: `caps` is readable for `DP_LTTPR_COMMON_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(caps, DP_LTTPR_COMMON_CAP_SIZE) };
    let max_lanes = dp_lttpr_common_cap(slice, DP_MAX_LANE_COUNT_PHY_REPEATER);
    (max_lanes & DP_MAX_LANE_COUNT_MASK) as c_int
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_voltage_swing_level_3_supported()
#[unsafe(no_mangle)]
/// # Safety
/// `caps` must point to a readable buffer of at least `DP_LTTPR_PHY_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_lttpr_voltage_swing_level_3_supported(caps: *const u8) -> bool {
    // SAFETY: `caps` is readable for `DP_LTTPR_PHY_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(caps, DP_LTTPR_PHY_CAP_SIZE) };
    let txcap = dp_lttpr_phy_cap(slice, DP_TRANSMITTER_CAPABILITY_PHY_REPEATER1 as usize);
    (txcap & DP_VOLTAGE_SWING_LEVEL_3_SUPPORTED) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_lttpr_pre_emphasis_level_3_supported()
#[unsafe(no_mangle)]
/// # Safety
/// `caps` must point to a readable buffer of at least `DP_LTTPR_PHY_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_lttpr_pre_emphasis_level_3_supported(caps: *const u8) -> bool {
    // SAFETY: `caps` is readable for `DP_LTTPR_PHY_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(caps, DP_LTTPR_PHY_CAP_SIZE) };
    let txcap = dp_lttpr_phy_cap(slice, DP_TRANSMITTER_CAPABILITY_PHY_REPEATER1 as usize);
    (txcap & DP_PRE_EMPHASIS_LEVEL_3_SUPPORTED) != 0
}

// ---------------------------------------------------------------------------
// PHY Test Pattern Helpers
// ---------------------------------------------------------------------------

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_get_phy_test_pattern()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `data` must point to an initialized, writable `DrmDpPhyTest` struct.
pub unsafe extern "C" fn drm_dp_get_phy_test_pattern(
    aux: *mut DrmDpAux,
    data: *mut DrmDpPhyTestParams,
) -> c_int {
    let mut rate = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `rate` is a live local.
    let err = unsafe { drm_dp_dpcd_read_byte(aux, DP_TEST_LINK_RATE, &mut rate) };
    if err < 0 {
        return err;
    }
    // SAFETY: `data` is writable per the function's safety contract; `drm_dp_bw_code_to_link_rate` is a pure numeric helper.
    unsafe { (*data).link_rate = drm_dp_bw_code_to_link_rate(rate) };

    let mut lanes = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `lanes` is a live local.
    let err = unsafe { drm_dp_dpcd_read_byte(aux, DP_TEST_LANE_COUNT, &mut lanes) };
    if err < 0 {
        return err;
    }
    // SAFETY: `data` is writable per the function's safety contract.
    unsafe { (*data).num_lanes = lanes & DP_MAX_LANE_COUNT_MASK };

    if (lanes & DP_ENHANCED_FRAME_CAP) != 0 {
        // SAFETY: `data` is writable per the function's safety contract.
        unsafe { (*data).enhanced_frame_cap = true };
    }

    // SAFETY: `aux` is valid and `data` is writable per the function's safety contract; `phy_pattern` is a field of `*data`.
    let err = unsafe { drm_dp_dpcd_read_byte(aux, DP_PHY_TEST_PATTERN, &mut (*data).phy_pattern) };
    if err < 0 {
        return err;
    }

    // SAFETY: `data` is readable per the function's safety contract.
    match unsafe { (*data).phy_pattern } {
        DP_PHY_TEST_PATTERN_80BIT_CUSTOM => {
            // SAFETY: `aux` is valid and `data` is initialized per the function's safety contract; `custom80` is a field of `*data`.
            let err = unsafe {
                drm_dp_dpcd_read_data(
                    aux,
                    DP_TEST_80BIT_CUSTOM_PATTERN_7_0,
                    (*data).custom80.as_mut_ptr() as *mut c_void,
                    (*data).custom80.len(),
                )
            };
            if err < 0 {
                return err;
            }
        }
        DP_PHY_TEST_PATTERN_CP2520 => {
            // SAFETY: `aux` is valid and `data` is initialized per the function's safety contract; `hbr2_reset` is a field of `*data`.
            let err = unsafe {
                drm_dp_dpcd_read_data(
                    aux,
                    DP_TEST_HBR2_SCRAMBLER_RESET,
                    (*data).hbr2_reset.as_mut_ptr() as *mut c_void,
                    (*data).hbr2_reset.len(),
                )
            };
            if err < 0 {
                return err;
            }
        }
        _ => {}
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_set_phy_test_pattern()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `data` must point to an initialized, readable `DrmDpPhyTest` struct.
pub unsafe extern "C" fn drm_dp_set_phy_test_pattern(
    aux: *mut DrmDpAux,
    data: *mut DrmDpPhyTestParams,
    dp_rev: u8,
) -> c_int {
    // SAFETY: `data` is readable per the function's safety contract.
    let mut test_pattern = unsafe { (*data).phy_pattern };
    if dp_rev < 0x12 {
        test_pattern = (test_pattern << 2) & DP_LINK_QUAL_PATTERN_11_MASK;
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
        let err = unsafe { drm_dp_dpcd_write_byte(aux, DP_TRAINING_PATTERN_SET, test_pattern) };
        if err < 0 {
            return err;
        }
    } else {
        // SAFETY: `data` is readable per the function's safety contract.
        for i in 0..(unsafe { (*data).num_lanes as u32 }) {
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            let err =
                unsafe { drm_dp_dpcd_write_byte(aux, DP_LINK_QUAL_LANE0_SET + i, test_pattern) };
            if err < 0 {
                return err;
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// SDP & Colorimetry String Names and Logs
// ---------------------------------------------------------------------------

fn dp_pixelformat_get_name(pixelformat: DpPixelformat) -> &'static str {
    match pixelformat {
        DP_PIXELFORMAT_RGB => "RGB",
        DP_PIXELFORMAT_YUV444 => "YUV444",
        DP_PIXELFORMAT_YUV422 => "YUV422",
        DP_PIXELFORMAT_YUV420 => "YUV420",
        DP_PIXELFORMAT_Y_ONLY => "Y_ONLY",
        DP_PIXELFORMAT_RAW => "RAW",
        _ => "Reserved",
    }
}

fn dp_colorimetry_get_name(pixelformat: DpPixelformat, colorimetry: DpColorimetry) -> &'static str {
    match colorimetry {
        DP_COLORIMETRY_DEFAULT => match pixelformat {
            DP_PIXELFORMAT_RGB => "sRGB",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "BT.601",
            DP_PIXELFORMAT_Y_ONLY => "DICOM PS3.14",
            DP_PIXELFORMAT_RAW => "Custom Color Profile",
            _ => "Reserved",
        },
        DP_COLORIMETRY_RGB_WIDE_FIXED => match pixelformat {
            DP_PIXELFORMAT_RGB => "Wide Fixed",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "BT.709",
            _ => "Reserved",
        },
        DP_COLORIMETRY_RGB_WIDE_FLOAT => match pixelformat {
            DP_PIXELFORMAT_RGB => "Wide Float",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "xvYCC 601",
            _ => "Reserved",
        },
        DP_COLORIMETRY_OPRGB => match pixelformat {
            DP_PIXELFORMAT_RGB => "OpRGB",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "xvYCC 709",
            _ => "Reserved",
        },
        DP_COLORIMETRY_DCI_P3_RGB => match pixelformat {
            DP_PIXELFORMAT_RGB => "DCI-P3",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "sYCC 601",
            _ => "Reserved",
        },
        DP_COLORIMETRY_RGB_CUSTOM => match pixelformat {
            DP_PIXELFORMAT_RGB => "Custom Profile",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "OpYCC 601",
            _ => "Reserved",
        },
        DP_COLORIMETRY_BT2020_RGB => match pixelformat {
            DP_PIXELFORMAT_RGB => "BT.2020 RGB",
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "BT.2020 CYCC",
            _ => "Reserved",
        },
        DP_COLORIMETRY_BT2020_YCC => match pixelformat {
            DP_PIXELFORMAT_YUV444 | DP_PIXELFORMAT_YUV422 | DP_PIXELFORMAT_YUV420 => "BT.2020 YCC",
            _ => "Reserved",
        },
        _ => "Invalid",
    }
}

fn dp_dynamic_range_get_name(dynamic_range: DpDynamicRange) -> &'static str {
    match dynamic_range {
        DP_DYNAMIC_RANGE_VESA => "VESA range",
        DP_DYNAMIC_RANGE_CTA => "CTA range",
        _ => "Invalid",
    }
}

fn dp_content_type_get_name(content_type: DpContentType) -> &'static str {
    match content_type {
        DP_CONTENT_TYPE_NOT_DEFINED => "Not defined",
        DP_CONTENT_TYPE_GRAPHICS => "Graphics",
        DP_CONTENT_TYPE_PHOTO => "Photo",
        DP_CONTENT_TYPE_VIDEO => "Video",
        DP_CONTENT_TYPE_GAME => "Game",
        _ => "Reserved",
    }
}

fn dp_sdp_type_get_name(sdp_type: u8) -> &'static str {
    match sdp_type {
        DP_SDP_AUDIO_TIMESTAMP => "Audio_TimeStamp",
        DP_SDP_AUDIO_STREAM => "Audio_Stream",
        DP_SDP_EXTENSION => "Extension",
        DP_SDP_AUDIO_COPYMANAGEMENT => "Audio_CopyManagement",
        DP_SDP_ISRC => "ISRC",
        DP_SDP_VSC => "VSC",
        DP_SDP_PPS => "PPS",
        DP_SDP_VSC_EXT_VESA => "VSC_EXT_VESA",
        DP_SDP_VSC_EXT_CEA => "VSC_EXT_CEA",
        DP_SDP_ADAPTIVE_SYNC => "Adaptive-Sync",
        _ => "Unknown",
    }
}

fn drm_printf_indent(p: *mut DrmPrinter, indent: usize, text: &str) {
    let mut line = alloc::string::String::new();
    for _ in 0..indent {
        line.push('\t');
    }
    line.push_str(text);
    // SAFETY: p is forwarded from caller (or null, which drm_printf safely handles) and line is a valid Rust String.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_vsc_sdp_log()
#[unsafe(no_mangle)]
/// # Safety
/// If `p` is non-null, it must point to a valid `DrmPrinter`.
/// `vsc` must point to an initialized, readable `DrmDpVscSdp` struct.
pub unsafe extern "C" fn drm_dp_vsc_sdp_log(p: *mut DrmPrinter, vsc: *const DrmDpVscSdp) {
    // SAFETY: `vsc` is initialized and readable per the function's safety contract.
    let sdp_type_str = dp_sdp_type_get_name(unsafe { (*vsc).sdp_type });
    // SAFETY: `vsc` is initialized and readable per the function's safety contract.
    let msg = unsafe {
        format!(
            "DP SDP: {}, revision {}, length {}\n",
            sdp_type_str,
            (*vsc).revision,
            (*vsc).length
        )
    };
    // SAFETY: `p` is null or a valid `DrmPrinter` per the function's safety contract; `msg` is a live String.
    unsafe { crate::drm_print_upstream::drm_printf(p, &msg) };

    let pix_str = format!(
        "pixelformat: {}\n",
        // SAFETY: `vsc` is initialized and readable per the function's safety contract.
        dp_pixelformat_get_name(unsafe { (*vsc).pixelformat })
    );
    drm_printf_indent(p, 1, &pix_str);

    let col_str = format!(
        "colorimetry: {}\n",
        // SAFETY: `vsc` is initialized and readable per the function's safety contract.
        dp_colorimetry_get_name(unsafe { (*vsc).pixelformat }, unsafe { (*vsc).colorimetry })
    );
    drm_printf_indent(p, 1, &col_str);

    // SAFETY: `vsc` is initialized and readable per the function's safety contract.
    let bpc_str = format!("bpc: {}\n", unsafe { (*vsc).bpc });
    drm_printf_indent(p, 1, &bpc_str);

    let dyn_str = format!(
        "dynamic range: {}\n",
        // SAFETY: `vsc` is initialized and readable per the function's safety contract.
        dp_dynamic_range_get_name(unsafe { (*vsc).dynamic_range })
    );
    drm_printf_indent(p, 1, &dyn_str);

    let cont_str = format!(
        "content type: {}\n",
        // SAFETY: `vsc` is initialized and readable per the function's safety contract.
        dp_content_type_get_name(unsafe { (*vsc).content_type })
    );
    drm_printf_indent(p, 1, &cont_str);
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_as_sdp_log()
#[unsafe(no_mangle)]
/// # Safety
/// If `p` is non-null, it must point to a valid `DrmPrinter`.
/// `as_sdp` must point to an initialized, readable `DrmDpAsSdp` struct.
pub unsafe extern "C" fn drm_dp_as_sdp_log(p: *mut DrmPrinter, as_sdp: *const DrmDpAsSdp) {
    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let sdp_type_str = dp_sdp_type_get_name(unsafe { (*as_sdp).sdp_type });
    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let msg = unsafe {
        format!(
            "DP SDP: {}, revision {}, length {}\n",
            sdp_type_str,
            (*as_sdp).revision,
            (*as_sdp).length
        )
    };
    // SAFETY: `p` is null or a valid `DrmPrinter` per the function's safety contract; `msg` is a live String.
    unsafe { crate::drm_print_upstream::drm_printf(p, &msg) };

    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let vtot_str = format!("vtotal: {}\n", unsafe { (*as_sdp).vtotal });
    drm_printf_indent(p, 1, &vtot_str);

    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let trr_str = format!("target rr: {}\n", unsafe { (*as_sdp).target_rr });
    drm_printf_indent(p, 1, &trr_str);

    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let dinc_str = format!("duration increase ms: {}\n", unsafe {
        (*as_sdp).duration_incr_ms
    });
    drm_printf_indent(p, 1, &dinc_str);

    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let ddec_str = format!("duration decrease ms: {}\n", unsafe {
        (*as_sdp).duration_decr_ms
    });
    drm_printf_indent(p, 1, &ddec_str);

    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let op_str = format!("operation mode: {}\n", unsafe { (*as_sdp).mode });
    drm_printf_indent(p, 1, &op_str);

    let div_str = format!(
        "target rr divider: {}\n",
        // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
        if unsafe { (*as_sdp).target_rr_divider } {
            "1.001"
        } else {
            "1.000"
        }
    );
    drm_printf_indent(p, 1, &div_str);

    // SAFETY: `as_sdp` is initialized and readable per the function's safety contract.
    let coast_str = format!("coasting vtotal: {}\n", unsafe {
        (*as_sdp).coasting_vtotal
    });
    drm_printf_indent(p, 1, &coast_str);
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_as_sdp_supported()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_as_sdp_supported(aux: *mut DrmDpAux, dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    if dpcd_slice[DP_DPCD_REV as usize] < DP_DPCD_REV_13 {
        return false;
    }

    let mut rx_feature = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `rx_feature` is a live local.
    if unsafe {
        drm_dp_dpcd_read_byte(
            aux,
            DP_DPRX_FEATURE_ENUMERATION_LIST_CONT_1,
            &mut rx_feature,
        )
    } < 0
    {
        return false;
    }

    (rx_feature & DP_ADAPTIVE_SYNC_SDP_SUPPORTED) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_vsc_sdp_supported()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_vsc_sdp_supported(aux: *mut DrmDpAux, dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    if dpcd_slice[DP_DPCD_REV as usize] < DP_DPCD_REV_13 {
        return false;
    }

    let mut rx_feature = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `rx_feature` is a live local.
    if unsafe { drm_dp_dpcd_read_byte(aux, DP_DPRX_FEATURE_ENUMERATION_LIST, &mut rx_feature) } < 0
    {
        return false;
    }

    (rx_feature & DP_VSC_SDP_EXT_FOR_COLORIMETRY_SUPPORTED) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_vsc_sdp_pack()
#[unsafe(no_mangle)]
/// # Safety
/// `vsc` must point to an initialized, readable `DrmDpVscSdp` struct.
/// `sdp` must point to an initialized, writable `DpSdp` struct.
pub unsafe extern "C" fn drm_dp_vsc_sdp_pack(vsc: *const DrmDpVscSdp, sdp: *mut DpSdp) -> isize {
    let length = core::mem::size_of::<DpSdp>() as isize;
    // SAFETY: `sdp` is writable for `size_of::<DpSdp>()` bytes per the function's safety contract.
    unsafe { ptr::write_bytes(sdp as *mut u8, 0, core::mem::size_of::<DpSdp>()) };

    // SAFETY: `sdp` is writable and `vsc` is readable per the function's safety contract.
    unsafe {
        (*sdp).sdp_header.hb0 = 0;
        (*sdp).sdp_header.hb1 = (*vsc).sdp_type;
        (*sdp).sdp_header.hb2 = (*vsc).revision;
        (*sdp).sdp_header.hb3 = (*vsc).length;
    }

    // SAFETY: `vsc` is readable per the function's safety contract.
    if unsafe { (*vsc).revision } == 0x6 {
        // SAFETY: `sdp` is writable per the function's safety contract.
        unsafe {
            (*sdp).db[0] = 1;
            (*sdp).db[3] = 1;
        }
    }

    // SAFETY: `vsc` is readable per the function's safety contract.
    if !unsafe { (*vsc).revision == 0x5 || (*vsc).revision == 0x7 } {
        return length;
    }

    // SAFETY: `sdp` is writable and `vsc` is readable per the function's safety contract.
    unsafe {
        (*sdp).db[16] = (((*vsc).pixelformat & 0xf) << 4) as u8;
        (*sdp).db[16] |= ((*vsc).colorimetry & 0xf) as u8;
    }

    // SAFETY: `vsc` is readable per the function's safety contract.
    match unsafe { (*vsc).bpc } {
        6 => {}
        // SAFETY: `sdp` is writable per the function's safety contract.
        8 => unsafe { (*sdp).db[17] = 0x1 },
        // SAFETY: `sdp` is writable per the function's safety contract.
        10 => unsafe { (*sdp).db[17] = 0x2 },
        // SAFETY: `sdp` is writable per the function's safety contract.
        12 => unsafe { (*sdp).db[17] = 0x3 },
        // SAFETY: `sdp` is writable per the function's safety contract.
        16 => unsafe { (*sdp).db[17] = 0x4 },
        _ => return -(EINVAL as isize),
    }

    // SAFETY: `vsc` is readable per the function's safety contract.
    if unsafe { (*vsc).dynamic_range } == DP_DYNAMIC_RANGE_CTA {
        // SAFETY: `sdp` is writable per the function's safety contract.
        unsafe { (*sdp).db[17] |= 0x80 };
    }

    // SAFETY: `sdp` is writable and `vsc` is readable per the function's safety contract.
    unsafe { (*sdp).db[18] = ((*vsc).content_type & 0x7) as u8 };

    length
}

// ---------------------------------------------------------------------------
// PCON Helpers
// ---------------------------------------------------------------------------

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_get_pcon_max_frl_bw()
#[unsafe(no_mangle)]
/// # Safety
/// `dpcd` must point to at least `DP_RECEIVER_CAP_SIZE` readable bytes.
/// `port_cap` must point to the 4 readable downstream port capability bytes.
pub unsafe extern "C" fn drm_dp_get_pcon_max_frl_bw(dpcd: *const u8, port_cap: *const u8) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: the function's safety contract makes `port_cap` 4 readable bytes.
    let port_cap_slice = unsafe { core::slice::from_raw_parts(port_cap, 4) };

    // SAFETY: `dpcd` is readable per the function's safety contract.
    if !unsafe { drm_dp_is_branch(dpcd) } || dpcd_slice[DP_DPCD_REV as usize] < 0x11 {
        return 0;
    }

    if (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DETAILED_CAP_INFO_AVAILABLE) == 0 {
        return 0;
    }

    if (port_cap_slice[0] & DP_DS_PORT_TYPE_MASK) != DP_DS_PORT_TYPE_HDMI {
        return 0;
    }

    let buf = port_cap_slice[2];
    let bw = buf & DP_PCON_MAX_FRL_BW;

    match bw {
        DP_PCON_MAX_9GBPS => 9,
        DP_PCON_MAX_18GBPS => 18,
        DP_PCON_MAX_24GBPS => 24,
        DP_PCON_MAX_32GBPS => 32,
        DP_PCON_MAX_40GBPS => 40,
        DP_PCON_MAX_48GBPS => 48,
        _ => 0,
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_frl_prepare()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_frl_prepare(
    aux: *mut DrmDpAux,
    enable_frl_ready_hpd: bool,
) -> c_int {
    let mut buf = DP_PCON_ENABLE_SOURCE_CTL_MODE | DP_PCON_ENABLE_LINK_FRL_MODE;
    if enable_frl_ready_hpd {
        buf |= DP_PCON_ENABLE_HPD_READY;
    }
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PCON_HDMI_LINK_CONFIG_1, buf) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_is_frl_ready()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_is_frl_ready(aux: *mut DrmDpAux) -> bool {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PCON_HDMI_TX_LINK_STATUS, &mut buf) };
    if ret < 0 {
        return false;
    }
    (buf & DP_PCON_FRL_READY) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_frl_configure_1()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_frl_configure_1(
    aux: *mut DrmDpAux,
    max_frl_gbps: c_int,
    frl_mode: u8,
) -> c_int {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PCON_HDMI_LINK_CONFIG_1, &mut buf) };
    if ret < 0 {
        return ret;
    }

    if frl_mode == DP_PCON_ENABLE_CONCURRENT_LINK {
        buf |= DP_PCON_ENABLE_CONCURRENT_LINK;
    } else {
        buf &= !DP_PCON_ENABLE_CONCURRENT_LINK;
    }

    match max_frl_gbps {
        9 => buf |= DP_PCON_ENABLE_MAX_BW_9GBPS,
        18 => buf |= DP_PCON_ENABLE_MAX_BW_18GBPS,
        24 => buf |= DP_PCON_ENABLE_MAX_BW_24GBPS,
        32 => buf |= DP_PCON_ENABLE_MAX_BW_32GBPS,
        40 => buf |= DP_PCON_ENABLE_MAX_BW_40GBPS,
        48 => buf |= DP_PCON_ENABLE_MAX_BW_48GBPS,
        0 => buf |= DP_PCON_ENABLE_MAX_BW_0GBPS,
        _ => return -EINVAL,
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PCON_HDMI_LINK_CONFIG_1, buf) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_frl_configure_2()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_frl_configure_2(
    aux: *mut DrmDpAux,
    max_frl_mask: c_int,
    frl_type: u8,
) -> c_int {
    let mut buf = max_frl_mask as u8;
    if frl_type == DP_PCON_FRL_LINK_TRAIN_EXTENDED {
        buf |= DP_PCON_FRL_LINK_TRAIN_EXTENDED;
    } else {
        buf &= !DP_PCON_FRL_LINK_TRAIN_EXTENDED;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PCON_HDMI_LINK_CONFIG_2, buf) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_reset_frl_config()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_reset_frl_config(aux: *mut DrmDpAux) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PCON_HDMI_LINK_CONFIG_1, 0x0) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_frl_enable()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_frl_enable(aux: *mut DrmDpAux) -> c_int {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PCON_HDMI_LINK_CONFIG_1, &mut buf) };
    if ret < 0 {
        return ret;
    }
    if (buf & DP_PCON_ENABLE_SOURCE_CTL_MODE) == 0 {
        return -EINVAL;
    }
    buf |= DP_PCON_ENABLE_HDMI_LINK;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PCON_HDMI_LINK_CONFIG_1, buf) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_hdmi_link_active()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_hdmi_link_active(aux: *mut DrmDpAux) -> bool {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PCON_HDMI_TX_LINK_STATUS, &mut buf) };
    if ret < 0 {
        return false;
    }
    (buf & DP_PCON_HDMI_TX_LINK_ACTIVE) != 0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_hdmi_link_mode()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// If non-null, `frl_trained_mask` must point to a writable `c_int`.
pub unsafe extern "C" fn drm_dp_pcon_hdmi_link_mode(
    aux: *mut DrmDpAux,
    frl_trained_mask: *mut u8,
) -> c_int {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PCON_HDMI_POST_FRL_STATUS, &mut buf) };
    if ret < 0 {
        return ret;
    }

    let mode = (buf & DP_PCON_HDMI_LINK_MODE) as c_int;
    if !frl_trained_mask.is_null() && mode == (DP_PCON_HDMI_MODE_FRL as c_int) {
        // SAFETY: `frl_trained_mask` is non-null (checked above) and writable per the function's safety contract.
        unsafe { *frl_trained_mask = (buf & DP_PCON_HDMI_FRL_TRAINED_BW) >> 1 };
    }

    mode
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_hdmi_frl_link_error_count()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `connector` must point to a live `DrmConnector`.
pub unsafe extern "C" fn drm_dp_pcon_hdmi_frl_link_error_count(
    aux: *mut DrmDpAux,
    connector: *mut DrmConnector,
) {
    // SAFETY: `connector` is a live `DrmConnector` per the function's safety contract.
    let hdmi = unsafe { core::ptr::addr_of!((*connector).display_info.hdmi) };
    // SAFETY: `hdmi` points into the live connector obtained above.
    let max_lanes = unsafe { (*hdmi).max_lanes as u32 };

    for i in 0..max_lanes {
        let mut buf = 0u8;
        // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
        if unsafe { drm_dp_dpcd_read_byte(aux, DP_PCON_HDMI_ERROR_STATUS_LN0 + i, &mut buf) } < 0 {
            return;
        }

        let error_count = buf & DP_PCON_HDMI_ERROR_COUNT_MASK;
        let num_error = match error_count {
            DP_PCON_HDMI_ERROR_COUNT_HUNDRED_PLUS => 100,
            DP_PCON_HDMI_ERROR_COUNT_TEN_PLUS => 10,
            DP_PCON_HDMI_ERROR_COUNT_THREE_PLUS => 3,
            _ => 0,
        };

        drm_kapi::drm_err!(
            // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
            unsafe { (*aux).drm_dev },
            "More than {} errors since the last read for lane {}\n",
            num_error,
            i
        );
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_enc_is_dsc_1_2()
#[unsafe(no_mangle)]
/// # Safety
/// `pcon_dsc_dpcd` must point to a readable buffer of at least `DP_PCON_DSC_ENCODER_CAP_SIZE` bytes.
pub unsafe extern "C" fn drm_dp_pcon_enc_is_dsc_1_2(pcon_dsc_dpcd: *const u8) -> bool {
    // SAFETY: `pcon_dsc_dpcd` is readable for `DP_PCON_DSC_ENCODER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(pcon_dsc_dpcd, DP_PCON_DSC_ENCODER_CAP_SIZE) };
    let buf = slice[(DP_PCON_DSC_VERSION - DP_PCON_DSC_ENCODER) as usize];
    let major_v = (buf & DP_PCON_DSC_MAJOR_MASK) >> DP_PCON_DSC_MAJOR_SHIFT;
    let minor_v = (buf & DP_PCON_DSC_MINOR_MASK) >> DP_PCON_DSC_MINOR_SHIFT;

    major_v == 1 && minor_v == 2
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_dsc_max_slices()
#[unsafe(no_mangle)]
/// # Safety
/// `pcon_dsc_dpcd` must point to readable PCON DSC DPCD capability bytes.
pub unsafe extern "C" fn drm_dp_pcon_dsc_max_slices(pcon_dsc_dpcd: *const u8) -> c_int {
    // SAFETY: `pcon_dsc_dpcd` is readable for `DP_PCON_DSC_ENCODER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(pcon_dsc_dpcd, DP_PCON_DSC_ENCODER_CAP_SIZE) };
    let slice_cap1 = slice[(DP_PCON_DSC_SLICE_CAP_1 - DP_PCON_DSC_ENCODER) as usize];
    let slice_cap2 = slice[(DP_PCON_DSC_SLICE_CAP_2 - DP_PCON_DSC_ENCODER) as usize];

    if (slice_cap2 & DP_PCON_DSC_24_PER_DSC_ENC) != 0 {
        return 24;
    }
    if (slice_cap2 & DP_PCON_DSC_20_PER_DSC_ENC) != 0 {
        return 20;
    }
    if (slice_cap2 & DP_PCON_DSC_16_PER_DSC_ENC) != 0 {
        return 16;
    }
    if (slice_cap1 & DP_PCON_DSC_12_PER_DSC_ENC) != 0 {
        return 12;
    }
    if (slice_cap1 & DP_PCON_DSC_10_PER_DSC_ENC) != 0 {
        return 10;
    }
    if (slice_cap1 & DP_PCON_DSC_8_PER_DSC_ENC) != 0 {
        return 8;
    }
    if (slice_cap1 & DP_PCON_DSC_6_PER_DSC_ENC) != 0 {
        return 6;
    }
    if (slice_cap1 & DP_PCON_DSC_4_PER_DSC_ENC) != 0 {
        return 4;
    }
    if (slice_cap1 & DP_PCON_DSC_2_PER_DSC_ENC) != 0 {
        return 2;
    }
    if (slice_cap1 & DP_PCON_DSC_1_PER_DSC_ENC) != 0 {
        return 1;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_dsc_max_slice_width()
#[unsafe(no_mangle)]
/// # Safety
/// `pcon_dsc_dpcd` must point to readable PCON DSC DPCD capability bytes.
pub unsafe extern "C" fn drm_dp_pcon_dsc_max_slice_width(pcon_dsc_dpcd: *const u8) -> c_int {
    // SAFETY: `pcon_dsc_dpcd` is readable for `DP_PCON_DSC_ENCODER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(pcon_dsc_dpcd, DP_PCON_DSC_ENCODER_CAP_SIZE) };
    let buf = slice[(DP_PCON_DSC_MAX_SLICE_WIDTH - DP_PCON_DSC_ENCODER) as usize];
    (buf as c_int) * (DP_DSC_SLICE_WIDTH_MULTIPLIER as c_int)
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_dsc_bpp_incr()
#[unsafe(no_mangle)]
/// # Safety
/// `pcon_dsc_dpcd` must point to readable PCON DSC DPCD capability bytes.
pub unsafe extern "C" fn drm_dp_pcon_dsc_bpp_incr(pcon_dsc_dpcd: *const u8) -> c_int {
    // SAFETY: `pcon_dsc_dpcd` is readable for `DP_PCON_DSC_ENCODER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(pcon_dsc_dpcd, DP_PCON_DSC_ENCODER_CAP_SIZE) };
    let buf = slice[(DP_PCON_DSC_BPP_INCR - DP_PCON_DSC_ENCODER) as usize];

    match buf & DP_PCON_DSC_BPP_INCR_MASK {
        DP_PCON_DSC_ONE_16TH_BPP => 16,
        DP_PCON_DSC_ONE_8TH_BPP => 8,
        DP_PCON_DSC_ONE_4TH_BPP => 4,
        DP_PCON_DSC_ONE_HALF_BPP => 2,
        DP_PCON_DSC_ONE_BPP => 1,
        _ => 0,
    }
}

/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
unsafe fn drm_dp_pcon_configure_dsc_enc(aux: *mut DrmDpAux, pps_buf_config: u8) -> c_int {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PROTOCOL_CONVERTER_CONTROL_2, &mut buf) };
    if ret < 0 {
        return ret;
    }

    buf |= DP_PCON_ENABLE_DSC_ENCODER;

    if pps_buf_config <= DP_PCON_ENC_PPS_OVERRIDE_EN_BUFFER {
        buf &= !DP_PCON_ENCODER_PPS_OVERRIDE_MASK;
        buf |= pps_buf_config << 2;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PROTOCOL_CONVERTER_CONTROL_2, buf) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_pps_default()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_pps_default(aux: *mut DrmDpAux) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_pcon_configure_dsc_enc(aux, DP_PCON_ENC_PPS_OVERRIDE_DISABLED) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_pps_override_buf()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `pps_buf` must point to an array of at least 128 readable bytes of PPS configuration data.
pub unsafe extern "C" fn drm_dp_pcon_pps_override_buf(
    aux: *mut DrmDpAux,
    pps_buf: *mut u8,
) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` and `pps_buf` is readable for 128 bytes per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_write_data(
            aux,
            DP_PCON_HDMI_PPS_OVERRIDE_BASE,
            pps_buf as *const c_void,
            128,
        )
    };
    if ret < 0 {
        return ret;
    }
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_pcon_configure_dsc_enc(aux, DP_PCON_ENC_PPS_OVERRIDE_EN_BUFFER) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_pps_override_param()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
/// `pps_param` must point to 6 initialized, readable bytes.
pub unsafe extern "C" fn drm_dp_pcon_pps_override_param(
    aux: *mut DrmDpAux,
    pps_param: *mut u8,
) -> c_int {
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` and `pps_param` is readable per the function's safety contract.
    let ret = unsafe {
        drm_dp_dpcd_write_data(
            aux,
            DP_PCON_HDMI_PPS_OVRD_SLICE_HEIGHT,
            pps_param as *const c_void,
            2,
        )
    };
    if ret < 0 {
        return ret;
    }
    // SAFETY: `aux` is initialized and `pps_param` holds 6 readable bytes per the
    // function's safety contract, so `pps_param.add(2)` is in bounds.
    let ret = unsafe {
        drm_dp_dpcd_write_data(
            aux,
            DP_PCON_HDMI_PPS_OVRD_SLICE_WIDTH,
            pps_param.add(2) as *const c_void,
            2,
        )
    };
    if ret < 0 {
        return ret;
    }
    // SAFETY: `aux` is initialized and `pps_param` holds 6 readable bytes per the
    // function's safety contract, so `pps_param.add(4)` is in bounds.
    let ret = unsafe {
        drm_dp_dpcd_write_data(
            aux,
            DP_PCON_HDMI_PPS_OVRD_BPP,
            pps_param.add(4) as *const c_void,
            2,
        )
    };
    if ret < 0 {
        return ret;
    }
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_pcon_configure_dsc_enc(aux, DP_PCON_ENC_PPS_OVERRIDE_EN_BUFFER) }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_pcon_convert_rgb_to_ycbcr()
#[unsafe(no_mangle)]
/// # Safety
/// `aux` must point to an initialized `DrmDpAux`.
pub unsafe extern "C" fn drm_dp_pcon_convert_rgb_to_ycbcr(
    aux: *mut DrmDpAux,
    color_spc: u8,
) -> c_int {
    let mut buf = 0u8;
    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract; `buf` is a live local.
    let ret = unsafe { drm_dp_dpcd_read_byte(aux, DP_PROTOCOL_CONVERTER_CONTROL_2, &mut buf) };
    if ret < 0 {
        return ret;
    }

    if (color_spc & DP_CONVERSION_RGB_YCBCR_MASK) != 0 {
        buf |= color_spc & DP_CONVERSION_RGB_YCBCR_MASK;
    } else {
        buf &= !DP_CONVERSION_RGB_YCBCR_MASK;
    }

    // SAFETY: `aux` is an initialized, valid `DrmDpAux` per the function's safety contract.
    unsafe { drm_dp_dpcd_write_byte(aux, DP_PROTOCOL_CONVERTER_CONTROL_2, buf) }
}

// ---------------------------------------------------------------------------
// Link Bandwidth & Symbol Cycles
// ---------------------------------------------------------------------------

fn drm_dp_link_data_symbol_cycles(
    lane_count: c_int,
    pixels: c_int,
    bpp_x16: c_int,
    symbol_size: c_int,
    is_mst: bool,
) -> c_int {
    let num = (pixels as i64) * (bpp_x16 as i64);
    let den = 16 * (symbol_size as i64) * (lane_count as i64);
    let cycles = ((num + den - 1) / den) as c_int;
    let align = if is_mst { 4 / lane_count } else { 1 };
    (cycles + align - 1) & !(align - 1)
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_link_symbol_cycles()
#[unsafe(no_mangle)]
/// # Safety
/// Pure numerical calculation of symbol cycles; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_link_symbol_cycles(
    lane_count: c_int,
    pixels: c_int,
    dsc_slice_count: c_int,
    bpp_x16: c_int,
    symbol_size: c_int,
    is_mst: bool,
) -> c_int {
    let slice_count = if dsc_slice_count != 0 {
        dsc_slice_count
    } else {
        1
    };
    let slice_pixels = (pixels + slice_count - 1) / slice_count;
    let slice_data_cycles =
        drm_dp_link_data_symbol_cycles(lane_count, slice_pixels, bpp_x16, symbol_size, is_mst);
    let slice_eoc_cycles = if dsc_slice_count != 0 {
        if is_mst { 4 / lane_count } else { 1 }
    } else {
        0
    };

    slice_count * (slice_data_cycles + slice_eoc_cycles)
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_bw_overhead()
#[unsafe(no_mangle)]
/// # Safety
/// Pure numerical calculation of bandwidth overhead; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_bw_overhead(
    lane_count: c_int,
    hactive: c_int,
    dsc_slice_count: c_int,
    bpp_x16: c_int,
    flags: c_ulong,
) -> c_int {
    let symbol_size = if (flags & DRM_DP_BW_OVERHEAD_UHBR) != 0 {
        32
    } else {
        8
    };
    let is_mst = (flags & DRM_DP_BW_OVERHEAD_MST) != 0;
    let mut overhead: u64 = 1000000;

    if lane_count == 0 || hactive == 0 || bpp_x16 == 0 {
        return 0;
    }

    if (flags & DRM_DP_BW_OVERHEAD_SSC_REF_CLK) != 0 {
        overhead += 6000;
    }

    if (flags & DRM_DP_BW_OVERHEAD_FEC) != 0 {
        overhead += 24016;
    }

    // SAFETY: `drm_dp_link_symbol_cycles` is a pure numeric function with no pointer preconditions.
    let symbol_cycles = unsafe {
        drm_dp_link_symbol_cycles(
            lane_count,
            hactive,
            dsc_slice_count,
            bpp_x16,
            symbol_size,
            is_mst,
        )
    };

    let num =
        ((symbol_cycles as u64) * (symbol_size as u64) * (lane_count as u64)) * (overhead * 16);
    let den = (hactive as u64) * (bpp_x16 as u64);

    num.div_ceil(den) as c_int
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_bw_channel_coding_efficiency()
#[unsafe(no_mangle)]
/// # Safety
/// Pure numerical calculation of channel coding efficiency; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_bw_channel_coding_efficiency(is_uhbr: bool) -> c_int {
    if is_uhbr { 967100 } else { 800000 }
}

// upstream: drivers/gpu/drm/display/drm_dp_helper.c drm_dp_max_dprx_data_rate()
#[unsafe(no_mangle)]
/// # Safety
/// Pure numerical calculation of DPRX max data rate; no pointer preconditions.
pub unsafe extern "C" fn drm_dp_max_dprx_data_rate(
    max_link_rate: c_int,
    max_lanes: c_int,
) -> c_int {
    // SAFETY: `drm_dp_bw_channel_coding_efficiency` is a pure numeric function with no pointer preconditions.
    let ch_coding_efficiency =
        unsafe { drm_dp_bw_channel_coding_efficiency(drm_dp_is_uhbr_rate(max_link_rate)) };
    let num = ((max_link_rate as u64) * 10 * (max_lanes as u64)) * (ch_coding_efficiency as u64);
    let den = 1000000 * 8;
    (num / den) as c_int
}

// ---------------------------------------------------------------------------
// Header inline helpers (from include/drm/display/drm_dp_helper.h)
// ---------------------------------------------------------------------------

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_max_link_rate(dpcd: *const u8) -> c_int {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    // SAFETY: `drm_dp_bw_code_to_link_rate` is a pure numeric function with no pointer preconditions.
    unsafe { drm_dp_bw_code_to_link_rate(dpcd_slice[DP_MAX_LINK_RATE as usize]) }
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_max_lane_count(dpcd: *const u8) -> u8 {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_MAX_LANE_COUNT as usize] & DP_MAX_LANE_COUNT_MASK
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_enhanced_frame_cap(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_DPCD_REV as usize] >= 0x11
        && (dpcd_slice[DP_MAX_LANE_COUNT as usize] & DP_ENHANCED_FRAME_CAP) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_post_lt_adj_req_supported(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_DPCD_REV as usize] >= 0x13
        && (dpcd_slice[DP_MAX_LANE_COUNT as usize] & DP_POST_LT_ADJ_REQ_SUPPORTED) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_fast_training_cap(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_DPCD_REV as usize] >= 0x11
        && (dpcd_slice[DP_MAX_DOWNSPREAD as usize] & DP_NO_AUX_HANDSHAKE_LINK_TRAINING) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_tps3_supported(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_DPCD_REV as usize] >= 0x12
        && (dpcd_slice[DP_MAX_LANE_COUNT as usize] & DP_TPS3_SUPPORTED) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_max_downspread(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_DPCD_REV as usize] >= 0x11
        && (dpcd_slice[DP_MAX_DOWNSPREAD as usize] & DP_MAX_DOWNSPREAD_0_5) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_tps4_supported(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    dpcd_slice[DP_DPCD_REV as usize] >= 0x14
        && (dpcd_slice[DP_MAX_DOWNSPREAD as usize] & DP_TPS4_SUPPORTED) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_training_pattern_mask(dpcd: *const u8) -> u8 {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    if dpcd_slice[DP_DPCD_REV as usize] >= 0x14 {
        DP_TRAINING_PATTERN_MASK_1_4
    } else {
        DP_TRAINING_PATTERN_MASK
    }
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_is_branch(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    (dpcd_slice[DP_DOWNSTREAMPORT_PRESENT as usize] & DP_DWN_STRM_PORT_PRESENT) != 0
}

#[inline]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_sink_supports_dsc(dsc_dpcd: *const u8) -> bool {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    (slice[0] & DP_DSC_DECOMPRESSION_IS_SUPPORTED) != 0 // DP_DSC_SUPPORT - DP_DSC_SUPPORT
}

#[inline]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_edp_dsc_sink_output_bpp(dsc_dpcd: *const u8) -> u16 {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    (slice[(DP_DSC_MAX_BITS_PER_PIXEL_LOW - DP_DSC_SUPPORT) as usize] as u16)
        | (((slice[(DP_DSC_MAX_BITS_PER_PIXEL_HI - DP_DSC_SUPPORT) as usize]
            & DP_DSC_MAX_BITS_PER_PIXEL_HI_MASK) as u16)
            << 8)
}

#[inline]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_dsc_sink_max_slice_width(dsc_dpcd: *const u8) -> u32 {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    (slice[(DP_DSC_MAX_SLICE_WIDTH - DP_DSC_SUPPORT) as usize] as u32)
        * DP_DSC_SLICE_WIDTH_MULTIPLIER
}

#[inline]
/// # Safety
/// `dsc_dpcd` must point to a readable buffer of at least `DP_DSC_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_dsc_sink_supports_format(dsc_dpcd: *const u8, output_format: u8) -> bool {
    // SAFETY: `dsc_dpcd` is readable for `DP_DSC_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let slice = unsafe { core::slice::from_raw_parts(dsc_dpcd, DP_DSC_RECEIVER_CAP_SIZE) };
    (slice[(DP_DSC_DEC_COLOR_FORMAT_CAP - DP_DSC_SUPPORT) as usize] & output_format) != 0
}

#[inline]
pub const fn drm_dp_sink_supports_fec(fec_capable: u8) -> bool {
    (fec_capable & DP_FEC_CAPABLE) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_channel_coding_supported(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    (dpcd_slice[DP_MAIN_LINK_CHANNEL_CODING as usize] & DP_CAP_ANSI_8B10B) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_128b132b_supported(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    (dpcd_slice[DP_MAIN_LINK_CHANNEL_CODING as usize] & DP_CAP_ANSI_128B132B) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_alternate_scrambler_reset_cap(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    (dpcd_slice[DP_EDP_CONFIGURATION_CAP as usize] & DP_ALTERNATE_SCRAMBLER_RESET_CAP) != 0
}

#[inline]
/// # Safety
/// `dpcd` must point to a readable buffer of at least `DP_RECEIVER_CAP_SIZE` bytes.
pub unsafe fn drm_dp_sink_can_do_video_without_timing_msa(dpcd: *const u8) -> bool {
    // SAFETY: `dpcd` is readable for `DP_RECEIVER_CAP_SIZE` bytes per the function's safety contract.
    let dpcd_slice = unsafe { core::slice::from_raw_parts(dpcd, DP_RECEIVER_CAP_SIZE) };
    (dpcd_slice[DP_DOWN_STREAM_PORT_COUNT as usize] & DP_MSA_TIMING_PAR_IGNORED) != 0
}

#[inline]
pub const fn drm_dp_is_uhbr_rate(link_rate: c_int) -> bool {
    link_rate >= 1000000
}

#[inline]
/// # Safety
/// `desc` must point to an initialized, readable `DrmDpDesc` struct.
pub unsafe fn drm_dp_has_quirk(desc: *const DrmDpDesc, quirk: u32) -> bool {
    // SAFETY: `desc` is an initialized, readable `DrmDpDesc` per the function's safety contract.
    (unsafe { (*desc).quirks } & (1 << quirk)) != 0
}

#[cfg(test)]
mod tests {
    use std::cell::UnsafeCell;

    use super::*;

    struct MockAuxCtx {
        reply: u8,
        ret: isize,
        call_count: usize,
        last_request: u8,
        last_address: u32,
        last_size: usize,
        data: [u8; 128],
        i2c_defer_remain: usize,
    }

    impl MockAuxCtx {
        const fn new() -> Self {
            Self {
                reply: 0,
                ret: 0,
                call_count: 0,
                last_request: 0,
                last_address: 0,
                last_size: 0,
                data: [0; 128],
                i2c_defer_remain: 0,
            }
        }
    }

    std::thread_local! {
        static MOCK_AUX: UnsafeCell<MockAuxCtx> = const { UnsafeCell::new(MockAuxCtx::new()) };
    }

    // Return a raw pointer rather than an `&mut`: tests must not retain a
    // mutable reference across a transfer callback, which accesses this cell.
    fn mock_ctx_ptr() -> *mut MockAuxCtx {
        MOCK_AUX.with(UnsafeCell::get)
    }

    fn with_mock_ctx<R>(f: impl FnOnce(&mut MockAuxCtx) -> R) -> R {
        MOCK_AUX.with(|ctx| {
            // SAFETY: The closure is synchronous, single-threaded in this test harness,
            // and accesses MOCK_AUX exclusively without concurrent alias creation.
            unsafe { f(&mut *ctx.get()) }
        })
    }

    /// # Safety
    /// `msg` must point to an initialized, writable `DrmDpAuxMsg` whose `buffer`
    /// satisfies read/write requirements for `(*msg).size` bytes.
    unsafe extern "C" fn mock_aux_transfer(_aux: *mut DrmDpAux, msg: *mut DrmDpAuxMsg) -> isize {
        // SAFETY: The fixture gives each libtest thread its own live context;
        // the synchronous callback exclusively accesses its request record.
        let ctx = unsafe { &mut *mock_ctx_ptr() };
        // SAFETY: The AUX caller retains this initialized request until return.
        let msg = unsafe { &mut *msg };
        ctx.call_count += 1;
        ctx.last_request = msg.request;
        ctx.last_address = msg.address;
        ctx.last_size = msg.size;

        let req_cmd = msg.request & !(DP_AUX_I2C_MOT | DP_AUX_I2C_WRITE_STATUS_UPDATE);

        if req_cmd == DP_AUX_NATIVE_WRITE {
            msg.reply = ctx.reply;
            if !msg.buffer.is_null() && msg.size > 0 {
                let len = msg.size.min(128);
                // SAFETY: The request supplies size readable bytes; len is
                // bounded by that size and the context's 128-byte storage.
                let bytes = unsafe { core::slice::from_raw_parts(msg.buffer.cast::<u8>(), len) };
                ctx.data[..len].copy_from_slice(bytes);
            }
            if ctx.ret != 0 {
                ctx.ret
            } else {
                msg.size as isize
            }
        } else if req_cmd == DP_AUX_NATIVE_READ {
            msg.reply = ctx.reply;
            if !msg.buffer.is_null() && msg.size > 0 {
                let len = msg.size.min(128);
                // SAFETY: A read request supplies size writable bytes.
                let bytes =
                    unsafe { core::slice::from_raw_parts_mut(msg.buffer.cast::<u8>(), len) };
                bytes.copy_from_slice(&ctx.data[..len]);
            }
            if ctx.ret != 0 {
                ctx.ret
            } else {
                msg.size as isize
            }
        } else if req_cmd == DP_AUX_I2C_READ {
            if ctx.i2c_defer_remain > 0 {
                ctx.i2c_defer_remain -= 1;
                msg.reply = DP_AUX_I2C_REPLY_DEFER;
                0
            } else {
                msg.reply = DP_AUX_I2C_REPLY_ACK;
                if !msg.buffer.is_null() && msg.size > 0 {
                    let len = msg.size.min(128);
                    // SAFETY: The read request supplies size writable bytes.
                    let bytes =
                        unsafe { core::slice::from_raw_parts_mut(msg.buffer.cast::<u8>(), len) };
                    for (k, byte) in bytes.iter_mut().enumerate() {
                        *byte = (k as u8).wrapping_add(0x40);
                    }
                }
                msg.size as isize
            }
        } else if req_cmd == DP_AUX_I2C_WRITE {
            if ctx.i2c_defer_remain > 0 {
                ctx.i2c_defer_remain -= 1;
                msg.reply = DP_AUX_I2C_REPLY_DEFER;
                0
            } else {
                msg.reply = DP_AUX_I2C_REPLY_ACK;
                msg.size as isize
            }
        } else {
            msg.reply = ctx.reply;
            ctx.ret
        }
    }

    fn new_mock_aux() -> DrmDpAux {
        crate::test_support::init_runtime();
        // SAFETY: Test harness zeroes memory for mock DrmDpAux and configures mock transfer callback.
        unsafe {
            with_mock_ctx(|ctx| {
                *ctx = MockAuxCtx::new();
                ctx.reply = DP_AUX_NATIVE_REPLY_ACK;
            });

            let mut aux: DrmDpAux = core::mem::zeroed();
            aux.transfer = Some(mock_aux_transfer);
            drm_dp_aux_init(&mut aux);
            aux.dpcd_probe_disabled = true;
            aux
        }
    }

    #[test]
    fn test_dpcd_read_write_success() {
        let mut aux = new_mock_aux();
        // SAFETY: Test invokes dpcd read and write against mock aux context with valid local buffers.
        unsafe {
            with_mock_ctx(|ctx| {
                ctx.data[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
            });

            let mut read_buf = [0u8; 4];
            let ret = drm_dp_dpcd_read(&mut aux, 0x100, read_buf.as_mut_ptr() as *mut c_void, 4);
            assert_eq!(ret, 4);
            assert_eq!(read_buf, [0x11, 0x22, 0x33, 0x44]);
            with_mock_ctx(|ctx| {
                assert_eq!(ctx.last_address, 0x100);
                assert_eq!(ctx.last_request, DP_AUX_NATIVE_READ);
            });

            let write_buf: [u8; 4] = [0xaa, 0xbb, 0xcc, 0xdd];
            let ret = drm_dp_dpcd_write(&mut aux, 0x200, write_buf.as_ptr() as *mut c_void, 4);
            assert_eq!(ret, 4);
            with_mock_ctx(|ctx| {
                assert_eq!(&ctx.data[0..4], &[0xaa, 0xbb, 0xcc, 0xdd]);
                assert_eq!(ctx.last_address, 0x200);
                assert_eq!(ctx.last_request, DP_AUX_NATIVE_WRITE);
            });

            // Remote MST AUX is intentionally unsupported. Rejection must
            // happen before the transport callback can observe either request.
            aux.is_remote = true;
            with_mock_ctx(|ctx| {
                *ctx = MockAuxCtx::new();
                ctx.reply = DP_AUX_NATIVE_REPLY_ACK;
                ctx.data[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
            });

            let mut read_buf = [0xa5; 4];
            let ret = drm_dp_dpcd_read(
                &mut aux,
                0x100,
                read_buf.as_mut_ptr().cast(),
                read_buf.len(),
            );
            assert_eq!(ret, -(EOPNOTSUPP as isize));
            assert_eq!(read_buf, [0xa5; 4]);

            let write_buf = [0xaa, 0xbb, 0xcc, 0xdd];
            let ret = drm_dp_dpcd_write(
                &mut aux,
                0x200,
                write_buf.as_ptr() as *mut c_void,
                write_buf.len(),
            );
            assert_eq!(ret, -(EOPNOTSUPP as isize));
            with_mock_ctx(|ctx| {
                assert_eq!(ctx.call_count, 0);
                assert_eq!(ctx.last_address, 0);
                assert_eq!(ctx.last_request, 0);
                assert_eq!(&ctx.data[0..4], &[0x11, 0x22, 0x33, 0x44]);
            });
        }
    }

    #[test]
    fn test_dpcd_short_read_eproto() {
        let mut aux = new_mock_aux();
        // SAFETY: Test sets mock error return and exercises drm_dp_dpcd_read with valid stack buffer.
        unsafe {
            with_mock_ctx(|ctx| ctx.ret = 2);

            let mut read_buf = [0u8; 4];
            let ret = drm_dp_dpcd_read(&mut aux, 0x100, read_buf.as_mut_ptr() as *mut c_void, 4);
            assert_eq!(ret, -(EPROTO as isize));
            with_mock_ctx(|ctx| assert_eq!(ctx.call_count, 32));
        }
    }

    #[test]
    fn test_dpcd_defer_retry_exhaustion() {
        let mut aux = new_mock_aux();
        // SAFETY: Test sets mock defer response and exercises drm_dp_dpcd_read with valid stack buffer.
        unsafe {
            with_mock_ctx(|ctx| ctx.reply = DP_AUX_NATIVE_REPLY_DEFER);

            let mut read_buf = [0u8; 4];
            let ret = drm_dp_dpcd_read(&mut aux, 0x100, read_buf.as_mut_ptr() as *mut c_void, 4);
            assert_eq!(ret, -(EIO as isize));
            with_mock_ctx(|ctx| assert_eq!(ctx.call_count, 32));
        }
    }

    #[test]
    fn test_dpcd_nack_eio() {
        let mut aux = new_mock_aux();
        // SAFETY: Test sets mock nack response and exercises drm_dp_dpcd_read with valid stack buffer.
        unsafe {
            with_mock_ctx(|ctx| ctx.reply = DP_AUX_NATIVE_REPLY_NACK);

            let mut read_buf = [0u8; 4];
            let ret = drm_dp_dpcd_read(&mut aux, 0x100, read_buf.as_mut_ptr() as *mut c_void, 4);
            assert_eq!(ret, -(EIO as isize));
        }
    }

    #[test]
    fn test_dpcd_timeout_etimedout() {
        let mut aux = new_mock_aux();
        // SAFETY: Test sets mock timeout response and exercises drm_dp_dpcd_read with valid stack buffer.
        unsafe {
            with_mock_ctx(|ctx| ctx.ret = -(ETIMEDOUT as isize));

            let mut read_buf = [0u8; 4];
            let ret = drm_dp_dpcd_read(&mut aux, 0x100, read_buf.as_mut_ptr() as *mut c_void, 4);
            assert_eq!(ret, -(ETIMEDOUT as isize));
            with_mock_ctx(|ctx| assert_eq!(ctx.call_count, 32));
        }
    }

    #[test]
    fn test_i2c_over_aux_multi_byte_split_and_defer() {
        let mut aux = new_mock_aux();
        aux.no_zero_sized = false;
        // SAFETY: msgs array elements point to valid stack buffers for the duration of the test.
        unsafe {
            with_mock_ctx(|ctx| ctx.i2c_defer_remain = 2);

            let mut write_addr = [0x00u8];
            let mut read_buf = [0u8; 32];

            let msg0 = I2cMsg::new(0x50, 0, &mut write_addr);
            let msg1 = I2cMsg::new(0x50, I2C_M_RD, &mut read_buf);
            let mut msgs = [msg0, msg1];

            let ret = drm_dp_i2c_xfer(&mut aux, msgs.as_mut_ptr(), 2);
            assert_eq!(ret, 2);
            assert_eq!(aux.i2c_defer_count, 2);
            assert_eq!(read_buf[0], 0x40);
            assert_eq!(read_buf[15], 0x4f);
        }
    }

    #[test]
    fn test_link_status_clock_recovery() {
        let mut status = [0u8; DP_LINK_STATUS_SIZE];
        // SAFETY: status array is properly sized to DP_LINK_STATUS_SIZE bytes.
        unsafe {
            status[DP_LANE0_1_STATUS as usize - DP_LANE0_1_STATUS as usize] = DP_LANE_CR_DONE;
            assert!(drm_dp_clock_recovery_ok(status.as_ptr(), 1));
            assert!(!drm_dp_clock_recovery_ok(status.as_ptr(), 2));

            status[0] = DP_LANE_CR_DONE | (DP_LANE_CR_DONE << 4);
            assert!(drm_dp_clock_recovery_ok(status.as_ptr(), 1));
            assert!(drm_dp_clock_recovery_ok(status.as_ptr(), 2));
            assert!(!drm_dp_clock_recovery_ok(status.as_ptr(), 4));

            status[1] = DP_LANE_CR_DONE | (DP_LANE_CR_DONE << 4);
            assert!(drm_dp_clock_recovery_ok(status.as_ptr(), 4));

            status[1] = DP_LANE_CR_DONE;
            assert!(!drm_dp_clock_recovery_ok(status.as_ptr(), 4));
            assert!(drm_dp_clock_recovery_ok(status.as_ptr(), 2));
        }
    }

    #[test]
    fn test_link_status_channel_eq() {
        let mut status = [0u8; DP_LINK_STATUS_SIZE];
        // SAFETY: status array is properly sized to DP_LINK_STATUS_SIZE bytes.
        unsafe {
            let eq_sym = DP_CHANNEL_EQ_BITS;
            status[0] = eq_sym | (eq_sym << 4);
            status[1] = eq_sym | (eq_sym << 4);
            assert!(!drm_dp_channel_eq_ok(status.as_ptr(), 4));

            status[DP_LANE_ALIGN_STATUS_UPDATED as usize - DP_LANE0_1_STATUS as usize] =
                DP_INTERLANE_ALIGN_DONE;
            assert!(drm_dp_channel_eq_ok(status.as_ptr(), 4));
            assert!(drm_dp_channel_eq_ok(status.as_ptr(), 2));
            assert!(drm_dp_channel_eq_ok(status.as_ptr(), 1));

            status[0] = DP_LANE_CHANNEL_EQ_DONE;
            assert!(!drm_dp_channel_eq_ok(status.as_ptr(), 1));
            assert!(!drm_dp_channel_eq_ok(status.as_ptr(), 2));
            assert!(!drm_dp_channel_eq_ok(status.as_ptr(), 4));
        }
    }

    #[test]
    fn test_link_status_adjust_request() {
        let mut status = [0u8; DP_LINK_STATUS_SIZE];
        // SAFETY: status array is properly sized to DP_LINK_STATUS_SIZE bytes.
        unsafe {
            let req_idx0 = DP_ADJUST_REQUEST_LANE0_1 as usize - DP_LANE0_1_STATUS as usize;
            let req_idx1 = DP_ADJUST_REQUEST_LANE2_3 as usize - DP_LANE0_1_STATUS as usize;

            status[req_idx0] = (2 << DP_ADJUST_VOLTAGE_SWING_LANE0_SHIFT)
                | (1 << DP_ADJUST_PRE_EMPHASIS_LANE0_SHIFT)
                | (1 << DP_ADJUST_VOLTAGE_SWING_LANE1_SHIFT)
                | (2 << DP_ADJUST_PRE_EMPHASIS_LANE1_SHIFT);

            status[req_idx1] = (3 << DP_ADJUST_VOLTAGE_SWING_LANE0_SHIFT)
                | (0 << DP_ADJUST_PRE_EMPHASIS_LANE0_SHIFT)
                | (0 << DP_ADJUST_VOLTAGE_SWING_LANE1_SHIFT)
                | (3 << DP_ADJUST_PRE_EMPHASIS_LANE1_SHIFT);

            assert_eq!(
                drm_dp_get_adjust_request_voltage(status.as_ptr(), 0),
                DP_TRAIN_VOLTAGE_SWING_LEVEL_2
            );
            assert_eq!(
                drm_dp_get_adjust_request_pre_emphasis(status.as_ptr(), 0),
                DP_TRAIN_PRE_EMPH_LEVEL_1
            );

            assert_eq!(
                drm_dp_get_adjust_request_voltage(status.as_ptr(), 1),
                DP_TRAIN_VOLTAGE_SWING_LEVEL_1
            );
            assert_eq!(
                drm_dp_get_adjust_request_pre_emphasis(status.as_ptr(), 1),
                DP_TRAIN_PRE_EMPH_LEVEL_2
            );

            assert_eq!(
                drm_dp_get_adjust_request_voltage(status.as_ptr(), 2),
                DP_TRAIN_VOLTAGE_SWING_LEVEL_3
            );
            assert_eq!(
                drm_dp_get_adjust_request_pre_emphasis(status.as_ptr(), 2),
                DP_TRAIN_PRE_EMPH_LEVEL_0
            );

            assert_eq!(
                drm_dp_get_adjust_request_voltage(status.as_ptr(), 3),
                DP_TRAIN_VOLTAGE_SWING_LEVEL_0
            );
            assert_eq!(
                drm_dp_get_adjust_request_pre_emphasis(status.as_ptr(), 3),
                DP_TRAIN_PRE_EMPH_LEVEL_3
            );
        }
    }

    #[test]
    fn test_rate_conversions_and_dprx_data_rate() {
        // SAFETY: Pure rate calculation helpers called with valid integer parameters.
        unsafe {
            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_1_62), 162000);
            assert_eq!(drm_dp_link_rate_to_bw_code(162000), DP_LINK_BW_1_62);

            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_2_7), 270000);
            assert_eq!(drm_dp_link_rate_to_bw_code(270000), DP_LINK_BW_2_7);

            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_5_4), 540000);
            assert_eq!(drm_dp_link_rate_to_bw_code(540000), DP_LINK_BW_5_4);

            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_8_1), 810000);
            assert_eq!(drm_dp_link_rate_to_bw_code(810000), DP_LINK_BW_8_1);

            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_10), 1000000);
            assert_eq!(drm_dp_link_rate_to_bw_code(1000000), DP_LINK_BW_10);

            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_13_5), 1350000);
            assert_eq!(drm_dp_link_rate_to_bw_code(1350000), DP_LINK_BW_13_5);

            assert_eq!(drm_dp_bw_code_to_link_rate(DP_LINK_BW_20), 2000000);
            assert_eq!(drm_dp_link_rate_to_bw_code(2000000), DP_LINK_BW_20);

            // 8b/10b data rate: DIV_ROUND_UP_ULL((540000 * 10 * 4) * 800000, 8000000) = 2160000
            assert_eq!(drm_dp_max_dprx_data_rate(540000, 4), 2160000);

            // 128b/132b data rate: DIV_ROUND_UP_ULL((1000000 * 10 * 4) * 967100, 8000000) = 4835500
            assert_eq!(drm_dp_max_dprx_data_rate(1000000, 4), 4835500);
        }
    }

    #[test]
    fn test_desc_and_quirk_matching() {
        let mut aux = new_mock_aux();
        // SAFETY: Test zeroes DrmDpDesc struct and passes valid pointers to drm_dp_read_desc and drm_dp_has_quirk.
        unsafe {
            with_mock_ctx(|ctx| {
                ctx.data[0..3].copy_from_slice(&[0x00, 0x22, 0xb9]);
                ctx.data[3..9].copy_from_slice(&[0; 6]);
            });

            let mut desc: DrmDpDesc = core::mem::zeroed();
            let ret = drm_dp_read_desc(&mut aux, &mut desc, true);
            assert_eq!(ret, 0);
            assert!(drm_dp_has_quirk(&desc, DP_DPCD_QUIRK_CONSTANT_N));
        }
    }
}

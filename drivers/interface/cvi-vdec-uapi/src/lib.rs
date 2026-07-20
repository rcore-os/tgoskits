//! SG2002 video-decoder userspace ABI.
//!
//! The command numbers and `repr(C)` layouts in this crate mirror Sophgo's
//! 64-bit `cvi_vc_dec` Linux interface. Vendor commands deliberately use
//! `_IO('V', nr)` even when the argument points at a structure.

#![no_std]

use bytemuck::{Pod, Zeroable};

/// Decoder device path used by channel zero.
pub const CVI_VDEC_DEVICE_0: &str = "/dev/cvi_vc_dec0";

const CVI_VC_DRV_IOCTL_MAGIC: u32 = b'V' as u32;

const fn vdec_ioctl(number: u32) -> u32 {
    (CVI_VC_DRV_IOCTL_MAGIC << 8) | number
}

pub const CVI_VC_VDEC_CREATE_CHN: u32 = vdec_ioctl(47);
pub const CVI_VC_VDEC_DESTROY_CHN: u32 = vdec_ioctl(48);
pub const CVI_VC_VDEC_GET_CHN_ATTR: u32 = vdec_ioctl(49);
pub const CVI_VC_VDEC_SET_CHN_ATTR: u32 = vdec_ioctl(50);
pub const CVI_VC_VDEC_START_RECV_STREAM: u32 = vdec_ioctl(51);
pub const CVI_VC_VDEC_STOP_RECV_STREAM: u32 = vdec_ioctl(52);
pub const CVI_VC_VDEC_QUERY_STATUS: u32 = vdec_ioctl(53);
pub const CVI_VC_VDEC_RESET_CHN: u32 = vdec_ioctl(54);
pub const CVI_VC_VDEC_SET_CHN_PARAM: u32 = vdec_ioctl(55);
pub const CVI_VC_VDEC_GET_CHN_PARAM: u32 = vdec_ioctl(56);
pub const CVI_VC_VDEC_SEND_STREAM: u32 = vdec_ioctl(57);
pub const CVI_VC_VDEC_GET_FRAME: u32 = vdec_ioctl(58);
pub const CVI_VC_VDEC_RELEASE_FRAME: u32 = vdec_ioctl(59);

/// StarryOS extension selecting the JPEG hardware scaler.
///
/// Sophgo's high-level VDEC path always programs scale mode zero. Number
/// `0x80` is outside the vendor command range (currently 0 through 80), so the
/// extension remains explicit without changing any vendor command.
pub const CVI_VC_VDEC_SET_JPEG_SCALE: u32 = vdec_ioctl(0x80);

pub const PT_JPEG: i32 = 26;
pub const PT_MJPEG: i32 = 1002;
pub const VIDEO_MODE_FRAME: i32 = 1;

pub const PIXEL_FORMAT_YUV_PLANAR_422: i32 = 12;
pub const PIXEL_FORMAT_YUV_PLANAR_420: i32 = 13;
pub const PIXEL_FORMAT_YUV_PLANAR_444: i32 = 14;
pub const PIXEL_FORMAT_YUV_400: i32 = 15;

pub const VIDEO_FORMAT_LINEAR: i32 = 0;
pub const COMPRESS_MODE_NONE: i32 = 0;
pub const DYNAMIC_RANGE_SDR8: i32 = 0;
pub const COLOR_GAMUT_BT601: i32 = 0;
pub const VB_INVALID_POOL_ID: u32 = u32::MAX;

pub const CVI_VDEC_JPEG_SCALE_FULL: u32 = 0;
pub const CVI_VDEC_JPEG_SCALE_HALF: u32 = 1;
pub const CVI_VDEC_JPEG_SCALE_QUARTER: u32 = 2;
pub const CVI_VDEC_JPEG_SCALE_EIGHTH: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VdecChnAttr {
    pub payload_type: i32,
    pub video_mode: i32,
    pub picture_width: u32,
    pub picture_height: u32,
    pub stream_buffer_size: u32,
    pub frame_buffer_size: u32,
    pub frame_buffer_count: u32,
    /// Wire storage for the anonymous `VDEC_ATTR_VIDEO_S` union member.
    pub video_attr: [u32; 3],
}

impl VdecChnAttr {
    pub const fn jpeg_frame(
        picture_width: u32,
        picture_height: u32,
        stream_buffer_size: u32,
        frame_buffer_size: u32,
    ) -> Self {
        Self {
            payload_type: PT_JPEG,
            video_mode: VIDEO_MODE_FRAME,
            picture_width,
            picture_height,
            stream_buffer_size,
            frame_buffer_size,
            frame_buffer_count: 1,
            video_attr: [0; 3],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VdecChnParam {
    pub payload_type: i32,
    pub pixel_format: i32,
    pub display_frame_count: u32,
    /// Wire storage for the largest anonymous parameter union member.
    pub codec_param: [u32; 5],
}

impl VdecChnParam {
    pub const fn jpeg_yuv420() -> Self {
        Self {
            payload_type: PT_JPEG,
            pixel_format: PIXEL_FORMAT_YUV_PLANAR_420,
            display_frame_count: 0,
            codec_param: [0; 5],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VdecStream {
    pub length: u32,
    #[doc(hidden)]
    pub padding_before_pts: [u8; 4],
    pub pts: u64,
    pub end_of_frame: u8,
    pub end_of_stream: u8,
    pub display: u8,
    #[doc(hidden)]
    pub padding_before_address: [u8; 5],
    pub address: u64,
}

impl VdecStream {
    pub const fn frame(address: u64, length: u32, pts: u64) -> Self {
        Self {
            length,
            padding_before_pts: [0; 4],
            pts,
            end_of_frame: 1,
            end_of_stream: 0,
            display: 1,
            padding_before_address: [0; 5],
            address,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VdecStreamEx {
    pub stream: u64,
    pub timeout_ms: i32,
    #[doc(hidden)]
    pub trailing_padding: [u8; 4],
}

impl VdecStreamEx {
    pub const fn new(stream: u64, timeout_ms: i32) -> Self {
        Self {
            stream,
            timeout_ms,
            trailing_padding: [0; 4],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VdecDecodeError {
    pub format_error: i32,
    pub picture_size_error: i32,
    pub unsupported_stream: i32,
    pub packet_error: i32,
    pub protocol_number_error: i32,
    pub reference_error: i32,
    pub picture_buffer_size_error: i32,
    pub stream_size_overflow: i32,
    pub stream_not_released: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VdecChnStatus {
    pub payload_type: i32,
    pub left_stream_bytes: i32,
    pub left_stream_frames: i32,
    pub left_pictures: i32,
    pub receiving_stream: u8,
    #[doc(hidden)]
    pub padding_after_receiving_stream: [u8; 3],
    pub received_stream_frames: u32,
    pub decoded_stream_frames: u32,
    pub decode_error: VdecDecodeError,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub pixel_format: i32,
    pub bayer_format: i32,
    pub video_format: i32,
    pub compress_mode: i32,
    pub dynamic_range: i32,
    pub color_gamut: i32,
    pub stride: [u32; 3],
    #[doc(hidden)]
    pub padding_before_physical_address: [u8; 4],
    pub physical_address: [u64; 3],
    pub virtual_address: [u64; 3],
    pub length: [u32; 3],
    pub offset_top: i16,
    pub offset_bottom: i16,
    pub offset_left: i16,
    pub offset_right: i16,
    pub time_ref: u32,
    pub pts: u64,
    pub private_data: u64,
    pub frame_flag: u32,
    #[doc(hidden)]
    pub trailing_padding: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VideoFrameInfo {
    pub frame: VideoFrame,
    pub pool_id: u32,
    #[doc(hidden)]
    pub trailing_padding: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct VideoFrameInfoEx {
    pub frame_info: u64,
    pub timeout_ms: i32,
    #[doc(hidden)]
    pub trailing_padding: [u8; 4],
}

impl VideoFrameInfoEx {
    pub const fn new(frame_info: u64, timeout_ms: i32) -> Self {
        Self {
            frame_info,
            timeout_ms,
            trailing_padding: [0; 4],
        }
    }
}

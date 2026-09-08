//! 格式协商类型。
//!
//! 涵盖 `v4l2_fmtdesc`、`v4l2_frmsizeenum`、`v4l2_frmivalenum`、
//! `v4l2_format` / `v4l2_pix_format` 以及全部像素格式 fourcc 常量。

use bitflags::bitflags;

use crate::interface::{
    BufType, Field, Fract, Rect,
    colorspace::{Colorspace, Quantization, XferFunc, YcbcrEncoding},
};

// ========================================================================
// 像素格式（v4l2_pix_format）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PixFormat {
    pub width: u32,             // [inout] 图像宽度（像素）
    pub height: u32,            // [inout] 图像高度（像素）
    pub pixelformat: u32,       // [inout] FourCC 像素格式代码
    pub field: Field,           // [inout] 场顺序
    pub bytesperline: u32,      // [inout] 每行字节数（0 表示由驱动决定）
    pub sizeimage: u32,         // [out] 帧大小（字节）
    pub colorspace: Colorspace, // [inout] 图像的色彩空间
    pub priv_data: u32,         // [out] 私有数据，取决于像素格式
    pub flags: u32,             // [out] 格式标志（V4L2_PIX_FMT_FLAG_*）
    /// ycbcr_enc / hsv_enc 联合体 — 与 C 的 `union { __u32 ycbcr_enc; __u32 hsv_enc; }` 对应
    pub ycbcr_enc: u32, // [inout] Y'CbCr / HSV 编码
    pub quantization: Quantization, // [inout] 量化范围
    pub xfer_func: XferFunc,    // [inout] 传输函数
}

impl Default for PixFormat {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pixelformat: 0,
            field: Field::Any,
            bytesperline: 0,
            sizeimage: 0,
            colorspace: Colorspace::Default,
            priv_data: 0,
            flags: 0,
            ycbcr_enc: YcbcrEncoding::Default as u32,
            quantization: Quantization::Default,
            xfer_func: XferFunc::Default,
        }
    }
}

// ========================================================================
// 像素格式常量（fourcc）
// ========================================================================

macro_rules! fourcc {
    ($a:expr, $b:expr, $c:expr, $d:expr) => {
        u32::from_le_bytes([$a, $b, $c, $d])
    };
}

// RGB 格式
pub const PIX_FMT_RGB332: u32 = fourcc!(b'R', b'G', b'B', b'1');
pub const PIX_FMT_RGB444: u32 = fourcc!(b'R', b'4', b'4', b'4');
pub const PIX_FMT_ARGB444: u32 = fourcc!(b'A', b'R', b'1', b'2');
pub const PIX_FMT_XRGB444: u32 = fourcc!(b'X', b'R', b'1', b'2');
pub const PIX_FMT_RGB555: u32 = fourcc!(b'R', b'G', b'B', b'O');
pub const PIX_FMT_ARGB555: u32 = fourcc!(b'A', b'R', b'1', b'5');
pub const PIX_FMT_XRGB555: u32 = fourcc!(b'X', b'R', b'1', b'5');
pub const PIX_FMT_RGB565: u32 = fourcc!(b'R', b'G', b'B', b'P');
pub const PIX_FMT_RGB555X: u32 = fourcc!(b'R', b'G', b'B', b'Q');
pub const PIX_FMT_ARGB555X: u32 = fourcc!(b'A', b'R', b'1', b'5') | (1 << 31);
pub const PIX_FMT_XRGB555X: u32 = fourcc!(b'X', b'R', b'1', b'5') | (1 << 31);
pub const PIX_FMT_RGB565X: u32 = fourcc!(b'R', b'G', b'B', b'R');
pub const PIX_FMT_BGR666: u32 = fourcc!(b'B', b'G', b'R', b'H');
pub const PIX_FMT_BGR24: u32 = fourcc!(b'B', b'G', b'R', b'3');
pub const PIX_FMT_RGB24: u32 = fourcc!(b'R', b'G', b'B', b'3');
pub const PIX_FMT_BGR32: u32 = fourcc!(b'B', b'G', b'R', b'4');
pub const PIX_FMT_ABGR32: u32 = fourcc!(b'A', b'R', b'2', b'4');
pub const PIX_FMT_XBGR32: u32 = fourcc!(b'X', b'R', b'2', b'4');
pub const PIX_FMT_RGB32: u32 = fourcc!(b'R', b'G', b'B', b'4');
pub const PIX_FMT_ARGB32: u32 = fourcc!(b'B', b'A', b'2', b'4');
pub const PIX_FMT_XRGB32: u32 = fourcc!(b'B', b'X', b'2', b'4');

// YUV 打包格式
pub const PIX_FMT_GREY: u32 = fourcc!(b'G', b'R', b'E', b'Y');
pub const PIX_FMT_Y4: u32 = fourcc!(b'Y', b'0', b'4', b' ');
pub const PIX_FMT_Y6: u32 = fourcc!(b'Y', b'0', b'6', b' ');
pub const PIX_FMT_Y10: u32 = fourcc!(b'Y', b'1', b'0', b' ');
pub const PIX_FMT_Y12: u32 = fourcc!(b'Y', b'1', b'2', b' ');
pub const PIX_FMT_Y16: u32 = fourcc!(b'Y', b'1', b'6', b' ');
pub const PIX_FMT_Y16_BE: u32 = u32::from_le_bytes(*b"Y16 ") | (1u32 << 31);
pub const PIX_FMT_Y10BPACK: u32 = fourcc!(b'Y', b'1', b'0', b'B');
pub const PIX_FMT_Y10P: u32 = fourcc!(b'Y', b'1', b'0', b'P');
pub const PIX_FMT_IPU3_Y10: u32 = fourcc!(b'i', b'p', b'3', b'y');
pub const PIX_FMT_PAL8: u32 = fourcc!(b'P', b'A', b'L', b'8');
pub const PIX_FMT_UV8: u32 = fourcc!(b'U', b'V', b'8', b' ');
pub const PIX_FMT_YUYV: u32 = fourcc!(b'Y', b'U', b'Y', b'V');
pub const PIX_FMT_YYUV: u32 = fourcc!(b'Y', b'Y', b'U', b'V');
pub const PIX_FMT_YVYU: u32 = fourcc!(b'Y', b'V', b'Y', b'U');
pub const PIX_FMT_UYVY: u32 = fourcc!(b'U', b'Y', b'V', b'Y');
pub const PIX_FMT_VYUY: u32 = fourcc!(b'V', b'Y', b'U', b'Y');
pub const PIX_FMT_Y41P: u32 = fourcc!(b'Y', b'4', b'1', b'P');
pub const PIX_FMT_YUV444: u32 = fourcc!(b'Y', b'4', b'4', b'4');
pub const PIX_FMT_YUV555: u32 = fourcc!(b'Y', b'U', b'V', b'O');
pub const PIX_FMT_YUV565: u32 = fourcc!(b'Y', b'U', b'V', b'P');
pub const PIX_FMT_YUV32: u32 = fourcc!(b'Y', b'U', b'V', b'4');
pub const PIX_FMT_AYUV32: u32 = fourcc!(b'A', b'Y', b'U', b'V');
pub const PIX_FMT_XYUV32: u32 = fourcc!(b'X', b'Y', b'U', b'V');
pub const PIX_FMT_VUYA32: u32 = fourcc!(b'V', b'U', b'Y', b'A');
pub const PIX_FMT_VUYX32: u32 = fourcc!(b'V', b'U', b'Y', b'X');
pub const PIX_FMT_YUV410: u32 = fourcc!(b'Y', b'U', b'V', b'9');
pub const PIX_FMT_YVU410: u32 = fourcc!(b'Y', b'V', b'U', b'9');
pub const PIX_FMT_YUV411P: u32 = fourcc!(b'4', b'1', b'1', b'P');
pub const PIX_FMT_YUV420: u32 = fourcc!(b'Y', b'U', b'1', b'2');
pub const PIX_FMT_YVU420: u32 = fourcc!(b'Y', b'V', b'1', b'2');
pub const PIX_FMT_YUV422P: u32 = fourcc!(b'4', b'2', b'2', b'P');
pub const PIX_FMT_NV12: u32 = fourcc!(b'N', b'V', b'1', b'2');
pub const PIX_FMT_NV21: u32 = fourcc!(b'N', b'V', b'2', b'1');
pub const PIX_FMT_NV16: u32 = fourcc!(b'N', b'V', b'1', b'6');
pub const PIX_FMT_NV61: u32 = fourcc!(b'N', b'V', b'6', b'1');
pub const PIX_FMT_NV24: u32 = fourcc!(b'N', b'V', b'2', b'4');
pub const PIX_FMT_NV42: u32 = fourcc!(b'N', b'V', b'4', b'2');

// MJPEG 压缩
pub const PIX_FMT_MJPEG: u32 = fourcc!(b'M', b'J', b'P', b'G');
pub const PIX_FMT_JPEG: u32 = fourcc!(b'J', b'P', b'E', b'G');
pub const PIX_FMT_H264: u32 = fourcc!(b'H', b'2', b'6', b'4');

// Bayer 拜耳
pub const PIX_FMT_SBGGR8: u32 = fourcc!(b'B', b'A', b'8', b'1');
pub const PIX_FMT_SGBRG8: u32 = fourcc!(b'G', b'B', b'R', b'G');
pub const PIX_FMT_SGRBG8: u32 = fourcc!(b'G', b'R', b'B', b'G');
pub const PIX_FMT_SRGGB8: u32 = fourcc!(b'R', b'G', b'G', b'B');

// ========================================================================
// 格式描述（v4l2_fmtdesc）
// ========================================================================

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FmtFlag: u32 {
        const COMPRESSED = 1 << 0;
        const EMULATED = 1 << 1;
        const CONTINUOUS_BUF = 1 << 2;
        const DYN_RESOLUTION = 1 << 3;
        const ENUM_FRMRATES = 1 << 4;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Fmtdesc {
    pub index: u32,            // [in] 格式索引
    pub ty: BufType,           // [in] 缓冲区类型
    pub flags: FmtFlag,        // [out] 格式标志
    pub description: [u8; 32], // [out] 格式描述
    pub pixelformat: u32,      // [out] 像素格式（fourcc）
    pub mbus_code: u32,        // [out] 媒体总线代码（用于原始格式）
    pub reserved: [u32; 3],
}

// ========================================================================
// 帧大小枚举（v4l2_frmsizeenum）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FrmsizeDiscrete {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FrmsizeStepwise {
    pub min_width: u32,
    pub max_width: u32,
    pub step_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub step_height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union FrmsizeUnion {
    pub discrete: FrmsizeDiscrete,
    pub stepwise: FrmsizeStepwise,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameSizeEnum {
    pub index: u32,         // [in] 帧大小索引
    pub pixel_format: u32,  // [in] 像素格式（fourcc）
    pub ty: FrameSizeType,  // [out] 帧大小类型
    pub size: FrmsizeUnion, // [out] 帧大小（离散或步进）
    pub reserved: [u32; 2],
}

/// 帧大小枚举类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSizeType {
    Discrete   = 1,
    Continuous = 2,
    Stepwise   = 3,
}

impl FrameSizeType {
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Discrete,
            2 => Self::Continuous,
            3 => Self::Stepwise,
            _ => return None,
        })
    }
}

// ========================================================================
// 帧间隔枚举（v4l2_frmivalenum）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FrmivalStepwise {
    pub min: Fract,
    pub max: Fract,
    pub step: Fract,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union FrmivalUnion {
    pub discrete: Fract,
    pub stepwise: FrmivalStepwise,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameIntervalEnum {
    pub index: u32,             // [in] 帧间隔索引
    pub pixel_format: u32,      // [in] 像素格式（fourcc）
    pub width: u32,             // [in] 帧宽度
    pub height: u32,            // [in] 帧高度
    pub ty: FrameIntervalType,  // [out] 帧间隔类型
    pub interval: FrmivalUnion, // [out] 帧间隔（离散或步进）
    pub reserved: [u32; 2],
}

/// 帧间隔枚举类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameIntervalType {
    Discrete   = 1,
    Continuous = 2,
    Stepwise   = 3,
}

impl FrameIntervalType {
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Discrete,
            2 => Self::Continuous,
            3 => Self::Stepwise,
            _ => return None,
        })
    }
}

// ========================================================================
// 叠加窗口（v4l2_window）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub w: Rect,        // [inout] 窗口矩形
    pub field: Field,   // [inout] 场顺序
    pub chromakey: u32, // [inout] 色键（chroma key）颜色
    /// __user *clips（裁剪矩形数组的用户态指针）
    pub clips: usize, // [in] 指向 clip 数组的用户态指针
    pub clipcount: u32, // [inout] clip 数量
    /// __user *bitmap（位图用户态指针）
    pub bitmap: usize, // [in] 指向位图（bitmap）的用户态指针
    pub global_alpha: u8, // [inout] 全局 alpha 值
}

// ========================================================================
// 格式联合体（v4l2_format）
// ========================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PixFormatMplane {
    pub width: u32,       // [inout] 图像宽度（像素）
    pub height: u32,      // [inout] 图像高度（像素）
    pub pixelformat: u32, // [inout] FourCC 像素格式代码
    pub field: u32,       // [inout] 场顺序
    pub colorspace: u32,  // [inout] 图像的色彩空间
    pub num_planes: u8,   // [inout] plane 数量
    pub flags: u8,        // [out] 格式标志（V4L2_PIX_FMT_FLAG_*）
    pub ycbcr_enc: u8,    // [inout] Y'CbCr 编码
    pub hsv_enc: u8,      // [inout] HSV 编码
    pub quantization: u8, // [inout] 量化范围
    pub xfer_func: u8,    // [inout] 传输函数
    pub reserved: [u8; 7],
    pub plane_fmt: [PlanePixFormat; 8], // [inout] 每个 plane 的格式
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PlanePixFormat {
    pub sizeimage: u32,    // [inout] plane 大小（字节）
    pub bytesperline: u32, // [inout] 每行字节数
    pub reserved: [u16; 6],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SdrFormat {
    pub pixelformat: u32, // [inout] FourCC 像素格式代码
    pub buffersize: u32,  // [out] 最大缓冲区大小（字节）
    pub reserved: [u8; 24],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MetaFormat {
    pub dataformat: u32, // [inout] FourCC 数据格式代码
    pub buffersize: u32, // [out] 最大缓冲区大小（字节）
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union FormatUnion {
    pub pix: PixFormat,
    pub pix_mp: PixFormatMplane,
    pub win: Window,
    pub sdr: SdrFormat,
    pub meta: MetaFormat,
    pub raw_data: [u8; 200],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Format {
    pub ty: BufType,      // [in] 缓冲区类型
    pub fmt: FormatUnion, // [inout] 格式数据
}

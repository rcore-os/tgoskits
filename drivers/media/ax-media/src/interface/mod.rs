pub mod buffer;
pub mod capability;
pub mod colorspace;
pub mod crop;
pub mod ctrl;
pub mod dv;
pub mod edid;
pub mod event;
pub mod format;
pub mod inout;
pub mod legacy;
pub mod stream;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// 分数（分子/分母）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fract {
    pub numerator: u32,
    pub denominator: u32,
}

impl Fract {
    /// 创建新的 `Fract`，指定分子和分母。
    pub const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// 使用连分数化简当前分数。
    pub fn simplify(&mut self) {
        let mut an = [0u32; 8];
        let mut x = self.numerator;
        let mut y = self.denominator;
        let mut n: usize = 0;
        while n < 8 && y != 0 {
            an[n] = x / y;
            if an[n] >= 333 {
                if n < 2 {
                    n += 1;
                }
                break;
            }
            let r = x - an[n] * y;
            x = y;
            y = r;
            n += 1;
        }
        // 回展为整数分数，固定 8 项无需堆分配。
        let mut exp_x: u32 = 0;
        let mut exp_y: u32 = 1;
        for i in (0..n).rev() {
            let r = exp_y;
            // 展开时用 u64 避免溢出，超出则截断到 u32::MAX。
            let tmp = (an[i] as u64) * (exp_y as u64) + (exp_x as u64);
            exp_y = if tmp > u32::MAX as u64 {
                u32::MAX
            } else {
                tmp as u32
            };
            exp_x = r;
        }
        self.numerator = exp_y;
        self.denominator = exp_x;
    }

    /// 从 `dwFrameInterval`（100ns 单位）创建 `Fract`。
    ///
    /// 等价于 `interval / 10_000_000` 再经 [`Self::simplify`] 化简，
    pub fn from_interval(interval: u32) -> Self {
        let mut f = Self {
            numerator: interval,
            denominator: 10_000_000,
        };
        f.simplify();
        f
    }

    /// 将本 `Fract`（timeperframe）转换为 `dwFrameInterval`（100ns 单位）。
    pub fn to_interval(self) -> u32 {
        // 分母为 0 或换算会溢出时饱和到 u32::MAX。
        if self.denominator == 0 || self.numerator / self.denominator >= u32::MAX / 10_000_000 {
            return u32::MAX;
        }
        let mut multiplier: u32 = 10_000_000;
        let mut denom = self.denominator;
        let numer = self.numerator;
        while numer > u32::MAX / multiplier {
            multiplier /= 2;
            denom /= 2;
        }
        if denom == 0 {
            return 0;
        }
        numer * multiplier / denom
    }
}

/// 场顺序。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Any          = 0, // 驱动可在无、顶场、底场、隔行中自行选择
    NoField      = 1, // 该设备没有场
    Top          = 2, // 仅顶场
    Bottom       = 3, // 仅底场
    Interlaced   = 4, // 两场隔行
    SeqTb        = 5, // 两场顺序，先顶后底
    SeqBt        = 6, // 两场顺序，先底后顶
    Alternate    = 7, // 两场交替放入独立的缓冲区
    InterlacedTb = 8, // 两场隔行，顶场在前，先传输顶场
    InterlacedBt = 9, // 两场隔行，顶场在前，先传输底场
}

impl Field {
    /// 若该 Field 包含顶场则返回 true。
    pub const fn has_top(self) -> bool {
        matches!(
            self,
            Self::Top
                | Self::Interlaced
                | Self::InterlacedTb
                | Self::InterlacedBt
                | Self::SeqTb
                | Self::SeqBt
        )
    }

    /// 若该 Field 包含底场则返回 true。
    pub const fn has_bottom(self) -> bool {
        matches!(
            self,
            Self::Bottom
                | Self::Interlaced
                | Self::InterlacedTb
                | Self::InterlacedBt
                | Self::SeqTb
                | Self::SeqBt
        )
    }

    /// 若该 Field 是隔行则返回 true。
    pub const fn is_interlaced(self) -> bool {
        matches!(
            self,
            Self::Interlaced | Self::InterlacedTb | Self::InterlacedBt
        )
    }

    /// 若该 Field 是顺序则返回 true。
    pub const fn is_sequential(self) -> bool {
        matches!(self, Self::SeqTb | Self::SeqBt)
    }
}

/// 缓冲区 / 流类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufType {
    VideoCapture       = 1,
    VideoOutput        = 2,
    VideoOverlay       = 3,
    VbiCapture         = 4,
    VbiOutput          = 5,
    SlicedVbiCapture   = 6,
    SlicedVbiOutput    = 7,
    VideoOutputOverlay = 8,
    VideoCaptureMplane = 9,
    VideoOutputMplane  = 10,
    SdrCapture         = 11,
    SdrOutput          = 12,
    MetaCapture        = 13,
    MetaOutput         = 14,
    Private            = 0x80,
}

impl BufType {
    pub const fn is_valid(self) -> bool {
        matches!(
            self,
            Self::VideoCapture
                | Self::VideoOutput
                | Self::VideoOverlay
                | Self::VbiCapture
                | Self::VbiOutput
                | Self::SlicedVbiCapture
                | Self::SlicedVbiOutput
                | Self::VideoOutputOverlay
                | Self::VideoCaptureMplane
                | Self::VideoOutputMplane
                | Self::SdrCapture
                | Self::SdrOutput
                | Self::MetaCapture
                | Self::MetaOutput
                | Self::Private
        )
    }

    pub const fn is_multiplanar(self) -> bool {
        matches!(self, Self::VideoCaptureMplane | Self::VideoOutputMplane)
    }

    pub const fn is_output(self) -> bool {
        matches!(
            self,
            Self::VideoOutput
                | Self::VideoOutputMplane
                | Self::VideoOutputOverlay
                | Self::VbiOutput
                | Self::SlicedVbiOutput
                | Self::SdrOutput
                | Self::MetaOutput
        )
    }

    pub const fn is_capture(self) -> bool {
        self.is_valid() && !self.is_output()
    }

    /// 尝试将原始 u32 值转换为 [`BufType`]。
    ///
    /// 若该值不对应任何已知变体，则返回 `None`。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::VideoCapture,
            2 => Self::VideoOutput,
            3 => Self::VideoOverlay,
            4 => Self::VbiCapture,
            5 => Self::VbiOutput,
            6 => Self::SlicedVbiCapture,
            7 => Self::SlicedVbiOutput,
            8 => Self::VideoOutputOverlay,
            9 => Self::VideoCaptureMplane,
            10 => Self::VideoOutputMplane,
            11 => Self::SdrCapture,
            12 => Self::SdrOutput,
            13 => Self::MetaCapture,
            14 => Self::MetaOutput,
            0x80 => Self::Private,
            _ => return None,
        })
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// 内核 timespec — 与 64 位系统上的 `struct __kernel_timespec` 一致（16 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Timecode {
    pub ty: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}

#[cfg(test)]
mod abi_tests {
    use super::{
        buffer::{Buffer, Exportbuffer, Requestbuffers},
        capability::Capability,
        crop::{Crop, Cropcap, Selection},
        ctrl::{Control, ExtControl, QueryCtrl, Querymenu},
        event::{Event, EventSubscription},
        format::{Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
        inout::{Input, Output},
        stream::StreamParm,
    };
    use crate::interface::{
        dv::{BtTimings, BtTimingsCap, DvTimings, DvTimingsCap, EnumDvTimings},
        edid::Edid,
        format::FmtFlag,
        legacy::{
            audio::{Audio, AudioOut},
            codec::{DecoderCmd, EncIndex, EncoderCmd},
            debug::{DbgChipInfo, DbgMatch, DbgRegister},
            framebuffer::Framebuffer,
            jpegcomp::JpegCompression,
            modulator::Modulator,
            standard::Standard,
            tuner::{Frequency, FrequencyBand, HwFreqSeek, Tuner},
            vbi::SlicedVbiCap,
        },
    };

    #[test]
    fn abi_sizes_match_linux() {
        // 核心 UAPI 结构（RISC-V 64 / x86_64，packed）
        assert_eq!(core::mem::size_of::<Capability>(), 104);
        assert_eq!(core::mem::size_of::<Fmtdesc>(), 64);
        assert_eq!(core::mem::size_of::<FrameSizeEnum>(), 44);
        assert_eq!(core::mem::size_of::<FrameIntervalEnum>(), 52);
        assert_eq!(core::mem::size_of::<Format>(), 208);
        assert_eq!(core::mem::size_of::<Requestbuffers>(), 20);
        assert_eq!(core::mem::size_of::<Buffer>(), 88);
        assert_eq!(core::mem::size_of::<Exportbuffer>(), 64);
        assert_eq!(core::mem::size_of::<Cropcap>(), 44);
        assert_eq!(core::mem::size_of::<Crop>(), 20);
        assert_eq!(core::mem::size_of::<Selection>(), 64);
        assert_eq!(core::mem::size_of::<Control>(), 8);
        assert_eq!(core::mem::size_of::<ExtControl>(), 20);
        assert_eq!(core::mem::size_of::<QueryCtrl>(), 68);
        assert_eq!(core::mem::size_of::<Querymenu>(), 44);
        assert_eq!(core::mem::size_of::<Input>(), 80);
        assert_eq!(core::mem::size_of::<Output>(), 72);
        assert_eq!(core::mem::size_of::<StreamParm>(), 204);
        assert_eq!(core::mem::size_of::<EventSubscription>(), 32);
        assert_eq!(core::mem::size_of::<Event>(), 136);
        assert_eq!(core::mem::size_of::<FmtFlag>(), 4);
        // 遗留/编解码/调谐等（与 videodev2.h / v4l2-common.h 一致）
        assert_eq!(core::mem::size_of::<Framebuffer>(), 48);
        assert_eq!(core::mem::size_of::<Standard>(), 72);
        assert_eq!(core::mem::size_of::<Tuner>(), 84);
        assert_eq!(core::mem::size_of::<Modulator>(), 68);
        assert_eq!(core::mem::size_of::<Frequency>(), 44);
        assert_eq!(core::mem::size_of::<FrequencyBand>(), 64);
        assert_eq!(core::mem::size_of::<HwFreqSeek>(), 48);
        assert_eq!(core::mem::size_of::<Audio>(), 52);
        assert_eq!(core::mem::size_of::<AudioOut>(), 52);
        assert_eq!(core::mem::size_of::<JpegCompression>(), 140);
        assert_eq!(core::mem::size_of::<SlicedVbiCap>(), 116);
        assert_eq!(core::mem::size_of::<EncIndex>(), 2072);
        assert_eq!(core::mem::size_of::<EncoderCmd>(), 40);
        assert_eq!(core::mem::size_of::<DecoderCmd>(), 72);
        assert_eq!(core::mem::size_of::<DbgMatch>(), 36);
        assert_eq!(core::mem::size_of::<DbgRegister>(), 56);
        assert_eq!(core::mem::size_of::<DbgChipInfo>(), 200);
        assert_eq!(core::mem::size_of::<BtTimings>(), 124);
        assert_eq!(core::mem::size_of::<DvTimings>(), 132);
        assert_eq!(core::mem::size_of::<EnumDvTimings>(), 148);
        assert_eq!(core::mem::size_of::<BtTimingsCap>(), 104);
        assert_eq!(core::mem::size_of::<DvTimingsCap>(), 144);
        assert_eq!(core::mem::size_of::<Edid>(), 40);
    }
}

use crate::interface::{
    buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
    capability::Capability,
    crop::{Crop, Cropcap, Selection},
    ctrl::{Control, ExtControls, QueryCtrl, QueryExtCtrl, Querymenu},
    dv::{DvTimings, DvTimingsCap, EnumDvTimings},
    edid::Edid,
    event::{Event, EventSubscription},
    format::{Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
    inout::{Input, Output, StdId},
    legacy::{
        audio::{Audio, AudioOut},
        codec::{DecoderCmd, EncIndex, EncoderCmd},
        debug::{DbgChipInfo, DbgRegister},
        framebuffer::Framebuffer,
        jpegcomp::JpegCompression,
        modulator::Modulator,
        standard::Standard,
        tuner::{Frequency, FrequencyBand, HwFreqSeek, Tuner},
        vbi::SlicedVbiCap,
    },
    stream::StreamParm,
};

// ── IOCTL 编码 ───────────────────────────────────────

const DIR_WRITE: u32 = 1;
const DIR_READ: u32 = 2;
const DIRSHIFT: u32 = 30;
const VT: u8 = b'V';

const fn io(nr: u8) -> u32 {
    ((VT as u32) << 8) | (nr as u32)
}
const fn ior(nr: u8, size: u32) -> u32 {
    (DIR_READ << DIRSHIFT) | ((VT as u32) << 8) | (nr as u32) | (size << 16)
}
const fn iow(nr: u8, size: u32) -> u32 {
    (DIR_WRITE << DIRSHIFT) | ((VT as u32) << 8) | (nr as u32) | (size << 16)
}
const fn iowr(nr: u8, size: u32) -> u32 {
    ((DIR_READ | DIR_WRITE) << DIRSHIFT) | ((VT as u32) << 8) | (nr as u32) | (size << 16)
}

// ── 命令枚举 ────────────────────────────────────────────────────────

macro_rules! ioctl_defs {
    ($name:ident, $(($variant:ident, $dir:ident, $nr:expr, $ty:ty)),* $(,)?) => {
        /// V4L2 ioctl 命令。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum $name {
            $($variant = ioctl_defs!(@val $dir, $nr, $ty),)*
        }

        impl $name {
            pub const COUNT: usize = [$(Self::$variant),*].len();
            pub const ALL: [$name; $name::COUNT] = [$(Self::$variant),*];

            pub fn try_from_u32(cmd: u32) -> Option<Self> {
                Some(match cmd {
                    $(c if c == Self::$variant as u32 => Self::$variant,)*
                    _ => return None,
                })
            }
        }
    };
    (@val io, $nr:expr, $ty:ty) => { io($nr) };
    (@val ior, $nr:expr, $ty:ty) => { ior($nr, core::mem::size_of::<$ty>() as u32) };
    (@val iow, $nr:expr, $ty:ty) => { iow($nr, core::mem::size_of::<$ty>() as u32) };
    (@val iowr, $nr:expr, $ty:ty) => { iowr($nr, core::mem::size_of::<$ty>() as u32) };
}

// 现代 V4L2 ioctl 命令（47 个）。
ioctl_defs!(
    IoctlCmd,
    // ── 查询与枚举 ──────────────────────────────────────────
    (QueryCap, ior, 0, Capability),
    (EnumFmt, iowr, 2, Fmtdesc),
    (EnumFrameSizes, iowr, 74, FrameSizeEnum),
    (EnumFrameIntervals, iowr, 75, FrameIntervalEnum),
    // ── 格式协商 ───────────────────────────────────────────
    (GFmt, iowr, 4, Format),
    (SFmt, iowr, 5, Format),
    (TryFmt, iowr, 64, Format),
    // ── 缓冲区管理 ────────────────────────────────────────────
    (ReqBufs, iowr, 8, Requestbuffers),
    (QueryBuf, iowr, 9, Buffer),
    (QBuf, iowr, 15, Buffer),
    (DQBuf, iowr, 17, Buffer),
    (PrepareBuf, iowr, 93, Buffer),
    (CreateBufs, iowr, 92, CreateBuffers),
    (RemoveBufs, iowr, 104, RemoveBuffers),
    (ExpBuf, iowr, 16, Exportbuffer),
    // ── 流式传输 ────────────────────────────────────────────────────
    (StreamOn, iow, 18, i32),
    (StreamOff, iow, 19, i32),
    // ── 流参数 ─────────────────────────────────────────
    (GParm, iowr, 21, StreamParm),
    (SParm, iowr, 22, StreamParm),
    // ── 优先级（core 层维护，device.rs 拦截） ──────────────
    (GPriority, ior, 67, u32),
    (SPriority, iow, 68, u32),
    // ── 输入/输出选择 ─────────────────────────────────────
    (EnumInput, iowr, 26, Input),
    (GInput, ior, 38, i32),
    (SInput, iowr, 39, i32),
    (EnumOutput, iowr, 48, Output),
    (GOutput, ior, 46, i32),
    (SOutput, iowr, 47, i32),
    // ── 控制 ─────────────────────────────────────────────────────
    (QueryCtrl, iowr, 36, QueryCtrl),
    (QueryExtCtrl, iowr, 103, QueryExtCtrl),
    (GExtCtrls, iowr, 71, ExtControls),
    (SExtCtrls, iowr, 72, ExtControls),
    (TryExtCtrls, iowr, 73, ExtControls),
    (QueryMenu, iowr, 37, Querymenu),
    // ── 裁剪 / Selection ─────────────────────────────────────────
    (CropCap, iowr, 58, Cropcap),
    (GSelection, iowr, 94, Selection),
    (SSelection, iowr, 95, Selection),
    // ── EDID ─────────────────────────────────────────────────────
    (GEdid, iowr, 40, Edid),
    (SEdid, iowr, 41, Edid),
    // ── DV timings ───────────────────────────────────────────────
    (SDvTimings, iowr, 87, DvTimings),
    (GDvTimings, iowr, 88, DvTimings),
    (EnumDvTimings, iowr, 98, EnumDvTimings),
    (QueryDvTimings, ior, 99, DvTimings),
    (DvTimingsCap, iowr, 100, DvTimingsCap),
    // ── 日志 ─────────────────────────────────────────────────────
    (LogStatus, io, 70, ()),
    // ── 事件（device.rs 拦截路由到驱动回调） ──────────────────
    (DQEvent, ior, 89, Event),
    (SubscribeEvent, iow, 90, EventSubscription),
    (UnsubscribeEvent, iow, 91, EventSubscription),
);

// 遗留 V4L2 ioctl 命令（36 个）
ioctl_defs!(
    LegacyIoctlCmd,
    // ── G/S_CTRL ───────
    (GCtrl, iowr, 27, Control),
    (SCtrl, iowr, 28, Control),
    // ── Overlay 帧缓冲 ──────────────────────────────────────
    (GFbuf, ior, 10, Framebuffer),
    (SFbuf, iow, 11, Framebuffer),
    (Overlay, iow, 14, i32),
    // ── 模拟电视标准 ───────────────────────────────────
    (GStd, ior, 23, StdId),
    (SStd, iow, 24, StdId),
    (EnumStd, iowr, 25, Standard),
    (QueryStd, ior, 63, StdId),
    // ── Tuner/Radio ────────────────────────────────────
    (GTuner, iowr, 29, Tuner),
    (STuner, iow, 30, Tuner),
    (GFrequency, iowr, 56, Frequency),
    (SFrequency, iow, 57, Frequency),
    (EnumFreqBands, iowr, 101, FrequencyBand),
    (SHwFreqSeek, iow, 82, HwFreqSeek),
    // ── 调制器 ─────────────────────────────────────────
    (GModulator, iowr, 54, Modulator),
    (SModulator, iow, 55, Modulator),
    // ── 音频 I/O ───────────────────────────────────────
    (GAudio, ior, 33, Audio),
    (SAudio, iow, 34, Audio),
    (EnumAudio, iowr, 65, Audio),
    (GAudioOut, ior, 49, AudioOut),
    (SAudioOut, iow, 50, AudioOut),
    (EnumAudioOut, iowr, 66, AudioOut),
    // ── 裁剪旧 API ─────────────────────────────────────
    (GCrop, iowr, 59, Crop),
    (SCrop, iow, 60, Crop),
    // ── JPEG 压缩旧 API ────────────────────────────────
    (GJpegComp, ior, 61, JpegCompression),
    (SJpegComp, iow, 62, JpegCompression),
    // ── Sliced VBI ─────────────────────────────────────
    (GSlicedVbiCap, iowr, 69, SlicedVbiCap),
    // ── Stateful codec ─────────────────────────────────
    (GEncIndex, ior, 76, EncIndex),
    (EncoderCmd, iowr, 77, EncoderCmd),
    (TryEncoderCmd, iowr, 78, EncoderCmd),
    (DecoderCmd, iowr, 96, DecoderCmd),
    (TryDecoderCmd, iowr, 97, DecoderCmd),
    // ── 调试 ───────────────────────────────────────────
    (DbgSRegister, iow, 79, DbgRegister),
    (DbgGRegister, iowr, 80, DbgRegister),
    (DbgGChipInfo, iowr, 102, DbgChipInfo),
);

// ── 统一命令枚举（modern + legacy）──────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoIoctl {
    Modern(IoctlCmd),
    Legacy(LegacyIoctlCmd),
}

impl VideoIoctl {
    /// 由原始 ioctl 命令号归一化；未知命令返回 `None`（对应 ENOTTY）。
    pub fn try_from_u32(cmd: u32) -> Option<Self> {
        IoctlCmd::try_from_u32(cmd)
            .map(Self::Modern)
            .or_else(|| LegacyIoctlCmd::try_from_u32(cmd).map(Self::Legacy))
    }

    /// 原始命令号。
    pub(crate) fn raw(self) -> u32 {
        match self {
            Self::Modern(c) => c as u32,
            Self::Legacy(c) => c as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 校验 priority 编码。
    #[test]
    fn priority_ioctl_codes_match_linux() {
        assert_eq!(IoctlCmd::GPriority as u32, 0x8004_5643);
        assert_eq!(IoctlCmd::SPriority as u32, 0x4004_5644);
        assert_eq!(
            IoctlCmd::try_from_u32(0x8004_5643),
            Some(IoctlCmd::GPriority)
        );
        assert_eq!(
            IoctlCmd::try_from_u32(0x4004_5644),
            Some(IoctlCmd::SPriority)
        );
    }

    /// 无效 ioctl 校验。
    #[test]
    fn unknown_and_normalization() {
        assert_eq!(IoctlCmd::try_from_u32(0xdead_beef), None);
        assert_eq!(IoctlCmd::try_from_u32(0x8004_562D), None); // 表外 nr=45
        assert_eq!(LegacyIoctlCmd::try_from_u32(0xdead_beef), None);
        assert_eq!(VideoIoctl::try_from_u32(0xdead_beef), None);
        assert_eq!(
            VideoIoctl::try_from_u32(IoctlCmd::QueryCtrl as u32),
            Some(VideoIoctl::Modern(IoctlCmd::QueryCtrl))
        );
        assert_eq!(
            VideoIoctl::try_from_u32(LegacyIoctlCmd::GCtrl as u32),
            Some(VideoIoctl::Legacy(LegacyIoctlCmd::GCtrl))
        );
        assert_eq!(IoctlCmd::try_from_u32(LegacyIoctlCmd::GCtrl as u32), None);
        assert_eq!(IoctlCmd::try_from_u32(LegacyIoctlCmd::SCtrl as u32), None);
    }

    /// 覆盖率检查。
    #[test]
    fn ioctl_count_matches_linux() {
        assert_eq!(IoctlCmd::COUNT, 47);
        assert_eq!(LegacyIoctlCmd::COUNT, 36);
        assert_eq!(IoctlCmd::COUNT + LegacyIoctlCmd::COUNT, 83);
        for c in IoctlCmd::ALL {
            assert_eq!(IoctlCmd::try_from_u32(c as u32), Some(c));
        }
        for c in LegacyIoctlCmd::ALL {
            assert_eq!(LegacyIoctlCmd::try_from_u32(c as u32), Some(c));
        }
        // 现代/遗留命令号互不重叠。
        for m in IoctlCmd::ALL {
            assert_eq!(LegacyIoctlCmd::try_from_u32(m as u32), None);
        }
        for l in LegacyIoctlCmd::ALL {
            assert_eq!(IoctlCmd::try_from_u32(l as u32), None);
        }
    }
}

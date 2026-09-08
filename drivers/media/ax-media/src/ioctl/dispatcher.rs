use core::{mem, ptr};

use super::define::{IoctlCmd, LegacyIoctlCmd, VideoIoctl};
use crate::{
    Result, V4l2Error,
    driver::V4L2DriverOps,
    interface::{
        buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
        capability::Capability,
        crop::{Crop, Cropcap, Selection},
        ctrl::{Control, QueryCtrl, QueryExtCtrl, Querymenu},
        dv::{DvTimings, DvTimingsCap, EnumDvTimings},
        edid::Edid,
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
    },
};

/// 从字节读取值。
///
/// # Safety
/// 调用方需保证类型与大小正确。
pub(crate) unsafe fn read_from_bytes<T: Copy>(bytes: &[u8]) -> T {
    assert!(bytes.len() >= mem::size_of::<T>());
    let ptr = bytes.as_ptr() as *const T;
    unsafe { ptr::read_unaligned(ptr) }
}

/// 将值写入字节。
///
/// # Safety
/// 调用方需保证类型与大小正确。
pub(crate) unsafe fn write_to_bytes<T: Copy>(bytes: &mut [u8], val: &T) {
    assert!(bytes.len() >= mem::size_of::<T>());
    let ptr = bytes.as_mut_ptr() as *mut T;
    unsafe { ptr::write_unaligned(ptr, *val) };
}

/// ioctl 分发辅助宏。
macro_rules! ioctl_body {
    (rw, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let mut v: $ty = $crate::ioctl::read_from_bytes($arg);
        $ops.$method(&mut v)?;
        $crate::ioctl::write_to_bytes($arg, &v);
        Ok(())
    }};
    (rw_ctrl, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let mut v: $ty = $crate::ioctl::read_from_bytes($arg);
        let handler = $ops.ctrl_handler().ok_or($crate::V4l2Error::NotSupported)?;
        handler.$method(&mut v)?;
        $crate::ioctl::write_to_bytes($arg, &v);
        Ok(())
    }};
    (wo, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let v: $ty = $crate::ioctl::read_from_bytes($arg);
        $ops.$method(&v)
    }};
    (get, $ops:ident, $arg:ident, $method:ident) => {{
        let v = $ops.$method()?;
        $crate::ioctl::write_to_bytes($arg, &v);
        Ok(())
    }};
    (val, $ops:ident, $arg:ident, $method:ident, $ty:ty) => {{
        let v: $ty = $crate::ioctl::read_from_bytes($arg);
        $ops.$method(v)
    }};
    (buf_type, $ops:ident, $arg:ident, $method:ident) => {{
        let ty: u32 = $crate::ioctl::read_from_bytes($arg);
        let bt = $crate::interface::BufType::try_from_u32(ty)
            .ok_or($crate::V4l2Error::InvalidArgument)?;
        $ops.$method(bt)
    }};
    (noarg, $ops:ident, $arg:ident, $method:ident) => {{ $ops.$method() }};
}

impl IoctlCmd {
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(self, ops: &mut dyn V4L2DriverOps, arg: &mut [u8]) -> Result<()> {
        // SAFETY: `arg` 长度已由 VFS 层按 ioctl 编码保证，
        // `read_from_bytes`/`write_to_bytes` 仅在长度足够时访问。
        unsafe {
            match self {
                // ── 查询与枚举 ──────────────────────────────
                Self::QueryCap => ioctl_body!(rw, ops, arg, querycap, Capability),
                Self::EnumFmt => ioctl_body!(rw, ops, arg, enum_fmt, Fmtdesc),
                Self::EnumFrameSizes => {
                    ioctl_body!(rw, ops, arg, enum_framesizes, FrameSizeEnum)
                }
                Self::EnumFrameIntervals => {
                    ioctl_body!(rw, ops, arg, enum_frameintervals, FrameIntervalEnum)
                }

                // ── 格式协商 ───────────────────────────────
                Self::GFmt => ioctl_body!(rw, ops, arg, g_fmt, Format),
                Self::SFmt => ioctl_body!(rw, ops, arg, s_fmt, Format),
                Self::TryFmt => ioctl_body!(rw, ops, arg, try_fmt, Format),

                // ── 缓冲区管理 ────────────────────────────────
                Self::ReqBufs => ioctl_body!(rw, ops, arg, reqbufs, Requestbuffers),
                Self::QueryBuf => ioctl_body!(rw, ops, arg, querybuf, Buffer),
                Self::QBuf => ioctl_body!(rw, ops, arg, qbuf, Buffer),
                Self::DQBuf => ioctl_body!(rw, ops, arg, dqbuf, Buffer),
                Self::PrepareBuf => ioctl_body!(rw, ops, arg, prepare_buf, Buffer),
                Self::CreateBufs => ioctl_body!(rw, ops, arg, create_bufs, CreateBuffers),
                Self::RemoveBufs => ioctl_body!(rw, ops, arg, remove_bufs, RemoveBuffers),
                Self::ExpBuf => ioctl_body!(rw, ops, arg, expbuf, Exportbuffer),

                // ── 流式传输 ────────────────────────────────────
                Self::StreamOn => ioctl_body!(buf_type, ops, arg, streamon),
                Self::StreamOff => ioctl_body!(buf_type, ops, arg, streamoff),

                // ── 流参数 ─────────────────────────────
                Self::GParm => ioctl_body!(rw, ops, arg, g_parm, StreamParm),
                Self::SParm => ioctl_body!(rw, ops, arg, s_parm, StreamParm),

                // ── 输入/输出选择 ─────────────────────────
                Self::EnumInput => ioctl_body!(rw, ops, arg, enum_input, Input),
                Self::GInput => ioctl_body!(get, ops, arg, g_input),
                Self::SInput => ioctl_body!(val, ops, arg, s_input, u32),
                Self::EnumOutput => ioctl_body!(rw, ops, arg, enum_output, Output),
                Self::GOutput => ioctl_body!(get, ops, arg, g_output),
                Self::SOutput => ioctl_body!(val, ops, arg, s_output, u32),

                // ── 控件查询（经驱动 CtrlHandler，核心统一处理）────
                Self::QueryCtrl => ioctl_body!(rw_ctrl, ops, arg, queryctrl, QueryCtrl),
                Self::QueryExtCtrl => {
                    ioctl_body!(rw_ctrl, ops, arg, query_ext_ctrl, QueryExtCtrl)
                }
                Self::QueryMenu => ioctl_body!(rw_ctrl, ops, arg, querymenu, Querymenu),

                // ── 核心代管 ioctl（priority / ext_ctrls / event）──
                // 由 VideoDevice::handle_ioctl 或 pseudofs glue 拦截路由，
                // 不进此分发器。
                Self::GPriority
                | Self::SPriority
                | Self::GExtCtrls
                | Self::SExtCtrls
                | Self::TryExtCtrls
                | Self::DQEvent
                | Self::SubscribeEvent
                | Self::UnsubscribeEvent => Err(V4l2Error::NotSupported),

                // ── 裁剪 / Selection ─────────────────────────────
                Self::CropCap => ioctl_body!(rw, ops, arg, cropcap, Cropcap),
                Self::GSelection => ioctl_body!(rw, ops, arg, g_selection, Selection),
                Self::SSelection => ioctl_body!(wo, ops, arg, s_selection, Selection),

                // ── EDID ─────────────────────────────────────────
                Self::GEdid => ioctl_body!(rw, ops, arg, g_edid, Edid),
                Self::SEdid => ioctl_body!(rw, ops, arg, s_edid, Edid),

                // ── DV timings ───────────────────────────────────
                Self::GDvTimings => ioctl_body!(rw, ops, arg, g_dv_timings, DvTimings),
                Self::SDvTimings => ioctl_body!(rw, ops, arg, s_dv_timings, DvTimings),
                Self::EnumDvTimings => {
                    ioctl_body!(rw, ops, arg, enum_dv_timings, EnumDvTimings)
                }
                Self::QueryDvTimings => ioctl_body!(rw, ops, arg, query_dv_timings, DvTimings),
                Self::DvTimingsCap => ioctl_body!(rw, ops, arg, dv_timings_cap, DvTimingsCap),

                // ── 日志 ────────────────────────────────────────
                Self::LogStatus => ioctl_body!(noarg, ops, arg, log_status),
            }
        }
    }
}

impl LegacyIoctlCmd {
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(self, ops: &mut dyn V4L2DriverOps, arg: &mut [u8]) -> Result<()> {
        // SAFETY: `arg` 长度已由 VFS 层按 ioctl 编码保证。
        unsafe {
            match self {
                Self::GCtrl => ioctl_body!(rw_ctrl, ops, arg, g_ctrl, Control),
                Self::SCtrl => ioctl_body!(rw_ctrl, ops, arg, s_ctrl, Control),

                // ── Overlay 帧缓冲 ────────────────────────────
                Self::GFbuf => ioctl_body!(rw, ops, arg, g_fbuf, Framebuffer),
                Self::SFbuf => ioctl_body!(wo, ops, arg, s_fbuf, Framebuffer),
                Self::Overlay => ioctl_body!(val, ops, arg, overlay, u32),

                // ── 模拟电视标准 ──────────────────────────────
                Self::GStd => ioctl_body!(rw, ops, arg, g_std, StdId),
                Self::SStd => ioctl_body!(val, ops, arg, s_std, StdId),
                Self::EnumStd => ioctl_body!(rw, ops, arg, enum_std, Standard),
                Self::QueryStd => ioctl_body!(rw, ops, arg, query_std, StdId),

                // ── Tuner/Radio ───────────────────────────────
                Self::GTuner => ioctl_body!(rw, ops, arg, g_tuner, Tuner),
                Self::STuner => ioctl_body!(wo, ops, arg, s_tuner, Tuner),
                Self::GFrequency => ioctl_body!(rw, ops, arg, g_frequency, Frequency),
                Self::SFrequency => ioctl_body!(wo, ops, arg, s_frequency, Frequency),
                Self::EnumFreqBands => {
                    ioctl_body!(rw, ops, arg, enum_freq_bands, FrequencyBand)
                }
                Self::SHwFreqSeek => ioctl_body!(wo, ops, arg, s_hw_freq_seek, HwFreqSeek),

                // ── 调制器 ─────────────────────────────────────
                Self::GModulator => ioctl_body!(rw, ops, arg, g_modulator, Modulator),
                Self::SModulator => ioctl_body!(wo, ops, arg, s_modulator, Modulator),

                // ── 音频 I/O ───────────────────────────────────
                Self::GAudio => ioctl_body!(rw, ops, arg, g_audio, Audio),
                Self::SAudio => ioctl_body!(wo, ops, arg, s_audio, Audio),
                Self::EnumAudio => ioctl_body!(rw, ops, arg, enum_audio, Audio),
                Self::GAudioOut => ioctl_body!(rw, ops, arg, g_audout, AudioOut),
                Self::SAudioOut => ioctl_body!(wo, ops, arg, s_audout, AudioOut),
                Self::EnumAudioOut => ioctl_body!(rw, ops, arg, enum_audout, AudioOut),

                // ── 裁剪旧 API ────────────────────────────────
                Self::GCrop => ioctl_body!(rw, ops, arg, g_crop, Crop),
                Self::SCrop => ioctl_body!(wo, ops, arg, s_crop, Crop),

                // ── JPEG 压缩旧 API ───────────────────────────
                Self::GJpegComp => ioctl_body!(rw, ops, arg, g_jpegcomp, JpegCompression),
                Self::SJpegComp => ioctl_body!(wo, ops, arg, s_jpegcomp, JpegCompression),

                // ── Sliced VBI ─────────────────────────────────
                Self::GSlicedVbiCap => {
                    ioctl_body!(rw, ops, arg, g_sliced_vbi_cap, SlicedVbiCap)
                }

                // ── Stateful codec ─────────────────────────────
                Self::GEncIndex => ioctl_body!(rw, ops, arg, g_enc_index, EncIndex),
                Self::EncoderCmd => ioctl_body!(rw, ops, arg, encoder_cmd, EncoderCmd),
                Self::TryEncoderCmd => ioctl_body!(rw, ops, arg, try_encoder_cmd, EncoderCmd),
                Self::DecoderCmd => ioctl_body!(rw, ops, arg, decoder_cmd, DecoderCmd),
                Self::TryDecoderCmd => ioctl_body!(rw, ops, arg, try_decoder_cmd, DecoderCmd),

                // ── 调试 ───────────────────────────────────────
                Self::DbgGRegister => ioctl_body!(rw, ops, arg, dbg_g_register, DbgRegister),
                Self::DbgSRegister => ioctl_body!(wo, ops, arg, dbg_s_register, DbgRegister),
                Self::DbgGChipInfo => ioctl_body!(rw, ops, arg, dbg_g_chip_info, DbgChipInfo),
            }
        }
    }
}

/// IOCTL 分发器。
pub struct IoctlDispatcher {
    valid: [u64; 4],
}

impl IoctlDispatcher {
    pub const fn new() -> Self {
        Self {
            valid: [u64::MAX; 4],
        }
    }

    /// 禁用 ioctl。
    pub fn disable_cmd(&mut self, cmd: u32) {
        let idx = (cmd & 0xff) as usize;
        self.valid[idx / 64] &= !(1u64 << (idx % 64));
    }

    fn is_valid(&self, cmd: u32) -> bool {
        let idx = (cmd & 0xff) as usize;
        self.valid[idx / 64] & (1u64 << (idx % 64)) != 0
    }

    pub fn dispatch(
        &self,
        ops: &mut dyn V4L2DriverOps,
        cmd: VideoIoctl,
        arg: &mut [u8],
    ) -> Result<()> {
        if !self.is_valid(cmd.raw()) {
            return Err(V4l2Error::NotSupported);
        }
        match cmd {
            VideoIoctl::Modern(c) => c.dispatch(ops, arg),
            VideoIoctl::Legacy(c) => c.dispatch(ops, arg),
        }
    }
}

impl Default for IoctlDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

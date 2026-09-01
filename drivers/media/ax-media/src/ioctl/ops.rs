//! Ioctl trait。

use crate::{
    Result, V4l2Error,
    filehandler::V4l2Fh,
    interface::{
        BufType,
        buffer::{Buffer, CreateBuffers, Exportbuffer, RemoveBuffers, Requestbuffers},
        capability::Capability,
        crop::{Crop, Cropcap, Selection},
        ctrl::Control,
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
    },
};

/// Modern ioctl。
#[allow(unused_variables)]
pub trait IoctlOps {
    // ── 查询与枚举 ──────────────────────────────────────────

    /// 查询设备能力（`VIDIOC_QUERYCAP`）
    fn querycap(&self, cap: &mut Capability) -> Result<()>;

    /// 枚举像素格式（`VIDIOC_ENUM_FMT`）
    fn enum_fmt(&self, f: &mut Fmtdesc) -> Result<()>;

    /// 枚举帧尺寸（`VIDIOC_ENUM_FRAMESIZES`）
    fn enum_framesizes(&self, f: &mut FrameSizeEnum) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 枚举帧间隔（`VIDIOC_ENUM_FRAMEINTERVALS`）
    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 格式协商 ───────────────────────────────────────────

    /// 获取当前格式（`VIDIOC_G_FMT`）
    fn g_fmt(&self, f: &mut Format) -> Result<()>;

    /// 设置格式（`VIDIOC_S_FMT`）
    fn s_fmt(&mut self, f: &mut Format) -> Result<()>;

    /// 试设格式（`VIDIOC_TRY_FMT`）
    fn try_fmt(&self, f: &mut Format) -> Result<()>;

    // ── 缓冲区管理 ───────────────────────────────────────────

    /// 申请缓冲区（`VIDIOC_REQBUFS`）
    fn reqbufs(&mut self, req: &mut Requestbuffers) -> Result<()>;

    /// 查询缓冲区（`VIDIOC_QUERYBUF`）
    fn querybuf(&self, buf: &mut Buffer) -> Result<()>;

    /// 入队缓冲区（`VIDIOC_QBUF`）
    fn qbuf(&mut self, buf: &mut Buffer) -> Result<()>;

    /// 出队缓冲区（`VIDIOC_DQBUF`）
    fn dqbuf(&mut self, buf: &mut Buffer) -> Result<()>;

    /// 预备缓冲区（`VIDIOC_PREPARE_BUF`）
    fn prepare_buf(&mut self, buf: &mut Buffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 创建缓冲区（`VIDIOC_CREATE_BUFS`）
    fn create_bufs(&mut self, bufs: &mut CreateBuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 移除缓冲区（`VIDIOC_REMOVE_BUFS`）
    fn remove_bufs(&mut self, bufs: &mut RemoveBuffers) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 导出缓冲区（`VIDIOC_EXPBUF`）。仅支持 DMABUF 时实现
    fn expbuf(&self, buf: &mut Exportbuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 流式传输 ────────────────────────────────────────────────────

    /// 开启流（`VIDIOC_STREAMON`）
    fn streamon(&mut self, ty: BufType) -> Result<()>;

    /// 关闭流（`VIDIOC_STREAMOFF`）
    fn streamoff(&mut self, ty: BufType) -> Result<()>;

    // ── 流参数 ─────────────────────────────────────────

    fn g_parm(&self, p: &mut StreamParm) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_parm(&mut self, p: &mut StreamParm) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 输入选择 ──────────────────────────────────────────────

    fn enum_input(&self, input: &mut Input) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_input(&self) -> Result<u32> {
        Err(V4l2Error::NotSupported)
    }

    fn s_input(&mut self, index: u32) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_output(&self, output: &mut Output) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_output(&self) -> Result<u32> {
        Err(V4l2Error::NotSupported)
    }

    fn s_output(&mut self, index: u32) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 裁剪 / Selection ─────────────────────────────────────────

    fn cropcap(&self, c: &mut Cropcap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_selection(&self, s: &mut Selection) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_selection(&mut self, s: &Selection) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── EDID ─────────────────────────────────────────────────────

    fn g_edid(&self, edid: &mut Edid) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_edid(&mut self, edid: &mut Edid) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── DV timings ───────────────────────────────────────────────

    fn g_dv_timings(&self, t: &mut DvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_dv_timings(&mut self, t: &mut DvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_dv_timings(&self, t: &mut EnumDvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn query_dv_timings(&self, t: &mut DvTimings) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dv_timings_cap(&self, c: &mut DvTimingsCap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 日志 ─────────────────────────────────────────────────────

    fn log_status(&self) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 事件 ────────────────────────────────────────────────────────

    /// 处理 `VIDIOC_SUBSCRIBE_EVENT`。
    fn subscribe_event(&mut self, _fh: &mut V4l2Fh, _sub: &EventSubscription) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// 处理 `VIDIOC_UNSUBSCRIBE_EVENT`。
    fn unsubscribe_event(&mut self, fh: &mut V4l2Fh, sub: &EventSubscription) -> Result<()> {
        fh.unsubscribe(sub);
        Ok(())
    }

    /// 处理 `VIDIOC_DQEVENT`（非阻塞）。
    fn dqevent(&mut self, fh: &mut V4l2Fh, event: &mut Event) -> Result<()> {
        *event = fh.dequeue()?;
        Ok(())
    }
}

/// Legacy ioctl。
#[allow(unused_variables)]
pub trait LegacyIoctlOps {
    /// G_CTRL。
    fn g_ctrl(&self, _c: &mut Control) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    /// S_CTRL。
    fn s_ctrl(&mut self, _c: &Control) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Overlay 帧缓冲 ─────────────────────────────────────────

    fn g_fbuf(&self, fb: &mut Framebuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_fbuf(&mut self, fb: &Framebuffer) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn overlay(&mut self, on: u32) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 模拟电视标准 ───────────────────────────────────────────

    fn g_std(&self, id: &mut StdId) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_std(&mut self, id: StdId) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_std(&self, s: &mut Standard) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn query_std(&self, id: &mut StdId) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Tuner/Radio ────────────────────────────────────────────

    fn g_tuner(&self, t: &mut Tuner) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_tuner(&mut self, t: &Tuner) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_frequency(&self, f: &mut Frequency) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_frequency(&mut self, f: &Frequency) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_freq_bands(&self, b: &mut FrequencyBand) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_hw_freq_seek(&mut self, s: &HwFreqSeek) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 调制器 ─────────────────────────────────────────────────

    fn g_modulator(&self, m: &mut Modulator) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_modulator(&mut self, m: &Modulator) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 音频 I/O ───────────────────────────────────────────────

    fn g_audio(&self, a: &mut Audio) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_audio(&mut self, a: &Audio) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_audio(&self, a: &mut Audio) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn g_audout(&self, a: &mut AudioOut) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_audout(&mut self, a: &AudioOut) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn enum_audout(&self, a: &mut AudioOut) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 裁剪旧 API ─────────────────────────────────────────────

    fn g_crop(&self, c: &mut Crop) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_crop(&mut self, c: &Crop) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── JPEG 压缩旧 API ────────────────────────────────────────

    fn g_jpegcomp(&self, j: &mut JpegCompression) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn s_jpegcomp(&mut self, j: &JpegCompression) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Sliced VBI ─────────────────────────────────────────────

    fn g_sliced_vbi_cap(&self, c: &mut SlicedVbiCap) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── Stateful codec ─────────────────────────────────────────

    fn g_enc_index(&self, idx: &mut EncIndex) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn encoder_cmd(&mut self, c: &mut EncoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn try_encoder_cmd(&mut self, c: &mut EncoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn decoder_cmd(&mut self, c: &mut DecoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn try_decoder_cmd(&mut self, c: &mut DecoderCmd) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    // ── 调试 ───────────────────────────────────────────────────

    fn dbg_g_register(&self, r: &mut DbgRegister) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dbg_s_register(&mut self, r: &DbgRegister) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }

    fn dbg_g_chip_info(&self, c: &mut DbgChipInfo) -> Result<()> {
        Err(V4l2Error::NotSupported)
    }
}

//! UVC V4L2 ioctl dispatch.

use alloc::{sync::Arc, vec::Vec};

use ax_media::{
    IoctlOps, LegacyIoctlOps, V4L2DriverOps, V4l2Error, V4l2Fh,
    interface::{
        BufType, Field, Memory, Timecode, Timeval, buffer,
        capability::{Capabilities, Capability},
        colorspace,
        event::EventSubscription,
        format::{
            self, Fmtdesc, Format, FrameIntervalEnum, FrameIntervalType, FrameSizeEnum,
            FrameSizeType,
        },
        stream::{StreamParm, StreamParmCap, StreamParmMode},
    },
    videobuffer::BufferState,
};
use axpoll::PollSet;
use log::*;

use crate::{UvcDevice, UvcHandle, VideoFormat};

#[allow(dead_code)]
const PIX_FMT_MJPEG: u32 = 0x47504a4d;
const PIX_FMT_YUYV: u32 = 0x56595559;

impl<H: UvcHandle> V4L2DriverOps for UvcDevice<H> {
    fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        self.queue.mmap(offset, length)
    }

    fn is_readable(&self) -> bool {
        self.queue.is_readable()
    }

    fn is_error(&self) -> bool {
        self.queue.is_error()
    }

    fn is_streaming(&self) -> bool {
        self.queue.is_streaming()
    }

    fn num_buffers(&self) -> u32 {
        self.queue.num_buffers()
    }

    fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        Some(self.queue.vb_poll_set().clone())
    }

    fn release(&self) {
        self.close_stream();
        self.queue.streamoff();
        *self.state.lock() = crate::UvcDeviceState::Configured;
    }

    fn ctrl_handler(&self) -> Option<&ax_media::CtrlHandler> {
        Some(&self.ctrls)
    }
}

impl<H: UvcHandle> LegacyIoctlOps for UvcDevice<H> {}

impl<H: UvcHandle> IoctlOps for UvcDevice<H> {
    fn querycap(&self, cap: &mut Capability) -> ax_media::Result<()> {
        let driver = b"uvc\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let card = b"Starry UVC Camera\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let bus = b"usb-sg2002\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

        cap.capabilities = Capabilities::VIDEO_CAPTURE
            | Capabilities::STREAMING
            | Capabilities::DEVICE_CAPS
            | Capabilities::EXT_PIX_FORMAT;
        cap.device_caps = Capabilities::VIDEO_CAPTURE | Capabilities::STREAMING;

        cap.driver[..driver.len()].copy_from_slice(driver);
        cap.card[..card.len()].copy_from_slice(card);
        cap.bus_info[..bus.len()].copy_from_slice(bus);
        cap.version = 0x00060000;
        cap.reserved = [0; 3];

        Ok(())
    }

    fn enum_fmt(&self, f: &mut Fmtdesc) -> ax_media::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        let mut seen = Vec::new();
        let mut uniq: Vec<&VideoFormat> = Vec::new();
        for fmt in &self.formats {
            let pf = fmt.pixelformat();
            if !seen.contains(&pf) {
                seen.push(pf);
                uniq.push(fmt);
            }
        }
        let format = uniq
            .get(f.index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;

        f.description = [0; 32];
        let description = format.description();
        let desc_bytes = description.as_bytes();
        let copy_len = desc_bytes.len().min(31);
        f.description[..copy_len].copy_from_slice(&desc_bytes[..copy_len]);
        f.pixelformat = format.pixelformat();
        f.flags = if format.is_compressed() {
            format::FmtFlag::COMPRESSED
        } else {
            format::FmtFlag::empty()
        };
        f.mbus_code = 0;
        f.reserved = [0; 3];

        Ok(())
    }

    fn enum_framesizes(&self, f: &mut FrameSizeEnum) -> ax_media::Result<()> {
        let pixel_format = f.pixel_format;
        let matching: Vec<&VideoFormat> = self
            .formats
            .iter()
            .filter(|fmt| fmt.pixelformat() == pixel_format)
            .collect();
        if matching.is_empty() {
            return Err(V4l2Error::InvalidArgument);
        }
        let format = matching
            .get(f.index as usize)
            .ok_or(V4l2Error::InvalidArgument)?;
        f.ty = FrameSizeType::Discrete;
        f.size.discrete.width = format.width as u32;
        f.size.discrete.height = format.height as u32;
        f.reserved = [0; 2];
        Ok(())
    }

    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> ax_media::Result<()> {
        let fmt = self
            .formats
            .iter()
            .find(|fmt| {
                fmt.pixelformat() == f.pixel_format
                    && fmt.width as u32 == f.width
                    && fmt.height as u32 == f.height
            })
            .ok_or(V4l2Error::InvalidArgument)?;
        if f.index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let fps = if fmt.frame_rate != 0 {
            fmt.frame_rate
        } else {
            30
        };
        f.ty = FrameIntervalType::Discrete;
        f.interval.discrete.numerator = 1;
        f.interval.discrete.denominator = fps;
        f.reserved = [0; 2];
        Ok(())
    }

    fn g_fmt(&self, f: &mut Format) -> ax_media::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        let current = self.active_format_ref();

        f.ty = BufType::VideoCapture;
        f.fmt.pix.width = current.width as u32;
        f.fmt.pix.height = current.height as u32;
        f.fmt.pix.pixelformat = current.pixelformat();
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = current.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = current.frame_bytes() as u32;
        f.fmt.pix.colorspace = current.colorspace();
        f.fmt.pix.priv_data = 0;
        f.fmt.pix.flags = 0;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FullRange;
        f.fmt.pix.xfer_func = colorspace::XferFunc::Default;
        Ok(())
    }

    fn s_fmt(&mut self, f: &mut Format) -> ax_media::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        // SAFETY: `f.ty` is VideoCapture, so `pix` is active.
        let pix = unsafe { f.fmt.pix };
        let width = pix.width as u16;
        let height = pix.height as u16;
        let mut pixelformat = pix.pixelformat;

        if !self
            .formats
            .iter()
            .any(|fmt| fmt.pixelformat() == pixelformat)
        {
            pixelformat = self
                .formats
                .first()
                .map(|fmt| fmt.pixelformat())
                .unwrap_or(PIX_FMT_YUYV);
        }

        let format = VideoFormat {
            format_type: pixelformat.into(),
            width,
            height,
            frame_rate: {
                let fps = self.active_format_ref().frame_rate;
                if fps != 0 { fps } else { 30 }
            },
            format_index: 0,
            frame_index: 0,
        };

        self.set_format(format)
            .map_err(|_| V4l2Error::InvalidArgument)?;

        let current = self.active_format_ref();
        f.fmt.pix.width = current.width as u32;
        f.fmt.pix.height = current.height as u32;
        f.fmt.pix.pixelformat = current.pixelformat();
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = current.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = current.frame_bytes() as u32;
        f.fmt.pix.colorspace = current.colorspace();
        f.fmt.pix.priv_data = 0;
        f.fmt.pix.flags = 0;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FullRange;
        f.fmt.pix.xfer_func = colorspace::XferFunc::Default;
        Ok(())
    }

    fn try_fmt(&self, f: &mut Format) -> ax_media::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        // SAFETY: `f.ty` is VideoCapture, so `pix` is active.
        let pix = unsafe { f.fmt.pix };
        let mut pixelformat = pix.pixelformat;
        if !self
            .formats
            .iter()
            .any(|fmt| fmt.pixelformat() == pixelformat)
        {
            pixelformat = self
                .formats
                .first()
                .map(|fmt| fmt.pixelformat())
                .unwrap_or(PIX_FMT_YUYV);
        }
        let negotiated = self
            .formats
            .iter()
            .find(|fmt| {
                fmt.pixelformat() == pixelformat
                    && fmt.width as u32 == pix.width
                    && fmt.height as u32 == pix.height
            })
            .or_else(|| {
                self.formats
                    .iter()
                    .find(|fmt| fmt.pixelformat() == pixelformat)
            });
        let (w, h, sizeimage, bytesperline, colorspace) = match negotiated {
            Some(fmt) => (
                fmt.width as u32,
                fmt.height as u32,
                fmt.frame_bytes() as u32,
                fmt.bytes_per_line() as u32,
                fmt.colorspace(),
            ),
            None => (
                pix.width.clamp(160, 1920),
                pix.height.clamp(120, 1080),
                pix.width * pix.height * 2,
                0,
                colorspace::Colorspace::Srgb,
            ),
        };
        f.fmt.pix.width = w;
        f.fmt.pix.height = h;
        f.fmt.pix.pixelformat = pixelformat;
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = bytesperline;
        f.fmt.pix.sizeimage = sizeimage;
        f.fmt.pix.colorspace = colorspace;
        f.fmt.pix.priv_data = 0;
        f.fmt.pix.flags = 0;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FullRange;
        f.fmt.pix.xfer_func = colorspace::XferFunc::Default;
        Ok(())
    }

    fn reqbufs(&mut self, req: &mut buffer::Requestbuffers) -> ax_media::Result<()> {
        if req.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        if req.memory != Memory::Mmap {
            return Err(V4l2Error::InvalidArgument);
        }
        let sizeimage = {
            let bytes = self.active_format_ref().frame_bytes() as u32;
            if bytes != 0 { bytes } else { 300 * 1024 }
        };
        let q = &self.queue;
        if req.count == 0 {
            q.reqbufs(0, &[sizeimage])?;
            req.count = 0;
            req.capabilities = buffer::BufCapabilities::SUPPORTS_MMAP;
            req.flags = 0;
            req.reserved = [0; 3];
            return Ok(());
        }
        q.reqbufs(req.count, &[sizeimage])?;
        req.count = q.num_buffers();
        req.capabilities = buffer::BufCapabilities::SUPPORTS_MMAP;
        req.flags = 0;
        req.reserved = [0; 3];
        Ok(())
    }

    fn querybuf(&self, buf: &mut buffer::Buffer) -> ax_media::Result<()> {
        let q = &self.queue;
        let vb = q
            .buffer_snapshot(buf.index)
            .ok_or(V4l2Error::InvalidArgument)?;

        let plane = vb.planes.first().ok_or(V4l2Error::InvalidArgument)?;
        buf.ty = BufType::VideoCapture;
        buf.length = plane.length;
        buf.m.offset = plane.offset as u32;
        buf.memory = Memory::Mmap;
        buf.field = Field::NoField;
        buf.timecode = Timecode::default();
        buf.reserved2 = 0;
        buf.request_fd = 0;

        let mut flags = buffer::BufFlags::MAPPED | buffer::BufFlags::TIMESTAMP_MONOTONIC;
        match vb.state {
            BufferState::Queued | BufferState::Active => flags |= buffer::BufFlags::QUEUED,
            BufferState::Done => {
                flags |= buffer::BufFlags::DONE;
            }
            BufferState::Error => {
                flags |= buffer::BufFlags::DONE | buffer::BufFlags::ERROR;
            }
            BufferState::Dequeued => {}
        }
        buf.flags = flags;

        if vb.state == BufferState::Done || vb.state == BufferState::Error {
            buf.bytesused = vb.bytesused;
            buf.sequence = vb.sequence;
            buf.timestamp = Timeval {
                tv_sec: (vb.timestamp / 1_000_000_000) as i64,
                tv_usec: ((vb.timestamp / 1_000) % 1_000_000) as i64,
            };
        } else {
            buf.bytesused = 0;
            buf.sequence = 0;
            buf.timestamp = Timeval {
                tv_sec: 0,
                tv_usec: 0,
            };
        }
        Ok(())
    }

    fn qbuf(&mut self, buf: &mut buffer::Buffer) -> ax_media::Result<()> {
        self.queue.qbuf(buf.index)?;
        buf.flags = buffer::BufFlags::QUEUED;
        Ok(())
    }

    fn dqbuf(&mut self, buf: &mut buffer::Buffer) -> ax_media::Result<()> {
        let q = &self.queue;
        if !q.is_streaming() {
            return Err(V4l2Error::InvalidArgument);
        }
        if q.is_error() {
            return Err(V4l2Error::Io);
        }
        if !q.is_readable() {
            return Err(V4l2Error::WouldBlock);
        }
        let idx = q.dqbuf()?;
        let vb = q.buffer_snapshot(idx).ok_or(V4l2Error::InvalidArgument)?;
        let (bytesused, sequence, timestamp, timestamp_flags) =
            (vb.bytesused, vb.sequence, vb.timestamp, vb.timestamp_flags);

        buf.index = idx;
        buf.flags =
            buffer::BufFlags::KEYFRAME | buffer::BufFlags::from_bits_retain(timestamp_flags);
        buf.bytesused = bytesused;
        buf.timestamp = Timeval {
            tv_sec: (timestamp / 1_000_000_000) as i64,
            tv_usec: ((timestamp / 1_000) % 1_000_000) as i64,
        };
        buf.field = Field::NoField;
        buf.sequence = sequence;
        buf.memory = Memory::Mmap;
        buf.ty = BufType::VideoCapture;
        buf.length = vb.planes.first().map(|p| p.length).unwrap_or(0);
        buf.m.offset = vb.planes.first().map(|p| p.offset as u32).unwrap_or(0);
        Ok(())
    }

    fn streamon(&mut self, ty: BufType) -> ax_media::Result<()> {
        if ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        self.start_streaming().map_err(|_| V4l2Error::Io)?;
        self.queue.streamon()?;
        Ok(())
    }

    fn streamoff(&mut self, ty: BufType) -> ax_media::Result<()> {
        if ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        self.close_stream();
        self.queue.streamoff();
        Ok(())
    }

    fn g_parm(&self, p: &mut StreamParm) -> ax_media::Result<()> {
        if p.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        p.parm.raw_data = [0; 200];
        // SAFETY: `p.ty` is VideoCapture, so `capture` union field is active.
        let cap = unsafe { &mut p.parm.capture };
        let interval = *self.parm_interval.lock();
        cap.capability = StreamParmCap::TIMEPERFRAME;
        cap.capturemode = StreamParmMode::empty();
        cap.timeperframe = interval;
        cap.extendedmode = 0;
        cap.readbuffers = 0;
        cap.reserved = [0; 4];
        Ok(())
    }

    fn s_parm(&mut self, p: &mut StreamParm) -> ax_media::Result<()> {
        if p.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        // SAFETY: `p.ty` is VideoCapture, so `capture` is active.
        let req = unsafe { p.parm.capture.timeperframe };
        let mut interval = req;
        if interval.numerator == 0 && interval.denominator == 0 {
            interval.numerator = 1;
            interval.denominator = 30;
        } else {
            if interval.numerator == 0 {
                interval.numerator = 1;
            }
            if interval.denominator == 0 {
                interval.denominator = 30;
            }
        }
        if interval.numerator > 1000 {
            interval.numerator = 1000;
        }
        if interval.denominator > 1000 {
            interval.denominator = 1000;
        }
        *self.parm_interval.lock() = interval;
        p.parm.raw_data = [0; 200];
        // SAFETY: `p.ty` is VideoCapture, so `capture` is active.
        let cap = unsafe { &mut p.parm.capture };
        cap.capability = StreamParmCap::TIMEPERFRAME;
        cap.capturemode = StreamParmMode::empty();
        cap.timeperframe = interval;
        cap.extendedmode = 0;
        cap.readbuffers = 0;
        cap.reserved = [0; 4];
        Ok(())
    }

    fn log_status(&self) -> ax_media::Result<()> {
        info!(
            "[UVC] log_status streaming={} buffers={} fmt={}x{} pf={:08x}",
            self.queue.is_streaming(),
            self.queue.num_buffers(),
            self.active_format_ref().width,
            self.active_format_ref().height,
            self.active_format_ref().pixelformat()
        );
        Ok(())
    }

    fn enum_input(&self, input: &mut ax_media::interface::inout::Input) -> ax_media::Result<()> {
        if input.index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let name = b"Camera\0";
        input.name = [0; 32];
        input.name[..name.len()].copy_from_slice(name);
        input.ty = ax_media::interface::inout::InputType::Camera;
        input.audioset = 0;
        input.tuner = 0;
        input.std = 0;
        input.status = ax_media::interface::inout::InStatus::empty();
        input.capabilities = ax_media::interface::inout::InCap::empty();
        input.reserved = [0; 3];
        Ok(())
    }

    fn g_input(&self) -> ax_media::Result<u32> {
        Ok(0)
    }

    fn s_input(&mut self, index: u32) -> ax_media::Result<()> {
        if index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        Ok(())
    }

    fn subscribe_event(
        &mut self,
        fh: &mut V4l2Fh,
        sub: &EventSubscription,
    ) -> ax_media::Result<()> {
        self.ctrls.subscribe_event(fh, sub)
    }
}

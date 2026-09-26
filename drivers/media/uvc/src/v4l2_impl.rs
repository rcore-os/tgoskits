//! UVC V4L2 ioctl dispatch.

use alloc::{sync::Arc, vec::Vec};
use core::any::Any;

use ax_media::{
    IoctlOps, LegacyIoctlOps, V4L2DriverOps, V4l2Error, V4l2Fh,
    interface::{
        BufType, Field, Fract, Timecode, Timeval, buffer,
        buffer::Memory,
        capability::{Capabilities, Capability},
        colorspace,
        event::EventSubscription,
        format::{
            self, Fmtdesc, Format, FrameIntervalEnum, FrameIntervalType, FrameSizeEnum,
            FrameSizeType,
        },
        stream::{StreamParm, StreamParmCap, StreamParmMode},
    },
    videobuffer::{BufferState, VbMemOps},
};
use axpoll_set::PollSet;
use log::*;

use crate::{FrameIntervals, UvcDevice, UvcHandle, VideoFormat};

fn claim_video_interfaces<H: UvcHandle>(handle: &H, vc: u8, vs: u8) -> ax_media::Result<()> {
    handle.claim_interface(vc, 0).map_err(|_| V4l2Error::Busy)?;
    if let Err(error) = handle.claim_interface(vs, 0) {
        let _ = handle.release_interface(vc);
        warn!("[UVC] failed to claim VS interface: {error:?}");
        return Err(V4l2Error::Busy);
    }
    Ok(())
}

impl<H: UvcHandle, M: VbMemOps + 'static> V4L2DriverOps for UvcDevice<H, M> {
    fn open(&self) -> ax_media::Result<()> {
        claim_video_interfaces(self.handle.as_ref(), self.vc_iface_num, self.vs_iface_num)?;
        self.register_controls(&self.vc_units);
        Ok(())
    }

    fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, Arc<dyn Any + Send + Sync>)> {
        self.pool.mmap(offset, length)
    }

    fn is_readable(&self) -> bool {
        self.pool.is_readable()
    }

    fn is_error(&self) -> bool {
        self.pool.is_error()
    }

    fn is_streaming(&self) -> bool {
        self.pool.is_streaming()
    }

    fn num_buffers(&self) -> u32 {
        self.pool.num_buffers()
    }

    fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        Some(self.pool.vb_poll_set().clone())
    }

    fn release(&self) {
        self.close_stream();
        self.pool.streamoff();
        let _ = self.pool.reqbufs(0, &[]);
        let _ = self.handle.release_interface(self.vs_iface_num);
        let _ = self.handle.release_interface(self.vc_iface_num);
    }

    fn close_owner(&self) {
        self.close_stream();
        self.pool.streamoff();
        let _ = self.pool.reqbufs(0, &[]);
    }

    fn ctrl_handler(&self) -> Option<Arc<ax_sync::Mutex<ax_media::CtrlHandler>>> {
        Some(self.ctrls.clone())
    }
}

impl<H: UvcHandle, M: VbMemOps + 'static> LegacyIoctlOps for UvcDevice<H, M> {}

impl<H: UvcHandle, M: VbMemOps + 'static> IoctlOps for UvcDevice<H, M> {
    fn querycap(&self, cap: &mut Capability) -> ax_media::Result<()> {
        let driver = b"uvc\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let card = b"Starry UVC Camera\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

        cap.capabilities = Capabilities::VIDEO_CAPTURE
            | Capabilities::STREAMING
            | Capabilities::DEVICE_CAPS
            | Capabilities::EXT_PIX_FORMAT;
        cap.device_caps = Capabilities::VIDEO_CAPTURE | Capabilities::STREAMING;

        cap.driver[..driver.len()].copy_from_slice(driver);
        cap.card[..card.len()].copy_from_slice(card);
        cap.bus_info = self.bus_info;
        cap.version = 0x00060000;
        cap.reserved = [0; 3];

        Ok(())
    }

    fn enum_fmt(&self, f: &mut Fmtdesc) -> ax_media::Result<()> {
        if f.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        let mut seen = Vec::new();
        let mut uniq: Vec<&VideoFormat> = Vec::new();
        for fmt in &self.formats {
            if !seen.contains(&fmt.format_index) {
                seen.push(fmt.format_index);
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
        let selected = self
            .formats
            .iter()
            .find(|format| format.pixelformat() == f.pixel_format)
            .ok_or(V4l2Error::InvalidArgument)?;
        let mut index = f.index;
        let mut previous_size = None;
        for format in self.formats.iter().filter(|format| {
            format.format_index == selected.format_index
                && format.pixelformat() == selected.pixelformat()
        }) {
            let size = (format.width, format.height);
            if previous_size == Some(size) {
                continue;
            }
            previous_size = Some(size);
            if index == 0 {
                f.ty = FrameSizeType::DISCRETE;
                f.size.discrete.width = format.width as u32;
                f.size.discrete.height = format.height as u32;
                f.reserved = [0; 2];
                return Ok(());
            }
            index -= 1;
        }
        Err(V4l2Error::InvalidArgument)
    }

    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> ax_media::Result<()> {
        let selected = self
            .formats
            .iter()
            .find(|format| format.pixelformat() == f.pixel_format)
            .ok_or(V4l2Error::InvalidArgument)?;
        let mut index = f.index as usize;
        for format in self.formats.iter().filter(|format| {
            format.format_index == selected.format_index
                && format.pixelformat() == selected.pixelformat()
                && format.width as u32 == f.width
                && format.height as u32 == f.height
        }) {
            match &format.intervals {
                FrameIntervals::Discrete(intervals) => {
                    let count = intervals.len().max(1);
                    if index >= count {
                        index -= count;
                        continue;
                    }
                    let interval = intervals.get(index).copied().unwrap_or_else(|| {
                        if format.default_interval != 0 {
                            format.default_interval
                        } else {
                            333_333
                        }
                    });
                    f.ty = FrameIntervalType::DISCRETE;
                    f.interval.discrete = Fract::from_interval(interval);
                }
                FrameIntervals::Continuous { min, max, step } => {
                    if index != 0 {
                        index -= 1;
                        continue;
                    }
                    f.ty = FrameIntervalType::STEPWISE;
                    f.interval.stepwise.min = Fract::from_interval(*min);
                    f.interval.stepwise.max = Fract::from_interval(*max);
                    f.interval.stepwise.step = Fract::from_interval(*step);
                }
            }
            f.reserved = [0; 2];
            return Ok(());
        }
        Err(V4l2Error::InvalidArgument)
    }

    fn g_fmt(&self, f: &mut Format) -> ax_media::Result<()> {
        if f.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        let current = self.active_format_ref();

        f.ty = BufType::VIDEO_CAPTURE;
        f.fmt.pix.width = current.width as u32;
        f.fmt.pix.height = current.height as u32;
        f.fmt.pix.pixelformat = current.pixelformat();
        f.fmt.pix.field = Field::NO_FIELD;
        f.fmt.pix.bytesperline = current.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = current.max_frame_size;
        f.fmt.pix.colorspace = current.colorspace();
        f.fmt.pix.priv_data = 0;
        f.fmt.pix.flags = 0;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FULL_RANGE;
        f.fmt.pix.xfer_func = colorspace::XferFunc::DEFAULT;
        Ok(())
    }

    fn s_fmt(&mut self, f: &mut Format) -> ax_media::Result<()> {
        if f.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        if self.pool.num_buffers() != 0 {
            return Err(V4l2Error::Busy);
        }
        // SAFETY: `f.ty` is VideoCapture, so `pix` is active.
        let pix = unsafe { f.fmt.pix };
        let index = crate::select_format_index(
            &self.formats,
            pix.pixelformat,
            pix.width as u16,
            pix.height as u16,
        )
        .ok_or(V4l2Error::InvalidArgument)?;
        self.set_format(self.formats[index].clone())
            .map_err(|_| V4l2Error::Io)?;

        let current = self.active_format_ref();
        f.fmt.pix.width = current.width as u32;
        f.fmt.pix.height = current.height as u32;
        f.fmt.pix.pixelformat = current.pixelformat();
        f.fmt.pix.field = Field::NO_FIELD;
        f.fmt.pix.bytesperline = current.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = current.max_frame_size;
        f.fmt.pix.colorspace = current.colorspace();
        f.fmt.pix.priv_data = 0;
        f.fmt.pix.flags = 0;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FULL_RANGE;
        f.fmt.pix.xfer_func = colorspace::XferFunc::DEFAULT;
        Ok(())
    }

    fn try_fmt(&self, f: &mut Format) -> ax_media::Result<()> {
        if f.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        // SAFETY: `f.ty` is VideoCapture, so `pix` is active.
        let pix = unsafe { f.fmt.pix };
        let index = crate::select_format_index(
            &self.formats,
            pix.pixelformat,
            pix.width as u16,
            pix.height as u16,
        )
        .ok_or(V4l2Error::InvalidArgument)?;
        let probed = self
            .probe_stream_control(&self.formats[index], None)
            .map_err(|_| V4l2Error::Io)?;
        let format = &self.formats[probed.format_index];
        f.fmt.pix.width = format.width as u32;
        f.fmt.pix.height = format.height as u32;
        f.fmt.pix.pixelformat = format.pixelformat();
        f.fmt.pix.field = Field::NO_FIELD;
        f.fmt.pix.bytesperline = format.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = format
            .max_frame_size
            .max(probed.control.max_video_frame_size);
        f.fmt.pix.colorspace = format.colorspace();
        f.fmt.pix.priv_data = 0;
        f.fmt.pix.flags = 0;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FULL_RANGE;
        f.fmt.pix.xfer_func = colorspace::XferFunc::DEFAULT;
        Ok(())
    }

    fn reqbufs(&mut self, req: &mut buffer::Requestbuffers) -> ax_media::Result<()> {
        if req.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        if req.memory != Memory::MMAP {
            return Err(V4l2Error::InvalidArgument);
        }
        let sizeimage = {
            let bytes = self.active_format_ref().max_frame_size;
            if bytes != 0 { bytes } else { 300 * 1024 }
        };
        let q = &self.pool;
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
        if buf.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        let q = &self.pool;
        let vb = q
            .buffer_snapshot(buf.index)
            .ok_or(V4l2Error::InvalidArgument)?;

        let plane = vb.planes.first().ok_or(V4l2Error::InvalidArgument)?;
        buf.ty = BufType::VIDEO_CAPTURE;
        buf.length = plane.length;
        buf.m.offset = plane.offset as u32;
        buf.memory = Memory::MMAP;
        buf.field = Field::NO_FIELD;
        buf.timecode = Timecode::default();
        buf.reserved2 = 0;
        buf.request_fd = 0;

        let mut flags = buffer::BufFlags::MAPPED | buffer::BufFlags::TIMESTAMP_MONOTONIC;
        match vb.state {
            BufferState::Ready | BufferState::Active => flags |= buffer::BufFlags::QUEUED,
            BufferState::Done => {
                flags |= buffer::BufFlags::DONE;
            }
            BufferState::Error => {
                flags |= buffer::BufFlags::DONE | buffer::BufFlags::ERROR;
            }
            BufferState::Free => {}
        }
        buf.flags = flags;

        if vb.state == BufferState::Done || vb.state == BufferState::Error {
            buf.bytesused = vb.bytesused;
            buf.sequence = vb.sequence;
            buf.timestamp = vb.timestamp.timeval();
        } else {
            buf.bytesused = 0;
            buf.sequence = 0;
            buf.timestamp = Timeval::default();
        }
        Ok(())
    }

    fn qbuf(&mut self, buf: &mut buffer::Buffer) -> ax_media::Result<()> {
        if buf.ty != BufType::VIDEO_CAPTURE || buf.memory != Memory::MMAP {
            return Err(V4l2Error::InvalidArgument);
        }
        self.pool.qbuf(buf.index)?;
        buf.flags = buffer::BufFlags::QUEUED;
        Ok(())
    }

    fn dqbuf(&mut self, buf: &mut buffer::Buffer) -> ax_media::Result<()> {
        if buf.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        let q = &self.pool;
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
        let (bytesused, sequence, timestamp) = (vb.bytesused, vb.sequence, vb.timestamp);

        buf.index = idx;
        buf.flags = buffer::BufFlags::KEYFRAME | timestamp.flags();
        buf.bytesused = bytesused;
        buf.timestamp = timestamp.timeval();
        buf.field = Field::NO_FIELD;
        buf.sequence = sequence;
        buf.memory = Memory::MMAP;
        buf.ty = BufType::VIDEO_CAPTURE;
        buf.length = vb.planes.first().map(|p| p.length).unwrap_or(0);
        buf.m.offset = vb.planes.first().map(|p| p.offset as u32).unwrap_or(0);
        Ok(())
    }

    fn streamon(&mut self, ty: BufType) -> ax_media::Result<()> {
        if ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        self.pool.streamon()?;
        if let Err(e) = self.start_streaming().map_err(|_| V4l2Error::Io) {
            self.pool.streamoff();
            self.close_stream();
            return Err(e);
        }
        Ok(())
    }

    fn streamoff(&mut self, ty: BufType) -> ax_media::Result<()> {
        if ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        self.close_stream();
        self.pool.streamoff();
        Ok(())
    }

    fn g_parm(&self, p: &mut StreamParm) -> ax_media::Result<()> {
        if p.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        p.parm.raw_data = [0; 200];
        // SAFETY: `p.ty` is VideoCapture, so `capture` union field is active.
        let cap = unsafe { &mut p.parm.capture };
        let interval = *self.cur_frame_interval.lock();
        let fract = Fract::from_interval(interval);
        cap.capability = StreamParmCap::TIMEPERFRAME;
        cap.capturemode = StreamParmMode::empty();
        cap.timeperframe = fract;
        cap.extendedmode = 0;
        cap.readbuffers = 0;
        cap.reserved = [0; 4];
        Ok(())
    }

    fn s_parm(&mut self, p: &mut StreamParm) -> ax_media::Result<()> {
        if p.ty != BufType::VIDEO_CAPTURE {
            return Err(V4l2Error::InvalidArgument);
        }
        // 匹配 Linux uvc 驱动行为，若流正在运行，则拒绝 s_parm
        if self.pool.is_streaming() {
            return Err(V4l2Error::Busy);
        }
        // SAFETY: `p.ty` is VideoCapture, so `capture` is active.
        let req = unsafe { p.parm.capture.timeperframe };
        let requested_interval = Fract::new(req.numerator, req.denominator).to_interval();

        let (best_pos, negotiated) = self.find_interval(requested_interval);
        self.probe_format(self.formats[best_pos].clone(), Some(negotiated))
            .map_err(|_| V4l2Error::Io)?;
        let result_fract = Fract::from_interval(*self.cur_frame_interval.lock());

        p.parm.raw_data = [0; 200];
        // SAFETY: `p.ty` is VideoCapture, so `capture` is active.
        let cap = unsafe { &mut p.parm.capture };
        cap.capability = StreamParmCap::TIMEPERFRAME;
        cap.capturemode = StreamParmMode::empty();
        cap.timeperframe = result_fract;
        cap.extendedmode = 0;
        cap.readbuffers = 0;
        cap.reserved = [0; 4];
        Ok(())
    }

    fn log_status(&self) -> ax_media::Result<()> {
        info!(
            "[UVC] log_status streaming={} buffers={} fmt={}x{} pf={:08x}",
            self.pool.is_streaming(),
            self.pool.num_buffers(),
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
        input.ty = ax_media::interface::inout::InputType::CAMERA;
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
        self.ctrls.lock().subscribe_event(fh, sub)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use crab_usb::{
        err::USBError,
        usb_if::{endpoint::TransferRequest, host::ControlSetup},
    };

    use super::*;

    struct FakeHandle {
        calls: Arc<Mutex<Vec<(bool, u8)>>>,
        fail_vs: Arc<AtomicBool>,
    }

    impl UvcHandle for FakeHandle {
        fn claim_interface(&self, interface: u8, _: u8) -> Result<(), USBError> {
            self.calls.lock().unwrap().push((true, interface));
            if interface == 3 && self.fail_vs.load(Ordering::Relaxed) {
                Err(USBError::NotSupported)
            } else {
                Ok(())
            }
        }

        fn release_interface(&self, interface: u8) -> Result<(), USBError> {
            self.calls.lock().unwrap().push((false, interface));
            Ok(())
        }

        fn control_in(&self, _: ControlSetup, _: &mut [u8]) -> Result<usize, USBError> {
            Err(USBError::NotSupported)
        }

        fn control_out(&self, _: ControlSetup, _: &[u8]) -> Result<(), USBError> {
            Err(USBError::NotSupported)
        }

        fn submit_endpoint_transfer(
            &self,
            _: u8,
            _: TransferRequest,
        ) -> Result<crate::IsoPending, USBError> {
            Err(USBError::NotSupported)
        }
    }

    #[test]
    fn failed_vs_claim_releases_vc_before_next_attempt() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fail_vs = Arc::new(AtomicBool::new(true));
        let handle = FakeHandle {
            calls: calls.clone(),
            fail_vs: fail_vs.clone(),
        };

        assert!(matches!(
            claim_video_interfaces(&handle, 0, 3),
            Err(V4l2Error::Busy)
        ));
        assert_eq!(*calls.lock().unwrap(), [(true, 0), (true, 3), (false, 0)]);

        fail_vs.store(false, Ordering::Relaxed);
        assert!(claim_video_interfaces(&handle, 0, 3).is_ok());
        assert_eq!(
            *calls.lock().unwrap(),
            [(true, 0), (true, 3), (false, 0), (true, 0), (true, 3),]
        );
    }
}

#![no_std]
#[cfg(test)]
extern crate std;

#[macro_use]
extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{future::Future, pin::Pin, sync::atomic::Ordering};

use anyhow::anyhow;
use ax_media::{
    CtrlHandler,
    interface::{colorspace, format},
    videobuffer::{VbMemOps, VbPool},
};
use ax_sync::Mutex;
use crab_usb::{
    err::USBError,
    usb_if::{
        endpoint::TransferRequest,
        host::ControlSetup,
        transfer::{Recipient, RequestType},
    },
};
use log::*;
pub use stream::IsoPending;
use uvc_if::stream_control::{StreamControl, stream_control_len};

use crate::{
    frame::FrameParser,
    helper::parse_uvc_device,
    stream::{FrameAssembler, ISO_BATCH, ISO_DEPTH, IsoStop, IsoStream},
};

pub(crate) mod controls;
pub(crate) mod descriptors;
pub(crate) use descriptors::*;
pub(crate) mod frame;
pub(crate) mod helper;
pub(crate) mod stream;
pub(crate) mod v4l2_impl;

/// USB device handle for control and ISO transfers.
pub trait UvcHandle: Send + Sync + 'static {
    fn claim_interface(&self, interface: u8, alternate: u8) -> Result<(), USBError>;

    fn release_interface(&self, interface: u8) -> Result<(), USBError>;

    fn control_in(&self, param: ControlSetup, data: &mut [u8]) -> Result<usize, USBError>;

    fn control_out(&self, param: ControlSetup, data: &[u8]) -> Result<(), USBError>;

    fn submit_endpoint_transfer(
        &self,
        endpoint: u8,
        request: TransferRequest,
    ) -> Result<IsoPending, USBError>;
}

/// Runtime capability supplied by the operating system adapter.
pub trait UvcRuntime: Send + Sync + 'static {
    fn spawn(
        &self,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Result<Box<dyn UvcWorker>, USBError>;
}

/// Join handle for one UVC stream worker.
pub trait UvcWorker: Send {
    fn join(self: Box<Self>);
}

impl<F: FnOnce() + Send> UvcWorker for F {
    fn join(self: Box<Self>) {
        (*self)();
    }
}

/// UVC frame interval description – strongly typed over the raw
/// `bFrameIntervalType` byte. `Continuous` corresponds to `bFrameIntervalType==0`
/// (min/max/step), `Discrete` to `bFrameIntervalType>0` (explicit list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameIntervals {
    Discrete(Vec<u32>),
    Continuous { min: u32, max: u32, step: u32 },
}

#[derive(Debug, Clone)]
pub(crate) struct VideoFormat {
    pub format_type: VideoFormatType,
    pub width: u16,
    pub height: u16,
    pub format_index: u8,
    pub frame_index: u8,
    pub default_interval: u32,
    pub intervals: FrameIntervals,
    pub max_frame_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum VideoFormatType {
    Uncompressed(UncompressedFormat),
    Mjpeg,
}

impl VideoFormat {
    /// Bytes per line.
    pub(crate) fn bytes_per_line(&self) -> usize {
        match self.format_type {
            VideoFormatType::Uncompressed(t) => {
                let pixel_size = match t {
                    UncompressedFormat::Yuyv | UncompressedFormat::Uyvy => 2,
                    UncompressedFormat::Nv12 => 1,
                    UncompressedFormat::Grey => 1,
                    UncompressedFormat::Bgr24 => 3,
                    UncompressedFormat::Xbgr32 => 4,
                };
                (self.width as usize) * pixel_size
            }
            VideoFormatType::Mjpeg => 0,
        }
    }

    /// V4L2 colorspace.
    pub(crate) fn colorspace(&self) -> colorspace::Colorspace {
        if self.is_compressed() {
            colorspace::Colorspace::JPEG
        } else {
            colorspace::Colorspace::SRGB
        }
    }

    /// Format description
    pub(crate) fn description(&self) -> String {
        match self.format_type {
            VideoFormatType::Uncompressed(t) => match t {
                UncompressedFormat::Yuyv => "YUYV 4:2:2".into(),
                UncompressedFormat::Uyvy => "UYVY 4:2:2".into(),
                UncompressedFormat::Nv12 => "Y/UV 4:2:0".into(),
                UncompressedFormat::Grey => "8-bit Greyscale".into(),
                UncompressedFormat::Bgr24 => "24-bit BGR 8-8-8".into(),
                UncompressedFormat::Xbgr32 => "32-bit BGRX 8-8-8-8".into(),
            },
            VideoFormatType::Mjpeg => "Motion-JPEG".into(),
        }
    }

    /// V4L2 pixel format.
    pub(crate) fn pixelformat(&self) -> u32 {
        match self.format_type {
            VideoFormatType::Uncompressed(t) => match t {
                UncompressedFormat::Yuyv => format::PIX_FMT_YUYV,
                UncompressedFormat::Uyvy => format::PIX_FMT_UYVY,
                UncompressedFormat::Nv12 => format::PIX_FMT_NV12,
                UncompressedFormat::Grey => format::PIX_FMT_GREY,
                UncompressedFormat::Bgr24 => format::PIX_FMT_BGR24,
                UncompressedFormat::Xbgr32 => format::PIX_FMT_XBGR32,
            },
            VideoFormatType::Mjpeg => format::PIX_FMT_MJPEG,
        }
    }

    /// Whether the format is compressed.
    pub(crate) fn is_compressed(&self) -> bool {
        matches!(self.format_type, VideoFormatType::Mjpeg)
    }

    /// Whether `self` shares the same image parameters as `other`.
    pub(crate) fn is_same_image(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.pixelformat() == other.pixelformat()
    }

    /// Frame rate in frames per second.
    pub(crate) fn frame_rate(&self) -> u32 {
        let interval = if self.default_interval != 0 {
            self.default_interval
        } else {
            match &self.intervals {
                FrameIntervals::Discrete(v) if !v.is_empty() => v[0],
                FrameIntervals::Continuous { min, .. } => *min,
                _ => 0,
            }
        };
        if interval == 0 {
            0
        } else {
            DescriptorParser::interval_to_fps(interval)
        }
    }
}

/// Uncompressed format type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UncompressedFormat {
    Yuyv,
    Uyvy,
    Nv12,
    Grey,
    Bgr24,
    Xbgr32,
}

impl UncompressedFormat {
    /// GUID to format.
    pub(crate) fn from_guid(guid: &[u8; 16]) -> Option<Self> {
        match guid {
            g if g == &crate::descriptors::format_guids::YUY2 => Some(Self::Yuyv),
            g if g == &crate::descriptors::format_guids::NV12 => Some(Self::Nv12),
            g if g == &crate::descriptors::format_guids::GREY => Some(Self::Grey),
            g if g == &crate::descriptors::format_guids::BGR24 => Some(Self::Bgr24),
            g if g == &crate::descriptors::format_guids::XBGR32 => Some(Self::Xbgr32),
            g if g == &crate::descriptors::format_guids::UYVY => Some(Self::Uyvy),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn guid(self) -> &'static [u8; 16] {
        match self {
            Self::Yuyv => &crate::descriptors::format_guids::YUY2,
            Self::Nv12 => &crate::descriptors::format_guids::NV12,
            Self::Grey => &crate::descriptors::format_guids::GREY,
            Self::Bgr24 => &crate::descriptors::format_guids::BGR24,
            Self::Xbgr32 => &crate::descriptors::format_guids::XBGR32,
            Self::Uyvy => &crate::descriptors::format_guids::UYVY,
        }
    }
}

/// Alternate setting.
#[derive(Debug, Clone)]
pub(crate) struct AlternateSetting {
    pub alt_setting: u8,
    pub ep: u8,
    pub mps: u16,
    pub packets_per_uframe: usize,
    pub interval: u8,
}

impl AlternateSetting {
    pub(crate) fn buf_len(&self) -> usize {
        self.mps as usize * self.packets_per_uframe
    }
}

pub(crate) struct IsoStreamWorker {
    task: Box<dyn UvcWorker>,
    stop: Arc<IsoStop>,
}

pub(crate) struct NegotiatedFormat {
    pub control: StreamControl,
    pub format_index: usize,
    pub alt_index: usize,
}

pub struct UvcDevice<H: UvcHandle, M: VbMemOps + 'static> {
    handle: Arc<H>,
    runtime: Arc<dyn UvcRuntime>,
    vs_iface_num: u8,
    vc_iface_num: u8,
    stream_control_len: usize,
    formats: Vec<VideoFormat>,
    alt_settings: Vec<AlternateSetting>,
    active_format: usize,
    active_alt_setting: usize,
    probed_control: Option<StreamControl>,
    vc_units: controls::VcUnits,
    pub(crate) ctrls: Arc<Mutex<CtrlHandler>>,
    pub(crate) pool: Arc<VbPool<M>>,
    stream: Mutex<Option<IsoStreamWorker>>,
    events: Arc<Mutex<Vec<ax_media::interface::event::Event>>>,
    pub(crate) cur_frame_interval: Mutex<u32>,
    pub(crate) bus_info: [u8; 32],
}

impl<H: UvcHandle, M: VbMemOps + 'static> UvcDevice<H, M> {
    pub fn check(blob: &[u8]) -> bool {
        parse_uvc_device(blob).is_ok()
    }

    /// Create UVC device.
    pub fn new(
        handle: H,
        allocator: M,
        runtime: Arc<dyn UvcRuntime>,
        monotonic_nanos: fn() -> u64,
        descriptor_blob: &[u8],
        bus_info: &str,
    ) -> Result<Self, USBError> {
        let parsed = parse_uvc_device(descriptor_blob).inspect_err(|err| {
            warn!("[UVC] Failed to parse UVC descriptor blob: {err:?}");
        })?;

        let initial_interval = parsed
            .formats
            .first()
            .map(|f| {
                if f.default_interval != 0 {
                    f.default_interval
                } else {
                    match &f.intervals {
                        FrameIntervals::Discrete(v) if !v.is_empty() => v[0],
                        FrameIntervals::Continuous { min, .. } => *min,
                        _ => 333_333u32,
                    }
                }
            })
            .unwrap_or(333_333);
        let mut bus_info_bytes = [0; 32];
        let copy_len = bus_info.len().min(bus_info_bytes.len() - 1);
        bus_info_bytes[..copy_len].copy_from_slice(&bus_info.as_bytes()[..copy_len]);
        let device = Self {
            handle: Arc::new(handle),
            runtime,
            vs_iface_num: parsed.vs_iface_num,
            vc_iface_num: parsed.vc_iface_num,
            stream_control_len: stream_control_len(parsed.uvc_version),
            ctrls: Arc::new(Mutex::new(ax_media::CtrlHandler::new())),
            vc_units: parsed.vc_units.clone(),
            formats: parsed.formats,
            alt_settings: parsed.alt_settings,
            active_format: 0,
            active_alt_setting: 0,
            probed_control: None,
            pool: Arc::new(VbPool::new(allocator, 2, 8, monotonic_nanos)),
            stream: Mutex::new(None),
            events: Arc::new(Mutex::new(Vec::new())),
            cur_frame_interval: Mutex::new(initial_interval),
            bus_info: bus_info_bytes,
        };

        for fmt in &device.formats {
            info!(
                "Supported format: {:?}, {}x{}, {} fps, format_index={}, frame_index={}",
                fmt.format_type,
                fmt.width,
                fmt.height,
                fmt.frame_rate(),
                fmt.format_index,
                fmt.frame_index
            );
        }
        info!(
            "[UVC] VC units: camera_terminal={:?} processing_unit={:?}",
            parsed.vc_units.camera_terminal_id, parsed.vc_units.processing_unit_id
        );
        let control_claimed = device
            .handle
            .claim_interface(device.vc_iface_num, 0)
            .is_ok();
        device.register_controls(&parsed.vc_units);
        if control_claimed {
            let _ = device.handle.release_interface(device.vc_iface_num);
        }
        info!("[UVC] registered {} controls", device.ctrls.lock().len());
        let ev = Arc::clone(&device.events);
        device
            .ctrls
            .lock()
            .set_change_notify(Box::new(move |event| ev.lock().push(event)));

        Ok(device)
    }

    /// V4L2 event source.
    pub fn event_source(&self) -> Arc<Mutex<Vec<ax_media::interface::event::Event>>> {
        Arc::clone(&self.events)
    }

    pub(crate) fn active_format_ref(&self) -> &VideoFormat {
        &self.formats[self.active_format]
    }

    pub(crate) fn set_format(&mut self, format: VideoFormat) -> Result<(), USBError> {
        self.probe_format(format, None)
    }

    pub(crate) fn probe_format(
        &mut self,
        format: VideoFormat,
        interval: Option<u32>,
    ) -> Result<(), USBError> {
        let negotiated = self.probe_stream_control(&format, interval)?;
        let accepted = negotiated.control;
        let accepted_pos = negotiated.format_index;
        let alt = negotiated.alt_index;
        if self.pool.num_buffers() != 0 {
            let allocated = self
                .pool
                .buffer_snapshot(0)
                .and_then(|buffer| buffer.planes.first().map(|plane| plane.length))
                .ok_or(USBError::InvalidParameter)?;
            if accepted.max_video_frame_size > allocated {
                return Err(USBError::InvalidParameter);
            }
        }
        info!(
            "[UVC] PROBE: accepted_format={} interval={} max_frame={} max_payload={} alt={}",
            accepted_pos,
            accepted.frame_interval,
            accepted.max_video_frame_size,
            accepted.max_payload_transfer_size,
            alt
        );
        self.active_format = accepted_pos;
        if accepted.max_video_frame_size != 0 {
            self.formats[accepted_pos].max_frame_size = self.formats[accepted_pos]
                .max_frame_size
                .max(accepted.max_video_frame_size);
        }
        self.active_alt_setting = alt;
        *self.cur_frame_interval.lock() = accepted.frame_interval;
        self.probed_control = Some(accepted);
        Ok(())
    }

    /// Negotiate a format without changing the driver's active format.
    pub(crate) fn probe_stream_control(
        &self,
        format: &VideoFormat,
        interval: Option<u32>,
    ) -> Result<NegotiatedFormat, USBError> {
        let mut requested = self.build_stream_control(format);
        if let Some(interval) = interval {
            requested.frame_interval = interval;
        }
        self.send_vs_control(VideoStreamingControl::Probe as u8, &requested)?;
        let response =
            self.get_vs_control(VideoStreamingControl::Probe as u8, self.stream_control_len)?;
        let accepted = StreamControl::parse(&response)?;
        let accepted_pos = accepted_format_index(&self.formats, format, &accepted)
            .ok_or(USBError::InvalidParameter)?;
        if accepted.frame_interval == 0 {
            return Err(USBError::InvalidParameter);
        }
        let payload = accepted.max_payload_transfer_size as usize;
        let alt = self.select_alt_index(payload);
        if self.alt_settings[alt].buf_len() < payload {
            return Err(USBError::InvalidParameter);
        }
        Ok(NegotiatedFormat {
            control: accepted,
            format_index: accepted_pos,
            alt_index: alt,
        })
    }

    pub(crate) fn start_streaming(&mut self) -> Result<(), USBError> {
        if self.probed_control.is_none() {
            let format = self.active_format_ref().clone();
            let interval = *self.cur_frame_interval.lock();
            self.probe_format(format, Some(interval))?;
        }
        let control = self.probed_control.ok_or(USBError::NotInitialized)?;
        self.send_vs_control(VideoStreamingControl::Commit as u8, &control)?;
        let best = self.alt_settings[self.active_alt_setting].clone();
        log::info!(
            "[UVC] Selected alt={} ep=0x{:02x} mps={} mult={} bInterval={}",
            best.alt_setting,
            best.ep,
            best.mps,
            best.packets_per_uframe,
            best.interval,
        );
        self.handle
            .claim_interface(self.vs_iface_num, best.alt_setting)
            .map_err(|e| {
                anyhow!(
                    "Failed to claim interface {} alt {}: {:?}",
                    self.vs_iface_num,
                    best.alt_setting,
                    e
                )
            })?;

        let packet_len = best.buf_len();
        info!(
            "[UVC] start_streaming: iso worker ep={:#x} batch={} packet_len={} depth={} buf={}",
            best.ep,
            ISO_BATCH,
            packet_len,
            ISO_DEPTH,
            packet_len * ISO_BATCH * ISO_DEPTH
        );
        // Initial submissions are part of STREAMON; report failure synchronously.
        let stop = Arc::new(IsoStop::new());
        let mut iso = IsoStream::new(
            self.handle.clone(),
            self.vs_iface_num,
            stop.clone(),
            best.ep,
            packet_len,
            ISO_BATCH,
            ISO_DEPTH,
        )?;
        let worker =
            {
                let pool = self.pool.clone();
                let stop = stop.clone();
                let fmt = self.active_format_ref();
                let expected = if fmt.is_compressed() {
                    None
                } else {
                    Some(fmt.max_frame_size as usize)
                };
                self.runtime.spawn(Box::pin(async move {
                    let mut assembler =
                        FrameAssembler::new(FrameParser::new(), pool.acquire(), expected);
                    loop {
                        if stop.cancel.load(Ordering::Acquire) {
                            iso.cancel_all();
                            break;
                        }
                        let res =
                            core::future::poll_fn(|cx| iso.poll_next(cx, &mut assembler)).await;
                        match res {
                            Ok(()) => {
                                if stop.cancel.load(Ordering::Acquire) {
                                    iso.cancel_all();
                                    break;
                                }
                            }
                            Err(err) => {
                                iso.cancel_all();
                                if stop.cancel.load(Ordering::Acquire)
                                    || matches!(err, USBError::TransferError(
                                            crab_usb::usb_if::err::TransferError::Cancelled
                                            | crab_usb::usb_if::err::TransferError::EndpointRevoked
                                        ))
                                {
                                    break;
                                }
                                error!("[UVC] stream: iso batch failed err={err:?}");
                                pool.set_error();
                                break;
                            }
                        }
                    }
                    // Drop of IsoStream waits for controller retirement before freeing slots.
                }))?
            };
        *self.stream.lock() = Some(IsoStreamWorker { task: worker, stop });
        info!("[UVC] start_streaming: iso worker armed");
        Ok(())
    }

    pub(crate) fn close_stream(&self) {
        if let Some(worker) = self.stream.lock().take() {
            {
                let _gate = worker.stop.submit_gate.lock();
                worker.stop.cancel.store(true, Ordering::Release);
                // Serialize endpoint retirement with the worker's next submission.
                if !worker.stop.quiesced.load(Ordering::Acquire) {
                    match self.handle.claim_interface(self.vs_iface_num, 0) {
                        Ok(()) => worker.stop.quiesced.store(true, Ordering::Release),
                        Err(error) => error!("[UVC] failed to stop ISO endpoint: {error:?}"),
                    }
                }
            }
            worker.task.join();
        }
    }

    fn send_vs_control(
        &self,
        control_selector: u8,
        stream_ctrl: &StreamControl,
    ) -> Result<(), USBError> {
        let vs_interface_num = self.vs_iface_num;

        let data = stream_ctrl.to_bytes()?;
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::SetCur.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        debug!(
            "Sending VS control: selector=0x{:02x}, data_len={}",
            control_selector,
            data.len()
        );

        self.handle
            .control_out(setup, &data)
            .map_err(|e| anyhow!("Failed to send VS control: {:?}", e))?;

        Ok(())
    }

    fn get_vs_control(&self, control_selector: u8, length: usize) -> Result<Vec<u8>, USBError> {
        let vs_interface_num = self.vs_iface_num;

        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetCur.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        let mut buffer = vec![0u8; length];
        let actual = self
            .handle
            .control_in(setup, &mut buffer)
            .map_err(|e| anyhow!("Failed to get VS control: {:?}", e))?;
        if actual != length {
            return Err(USBError::InvalidParameter);
        }

        debug!(
            "Received VS control response: selector=0x{:02x}, data_len={}",
            control_selector,
            buffer.len()
        );

        Ok(buffer)
    }

    /// Build stream control for an already selected descriptor frame.
    fn build_stream_control(&self, format: &VideoFormat) -> StreamControl {
        let frame_interval = if format.default_interval != 0 {
            format.default_interval
        } else {
            match &format.intervals {
                FrameIntervals::Discrete(v) if !v.is_empty() => v[0],
                FrameIntervals::Continuous { min, .. } => *min,
                _ => 333_333,
            }
        };

        StreamControl {
            hint: 0x0001,
            format_index: format.format_index,
            frame_index: format.frame_index,
            frame_interval,
            key_frame_rate: 0,
            p_frame_rate: 0,
            comp_quality: 0,
            comp_window_size: 0,
            delay: 0,
            max_video_frame_size: format.max_frame_size,
            max_payload_transfer_size: 0,
            extension: [0; 22],
            wire_len: self.stream_control_len,
        }
    }

    fn select_alt_index(&self, payload: usize) -> usize {
        if self.alt_settings.is_empty() {
            return 0;
        }
        let mut best_index = 0;
        for (index, alt) in self.alt_settings.iter().enumerate() {
            let total = alt.buf_len();
            if total >= payload {
                return index;
            }
            let best_total = self.alt_settings[best_index].buf_len();
            if total > best_total {
                best_index = index;
            }
        }
        best_index
    }

    /// Find the best `(format_index, interval)` for `requested` `dwFrameInterval`.
    pub(crate) fn find_interval(&self, requested: u32) -> (usize, u32) {
        let active_pos = self.active_format;
        let active_fmt = &self.formats[active_pos];

        let mut best = (active_pos, uvc_try_frame_interval(active_fmt, requested));
        for (pos, fmt) in self.formats.iter().enumerate() {
            // 保证图像格式不变
            if pos == active_pos || !fmt.is_same_image(active_fmt) {
                continue;
            }
            let cand = uvc_try_frame_interval(fmt, requested);
            if cand.abs_diff(requested) < best.1.abs_diff(requested) {
                best = (pos, cand);
            }
        }
        best
    }
}

/// Match Linux UVC's non-overlapping image area metric for TRY_FMT/S_FMT.
fn select_format_index(
    formats: &[VideoFormat],
    pixel_format: u32,
    width: u16,
    height: u16,
) -> Option<usize> {
    let selected = formats
        .iter()
        .find(|format| format.pixelformat() == pixel_format)
        .or_else(|| formats.first())?;
    let requested_area = u64::from(width) * u64::from(height);
    formats
        .iter()
        .enumerate()
        .filter(|(_, format)| {
            format.pixelformat() == selected.pixelformat()
                && format.format_index == selected.format_index
        })
        .min_by_key(|(_, format)| {
            let frame_width = u64::from(format.width);
            let frame_height = u64::from(format.height);
            let overlap = frame_width.min(u64::from(width)) * frame_height.min(u64::from(height));
            frame_width * frame_height + requested_area - 2 * overlap
        })
        .map(|(index, _)| index)
}

/// Keep the requested frame when a camera returns unknown PROBE indices.
fn accepted_format_index(
    formats: &[VideoFormat],
    requested: &VideoFormat,
    control: &StreamControl,
) -> Option<usize> {
    let requested_index = formats.iter().position(|format| {
        format.format_index == requested.format_index && format.frame_index == requested.frame_index
    })?;
    formats
        .iter()
        .position(|format| {
            format.format_index == control.format_index && format.frame_index == control.frame_index
        })
        .or_else(|| {
            warn!(
                "[UVC] camera returned unknown PROBE format/frame {}/{}; keeping requested {}/{}",
                control.format_index,
                control.frame_index,
                requested.format_index,
                requested.frame_index
            );
            Some(requested_index)
        })
}

impl<H: UvcHandle, M: VbMemOps + 'static> Drop for UvcDevice<H, M> {
    fn drop(&mut self) {
        self.close_stream();
        self.pool.streamoff();
        let _ = self.handle.release_interface(self.vs_iface_num);
        let _ = self.handle.release_interface(self.vc_iface_num);
    }
}

/// Find closest frame interval for the given `VideoFormat`, matching Linux
/// `uvc_try_frame_interval` in `drivers/media/usb/uvc/uvc_v4l2.c:198`.
pub(crate) fn uvc_try_frame_interval(format: &VideoFormat, interval: u32) -> u32 {
    match &format.intervals {
        FrameIntervals::Discrete(intervals) => {
            if intervals.is_empty() {
                if format.default_interval != 0 {
                    return format.default_interval;
                }
                return interval;
            }
            // Discrete intervals: pick the one with minimal distance.
            // Linux does early-break assuming sorted list; we do full scan for robustness
            // while preserving the "last wins on tie / sorted" behaviour via linear scan.
            let mut best = intervals[0];
            let mut best_dist = interval.abs_diff(best);
            for &cand in intervals.iter().skip(1) {
                let dist = interval.abs_diff(cand);
                // Linux breaks when dist > best (sorted), we replicate "pick closest,
                // break on increase" by keeping the first minimal distance encountered
                // with early exit optimisation for sorted data.
                if dist > best_dist && cand > best && interval >= best {
                    break;
                }
                if dist < best_dist {
                    best_dist = dist;
                    best = cand;
                }
            }
            best
        }
        FrameIntervals::Continuous { min, max, step } => {
            let min = *min;
            let max = *max;
            let step = *step;
            if step == 0 || max < min {
                return min;
            }
            if interval <= min {
                return min;
            }
            if interval >= max {
                return max;
            }
            // Round to nearest step, matching Linux's `min + (interval-min+step/2)/step*step`.
            let rounded = u64::from(min)
                + (u64::from(interval - min) + u64::from(step / 2)) / u64::from(step)
                    * u64::from(step);
            rounded.min(u64::from(max)) as u32
        }
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn non_exact_size_selects_closest_frame_of_requested_pixel_format() {
        let formats = [(160, 120), (640, 480), (1280, 720)]
            .into_iter()
            .enumerate()
            .map(|(index, (width, height))| VideoFormat {
                format_type: VideoFormatType::Mjpeg,
                width,
                height,
                format_index: 1,
                frame_index: index as u8 + 1,
                default_interval: 333_333,
                intervals: FrameIntervals::Discrete(Vec::new()),
                max_frame_size: width as u32 * height as u32,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            select_format_index(&formats, format::PIX_FMT_MJPEG, 600, 450),
            Some(1)
        );
    }

    #[test]
    fn same_pixel_format_does_not_cross_format_descriptors() {
        let formats = [
            VideoFormat {
                format_type: VideoFormatType::Mjpeg,
                width: 160,
                height: 120,
                format_index: 1,
                frame_index: 1,
                default_interval: 333_333,
                intervals: FrameIntervals::Discrete(Vec::new()),
                max_frame_size: 160 * 120,
            },
            VideoFormat {
                format_type: VideoFormatType::Mjpeg,
                width: 640,
                height: 480,
                format_index: 2,
                frame_index: 1,
                default_interval: 333_333,
                intervals: FrameIntervals::Discrete(Vec::new()),
                max_frame_size: 640 * 480,
            },
        ];
        assert_eq!(
            select_format_index(&formats, format::PIX_FMT_MJPEG, 640, 480),
            Some(0)
        );
    }

    #[test]
    fn unknown_probe_indices_keep_requested_frame() {
        let format = VideoFormat {
            format_type: VideoFormatType::Mjpeg,
            width: 640,
            height: 480,
            format_index: 1,
            frame_index: 2,
            default_interval: 333_333,
            intervals: FrameIntervals::Discrete(Vec::new()),
            max_frame_size: 640 * 480,
        };
        let formats = [
            VideoFormat {
                frame_index: 1,
                ..format.clone()
            },
            format.clone(),
        ];
        let mut control = StreamControl {
            hint: 1,
            format_index: 99,
            frame_index: 99,
            frame_interval: 333_333,
            key_frame_rate: 0,
            p_frame_rate: 0,
            comp_quality: 0,
            comp_window_size: 0,
            delay: 0,
            max_video_frame_size: 640 * 480,
            max_payload_transfer_size: 1024,
            extension: [0; 22],
            wire_len: 26,
        };
        assert_eq!(accepted_format_index(&formats, &format, &control), Some(1));
        control.format_index = 1;
        control.frame_index = 2;
        assert_eq!(accepted_format_index(&formats, &format, &control), Some(1));
    }
}

#![no_std]
#[cfg(test)]
extern crate std;

#[macro_use]
extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use ax_media::{
    CtrlHandler,
    interface::{Fract, colorspace, format},
    videobuffer::{Vb2Queue, VirtualAllocator},
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

use crate::{
    frame::FrameParser,
    helper::{parse_stream_control, parse_uvc_device},
    stream::{FrameAssembler, ISO_BATCH, ISO_DEPTH, IsoStream},
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

#[derive(Debug, Clone)]
pub(crate) struct VideoFormat {
    pub format_type: VideoFormatType,
    pub width: u16,
    pub height: u16,
    pub frame_rate: u32,
    pub format_index: u8,
    pub frame_index: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum VideoFormatType {
    Uncompressed(UncompressedFormat),
    Mjpeg,
    H264,
}

impl From<u32> for VideoFormatType {
    fn from(value: u32) -> Self {
        match value {
            format::PIX_FMT_YUYV => VideoFormatType::Uncompressed(UncompressedFormat::Yuyv),
            format::PIX_FMT_UYVY => VideoFormatType::Uncompressed(UncompressedFormat::Uyvy),
            format::PIX_FMT_NV12 => VideoFormatType::Uncompressed(UncompressedFormat::Nv12),
            format::PIX_FMT_GREY => VideoFormatType::Uncompressed(UncompressedFormat::Grey),
            format::PIX_FMT_BGR24 => VideoFormatType::Uncompressed(UncompressedFormat::Bgr24),
            format::PIX_FMT_XBGR32 => VideoFormatType::Uncompressed(UncompressedFormat::Xbgr32),
            format::PIX_FMT_MJPEG => VideoFormatType::Mjpeg,
            format::PIX_FMT_H264 => VideoFormatType::H264,
            _ => VideoFormatType::Uncompressed(UncompressedFormat::Yuyv),
        }
    }
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
            VideoFormatType::Mjpeg | VideoFormatType::H264 => 0,
        }
    }

    /// V4L2 colorspace.
    pub(crate) fn colorspace(&self) -> colorspace::Colorspace {
        if self.is_compressed() {
            colorspace::Colorspace::Jpeg
        } else {
            colorspace::Colorspace::Srgb
        }
    }

    pub(crate) fn frame_bytes(&self) -> usize {
        match self.format_type {
            VideoFormatType::Uncompressed(_) => self.bytes_per_line() * (self.height as usize),
            VideoFormatType::Mjpeg => ((self.width as usize) * (self.height as usize) * 3) / 10,
            VideoFormatType::H264 => ((self.width as usize) * (self.height as usize) * 3) / 20,
        }
    }

    /// Format description.
    pub(crate) fn description(&self) -> String {
        match self.format_type {
            VideoFormatType::Uncompressed(_t) => "YUYV 4:2:2".into(),
            VideoFormatType::Mjpeg => "Motion-JPEG".into(),
            VideoFormatType::H264 => "H.264".into(),
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
            VideoFormatType::H264 => format::PIX_FMT_H264,
        }
    }

    /// Whether the format is compressed.
    pub(crate) fn is_compressed(&self) -> bool {
        matches!(
            self.format_type,
            VideoFormatType::Mjpeg | VideoFormatType::H264
        )
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

/// UVC device state.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum UvcDeviceState {
    Unconfigured,
    Configured,
    Streaming,
    Error(String),
}

/// Stream control.
#[derive(Debug, Clone)]
pub(crate) struct StreamControl {
    hint: u16,
    format_index: u8,
    frame_index: u8,
    frame_interval: u32,
    key_frame_rate: u16,
    p_frame_rate: u16,
    comp_quality: u16,
    comp_window_size: u16,
    delay: u16,
    max_video_frame_size: u32,
    max_payload_transfer_size: u32,
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
    task: ax_task::AxTaskRef,
    cancel: Arc<AtomicBool>,
    iso: Arc<Mutex<IsoStream>>,
}

pub struct UvcDevice<H: UvcHandle> {
    handle: Arc<H>,
    vs_iface_num: u8,
    vc_iface_num: u8,
    formats: Vec<VideoFormat>,
    alt_settings: Vec<AlternateSetting>,
    active_format: usize,
    active_alt_setting: usize,
    pub(crate) ctrls: CtrlHandler,
    pub(crate) queue: Arc<Vb2Queue<VirtualAllocator>>,
    pub(crate) state: Mutex<UvcDeviceState>,
    stream: Mutex<Option<IsoStreamWorker>>,
    events: Arc<Mutex<Vec<ax_media::interface::event::Event>>>,
    pub(crate) parm_interval: Mutex<Fract>,
}

impl<H: UvcHandle> UvcDevice<H> {
    /// Create UVC device.
    pub fn new(handle: H, descriptor_blob: &[u8]) -> Result<Self, USBError> {
        let parsed = parse_uvc_device(descriptor_blob)?;

        handle
            .claim_interface(parsed.vc_iface_num, 0)
            .map_err(|e| {
                anyhow!(
                    "Failed to claim VC interface {}: {e:?}",
                    parsed.vc_iface_num
                )
            })?;
        handle
            .claim_interface(parsed.vs_iface_num, 0)
            .map_err(|e| {
                anyhow!(
                    "Failed to claim VS interface {}: {e:?}",
                    parsed.vs_iface_num
                )
            })?;

        let initial_fps = parsed
            .formats
            .first()
            .map(|f| if f.frame_rate != 0 { f.frame_rate } else { 30 })
            .unwrap_or(30);
        let mut device = Self {
            handle: Arc::new(handle),
            vs_iface_num: parsed.vs_iface_num,
            vc_iface_num: parsed.vc_iface_num,
            ctrls: ax_media::CtrlHandler::new(),
            formats: parsed.formats,
            alt_settings: parsed.alt_settings,
            active_format: 0,
            active_alt_setting: 0,
            state: Mutex::new(UvcDeviceState::Configured),
            queue: Arc::new(Vb2Queue::new(VirtualAllocator::new(), 2, 8)),
            stream: Mutex::new(None),
            events: Arc::new(Mutex::new(Vec::new())),
            parm_interval: Mutex::new(Fract {
                numerator: 1,
                denominator: initial_fps,
            }),
        };

        for fmt in &device.formats {
            info!(
                "Supported format: {:?}, {}x{}, {} fps, format_index={}, frame_index={}",
                fmt.format_type,
                fmt.width,
                fmt.height,
                fmt.frame_rate,
                fmt.format_index,
                fmt.frame_index
            );
        }
        info!(
            "[UVC] VC units: camera_terminal={:?} processing_unit={:?}",
            parsed.vc_units.camera_terminal_id, parsed.vc_units.processing_unit_id
        );
        device.register_controls(&parsed.vc_units);
        info!("[UVC] registered {} controls", device.ctrls.len());
        let ev = Arc::clone(&device.events);
        device
            .ctrls
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
        debug!("Setting video format: {format:?}");

        let (mut stream_ctrl, pos) = self.build_stream_control(&format)?;

        self.send_vs_control(VideoStreamingControl::Probe as u8, &stream_ctrl)?;

        let probe_response = self.get_vs_control(VideoStreamingControl::Probe as u8, 26)?;
        stream_ctrl = parse_stream_control(&probe_response)?;
        let payload = stream_ctrl.max_payload_transfer_size as usize;
        self.active_alt_setting = self.select_alt_index(payload);
        info!(
            "[UVC] PROBE: fmt_ix={} frm_ix={} interval={} max_frame={} max_payload={} \
             active_alt={}",
            stream_ctrl.format_index,
            stream_ctrl.frame_index,
            stream_ctrl.frame_interval,
            stream_ctrl.max_video_frame_size,
            stream_ctrl.max_payload_transfer_size,
            self.active_alt_setting
        );

        self.send_vs_control(VideoStreamingControl::Commit as u8, &stream_ctrl)?;

        debug!("Video format set successfully");
        self.active_format = pos;
        Ok(())
    }

    pub(crate) fn start_streaming(&mut self) -> Result<(), USBError> {
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

        let slot_len = best.buf_len();
        info!(
            "[UVC] start_streaming: iso worker ep={:#x} batch={} slot_len={} depth={} buf={}",
            best.ep,
            ISO_BATCH,
            slot_len,
            ISO_DEPTH,
            slot_len * ISO_BATCH * ISO_DEPTH
        );
        let iso = alloc::sync::Arc::new(Mutex::new(IsoStream::new(slot_len, ISO_DEPTH)));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = {
            let handle = self.handle.clone();
            let queue = self.queue.clone();
            let endpoint = best.ep;
            let iso = iso.clone();
            let cancel = cancel.clone();
            let fmt = self.active_format_ref();
            let expected = (!fmt.is_compressed()).then_some(fmt.frame_bytes());
            ax_task::spawn_with_name(
                move || {
                    ax_task::future::block_on(async move {
                        let mut parser = FrameParser::new();
                        let mut dest = None;
                        {
                            let mut iso = iso.lock();
                            if let Err(err) = iso.fill(&*handle, endpoint) {
                                error!("[UVC] stream: initial submit err={err:?}");
                                queue.set_error();
                                return;
                            }
                            if iso.in_flight() == 0 {
                                error!("[UVC] stream: iso pipeline has no in-flight batch");
                                queue.set_error();
                                return;
                            }
                        }
                        loop {
                            if cancel.load(Ordering::Acquire) {
                                let _ = iso.lock().cancel_all();
                                break;
                            }
                            let res = core::future::poll_fn(|cx| {
                                let mut assembler =
                                    FrameAssembler::new(&mut parser, &mut dest, expected, &queue);
                                iso.lock().poll_next(cx, &mut assembler)
                            })
                            .await;
                            match res {
                                Ok(()) => {
                                    if cancel.load(Ordering::Acquire) {
                                        let _ = iso.lock().cancel_all();
                                        break;
                                    }
                                    let mut iso = iso.lock();
                                    if let Err(err) = iso.fill(&*handle, endpoint) {
                                        error!("[UVC] stream: submit after complete err={err:?}");
                                        let _ = iso.cancel_all();
                                        queue.set_error();
                                        break;
                                    }
                                    if iso.in_flight() == 0 {
                                        error!("[UVC] stream: pipeline drained");
                                        queue.set_error();
                                        break;
                                    }
                                }
                                Err(err) => {
                                    let _ = iso.lock().cancel_all();
                                    if cancel.load(Ordering::Acquire)
                                        || matches!(
                                            err,
                                            USBError::TransferError(
                                                crab_usb::usb_if::err::TransferError::Cancelled
                                            )
                                        )
                                    {
                                        break;
                                    }
                                    error!("[UVC] stream: iso batch failed err={err:?}");
                                    queue.set_error();
                                    break;
                                }
                            }
                        }
                    });
                },
                alloc::string::String::from("uvc-stream"),
            )
        };
        *self.stream.lock() = Some(IsoStreamWorker {
            task: worker,
            cancel,
            iso,
        });
        info!("[UVC] start_streaming: iso worker armed");
        *self.state.lock() = UvcDeviceState::Streaming;
        Ok(())
    }

    pub(crate) fn close_stream(&self) {
        if let Some(worker) = self.stream.lock().take() {
            worker.cancel.store(true, Ordering::Release);
            let _ = worker.iso.lock().cancel_all();
            worker.task.join();
        }
        let _ = self.handle.claim_interface(self.vs_iface_num, 0);
        *self.state.lock() = UvcDeviceState::Configured;
    }

    fn send_vs_control(
        &mut self,
        control_selector: u8,
        stream_ctrl: &StreamControl,
    ) -> Result<(), USBError> {
        let vs_interface_num = self.vs_iface_num;

        let data = helper::serialize_stream_control(stream_ctrl);
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

    fn get_vs_control(&mut self, control_selector: u8, length: usize) -> Result<Vec<u8>, USBError> {
        let vs_interface_num = self.vs_iface_num;

        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetCur.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        let mut buffer = vec![0u8; length];
        self.handle
            .control_in(setup, &mut buffer)
            .map_err(|e| anyhow!("Failed to get VS control: {:?}", e))?;

        debug!(
            "Received VS control response: selector=0x{:02x}, data_len={}",
            control_selector,
            buffer.len()
        );

        Ok(buffer)
    }

    /// Build stream control.
    fn build_stream_control(
        &self,
        format: &VideoFormat,
    ) -> Result<(StreamControl, usize), USBError> {
        debug!("Building stream control for format: {format:?}");

        let pos = self.find_format_index(format).ok_or_else(|| {
            warn!("Failed to find matching format for: {format:?}");
            anyhow!("No matching format found")
        })?;
        let negotiated = &self.formats[pos];
        let format_index = negotiated.format_index;
        let frame_index = negotiated.frame_index;
        info!(
            "Found format_index={} frame_index={} for format: {format:?} at pos={}",
            format_index, frame_index, pos
        );

        let effective_fps = if format.frame_rate != 0 {
            format.frame_rate
        } else {
            negotiated.frame_rate
        };
        let frame_interval = 10_000_000u32.checked_div(effective_fps).unwrap_or(333333);

        let width = negotiated.width as u32;
        let height = negotiated.height as u32;

        let max_frame_size = match negotiated.format_type {
            VideoFormatType::Mjpeg => width * height * 2,
            VideoFormatType::Uncompressed(fmt) => match fmt {
                UncompressedFormat::Yuyv | UncompressedFormat::Uyvy => width * height * 2,
                UncompressedFormat::Nv12 => width * height * 3 / 2,
                UncompressedFormat::Grey => width * height,
                UncompressedFormat::Bgr24 => width * height * 3,
                UncompressedFormat::Xbgr32 => width * height * 4,
            },
            VideoFormatType::H264 => width * height / 2,
        };

        Ok((
            StreamControl {
                hint: 0x0001,
                format_index,
                frame_index,
                frame_interval,
                key_frame_rate: 0,
                p_frame_rate: 0,
                comp_quality: 0,
                comp_window_size: 0,
                delay: 0,
                max_video_frame_size: max_frame_size,
                max_payload_transfer_size: 0,
            },
            pos,
        ))
    }

    /// Find format index.
    fn find_format_index(&self, target: &VideoFormat) -> Option<usize> {
        for (idx, format) in self.formats.iter().enumerate() {
            if format.format_type != target.format_type {
                continue;
            }

            if let (
                VideoFormatType::Uncompressed(format_type),
                VideoFormatType::Uncompressed(target_type),
            ) = (&format.format_type, &target.format_type)
                && format_type != target_type
            {
                continue;
            }

            if format.width == target.width && format.height == target.height {
                debug!(
                    "Found matching format: pos={} format_index={}, frame_index={}",
                    idx, format.format_index, format.frame_index
                );
                return Some(idx);
            }
        }

        for (idx, format) in self.formats.iter().enumerate() {
            if format.format_type == target.format_type {
                info!(
                    "Using fallback format: pos={} format_index={}, frame_index={}",
                    idx, format.format_index, format.frame_index
                );
                return Some(idx);
            }
        }

        debug!("No matching format found, using default indices");
        None
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
}

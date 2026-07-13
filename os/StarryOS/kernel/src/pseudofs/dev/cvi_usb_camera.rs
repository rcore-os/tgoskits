use alloc::vec::Vec;
use core::{any::Any, time::Duration};

use ax_errno::AxError;
use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_runtime::hal::{mem::virt_to_phys, time::busy_wait};
use ax_sync::Mutex;
use axfs_ng_vfs::{NodeFlags, VfsResult};
use cvi_camera_uapi::{
    CVI_CAMERA_CHROMA_SITING_CENTER, CVI_CAMERA_COLOR_MATRIX_BT601, CVI_CAMERA_COLOR_RANGE_FULL,
    CVI_CAMERA_FORMAT_GRAYSCALE, CVI_CAMERA_FORMAT_YUV420_PLANAR,
    CVI_CAMERA_FORMAT_YUV422_HORIZONTAL_PLANAR, CVI_CAMERA_FORMAT_YUV422_VERTICAL_PLANAR,
    CVI_CAMERA_FORMAT_YUV444_PLANAR, CVI_CAMERA_IOCTL_CAPTURE_SCALED, CVI_CAMERA_IOCTL_DECODE_JPEG,
    CVI_CAMERA_IOCTL_GET_FRAME, CVI_CAMERA_IOCTL_GET_INFO, CVI_CAMERA_IOCTL_GET_YUV_FRAME,
    CVI_CAMERA_IOCTL_INIT, CVI_CAMERA_SCALE_EIGHTH, CVI_CAMERA_SCALE_FULL, CVI_CAMERA_SCALE_HALF,
    CVI_CAMERA_SCALE_QUARTER, CviCameraExtentV1, CviCameraFrameLayoutV1, CviCameraPlaneV1,
    CviCameraRequestV1,
};
use dma_api::DmaError;
use sg200x_bsp::{
    gpio::{Direction, GPIO, GPIO1_BASE},
    pinmux::{FMUX_USB_VBUS_DET, Pinmux},
    soc::{
        CLKGEN_BASE, CV182X_USB2_PHY_BASE, DWC2_BASE, FMUX_BASE, IOBLK_BASE, IOBLK_GRTC_BASE,
        TOP_BASE,
    },
    usb::{
        self,
        class::uvc,
        error::UsbError,
        host::{self, UvcEnumerated, dwc2, dwc2::ep0 as dwc2_ep0},
    },
};
use sg200x_jpu::{
    FrameLayout, FrameLayoutError, JpuCreateError, JpuDecodeError, JpuDecoder, JpuInspectError,
    JpuMmio, JpuPixelFormat, JpuScale, PlaneLayout, inspect_jpeg_layout,
};
use starry_vm::{VmMutPtr, VmPtr, vm_read_slice, vm_write_slice};
use tock_registers::interfaces::Writeable;

use crate::pseudofs::DeviceOps;

const IOBLK_G1_USB_VBUS_DET_OFF: usize = 0x020;

const VBUS_GPIO_PIN: u8 = 6;
const VBUS_GPIO_ACTIVE_HIGH: bool = true;

/// MMIO span of the TOP control block. The PHY ID-pad reset register lives at
/// `TOP_BASE + 0x3000`, so a single 4K page is not enough — map four pages.
const TOP_MMIO_SIZE: usize = 0x4000;
/// MMIO span for the single-page register blocks (CLKGEN, FMUX, IOBLK, GRTC,
/// GPIO, DWC2 controller, USB2 PHY). Each block's registers fit within one 4K
/// page; FMUX/IOBLK share a page so their mappings coincide (idempotent).
const REG_MMIO_SIZE: usize = 0x1000;
const JPU_REG_BASE: usize = 0x0B00_0000;
const VC_REG_BASE: usize = 0x0B03_0000;

/// Map a physical MMIO region into the kernel address space and return its
/// virtual base. Unlike `phys_to_virt`, this works on dynamic platforms where
/// `PHYS_VIRT_OFFSET == 0` and there is no static linear MMIO window — `iomap`
/// installs a real device mapping and is idempotent for already-mapped pages.
fn iomap_usize(paddr: usize, size: usize) -> usize {
    ax_mm::iomap(PhysAddr::from_usize(paddr), size)
        .unwrap_or_else(|err| panic!("failed to iomap MMIO at {paddr:#x}+{size:#x}: {err:?}"))
        .as_usize()
}

const CAMERA_FORMAT_MJPEG: u8 = 1;
const MIN_VALID_JPEG_BYTES: usize = 4096;
const MAX_CAPTURE_TRIES: u32 = 8;
const MAX_SUBMITTED_JPEG_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const NO_UVC_CAMERA: &str = "no UVC camera detected";
/// Default resolution cap (640×480 = 307200 pixels) guiding UVC frame selection.
const DEFAULT_RESOLUTION: u32 = 640 * 480;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CameraInfo {
    pub width: u16,
    pub height: u16,
    /// 1 means MJPEG.
    pub format: u8,
    pub connected: u8,
}

struct UsbCameraSession {
    cam: UvcEnumerated,
    sel: uvc::UvcStreamSelection,
}

#[derive(Default)]
struct UsbCameraState {
    session: Option<UsbCameraSession>,
    jpu: Option<JpuDecoder>,
    jpeg_scratch: Vec<u8>,
}

pub struct CviCamera {
    state: Mutex<UsbCameraState>,
}

fn ep0_dma_virt_to_phys(p: *const u8) -> u32 {
    virt_to_phys(VirtAddr::from(p as usize)).as_usize() as u32
}

fn root_usb_device_connected() -> bool {
    // SAFETY: camera initialization has installed the DWC2 MMIO base, and the
    // camera state mutex serializes this read with all other host operations.
    let hprt0 = unsafe { dwc2::dwc2_hprt0_read() };
    dwc2::hprt_connsts(hprt0)
}

unsafe fn enable_usb_clocks_cv181x() {
    let b = iomap_usize(CLKGEN_BASE, REG_MMIO_SIZE);
    let en1 = (b + 0x004) as *mut u32;
    let en2 = (b + 0x008) as *mut u32;
    let byp0 = (b + 0x030) as *mut u32;
    unsafe {
        let v1_pre = core::ptr::read_volatile(en1);
        let v2_pre = core::ptr::read_volatile(en2);
        let byp_pre = core::ptr::read_volatile(byp0);
        core::ptr::write_volatile(en1, v1_pre | (0xFu32 << 28));
        core::ptr::write_volatile(en2, v2_pre | 1u32);
        core::ptr::write_volatile(byp0, byp_pre & !((1u32 << 17) | (1u32 << 18)));
    }
}

/// PHY ID pad toggle workaround: switch to device mode first, then host mode.
unsafe fn cvitek_usb_top_host_bringup() {
    let top = iomap_usize(TOP_BASE, TOP_MMIO_SIZE);
    let rst = (top + 0x3000) as *mut u32;
    unsafe {
        let v = core::ptr::read_volatile(rst);
        core::ptr::write_volatile(rst, v & !(1 << 11));
        busy_wait(Duration::from_micros(50));
        core::ptr::write_volatile(rst, v | (1 << 11));
        busy_wait(Duration::from_micros(50));

        let usb_pin = (top + 0x48) as *mut u32;
        let x = core::ptr::read_volatile(usb_pin);
        let dev_mode = (x & !0xC0u32) | 0xC0u32 | 0x01u32;
        core::ptr::write_volatile(usb_pin, dev_mode);
        busy_wait(Duration::from_micros(1000));
        let host_mode = (x & !0xC0u32) | 0x40u32 | 0x01u32;
        core::ptr::write_volatile(usb_pin, host_mode);
        busy_wait(Duration::from_micros(1000));

        let eco = (top + 0xB4) as *mut u32;
        core::ptr::write_volatile(eco, core::ptr::read_volatile(eco) | 0x80);
    }
}

fn pinmux_usb_vbus_det_gpio_output_prep() {
    let fmux_vaddr = iomap_usize(FMUX_BASE, REG_MMIO_SIZE);
    let ioblk_vaddr = iomap_usize(IOBLK_BASE, REG_MMIO_SIZE);
    let ioblk_grtc_vaddr = iomap_usize(IOBLK_GRTC_BASE, REG_MMIO_SIZE);
    let pinmux = unsafe { Pinmux::new(fmux_vaddr, ioblk_vaddr, ioblk_grtc_vaddr) };
    pinmux
        .fmux()
        .usb_vbus_det
        .write(FMUX_USB_VBUS_DET::FSEL::XGPIOB_6);
    let r = (ioblk_vaddr + IOBLK_G1_USB_VBUS_DET_OFF) as *mut u32;
    unsafe {
        let v = core::ptr::read_volatile(r);
        core::ptr::write_volatile(r, v | (7 << 5));
    }
}

fn enable_usb_vbus_gpio() {
    let gpio = unsafe { GPIO::new(iomap_usize(GPIO1_BASE, REG_MMIO_SIZE)) };
    gpio.pin(VBUS_GPIO_PIN).set_direction(Direction::Output);
    gpio.pin(VBUS_GPIO_PIN).set(VBUS_GPIO_ACTIVE_HIGH);
}

fn map_usb_init_error(e: UsbError) -> &'static str {
    match e {
        UsbError::NotImplemented => "no VS bulk/isoch video endpoint found",
        _ => "failed to parse UVC stream parameters",
    }
}

fn init_usb_camera() -> Result<UsbCameraSession, &'static str> {
    unsafe {
        enable_usb_clocks_cv181x();
        cvitek_usb_top_host_bringup();
    }
    pinmux_usb_vbus_det_gpio_output_prep();
    enable_usb_vbus_gpio();
    ax_task::sleep(Duration::from_micros(2_000_000));

    usb::set_dwc2_base_virt(iomap_usize(DWC2_BASE, REG_MMIO_SIZE));
    usb::set_cv182x_phy_base_virt(iomap_usize(CV182X_USB2_PHY_BASE, REG_MMIO_SIZE));
    usb::set_usb_dma_to_phys_fn(Some(ep0_dma_virt_to_phys));

    unsafe {
        dwc2::dwc2_probe().map_err(|e| {
            warn!("cvi-camera: DWC2 probe failed: {e:?}");
            "DWC2 probe failed"
        })?;
    }

    let mut last_err = None;
    let extras = (0..4)
        .find_map(|attempt| {
            if attempt > 0 {
                busy_wait(Duration::from_micros(1_500_000 * attempt as u64));
            }
            match host::enumerate_topology_only() {
                Ok(extras) => Some(extras),
                Err(e) => {
                    warn!("cvi-camera: USB enumerate failed #{}: {:?}", attempt + 1, e);
                    last_err = Some(e);
                    None
                }
            }
        })
        .ok_or_else(|| {
            warn!(
                "cvi-camera: USB enumerate retries exhausted: {:?}",
                last_err
            );
            if root_usb_device_connected() {
                "USB topology enumeration failed"
            } else {
                NO_UVC_CAMERA
            }
        })?;

    let cam = extras.uvc.ok_or(NO_UVC_CAMERA)?;
    info!(
        "cvi-camera: UVC addr={} VID={:04x} PID={:04x} ep0_mps={}",
        cam.addr, cam.vid, cam.pid, cam.ep0_mps
    );

    let dev = u32::from(cam.addr);
    let ep0 = cam.ep0_mps;
    let cfg_buf = uvc::read_configuration_descriptor(dev, ep0, 1).map_err(|e| {
        warn!("cvi-camera: read configuration descriptor failed: {e:?}");
        "failed to read configuration descriptor"
    })?;
    let cfg_total = u16::from_le_bytes([cfg_buf[2], cfg_buf[3]]) as usize;
    let cfg = &cfg_buf[..cfg_total.min(cfg_buf.len())];
    uvc::set_preferred_max_pixels(DEFAULT_RESOLUTION);
    let mut sel = uvc::parse_uvc_video_stream(cfg, cfg_total).map_err(|e| {
        warn!("cvi-camera: parse UVC video stream failed: {e:?}");
        map_usb_init_error(e)
    })?;

    if let Some(entities) = uvc::parse_uvc_control_entities(cfg, cfg_total) {
        let tune = uvc::UvcImageTuning {
            brightness: Some(96),
            ..uvc::UvcImageTuning::default()
        };
        let _ = uvc::uvc_init_camera_controls(dev, ep0, &entities, &tune);
    }

    uvc::uvc_start_video_stream(dev, ep0, &mut sel).map_err(|e| {
        warn!("cvi-camera: start UVC stream failed: {e:?}");
        "UVC PROBE/COMMIT or SET_INTERFACE failed"
    })?;
    info!(
        "cvi-camera: stream ready {}x{} payload={} frame_size={}",
        sel.frame_w, sel.frame_h, sel.negotiated_payload_size, sel.negotiated_frame_size
    );

    // Warm-up frame: discard the first capture after stream start so the
    // isochronous pipeline and DMA buffer are ready for real reads.
    let _ = uvc::uvc_capture_one_frame(dev, ep0, &sel);
    Ok(UsbCameraSession { cam, sel })
}

fn capture_frame(session: &UsbCameraSession) -> Result<&'static [u8], &'static str> {
    let dev = u32::from(session.cam.addr);
    let ep0 = session.cam.ep0_mps;
    let mut last_n = 0;
    let mut last_msg = None;
    for attempt in 0..MAX_CAPTURE_TRIES {
        let n = uvc::uvc_capture_one_frame(dev, ep0, &session.sel).map_err(|e| {
            warn!("cvi-camera: capture failed: {e:?}");
            "frame capture failed"
        })?;
        last_n = n;
        let frame = dwc2_ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, n)
            .ok_or("DMA slice out of bounds")?;
        let starts_jpeg = n >= 2 && frame[0] == 0xff && frame[1] == 0xd8;
        let ends_jpeg = n >= 2 && frame[n - 2] == 0xff && frame[n - 1] == 0xd9;
        if starts_jpeg && ends_jpeg && n >= MIN_VALID_JPEG_BYTES {
            return Ok(frame);
        }
        last_msg = Some(if !starts_jpeg {
            "first bytes are not ff d8"
        } else if !ends_jpeg {
            "last bytes are not ff d9 (truncated)"
        } else {
            "frame too small"
        });
        warn!(
            "cvi-camera: invalid frame (try #{}/{}, size={}, {}), reset FID",
            attempt + 1,
            MAX_CAPTURE_TRIES,
            n,
            last_msg.unwrap_or("?")
        );
        uvc::reset_frame_continuity();
    }
    warn!(
        "cvi-camera: no complete JPEG after {} retries, size={} {}",
        MAX_CAPTURE_TRIES,
        last_n,
        last_msg.unwrap_or("?")
    );
    Err("no complete JPEG frame after capture retries")
}

fn jpu_scale_from_uapi(scale: u32) -> VfsResult<JpuScale> {
    match scale {
        CVI_CAMERA_SCALE_FULL => Ok(JpuScale::Full),
        CVI_CAMERA_SCALE_HALF => Ok(JpuScale::Half),
        CVI_CAMERA_SCALE_QUARTER => Ok(JpuScale::Quarter),
        CVI_CAMERA_SCALE_EIGHTH => Ok(JpuScale::Eighth),
        _ => Err(AxError::InvalidInput),
    }
}

fn map_frame_layout_error(_error: FrameLayoutError) -> AxError {
    AxError::OperationNotSupported
}

fn map_jpu_inspect_error(error: JpuInspectError) -> AxError {
    match error {
        JpuInspectError::Layout(error) => map_frame_layout_error(error),
        JpuInspectError::EmptyStream | JpuInspectError::InvalidJpeg(_) => AxError::Io,
    }
}

fn map_jpu_create_error(error: JpuCreateError) -> AxError {
    match error {
        JpuCreateError::AlreadyOwned => AxError::ResourceBusy,
        JpuCreateError::Initialization(_) => AxError::Io,
    }
}

fn map_jpu_decode_error(error: &JpuDecodeError) -> AxError {
    match error {
        JpuDecodeError::Layout(error) => map_frame_layout_error(*error),
        JpuDecodeError::Dma(DmaError::NoMemory) => AxError::NoMemory,
        JpuDecodeError::Dma(_) => AxError::Io,
        JpuDecodeError::Timeout => AxError::TimedOut,
        JpuDecodeError::Poisoned
        | JpuDecodeError::EmptyStream
        | JpuDecodeError::InvalidJpeg(_)
        | JpuDecodeError::BufferInvariant(_)
        | JpuDecodeError::DmaAddress(_)
        | JpuDecodeError::HardwareSetup(_)
        | JpuDecodeError::DecodeFailed => AxError::Io,
    }
}

fn checked_user_range(pointer: u64, length: u64) -> VfsResult<(usize, usize)> {
    let pointer = usize::try_from(pointer).map_err(|_| AxError::InvalidInput)?;
    let length = usize::try_from(length).map_err(|_| AxError::InvalidInput)?;
    pointer.checked_add(length).ok_or(AxError::InvalidInput)?;
    Ok((pointer, length))
}

const fn ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start < b_end && b_start < a_end
}

fn validate_request_buffers_disjoint(
    request_pointer: *mut CviCameraRequestV1,
    request: &CviCameraRequestV1,
) -> VfsResult<()> {
    let request_start = request_pointer as usize;
    let request_len = core::mem::size_of::<CviCameraRequestV1>();
    let request_end = request_start
        .checked_add(request_len)
        .ok_or(AxError::InvalidInput)?;

    for (pointer, length) in [
        (request.jpeg_ptr, request.jpeg_len),
        (request.data_ptr, request.capacity),
    ] {
        if length == 0 {
            continue;
        }
        let (buffer_start, buffer_len) = checked_user_range(pointer, length)?;
        let buffer_end = buffer_start
            .checked_add(buffer_len)
            .ok_or(AxError::InvalidInput)?;
        if ranges_overlap(request_start, request_end, buffer_start, buffer_end) {
            return Err(AxError::InvalidInput);
        }
    }
    Ok(())
}

fn read_user_bytes_into(bytes: &mut Vec<u8>, pointer: u64, length: u64) -> VfsResult<()> {
    let (pointer, length) = checked_user_range(pointer, length)?;
    bytes.clear();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| AxError::NoMemory)?;
    vm_read_slice(
        pointer as *const u8,
        &mut bytes.spare_capacity_mut()[..length],
    )?;
    // SAFETY: `vm_read_slice` initialized every byte in the reserved range.
    unsafe { bytes.set_len(length) };
    Ok(())
}

const fn extent_to_uapi(extent: sg200x_jpu::Extent) -> CviCameraExtentV1 {
    CviCameraExtentV1 {
        width: extent.width,
        height: extent.height,
    }
}

const fn half_ceil(value: u32) -> u32 {
    value / 2 + value % 2
}

fn plane_to_uapi(plane: PlaneLayout, visible: CviCameraExtentV1) -> VfsResult<CviCameraPlaneV1> {
    Ok(CviCameraPlaneV1 {
        offset: u64::try_from(plane.offset).map_err(|_| AxError::Io)?,
        len: u64::try_from(plane.len).map_err(|_| AxError::Io)?,
        stride: plane.stride,
        storage: extent_to_uapi(plane.storage),
        visible,
        reserved: 0,
    })
}

fn frame_layout_to_uapi(layout: FrameLayout) -> VfsResult<CviCameraFrameLayoutV1> {
    let visible = extent_to_uapi(layout.visible);
    let (format, plane_count, chroma_visible) = match layout.format {
        JpuPixelFormat::Yuv420 => (
            CVI_CAMERA_FORMAT_YUV420_PLANAR,
            3,
            CviCameraExtentV1 {
                width: half_ceil(layout.visible.width),
                height: half_ceil(layout.visible.height),
            },
        ),
        JpuPixelFormat::Yuv422Horizontal => (
            CVI_CAMERA_FORMAT_YUV422_HORIZONTAL_PLANAR,
            3,
            CviCameraExtentV1 {
                width: half_ceil(layout.visible.width),
                height: layout.visible.height,
            },
        ),
        JpuPixelFormat::Yuv422Vertical => (
            CVI_CAMERA_FORMAT_YUV422_VERTICAL_PLANAR,
            3,
            CviCameraExtentV1 {
                width: layout.visible.width,
                height: half_ceil(layout.visible.height),
            },
        ),
        JpuPixelFormat::Yuv444 => (CVI_CAMERA_FORMAT_YUV444_PLANAR, 3, visible),
        JpuPixelFormat::Grayscale => (CVI_CAMERA_FORMAT_GRAYSCALE, 1, CviCameraExtentV1::default()),
        _ => return Err(AxError::OperationNotSupported),
    };

    let y = plane_to_uapi(layout.y, visible)?;
    let (cb, cr) = match (layout.cb, layout.cr) {
        (Some(cb), Some(cr)) => (
            plane_to_uapi(cb, chroma_visible)?,
            plane_to_uapi(cr, chroma_visible)?,
        ),
        (None, None) if matches!(layout.format, JpuPixelFormat::Grayscale) => {
            (CviCameraPlaneV1::default(), CviCameraPlaneV1::default())
        }
        _ => return Err(AxError::Io),
    };

    let converted = CviCameraFrameLayoutV1 {
        data_len: u64::try_from(layout.total_len).map_err(|_| AxError::Io)?,
        format,
        plane_count,
        color_range: CVI_CAMERA_COLOR_RANGE_FULL,
        color_matrix: CVI_CAMERA_COLOR_MATRIX_BT601,
        chroma_siting: CVI_CAMERA_CHROMA_SITING_CENTER,
        reserved: 0,
        source: extent_to_uapi(layout.source),
        visible,
        source_aligned: extent_to_uapi(layout.source_aligned),
        coded: extent_to_uapi(layout.coded),
        storage: extent_to_uapi(layout.storage),
        y,
        cb,
        cr,
    };
    converted
        .validate_for_buffer_len(converted.data_len)
        .map_err(|_| AxError::Io)?;
    Ok(converted)
}

impl UsbCameraState {
    fn ensure_initialized(&mut self) -> VfsResult<()> {
        if self.session.is_none() {
            self.session = Some(init_usb_camera().map_err(|msg| {
                warn!("cvi-camera: init failed: {msg}");
                if msg == NO_UVC_CAMERA {
                    AxError::NoSuchDevice
                } else {
                    AxError::Io
                }
            })?);
        }
        Ok(())
    }

    fn info(&mut self) -> VfsResult<CameraInfo> {
        self.ensure_initialized()?;
        let session = self.session.as_ref().ok_or(AxError::NoSuchDevice)?;
        Ok(CameraInfo {
            width: session.sel.frame_w,
            height: session.sel.frame_h,
            format: CAMERA_FORMAT_MJPEG,
            connected: 1,
        })
    }

    fn frame(&mut self) -> VfsResult<&'static [u8]> {
        self.ensure_initialized()?;
        capture_frame(self.session.as_ref().ok_or(AxError::NoSuchDevice)?).map_err(|msg| {
            warn!("cvi-camera: capture failed: {msg}");
            AxError::Io
        })
    }

    fn ensure_jpu(&mut self) -> VfsResult<&mut JpuDecoder> {
        if self.jpu.is_none() {
            let jpu_v = iomap_usize(JPU_REG_BASE, REG_MMIO_SIZE);
            let top_v = iomap_usize(TOP_BASE, TOP_MMIO_SIZE);
            let vc_v = iomap_usize(VC_REG_BASE, REG_MMIO_SIZE);
            let dma = axklib::dma::device_with_mask(u32::MAX as u64);
            let decoder = unsafe {
                JpuDecoder::new(JpuMmio::new(jpu_v, top_v, vc_v), dma).map_err(|e| {
                    warn!("cvi-camera: JPU init failed: {e}");
                    map_jpu_create_error(e)
                })?
            };
            self.jpu = Some(decoder);
        }
        self.jpu.as_mut().ok_or(AxError::Io)
    }

    fn write_yuv_frame(&mut self, destination: *mut u8, scale: JpuScale) -> VfsResult<usize> {
        let jpeg = self.frame()?;
        let jpu = self.ensure_jpu()?;
        let result = jpu.decode_scaled(jpeg, scale).map_err(|e| {
            warn!("cvi-camera: JPU decode failed: {e}");
            AxError::Io
        })?;
        info!(
            "cvi-camera: JPU decode OK scale={:?} visible={}x{} storage={}x{} y_stride={} yuv={} \
             bytes",
            result.layout.scale,
            result.width,
            result.height,
            result.layout.storage.width,
            result.layout.storage.height,
            result.layout.y.stride,
            result.yuv_data.len()
        );
        vm_write_slice(destination, result.yuv_data)?;
        Ok(result.yuv_data.len())
    }

    fn process_v1_jpeg(
        &mut self,
        request_pointer: *mut CviCameraRequestV1,
        mut request: CviCameraRequestV1,
        jpeg: &[u8],
        scale: JpuScale,
    ) -> VfsResult<usize> {
        let output = if request.is_query_only() {
            None
        } else {
            Some(checked_user_range(request.data_ptr, request.capacity)?)
        };

        let inspected = inspect_jpeg_layout(jpeg, scale).map_err(|error| {
            warn!("cvi-camera: JPU layout inspection failed: {error}");
            map_jpu_inspect_error(error)
        })?;
        if inspected.total_len > MAX_OUTPUT_BYTES {
            warn!(
                "cvi-camera: refusing {}-byte JPU output above {}-byte limit",
                inspected.total_len, MAX_OUTPUT_BYTES
            );
            return Err(AxError::OperationNotSupported);
        }

        request.layout = frame_layout_to_uapi(inspected)?;
        if request.is_query_only() {
            request_pointer.vm_write(request)?;
            return Ok(0);
        }

        let (output_pointer, capacity) = output.ok_or(AxError::InvalidInput)?;
        if capacity < inspected.total_len {
            request_pointer.vm_write(request)?;
            return Err(AxError::StorageFull);
        }

        // Check write access and publish the final metadata before starting the
        // hardware operation. A read-only control block must not lead to a
        // successful payload copy followed by EFAULT on the response write.
        request_pointer.vm_write(request)?;
        let jpu = self.ensure_jpu()?;
        let result = jpu.decode_scaled(jpeg, scale).map_err(|error| {
            let mapped = map_jpu_decode_error(&error);
            warn!("cvi-camera: JPU decode failed: {error}");
            mapped
        })?;
        if result.layout != inspected || result.yuv_data.len() != inspected.total_len {
            warn!(
                "cvi-camera: inspected/decode layout mismatch inspected={:?} decoded={:?} bytes={}",
                inspected,
                result.layout,
                result.yuv_data.len()
            );
            return Err(AxError::Io);
        }

        vm_write_slice(output_pointer as *mut u8, result.yuv_data)?;
        Ok(0)
    }

    fn process_v1_submitted_jpeg(
        &mut self,
        request_pointer: *mut CviCameraRequestV1,
        request: CviCameraRequestV1,
        scale: JpuScale,
    ) -> VfsResult<usize> {
        let mut jpeg = core::mem::take(&mut self.jpeg_scratch);
        let result = read_user_bytes_into(&mut jpeg, request.jpeg_ptr, request.jpeg_len)
            .and_then(|_| self.process_v1_jpeg(request_pointer, request, jpeg.as_slice(), scale));
        self.jpeg_scratch = jpeg;
        result
    }
}

impl CviCamera {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(UsbCameraState::default()),
        }
    }
}

impl DeviceOps for CviCamera {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            CVI_CAMERA_IOCTL_INIT => {
                self.state.lock().ensure_initialized()?;
                Ok(0)
            }
            CVI_CAMERA_IOCTL_GET_INFO => {
                let info = self.state.lock().info()?;
                (arg as *mut CameraInfo).vm_write(info)?;
                Ok(0)
            }
            CVI_CAMERA_IOCTL_GET_FRAME => {
                let frame = self.state.lock().frame()?;
                vm_write_slice(arg as *mut u8, frame)?;
                Ok(frame.len())
            }
            CVI_CAMERA_IOCTL_GET_YUV_FRAME => self
                .state
                .lock()
                .write_yuv_frame(arg as *mut u8, JpuScale::Full),
            CVI_CAMERA_IOCTL_CAPTURE_SCALED => {
                let request_pointer = arg as *mut CviCameraRequestV1;
                let request = request_pointer.vm_read()?;
                request
                    .validate_capture()
                    .map_err(|_| AxError::InvalidInput)?;
                validate_request_buffers_disjoint(request_pointer, &request)?;
                let scale = jpu_scale_from_uapi(request.header.scale)?;

                let mut state = self.state.lock();
                let jpeg = state.frame()?;
                state.process_v1_jpeg(request_pointer, request, jpeg, scale)
            }
            CVI_CAMERA_IOCTL_DECODE_JPEG => {
                let request_pointer = arg as *mut CviCameraRequestV1;
                let request = request_pointer.vm_read()?;
                request
                    .validate_decode()
                    .map_err(|_| AxError::InvalidInput)?;
                validate_request_buffers_disjoint(request_pointer, &request)?;
                let scale = jpu_scale_from_uapi(request.header.scale)?;
                let jpeg_len =
                    usize::try_from(request.jpeg_len).map_err(|_| AxError::InvalidInput)?;
                if jpeg_len > MAX_SUBMITTED_JPEG_BYTES {
                    return Err(AxError::InvalidInput);
                }
                self.state
                    .lock()
                    .process_v1_submitted_jpeg(request_pointer, request, scale)
            }
            _ => Err(AxError::NotATty),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_jpu_formats_to_valid_v1_layouts() {
        for (format, expected_format, expected_planes) in [
            (JpuPixelFormat::Yuv420, CVI_CAMERA_FORMAT_YUV420_PLANAR, 3),
            (
                JpuPixelFormat::Yuv422Horizontal,
                CVI_CAMERA_FORMAT_YUV422_HORIZONTAL_PLANAR,
                3,
            ),
            (
                JpuPixelFormat::Yuv422Vertical,
                CVI_CAMERA_FORMAT_YUV422_VERTICAL_PLANAR,
                3,
            ),
            (JpuPixelFormat::Yuv444, CVI_CAMERA_FORMAT_YUV444_PLANAR, 3),
            (JpuPixelFormat::Grayscale, CVI_CAMERA_FORMAT_GRAYSCALE, 1),
        ] {
            let layout = FrameLayout::new(641, 481, format, JpuScale::Full)
                .expect("test dimensions produce a valid JPU layout");
            let converted =
                frame_layout_to_uapi(layout).expect("JPU layout maps to the frozen UAPI");

            assert_eq!(converted.format, expected_format);
            assert_eq!(converted.plane_count, expected_planes);
            assert_eq!(
                converted.visible,
                CviCameraExtentV1 {
                    width: 641,
                    height: 481
                }
            );
            assert_eq!(converted.data_len, layout.total_len as u64);
            assert_eq!(
                converted.validate_for_buffer_len(converted.data_len),
                Ok(())
            );
        }
    }

    #[test]
    fn maps_scaled_yuv420_visible_chroma_with_ceil_division() {
        let layout = FrameLayout::new(1279, 1706, JpuPixelFormat::Yuv420, JpuScale::Half)
            .expect("known SG2002 test dimensions are supported");
        let converted = frame_layout_to_uapi(layout).expect("scaled layout maps to UAPI");

        assert_eq!(
            converted.visible,
            CviCameraExtentV1 {
                width: 640,
                height: 853
            }
        );
        assert_eq!(
            converted.storage,
            CviCameraExtentV1 {
                width: 640,
                height: 856
            }
        );
        assert_eq!(
            converted.cb.visible,
            CviCameraExtentV1 {
                width: 320,
                height: 427
            }
        );
        assert_eq!(converted.cr.visible, converted.cb.visible);
    }

    #[test]
    fn rejects_user_ranges_that_overflow_usize() {
        assert_eq!(checked_user_range(u64::MAX, 1), Err(AxError::InvalidInput));
    }

    #[test]
    fn rejects_request_overlap_with_input_or_output_buffers() {
        let request_pointer = 0x1000usize as *mut CviCameraRequestV1;
        let mut request =
            CviCameraRequestV1::new_decode(0x2000, 128, CVI_CAMERA_SCALE_HALF, 0x3000, 4096);
        assert_eq!(
            validate_request_buffers_disjoint(request_pointer, &request),
            Ok(())
        );

        request.data_ptr = 0x1080;
        assert_eq!(
            validate_request_buffers_disjoint(request_pointer, &request),
            Err(AxError::InvalidInput)
        );

        request.data_ptr = 0x3000;
        request.jpeg_ptr = 0x0ff0;
        assert_eq!(
            validate_request_buffers_disjoint(request_pointer, &request),
            Err(AxError::InvalidInput)
        );
    }
}

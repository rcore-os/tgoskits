use core::{any::Any, time::Duration};

use ax_config::plat::PHYS_VIRT_OFFSET;
use ax_errno::AxError;
use ax_kspin::SpinNoIrq as Mutex;
use ax_memory_addr::{PhysAddr, VirtAddr};
use ax_runtime::hal::{
    mem::{phys_to_virt, virt_to_phys},
    time::busy_wait,
};
use axfs_ng_vfs::{NodeFlags, VfsResult};
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
use starry_vm::{VmMutPtr, vm_write_slice};
use tock_registers::interfaces::Writeable;

use crate::pseudofs::DeviceOps;

const IOBLK_G1_USB_VBUS_DET_OFF: usize = 0x020;

const VBUS_GPIO_PIN: u8 = 6;
const VBUS_GPIO_ACTIVE_HIGH: bool = true;

const CAMERA_FORMAT_MJPEG: u8 = 1;
const MIN_VALID_JPEG_BYTES: usize = 4096;
const MAX_CAPTURE_TRIES: u32 = 8;

pub const CVI_CAMERA_IOCTL_INIT: u32 = 1;
pub const CVI_CAMERA_IOCTL_GET_INFO: u32 = 2;
pub const CVI_CAMERA_IOCTL_GET_FRAME: u32 = 3;

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
}

pub struct CviCamera {
    state: Mutex<UsbCameraState>,
}

fn ep0_dma_virt_to_phys(p: *const u8) -> u32 {
    virt_to_phys(VirtAddr::from(p as usize)).as_usize() as u32
}

unsafe fn enable_usb_clocks_cv181x() {
    let b = phys_to_virt(PhysAddr::from_usize(CLKGEN_BASE)).as_usize();
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
    let top = phys_to_virt(PhysAddr::from_usize(TOP_BASE)).as_usize();
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
    let pinmux = unsafe {
        Pinmux::new(
            FMUX_BASE + PHYS_VIRT_OFFSET,
            IOBLK_BASE + PHYS_VIRT_OFFSET,
            IOBLK_GRTC_BASE + PHYS_VIRT_OFFSET,
        )
    };
    pinmux
        .fmux()
        .usb_vbus_det
        .write(FMUX_USB_VBUS_DET::FSEL::XGPIOB_6);
    let iob = phys_to_virt(PhysAddr::from_usize(IOBLK_BASE)).as_usize();
    let r = (iob + IOBLK_G1_USB_VBUS_DET_OFF) as *mut u32;
    unsafe {
        let v = core::ptr::read_volatile(r);
        core::ptr::write_volatile(r, v | (7 << 5));
    }
}

fn enable_usb_vbus_gpio() {
    let gpio = unsafe { GPIO::new(GPIO1_BASE + PHYS_VIRT_OFFSET) };
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

    usb::set_dwc2_base_virt(DWC2_BASE + PHYS_VIRT_OFFSET);
    usb::set_cv182x_phy_base_virt(CV182X_USB2_PHY_BASE + PHYS_VIRT_OFFSET);
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
            "USB topology enumeration failed"
        })?;

    let cam = extras.uvc.ok_or("no UVC camera detected")?;
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
    dwc2_ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, last_n).ok_or("DMA slice out of bounds")
}

impl UsbCameraState {
    fn ensure_initialized(&mut self) -> VfsResult<()> {
        if self.session.is_none() {
            self.session = Some(init_usb_camera().map_err(|msg| {
                warn!("cvi-camera: init failed: {msg}");
                AxError::Io
            })?);
        }
        Ok(())
    }

    fn info(&mut self) -> VfsResult<CameraInfo> {
        self.ensure_initialized()?;
        let session = self.session.as_ref().ok_or(AxError::BadState)?;
        Ok(CameraInfo {
            width: session.sel.frame_w,
            height: session.sel.frame_h,
            format: CAMERA_FORMAT_MJPEG,
            connected: 1,
        })
    }

    fn frame(&mut self) -> VfsResult<&'static [u8]> {
        self.ensure_initialized()?;
        capture_frame(self.session.as_ref().ok_or(AxError::BadState)?).map_err(|msg| {
            warn!("cvi-camera: capture failed: {msg}");
            AxError::Io
        })
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
            _ => Err(AxError::InvalidInput),
        }
    }
}

use alloc::{boxed::Box, string::String};
use core::ptr::NonNull;

use rdif_display::{DisplayError as RdifDisplayError, Interface};

use crate::{DisplayDevice, DisplayError, DisplayInfo, PixelFormat};

pub struct RdifDisplayDevice {
    name: String,
    device: Box<dyn Interface>,
    fb_base_vaddr: NonNull<u8>,
}

unsafe impl Send for RdifDisplayDevice {}

impl RdifDisplayDevice {
    pub fn new(mut device: Box<dyn Interface>) -> Result<Self, DisplayError> {
        let name = device.name().into();
        let fb_base_vaddr = {
            let mut framebuffer = device.framebuffer().map_err(map_display_error)?;
            NonNull::new(framebuffer.as_mut_slice().as_mut_ptr())
                .ok_or(DisplayError::InvalidFramebuffer)?
        };
        Ok(Self {
            name,
            device,
            fb_base_vaddr,
        })
    }

    pub fn from_interface(device: impl Interface + 'static) -> Result<Self, DisplayError> {
        Self::new(Box::new(device))
    }
}

impl DisplayDevice for RdifDisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> DisplayInfo {
        let info = self.device.info();
        DisplayInfo {
            width: info.width,
            height: info.height,
            fb_base_vaddr: self.fb_base_vaddr.as_ptr() as usize,
            fb_size: info.fb_size,
            stride: info.stride,
            format: info.format.into(),
        }
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        if self.device.need_flush() {
            self.device.flush().map_err(map_display_error)?;
        }
        Ok(())
    }
}

impl From<rdif_display::PixelFormat> for PixelFormat {
    fn from(value: rdif_display::PixelFormat) -> Self {
        match value {
            rdif_display::PixelFormat::Rgb565 => Self::Rgb565,
            rdif_display::PixelFormat::Rgb888 => Self::Rgb888,
            rdif_display::PixelFormat::Xrgb8888 => Self::Xrgb8888,
            rdif_display::PixelFormat::Argb8888 => Self::Argb8888,
            rdif_display::PixelFormat::Bgr888 => Self::Bgr888,
            rdif_display::PixelFormat::Xbgr8888 => Self::Xbgr8888,
        }
    }
}

fn map_display_error(error: RdifDisplayError) -> DisplayError {
    match error {
        RdifDisplayError::NotSupported => DisplayError::NotSupported,
        RdifDisplayError::NotAvailable => DisplayError::NotAvailable,
        RdifDisplayError::InvalidFramebuffer => DisplayError::InvalidFramebuffer,
        RdifDisplayError::Other(_) => DisplayError::BadState,
    }
}

#[cfg(feature = "ax-std")]
use std::os::arceos::api::display as api;

use embedded_graphics::{
    draw_target::DrawTarget,
    pixelcolor::Rgb888,
    prelude::{OriginDimensions, RgbColor, Size},
};

pub struct Display {
    size: Size,
    fb: &'static mut [u8],
}

impl Display {
    pub fn new() -> Self {
        #[cfg(feature = "ax-std")]
        let info = api::ax_framebuffer_info();
        #[cfg(feature = "ax-std")]
        let fb =
            unsafe { core::slice::from_raw_parts_mut(info.fb_base_vaddr as *mut u8, info.fb_size) };
        #[cfg(feature = "ax-std")]
        let size = Size::new(info.width, info.height);

        #[cfg(not(feature = "ax-std"))]
        let size = Size::new(640, 480);
        #[cfg(not(feature = "ax-std"))]
        let fb = std::vec![0; size.width as usize * size.height as usize * 4].leak();

        Self { size, fb }
    }

    pub fn flush(&self) {
        #[cfg(feature = "ax-std")]
        api::ax_framebuffer_flush();
    }
}

impl OriginDimensions for Display {
    fn size(&self) -> Size {
        self.size
    }
}

impl DrawTarget for Display {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        pixels.into_iter().for_each(|px| {
            let idx = (px.0.y * self.size.width as i32 + px.0.x) as usize * 4;
            if idx + 2 >= self.fb.len() {
                return;
            }
            self.fb[idx] = px.1.b();
            self.fb[idx + 1] = px.1.g();
            self.fb[idx + 2] = px.1.r();
        });
        Ok(())
    }
}

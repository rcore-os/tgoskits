use ax_std::os::arceos::api::display as api;
use embedded_graphics::{
    draw_target::DrawTarget,
    pixelcolor::Rgb888,
    prelude::{OriginDimensions, RgbColor, Size},
};

pub struct Display {
    size: Size,
}

impl Display {
    pub fn new() -> Self {
        let info = api::ax_framebuffer_info().expect("display output is required");
        let size = Size::new(info.width, info.height);
        Self { size }
    }

    pub fn flush(&self) {
        api::ax_framebuffer_flush().expect("failed to flush framebuffer");
    }
}

impl OriginDimensions for Display {
    fn size(&self) -> Size {
        self.size
    }
}

impl DrawTarget for Display {
    type Color = Rgb888;
    type Error = api::AxDisplayError;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        let mut pixels = pixels.into_iter();
        // SAFETY: this standalone ArceOS display test owns the framebuffer;
        // it creates no userspace mapping or concurrent writer, and all GPU
        // commands complete before the next drawing call.
        unsafe {
            api::ax_with_framebuffer(&mut |bytes, info| {
                for pixel in pixels.by_ref() {
                    if pixel.0.x < 0 || pixel.0.y < 0 {
                        continue;
                    }
                    let x = pixel.0.x as usize;
                    let y = pixel.0.y as usize;
                    if x >= info.width as usize || y >= info.height as usize {
                        continue;
                    }
                    let Some(index) = x
                        .checked_mul(4)
                        .and_then(|column| y.checked_mul(info.stride)?.checked_add(column))
                    else {
                        continue;
                    };
                    if index.checked_add(3).is_none_or(|last| last >= bytes.len()) {
                        continue;
                    }
                    bytes[index] = pixel.1.b();
                    bytes[index + 1] = pixel.1.g();
                    bytes[index + 2] = pixel.1.r();
                }
            })
        }
    }
}

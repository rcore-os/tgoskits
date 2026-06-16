#![no_std]

extern crate alloc;

mod error;
mod interface;
mod types;

pub use error::*;
pub use interface::*;
pub use rdif_base::{DriverGeneric, KError, io};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDisplay {
        fb: [u8; 16],
    }

    impl DriverGeneric for TestDisplay {
        fn name(&self) -> &str {
            "test-display"
        }
    }

    impl Interface for TestDisplay {
        fn info(&self) -> DisplayInfo {
            DisplayInfo {
                width: 2,
                height: 2,
                stride: 8,
                format: PixelFormat::Xrgb8888,
                fb_size: self.fb.len(),
            }
        }

        fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
            Ok(FrameBuffer::from_slice(&mut self.fb))
        }
    }

    #[test]
    fn display_interface_exposes_layout_and_framebuffer() {
        let mut display = TestDisplay { fb: [0; 16] };
        let info = display.info();
        assert_eq!(info.stride, 8);
        assert_eq!(info.format, PixelFormat::Xrgb8888);
        assert_eq!(info.fb_size, 16);

        let mut fb = display.framebuffer().unwrap();
        fb.as_mut_slice()[0] = 0xaa;
        assert_eq!(fb.as_slice()[0], 0xaa);
        assert_eq!(display.handle_irq(), Event::none());
    }
}

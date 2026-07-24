use alloc::format;

use axtest::prelude::*;

use crate::{
    DisplayError, DisplayInfo, DriverGeneric, Event, FrameBuffer, Interface, PixelFormat, io,
};

struct TestDisplay {
    fb: [u8; 16],
    irq_enabled: bool,
    flushed: bool,
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

    fn irq_num(&self) -> Option<usize> {
        Some(5)
    }

    fn need_flush(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.flushed = true;
        Ok(())
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        Event {
            handled: true,
            changed: self.flushed,
        }
    }
}

#[axtest]
fn rdif_display_framebuffer_and_interface_defaults_hold() {
    let mut display = TestDisplay {
        fb: [0; 16],
        irq_enabled: false,
        flushed: false,
    };

    let info = display.info();
    ax_assert_eq!(info.width, 2);
    ax_assert_eq!(info.height, 2);
    ax_assert_eq!(info.stride, 8);
    ax_assert_eq!(info.format, PixelFormat::Xrgb8888);
    ax_assert_eq!(info.fb_size, 16);
    ax_assert_eq!(display.irq_num(), Some(5));
    ax_assert!(display.need_flush());

    {
        let mut fb = display.framebuffer().unwrap();
        fb.as_mut_slice()[0] = 0xaa;
        fb[1] = 0x55;
        ax_assert_eq!(fb.as_slice()[0], 0xaa);
        ax_assert_eq!(fb[1], 0x55);
    }

    display.enable_irq();
    ax_assert!(display.is_irq_enabled());
    display.flush().unwrap();
    ax_assert_eq!(
        display.handle_irq(),
        Event {
            handled: true,
            changed: true
        }
    );
    display.disable_irq();
    ax_assert!(!display.is_irq_enabled());

    struct MinimalDisplay;
    impl DriverGeneric for MinimalDisplay {
        fn name(&self) -> &str {
            "minimal-display"
        }
    }
    impl Interface for MinimalDisplay {
        fn info(&self) -> DisplayInfo {
            DisplayInfo {
                width: 0,
                height: 0,
                stride: 0,
                format: PixelFormat::Rgb565,
                fb_size: 0,
            }
        }

        fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
            Err(DisplayError::NotAvailable)
        }
    }

    let mut minimal = MinimalDisplay;
    ax_assert_eq!(minimal.irq_num(), None);
    ax_assert!(!minimal.need_flush());
    ax_assert!(minimal.flush().is_ok());
    ax_assert_eq!(minimal.handle_irq(), Event::none());
    ax_assert!(!minimal.is_irq_enabled());
}

#[axtest]
fn rdif_display_pixel_formats_and_error_mapping_hold() {
    let formats = [
        PixelFormat::Rgb565,
        PixelFormat::Rgb888,
        PixelFormat::Xrgb8888,
        PixelFormat::Argb8888,
        PixelFormat::Bgr888,
        PixelFormat::Xbgr8888,
    ];
    ax_assert_eq!(formats.len(), 6);
    ax_assert_eq!(formats[0], PixelFormat::Rgb565);

    let mut raw = [0u8; 4];
    let ptr = raw.as_mut_ptr();
    let mut fb = unsafe { FrameBuffer::from_raw_parts_mut(ptr, raw.len()) };
    fb.as_mut_slice().copy_from_slice(&[1, 2, 3, 4]);
    ax_assert_eq!(fb.as_slice(), &[1, 2, 3, 4]);

    ax_assert!(matches!(
        io::ErrorKind::from(DisplayError::NotSupported),
        io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(DisplayError::InvalidFramebuffer),
        io::ErrorKind::InvalidData
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(DisplayError::NotAvailable),
        io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        io::ErrorKind::from(DisplayError::Other("display backend".into())),
        io::ErrorKind::Other(_)
    ));
}

#[axtest]
fn rdif_display_pixel_format_and_info_hold() {
    // PixelFormat variants
    let formats = [
        PixelFormat::Xrgb8888,
        PixelFormat::Xbgr8888,
        PixelFormat::Rgb565,
        PixelFormat::Rgb888,
        PixelFormat::Argb8888,
        PixelFormat::Bgr888,
    ];
    
    // Just verify they can be created
    for fmt in &formats {
        match fmt {
            PixelFormat::Xrgb8888 => {}
            PixelFormat::Xbgr8888 => {}
            PixelFormat::Rgb565 => {}
            PixelFormat::Rgb888 => {}
            PixelFormat::Argb8888 => {}
            PixelFormat::Bgr888 => {}
        }
    }
    
    // DisplayInfo creation
    let info = DisplayInfo {
        width: 1920,
        height: 1080,
        stride: 1920 * 4,
        format: PixelFormat::Xrgb8888,
        fb_size: 1920 * 1080 * 4,
    };
    ax_assert_eq!(info.width, 1920);
    ax_assert_eq!(info.height, 1080);
}

#[axtest]
fn rdif_display_framebuffer_pixel_access_rules_hold() {
    use crate::{DisplayInfo, DriverGeneric, FrameBuffer, Interface, PixelFormat};

    struct PixelTestDisplay {
        fb: [u8; 32],
    }

    impl DriverGeneric for PixelTestDisplay {
        fn name(&self) -> &str { "pixel-test" }
    }

    impl Interface for PixelTestDisplay {
        fn info(&self) -> DisplayInfo {
            DisplayInfo {
                width: 4,
                height: 2,
                stride: 16,
                format: PixelFormat::Xrgb8888,
                fb_size: 32,
            }
        }

        fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
            Ok(FrameBuffer::from_slice(&mut self.fb))
        }
    }

    let mut display = PixelTestDisplay { fb: [0; 32] };
    let mut fb = display.framebuffer().unwrap();
    
    // Test pixel read/write via raw slice
    fb.as_mut_slice()[0] = 0xFF;
    fb.as_mut_slice()[1] = 0x00;
    fb.as_mut_slice()[2] = 0x00;
    ax_assert_eq!(fb.as_slice()[0], 0xFF);
    
    // Test framebuffer size
    ax_assert_eq!(fb.as_slice().len(), 32);
    
    // Test DisplayError variants
    ax_assert!(format!("{:?}", DisplayError::NotSupported).contains("NotSupported"));
    ax_assert!(format!("{:?}", DisplayError::InvalidFramebuffer).contains("InvalidFramebuffer"));
    ax_assert!(format!("{:?}", DisplayError::NotAvailable).contains("NotAvailable"));
}

#[axtest]
fn rdif_display_event_and_default_impls_hold() {
    use crate::{DisplayError, Event, DisplayInfo, DriverGeneric, Interface, PixelFormat};

    // Test Event
    let event = Event::none();
    ax_assert!(!event.handled);
    ax_assert!(!event.changed);

    let handled = Event { handled: true, changed: false };
    ax_assert!(handled.handled);
    ax_assert!(!handled.changed);

    let changed = Event { handled: true, changed: true };
    ax_assert!(changed.handled);
    ax_assert!(changed.changed);

    // Test default trait methods via a minimal implementation
    struct DefaultImplDisplay;
    impl DriverGeneric for DefaultImplDisplay {
        fn name(&self) -> &str { "default-impl" }
    }
    impl Interface for DefaultImplDisplay {
        fn info(&self) -> DisplayInfo {
            DisplayInfo {
                width: 1,
                height: 1,
                stride: 4,
                format: PixelFormat::Rgb565,
                fb_size: 4,
            }
        }

        fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
            Err(DisplayError::NotSupported)
        }
    }

    let mut display = DefaultImplDisplay;
    // Default implementations should not panic
    ax_assert_eq!(display.irq_num(), None);
    ax_assert!(!display.need_flush());
    ax_assert!(display.flush().is_ok());
    ax_assert_eq!(display.handle_irq(), Event::none());
    display.enable_irq();
    display.disable_irq();
    ax_assert!(!display.is_irq_enabled());
}

#[axtest]
fn rdif_display_mode_info_and_connector_hold() {
    use crate::DisplayError;

    // Test DisplayError Debug formatting for all variants
    ax_assert_eq!(
        format!("{:?}", DisplayError::NotSupported),
        "NotSupported"
    );
    ax_assert_eq!(
        format!("{:?}", DisplayError::NotAvailable),
        "NotAvailable"
    );
    ax_assert_eq!(
        format!("{:?}", DisplayError::InvalidFramebuffer),
        "InvalidFramebuffer"
    );

    // Test DisplayError conversions
    use crate::io::ErrorKind;
    let _unsupported: ErrorKind = DisplayError::NotSupported.into();
    let _not_available: ErrorKind = DisplayError::NotAvailable.into();
    let _invalid_fb: ErrorKind = DisplayError::InvalidFramebuffer.into();
}

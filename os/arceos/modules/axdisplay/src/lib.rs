//! [ArceOS](https://github.com/arceos-org/arceos) display module.
//!
//! Currently only supports direct writing to the framebuffer.

#![no_std]

#[macro_use]
extern crate log;

#[doc(no_inline)]
pub use axdriver::prelude::DisplayInfo;
use axdriver::{AxDeviceContainer, prelude::*};
use axsync::Mutex;
use lazyinit::LazyInit;

static MAIN_DISPLAY: LazyInit<Mutex<AxDisplayDevice>> = LazyInit::new();

/// Initializes the display subsystem by underlayer devices.
pub fn init_display(mut display_devs: AxDeviceContainer<AxDisplayDevice>) {
    info!("Initialize display subsystem...");

    if let Some(dev) = display_devs.take_one() {
        info!("  use display device 0: {:?}", dev.device_name());
        MAIN_DISPLAY.init_once(Mutex::new(dev));
    } else {
        warn!("  No display device found!");
    }
}

/// Checks if there is a display device.
pub fn has_display() -> bool {
    MAIN_DISPLAY.is_inited()
}

/// Gets the framebuffer information.
pub fn framebuffer_info() -> DisplayInfo {
    MAIN_DISPLAY.lock().info()
}

/// Flushes the framebuffer, i.e. show on the screen.
pub fn framebuffer_flush() -> bool {
    MAIN_DISPLAY.lock().flush().is_ok()
}

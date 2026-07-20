//! [ArceOS](https://github.com/arceos-org/arceos) display facade.
//!
//! The runtime publishes immutable framebuffer metadata here after the final
//! CPU-pinned display owner has initialized the controller. Driver state and
//! IRQ endpoints never enter this crate; flushes cross a typed service owned by
//! that runtime domain.

#![no_std]

extern crate alloc;

mod device;
mod types;

use ax_lazyinit::LazyInit;
pub use device::{DisplayError, DisplayFacade, DisplayFlushService, DisplayResult};
pub use types::{DisplayInfo, PixelFormat};

static MAIN_DISPLAY: LazyInit<DisplayFacade> = LazyInit::new();

/// Publishes the first fully activated display facade.
pub fn init_display(display: Option<DisplayFacade>) {
    log::info!("Initialize display subsystem...");
    if let Some(display) = display {
        log::info!("  use display device 0: {}", display.name());
        MAIN_DISPLAY.init_once(display);
    } else {
        log::warn!("  No display device found!");
    }
}

/// Checks if a fully activated display is available.
pub fn has_display() -> bool {
    MAIN_DISPLAY.is_inited()
}

/// Gets immutable framebuffer information captured during activation.
pub fn framebuffer_info() -> DisplayInfo {
    MAIN_DISPLAY.info()
}

/// Submits one flush and waits for owner-thread completion.
pub fn framebuffer_flush() -> bool {
    MAIN_DISPLAY.flush().is_ok()
}

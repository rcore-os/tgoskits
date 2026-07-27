//! [ArceOS](https://github.com/arceos-org/arceos) display module.
//!
//! Currently only supports direct writing to the framebuffer.

#![no_std]

extern crate alloc;

mod device;
pub mod rdif;
mod types;

use ax_lazyinit::LazyInit;
use ax_sync::SpinMutex;
pub use device::{DisplayDevice, DisplayError, DisplayResult, ErasedDisplayDevice};
pub use types::{DisplayInfo, PixelFormat};

static MAIN_DISPLAY: LazyInit<SpinMutex<ErasedDisplayDevice>> = LazyInit::new();

/// Initializes the display subsystem by underlayer devices.
pub fn init_display(display_devs: impl IntoIterator<Item = ErasedDisplayDevice>) {
    log::info!("Initialize display subsystem...");

    if let Some(dev) = display_devs.into_iter().next() {
        log::info!("  use display device 0: {}", dev.name());
        MAIN_DISPLAY.init_once(SpinMutex::new(dev));
    } else {
        log::warn!("  No display device found!");
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

/// Returns the resolved main display IRQ, if the runtime provided one.
pub fn framebuffer_irq_id() -> Option<irq_framework::IrqId> {
    MAIN_DISPLAY.lock().irq_id()
}

/// Enables IRQ handling in the main display driver.
pub fn framebuffer_enable_irq() {
    MAIN_DISPLAY.lock().enable_irq();
}

/// Disables IRQ handling in the main display driver.
pub fn framebuffer_disable_irq() {
    MAIN_DISPLAY.lock().disable_irq();
}

/// Acknowledges the main display IRQ source.
pub fn framebuffer_handle_irq() -> bool {
    let mut display = MAIN_DISPLAY.lock();
    display.is_irq_enabled() && display.handle_irq()
}

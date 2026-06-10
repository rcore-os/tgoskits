//! ArceOS timing glue (feature `arceos`).
//!
//! Implements [`crate::runtime::SdhciDelay`] on top of `ax-task` and installs it
//! into the driver core. The rest of the crate has no ArceOS dependency.

use core::time::Duration;

use crate::runtime::SdhciDelay;

/// ArceOS-backed delay/yield provider.
pub struct ArceosDelay;

impl SdhciDelay for ArceosDelay {
    fn delay_ms(&self, ms: u64) {
        ax_task::sleep(Duration::from_millis(ms));
    }

    fn yield_now(&self) {
        ax_task::yield_now();
    }
}

static ARCEOS_DELAY: ArceosDelay = ArceosDelay;

/// Installs the ArceOS timing provider. Call once during init.
pub fn install_delay() {
    crate::runtime::set_delay(&ARCEOS_DELAY);
}

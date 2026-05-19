//! Legacy leaf-driver traits re-exported for platform glue that still needs
//! concrete driver cores.

pub use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
#[cfg(feature = "block")]
pub use ax_driver_block::BlockDriverOps;
#[cfg(feature = "display")]
pub use ax_driver_display::DisplayDriverOps;
#[cfg(feature = "input")]
pub use ax_driver_input::InputDriverOps;
#[cfg(feature = "net")]
pub use ax_driver_net::NetDriverOps;
#[cfg(feature = "vsock")]
pub use ax_driver_vsock::VsockDriverOps;

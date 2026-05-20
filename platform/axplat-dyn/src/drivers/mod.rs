use ax_errno::AxError;

mod pci;
#[cfg(feature = "rknpu")]
mod rknpu;
mod soc;
#[cfg(feature = "rtc")]
mod time;

pub mod blk;
#[cfg(feature = "usb")]
pub mod usb;

pub use blk::take_block_devices;

pub fn probe_all_devices() -> Result<(), AxError> {
    rdrive::probe_all(false).map_err(|_| AxError::BadState)
}

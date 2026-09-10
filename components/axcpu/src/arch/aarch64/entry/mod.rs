//! Assembly entry points for the current AArch64 execution mode.

#[cfg(feature = "virtualization")]
mod guest;
#[cfg(feature = "virtualization")]
pub use guest::{enter_guest, guest_vector};

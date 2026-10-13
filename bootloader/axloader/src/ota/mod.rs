//! A/B OTA state, EFI system partition access, and runtime control.

mod state;

#[cfg(any(target_os = "uefi", target_arch = "loongarch64", test))]
pub use state::newest;
pub use state::{Outcome, Slot, State, StateError};

#[cfg(target_os = "uefi")]
mod disk;
#[cfg(target_os = "uefi")]
mod runtime;

#[cfg(target_os = "uefi")]
pub use disk::{InactiveWriter, MAX_IMAGE_BYTES, OtaDisk, load_slot};
#[cfg(target_os = "uefi")]
pub use runtime::OtaController;

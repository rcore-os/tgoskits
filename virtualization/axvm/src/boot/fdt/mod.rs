//! Guest device-tree artifact and selected architecture compatibility facade.

use alloc::vec::Vec;

mod interrupts;

pub use interrupts::project_guest_physical_timer_interrupts;

pub use crate::arch::fdt::*;

#[cfg(test)]
#[path = "core/mod.rs"]
pub mod test_core;

/// Guest DTB artifact produced or patched before AxVM owns it.
#[derive(Debug, Clone)]
pub struct GuestDtbImage {
    bytes: Vec<u8>,
}

impl GuestDtbImage {
    /// Wraps finalized guest DTB bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Returns the encoded guest DTB.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
fn guest_fdt_policy() -> test_core::GuestFdtPolicy {
    test_core::GuestFdtPolicy {
        patch_runtime: test_runtime_patch,
    }
}

#[cfg(test)]
fn host_fdt_bytes() -> Option<&'static [u8]> {
    None
}

#[cfg(test)]
fn test_runtime_patch(
    fdt: &[u8],
    _vm: &crate::AxVMRef,
    _config: &axvmconfig::AxVMCrateConfig,
) -> crate::AxVmResult<Vec<u8>> {
    Ok(fdt.to_vec())
}

//! Narrow contracts shared by the architecture frontends.

use httpboot_protocol::MacAddress;

use crate::ota::State;

/// Supplies the identity used by discovery and the HTTP control plane.
pub trait BootTransport {
    /// Returns the permanent or currently active link address.
    fn mac_address(&self) -> MacAddress;

    /// Returns the address currently reported by the network controller.
    fn current_mac_address(&self) -> MacAddress;
}

/// Reserves physical memory for an image before it is copied into place.
pub trait ImageMemory {
    /// Allocation error reported by the platform memory owner.
    type Error;

    /// Reserves `pages` contiguous pages and returns their physical address.
    fn allocate_image(&self, pages: usize) -> Result<usize, Self::Error>;
}

/// Transfers control to a validated EFI image.
pub trait EfiHandoff {
    /// Handoff error reported by the firmware ABI wrapper.
    type Error;

    /// Calls the image's fixed EFI entry point.
    fn handoff(&self, entry_point: u64) -> Result<(), Self::Error>;
}

/// Commits an OTA state record to persistent storage.
pub trait OtaStorage {
    /// Storage error reported while flushing the record.
    type Error;

    /// Persists the supplied state and makes it the in-memory state.
    fn commit_state(&mut self, state: &State) -> Result<(), Self::Error>;
}

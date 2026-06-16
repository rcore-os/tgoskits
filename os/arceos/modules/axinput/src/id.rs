#[repr(C)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InputDeviceId {
    /// The bustype identifier.
    pub bus_type: u16,
    /// The vendor identifier.
    pub vendor: u16,
    /// The product identifier.
    pub product: u16,
    /// The version identifier.
    pub version: u16,
}

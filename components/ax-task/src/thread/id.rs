//! Generation-checked thread identity.

/// A thread registry identity containing a slot and reuse generation.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ThreadId(u64);

impl ThreadId {
    /// Creates an identifier from a registry slot and generation.
    pub const fn from_parts(slot: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | slot as u64)
    }

    /// Returns the registry slot.
    pub const fn slot(self) -> u32 {
        self.0 as u32
    }

    /// Returns the slot reuse generation.
    pub const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }

    /// Returns the stable integer representation.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

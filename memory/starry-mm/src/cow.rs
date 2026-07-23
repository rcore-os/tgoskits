//! Copy-on-write frame reference policy.

/// Error returned when a COW frame cannot accept another reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CowReferenceError {
    #[error("copy-on-write frame reference count overflow")]
    Overflow,
}

/// Result of releasing one COW frame reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowRelease {
    Shared,
    LastReference,
}

/// Checked reference count for one resident COW frame.
pub struct CowFrameReferences {
    count: u32,
}

impl CowFrameReferences {
    /// Creates the initial owner reference.
    pub const fn new() -> Self {
        Self { count: 1 }
    }

    /// Returns the current reference count.
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Adds one reference without mutating the count on overflow.
    pub fn try_add(&mut self) -> Result<(), CowReferenceError> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or(CowReferenceError::Overflow)?;
        Ok(())
    }

    /// Releases one reference and identifies the final owner.
    pub fn release(&mut self) -> CowRelease {
        assert!(self.count > 0, "releasing an unreferenced COW frame");
        self.count -= 1;
        if self.count == 0 {
            CowRelease::LastReference
        } else {
            CowRelease::Shared
        }
    }
}

impl Default for CowFrameReferences {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_does_not_mutate_the_reference_count() {
        let mut references = CowFrameReferences { count: u32::MAX };

        assert_eq!(references.try_add(), Err(CowReferenceError::Overflow));
        assert_eq!(references.count(), u32::MAX);
    }

    #[test]
    fn release_identifies_the_last_reference() {
        let mut references = CowFrameReferences::new();
        references.try_add().unwrap();

        assert_eq!(references.release(), CowRelease::Shared);
        assert_eq!(references.release(), CowRelease::LastReference);
    }
}

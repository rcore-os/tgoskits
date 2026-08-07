//! Checked ACPI GPA allocation and byte placement.

use std::{string::String, vec::Vec};

use super::AcpiBuildError;

/// One checked reservation in an ACPI arena.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcpiAllocation {
    name: String,
    gpa: u64,
    offset: usize,
    size: usize,
}

impl AcpiAllocation {
    /// Returns its guest physical address.
    pub(crate) const fn gpa(&self) -> u64 {
        self.gpa
    }

    /// Returns its file- or image-relative offset.
    pub(crate) const fn offset(&self) -> usize {
        self.offset
    }
}

/// A bounded, monotonically allocated ACPI image arena.
pub(crate) struct AcpiTableArena {
    base: u64,
    limit: u64,
    cursor: u64,
    bytes: Vec<u8>,
}

impl AcpiTableArena {
    /// Creates an empty arena over `[base, limit)`.
    pub(crate) fn new(base: u64, limit: u64) -> Result<Self, AcpiBuildError> {
        if limit < base {
            return Err(AcpiBuildError::AddressOverflow {
                object: "ACPI arena".into(),
            });
        }
        Ok(Self {
            base,
            limit,
            cursor: base,
            bytes: Vec::new(),
        })
    }

    /// Reserves the lowest aligned range available in this arena.
    pub(crate) fn reserve(
        &mut self,
        name: impl Into<String>,
        size: usize,
        alignment: usize,
    ) -> Result<AcpiAllocation, AcpiBuildError> {
        let name = name.into();
        if !alignment.is_power_of_two() {
            return Err(AcpiBuildError::InvalidAlignment {
                object: name,
                alignment,
            });
        }
        let mask = u64::try_from(alignment - 1).map_err(|_| AcpiBuildError::AddressOverflow {
            object: name.clone(),
        })?;
        let start = self
            .cursor
            .checked_add(mask)
            .map(|value| value & !mask)
            .ok_or_else(|| AcpiBuildError::AddressOverflow {
                object: name.clone(),
            })?;
        let size_u64 = u64::try_from(size).map_err(|_| AcpiBuildError::AddressOverflow {
            object: name.clone(),
        })?;
        let end = start
            .checked_add(size_u64)
            .ok_or_else(|| AcpiBuildError::AddressOverflow {
                object: name.clone(),
            })?;
        if end > self.limit {
            return Err(AcpiBuildError::ArenaExhausted {
                base: self.base,
                limit: self.limit,
                object: name,
                size,
                alignment,
            });
        }
        let offset =
            usize::try_from(start - self.base).map_err(|_| AcpiBuildError::AddressOverflow {
                object: name.clone(),
            })?;
        let image_len =
            usize::try_from(end - self.base).map_err(|_| AcpiBuildError::AddressOverflow {
                object: name.clone(),
            })?;
        self.bytes.resize(image_len, 0);
        self.cursor = end;
        Ok(AcpiAllocation {
            name,
            gpa: start,
            offset,
            size,
        })
    }

    /// Writes bytes into exactly one prior reservation.
    pub(crate) fn write(
        &mut self,
        allocation: &AcpiAllocation,
        bytes: &[u8],
    ) -> Result<(), AcpiBuildError> {
        if bytes.len() != allocation.size {
            return Err(AcpiBuildError::LengthMismatch {
                object: allocation.name.clone(),
                expected: allocation.size,
                actual: bytes.len(),
            });
        }
        let end = allocation.offset + allocation.size;
        self.bytes[allocation.offset..end].copy_from_slice(bytes);
        Ok(())
    }

    /// Consumes the arena and returns its compact image bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_object_that_crosses_the_arena_limit() {
        let mut arena = AcpiTableArena::new(0xe0000, 0xe0040).unwrap();
        assert!(arena.reserve("oversized", 0x41, 1).is_err());
    }
}

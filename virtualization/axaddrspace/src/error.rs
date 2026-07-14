// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ax_memory_set::MappingError;
use axvm_types::GuestPhysAddr;

/// Result returned by guest address-space operations.
pub type AddrSpaceResult<T = ()> = Result<T, AddrSpaceError>;

/// Failures reported while managing or accessing a guest address space.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AddrSpaceError {
    /// A guest address range lies outside the configured address space.
    #[error(
        "guest address range [{start:#x}, +{size:#x}) is outside [{space_start:#x}, \
         {space_end:#x})"
    )]
    OutOfRange {
        /// Start of the requested range.
        start: GuestPhysAddr,
        /// Size of the requested range.
        size: usize,
        /// Start of the configured address space.
        space_start: GuestPhysAddr,
        /// End of the configured address space.
        space_end: GuestPhysAddr,
    },
    /// An address or size does not satisfy the required alignment.
    #[error("{subject} value {value:#x} is not aligned to {alignment:#x}")]
    Unaligned {
        /// Kind of value that failed validation.
        subject: &'static str,
        /// Misaligned value.
        value: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// Computing an address range overflowed.
    #[error("address range starting at {start:#x} with size {size:#x} overflows")]
    AddressOverflow {
        /// Start of the overflowing range.
        start: usize,
        /// Size of the overflowing range.
        size: usize,
    },
    /// A mapping overlaps an existing mapping.
    #[error("guest address mapping conflicts with an existing mapping")]
    MappingConflict,
    /// The mapping request is invalid for the lower mapping layer.
    #[error("guest address mapping request is invalid")]
    InvalidMapping,
    /// The mapping layer is not in a state that permits the operation.
    #[error("guest address mapping state does not permit the operation")]
    MappingState,
    /// A guest address cannot be translated.
    #[error("guest address {address:#x} is not mapped")]
    Unmapped {
        /// Guest physical address that could not be translated.
        address: GuestPhysAddr,
    },
    /// A translated region is too small for an access.
    #[error(
        "cannot {operation} {requested} bytes at guest address {address:#x}: only {available} \
         bytes are accessible"
    )]
    InsufficientAccess {
        /// Access operation being performed.
        operation: &'static str,
        /// Guest physical address being accessed.
        address: GuestPhysAddr,
        /// Number of bytes requested.
        requested: usize,
        /// Number of bytes available in the translated region.
        available: usize,
    },
}

impl From<MappingError> for AddrSpaceError {
    fn from(error: MappingError) -> Self {
        match error {
            MappingError::InvalidParam => Self::InvalidMapping,
            MappingError::AlreadyExists => Self::MappingConflict,
            MappingError::BadState => Self::MappingState,
        }
    }
}

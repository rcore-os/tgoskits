// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

//! Manages system resources that can be allocated to VMs and their devices.
//!
//! # Example
//!
//! Depending on the use case of the VMM, both the `IdAllocator` and the `AddressAllocator`
//! can be used. In the example below we assume that the `IdAllocator` is used for allocating
//! unique identifiers for VM devices. We use the address allocator for allocating MMIO ranges
//! for virtio devices.
//!
//! In the example below we use constants that are typical for the x86 platform, but this has no
//! impact on the code actually working on aarch64.
//!
//! ```rust
//! use std::collections::HashMap;
//!
//! use rdif_pcie::addr_alloc::{
//!     AddressAllocator, AllocPolicy, Error, IdAllocator, RangeInclusive, Result,
//! };
//!
//! const FIRST_ADDR_PAST_32BITS: u64 = 1 << 32;
//! const MEM_32BIT_GAP_SIZE: u64 = 768 << 20;
//! const MMIO_MEM_START: u64 = FIRST_ADDR_PAST_32BITS - MEM_32BIT_GAP_SIZE;
//! const PAGE_SIZE: u64 = 0x1000;
//!
//! struct DeviceManager {
//!     id_allocator: IdAllocator,
//!     mmio_allocator: AddressAllocator,
//!     slots: HashMap<u32, RangeInclusive>,
//! }
//!
//! #[derive(Clone, Copy)]
//! struct DeviceSlot {
//!     id: u32,
//!     mmio_range: RangeInclusive,
//! }
//!
//! impl DeviceManager {
//!     pub fn new() -> Result<Self> {
//!         Ok(DeviceManager {
//!             id_allocator: IdAllocator::new(0, 100)?,
//!             mmio_allocator: AddressAllocator::new(MMIO_MEM_START, MEM_32BIT_GAP_SIZE)?,
//!             slots: HashMap::new(),
//!         })
//!     }
//!
//!     pub fn reserve_device_slot(&mut self) -> Result<DeviceSlot> {
//!         // We're reserving the first available address that is aligned to the page size.
//!         // For each device we reserve one page of addresses.
//!         let mmio_range =
//!             self.mmio_allocator
//!                 .allocate(PAGE_SIZE, PAGE_SIZE, AllocPolicy::FirstMatch)?;
//!         let slot = DeviceSlot {
//!             id: self.id_allocator.allocate_id()?,
//!             mmio_range,
//!         };
//!         self.slots.insert(slot.id, slot.mmio_range);
//!         Ok(slot)
//!     }
//!
//!     // Free the device slot corresponding to the passed device ID.
//!     pub fn free_device_slot(&mut self, id: u32) -> Result<()> {
//!         let mmio_range = self.slots.get(&id).ok_or(Error::NeverAllocated(id))?;
//!         let _ = self.id_allocator.free_id(id)?;
//!         self.mmio_allocator.free(mmio_range)
//!     }
//! }
//!
//! # fn main() {
//! #     let mut dm = DeviceManager::new().unwrap();
//! #    let slot = dm.reserve_device_slot().unwrap();
//! #    dm.free_device_slot(slot.id).unwrap();
//! # }
//! ```

#![deny(missing_docs)]

extern crate alloc;
extern crate core as std;

mod address_allocator;
/// Allocation engine used by address allocator.
mod allocation_engine;
mod id_allocator;

use std::{
    cmp::{max, min},
    result,
};

pub use address_allocator::AddressAllocator;
use allocation_engine::NodeState;
pub use id_allocator::IdAllocator;
use thiserror::Error;

/// Default alignment that can be used for creating a `Constraint`.
#[allow(dead_code)]
pub const DEFAULT_CONSTRAINT_ALIGN: u64 = 4;

/// Error type for IdAllocator usage.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Error)]
pub enum Error {
    /// Management operations on desired resource resulted in overflow.
    #[error("Management operations on desired resource resulted in overflow.")]
    Overflow,
    /// An id that is not part of the specified range was requested to be released.
    #[error("Specified id: {0} is not in the range.")]
    OutOfRange(u32),
    /// An id that was already released was requested to be released.
    #[error("Specified id: {0} is already released.")]
    AlreadyReleased(u32),
    /// An id  that was never allocated was requested to be released.
    #[error("Specified id: {0} was never allocated, can't release it.")]
    NeverAllocated(u32),
    /// The resource we want to use or update is not available.
    #[error("The requested resource is not available.")]
    ResourceNotAvailable,
    /// The range to manage is invalid.
    #[error("The range specified: {0}-{1} is not valid.")]
    InvalidRange(u64, u64),
    /// Alignment value is invalid
    #[error("Alignment value is invalid.")]
    InvalidAlignment,
    /// The range that we try to insert into the interval tree is overlapping
    /// with another node from the tree.
    #[error("Addresses are overlapping.{0:?} intersects with existing {1:?}")]
    Overlap(RangeInclusive, RangeInclusive),
    /// A node state can be changed just from Free to Allocated, other transitions
    /// are not valid.
    #[error("Invalid state transition for node {0:?} from {1:?} to NodeState::Free")]
    InvalidStateTransition(RangeInclusive, NodeState),
    /// Address is unaligned
    #[error("The address is not aligned.")]
    UnalignedAddress,
    /// Management operations on desired resource resulted in underflow.
    #[error("Management operations on desired resource resulted in underflow.")]
    Underflow,
    /// The size of the desired resource is not invalid.
    #[error("The specified size: {0} is not valid.")]
    InvalidSize(u64),
}

/// Wrapper over std::result::Result
pub type Result<T> = result::Result<T, Error>;

/// A closed interval range [start, end].
/// The range describes a memory slot which is assigned by the VMM to a device.
///
/// # Example
///
/// ```rust
/// use rdif_pcie::addr_alloc::RangeInclusive;
///
/// let r = RangeInclusive::new(0x0, 0x100).unwrap();
/// assert_eq!(r.len(), 0x101);
/// assert_eq!(r.start(), 0x0);
/// assert_eq!(r.end(), 0x100);
///
/// // Check if a region contains another region.
/// let other = RangeInclusive::new(0x50, 0x80).unwrap();
/// assert!(r.contains(&other));
///
/// // Check if a region overlaps with another one.
/// let other = RangeInclusive::new(0x99, 0x150).unwrap();
/// assert!(r.overlaps(&other));
/// ```
// This structure represents the key of the Node object in the interval tree implementation.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Hash, Ord, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RangeInclusive {
    /// Lower boundary of the interval.
    start: u64,
    /// Upper boundary of the interval.
    end: u64,
}

#[allow(clippy::len_without_is_empty)]
impl RangeInclusive {
    /// Creates a new RangeInclusive.
    pub fn new(start: u64, end: u64) -> Result<Self> {
        // The length of the interval [0, u64::MAX] is u64::MAX + 1 which does
        // not fit in a u64::MAX, hence we return `Error::InvalidRange` when
        // there is an attempt to use that range.
        if start > end || (start == 0 && end == u64::MAX) {
            return Err(Error::InvalidRange(start, end));
        }
        Ok(RangeInclusive { start, end })
    }

    /// Returns the length of the range.
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Returns true if the regions overlap.
    pub fn overlaps(&self, other: &RangeInclusive) -> bool {
        max(self.start, other.start) <= min(self.end, other.end)
    }

    /// Returns true if the current range contains the range passed as a parameter.
    pub fn contains(&self, other: &RangeInclusive) -> bool {
        self.start <= other.start && self.end >= other.end
    }

    /// Returns the lower boundary of the range.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Returns the upper boundary of the range.
    pub fn end(&self) -> u64 {
        self.end
    }
}

/// A resource allocation constraint.
///
/// # Example
///
/// ```rust
/// use rdif_pcie::addr_alloc::{AllocPolicy, Constraint, DEFAULT_CONSTRAINT_ALIGN, Error};
///
/// let constraint =
///     Constraint::new(0x4, DEFAULT_CONSTRAINT_ALIGN, AllocPolicy::FirstMatch).unwrap();
/// assert_eq!(constraint.size(), 0x4);
/// assert_eq!(constraint.align(), 0x4);
///
/// // Alignments need to be a power of 2, otherwise an error is returned.
/// assert_eq!(
///     Constraint::new(0x4, 0x3, AllocPolicy::LastMatch).unwrap_err(),
///     Error::InvalidAlignment
/// );
///
/// // When using the ExactMatch policy, the start address must also be aligned, otherwise
/// // an error is returned.
/// assert_eq!(
///     Constraint::new(0x4, 0x4, AllocPolicy::ExactMatch(0x3)).unwrap_err(),
///     Error::UnalignedAddress
/// );
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Constraint {
    /// Size to allocate.
    size: u64,
    /// Alignment for the allocated resource.
    align: u64,
    /// Resource allocation policy.
    policy: AllocPolicy,
}

impl Constraint {
    /// Creates a new constraint based on the passed configuration.
    ///
    /// When the `ExactMatch` policy is used, the start address MUST be aligned to the
    /// alignment passed as a parameter.
    ///
    /// # Arguments:
    /// - `size`: size to allocate.
    /// - `align`: alignment to be used for the allocated resources.
    ///   Valid alignments are a power of 2.
    /// - `policy`: allocation policy.
    pub fn new(size: u64, align: u64, policy: AllocPolicy) -> Result<Self> {
        if size == 0 {
            return Err(Error::InvalidSize(size));
        }

        if !align.is_power_of_two() || align == 0 {
            return Err(Error::InvalidAlignment);
        }

        if let AllocPolicy::ExactMatch(start_address) = policy
            && start_address % align != 0
        {
            return Err(Error::UnalignedAddress);
        }

        Ok(Constraint {
            size,
            align,
            policy,
        })
    }

    /// Returns the alignment constraint.
    #[allow(dead_code)]
    pub fn align(self) -> u64 {
        self.align
    }

    /// Returns the size constraint.
    pub fn size(self) -> u64 {
        self.size
    }
}

/// Policy for resource allocation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[allow(dead_code, clippy::enum_variant_names)]
pub enum AllocPolicy {
    /// Allocate the first matched entry.
    #[default]
    FirstMatch,
    /// Allocate first matched entry from the end of the range.
    LastMatch,
    /// Allocate a memory slot starting with the specified address
    /// if it is available.
    ExactMatch(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_range() {
        assert_eq!(
            RangeInclusive::new(2, 1).unwrap_err(),
            Error::InvalidRange(2, 1)
        );
        assert_eq!(
            RangeInclusive::new(0, u64::MAX).unwrap_err(),
            Error::InvalidRange(0, u64::MAX)
        );
    }

    #[test]
    fn test_range_overlaps() {
        let range_a = RangeInclusive::new(1u64, 4u64).unwrap();
        let range_b = RangeInclusive::new(4u64, 6u64).unwrap();
        let range_c = RangeInclusive::new(2u64, 3u64).unwrap();
        let range_e = RangeInclusive::new(5u64, 6u64).unwrap();

        assert!(range_a.overlaps(&range_b));
        assert!(range_b.overlaps(&range_a));
        assert!(range_a.overlaps(&range_c));
        assert!(range_c.overlaps(&range_a));
        assert!(!range_a.overlaps(&range_e));
        assert!(!range_e.overlaps(&range_a));

        assert_eq!(range_a.len(), 4);
    }

    #[test]
    fn test_range_contain() {
        let range_a = RangeInclusive::new(2u64, 6u64).unwrap();
        assert!(range_a.contains(&RangeInclusive::new(2u64, 3u64).unwrap()));
        assert!(range_a.contains(&RangeInclusive::new(3u64, 4u64).unwrap()));
        assert!(range_a.contains(&RangeInclusive::new(5u64, 6u64).unwrap()));
        assert!(!range_a.contains(&RangeInclusive::new(1u64, 2u64).unwrap()));
        assert!(!range_a.contains(&RangeInclusive::new(1u64, 3u64).unwrap()));
        assert!(!range_a.contains(&RangeInclusive::new(1u64, 7u64).unwrap()));
        assert!(!range_a.contains(&RangeInclusive::new(7u64, 8u64).unwrap()));
        assert!(!range_a.contains(&RangeInclusive::new(6u64, 7u64).unwrap()));
        assert!(!range_a.contains(&RangeInclusive::new(7u64, 8u64).unwrap()));
    }

    #[test]
    fn test_range_ord() {
        let range_a = RangeInclusive::new(1, 4).unwrap();
        let range_b = RangeInclusive::new(1, 4).unwrap();
        let range_c = RangeInclusive::new(1, 3).unwrap();
        let range_d = RangeInclusive::new(1, 5).unwrap();

        assert_eq!(range_a, range_b);
        assert_eq!(range_b, range_a);
        assert!(range_a > range_c);
        assert!(range_c < range_a);
        assert!(range_a < range_d);
        assert!(range_d > range_a);
    }

    #[test]
    fn test_getters() {
        let range = RangeInclusive::new(3, 5).unwrap();
        assert_eq!(range.start(), 3);
        assert_eq!(range.end(), 5);
    }

    #[test]
    fn test_range_upper_bound() {
        let range = RangeInclusive::new(0, u64::MAX);
        assert_eq!(range.unwrap_err(), Error::InvalidRange(0, u64::MAX));
    }

    #[test]
    fn constraint_getter() {
        let bad_constraint = Constraint::new(0x1000, 0x1000, AllocPolicy::ExactMatch(0xC));
        assert_eq!(bad_constraint.unwrap_err(), Error::UnalignedAddress);
        let constraint = Constraint::new(0x1000, 0x1000, AllocPolicy::default()).unwrap();
        assert_eq!(constraint.align(), 0x1000);
        assert_eq!(constraint.size(), 0x1000);
    }
}

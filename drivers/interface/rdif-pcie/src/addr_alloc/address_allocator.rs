// Copyright © 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright © 2022 Alibaba Cloud. All rights reserved.
// Copyright © 2019 Intel Corporation. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

//! Provides allocation and releasing strategy for memory slots.
//!
//! This module implements an allocation strategies for memory slots in an
//! address space (for example MMIO and PIO).

use super::{
    AllocPolicy, Constraint, Error, RangeInclusive, Result, allocation_engine::IntervalTree,
};

// Internal representation of AddressAllocator. Contains the managed address
// space represented through an instance of RangeInclusive. The address
// allocator also contains a node that represents the root of the interval tree
// used for memory slots management. The reason we chose to use an interval tree
// is that the average complexity for deletion and insertion is O(log N) and for
// searching a node is O(N).
/// An address space allocator.
///
/// The `AddressAllocator` manages an address space by exporting functionality to reserve and
/// free address ranges based on a user defined [Allocation Policy](enum.AllocPolicy.html).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct AddressAllocator {
    // Address space that we want to manage.
    address_space: RangeInclusive,
    // Internal representation of the managed address space. Each node in the
    // tree will represent a memory location and can have two states either
    // `NodeState::Free` or `NodeState::Allocated`.
    interval_tree: IntervalTree,
}

impl AddressAllocator {
    /// Creates a new instance of AddressAllocator that will be used to manage
    /// the allocation and release of memory slots from the managed address
    /// space.
    pub fn new(base: u64, size: u64) -> core::result::Result<Self, Error> {
        let end = base
            .checked_add(size.checked_sub(1).ok_or(Error::Underflow)?)
            .ok_or(Error::Overflow)?;
        let aux_range = RangeInclusive::new(base, end)?;
        Ok(AddressAllocator {
            address_space: aux_range,
            interval_tree: IntervalTree::new(aux_range),
        })
    }

    /// Allocates a new aligned memory slot. Returns the allocated range in case of success.
    ///
    /// When the `ExactMatch` policy is used, the start address MUST be aligned to the
    /// alignment passed as a parameter.
    ///
    /// # Arguments:
    /// - `size`: size to allocate.
    /// - `alignment`: alignment to be used for the allocated resources.
    ///   Valid alignments are a power of 2.
    /// - `policy`: allocation policy.
    pub fn allocate(
        &mut self,
        size: u64,
        alignment: u64,
        policy: AllocPolicy,
    ) -> Result<RangeInclusive> {
        let constraint = Constraint::new(size, alignment, policy)?;
        self.interval_tree.allocate(constraint)
    }

    /// Deletes the specified memory slot or returns `ResourceNotAvailable` if
    /// the node was not allocated before.
    #[allow(dead_code)]
    pub fn free(&mut self, key: &RangeInclusive) -> Result<()> {
        self.interval_tree.free(key)
    }

    /// First address of the allocator.
    #[allow(dead_code)]
    pub fn base(&self) -> u64 {
        self.address_space.start()
    }

    /// Last address of the allocator.
    #[allow(dead_code)]
    pub fn end(&self) -> u64 {
        self.address_space.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regression_exact_match_length_check() {
        let mut pool = AddressAllocator::new(0x0, 0x2000).unwrap();
        let res = pool
            .allocate(0x1000, 0x1000, AllocPolicy::ExactMatch(0x1000))
            .unwrap();
        assert_eq!(
            pool.allocate(0x0, 0x1000, AllocPolicy::FirstMatch)
                .unwrap_err(),
            Error::InvalidSize(0x0)
        );
        assert_eq!(
            pool.allocate(0x1000, 0x1000, AllocPolicy::ExactMatch(0x3))
                .unwrap_err(),
            Error::UnalignedAddress
        );
        assert_eq!(res, RangeInclusive::new(0x1000, 0x1FFF).unwrap());
        let res = pool
            .allocate(0x1000, 0x1000, AllocPolicy::ExactMatch(0x0))
            .unwrap();
        assert_eq!(res, RangeInclusive::new(0x0, 0x0FFF).unwrap());
    }

    #[test]
    fn test_new_fails_overflow() {
        assert_eq!(
            AddressAllocator::new(u64::MAX, 0x100).unwrap_err(),
            Error::Overflow
        );
    }

    #[test]
    fn test_new_fails_size_zero() {
        assert_eq!(
            AddressAllocator::new(0x1000, 0x0).unwrap_err(),
            Error::Underflow
        );
    }

    #[test]
    fn test_allocate_fails_alignment_zero() {
        let mut pool = AddressAllocator::new(0x1000, 0x10000).unwrap();
        assert_eq!(
            pool.allocate(0x100, 0, AllocPolicy::FirstMatch)
                .unwrap_err(),
            Error::InvalidAlignment
        );
    }

    #[test]
    fn test_allocate_fails_alignment_non_power_of_two() {
        let mut pool = AddressAllocator::new(0x1000, 0x10000).unwrap();
        assert_eq!(
            pool.allocate(0x100, 200, AllocPolicy::FirstMatch)
                .unwrap_err(),
            Error::InvalidAlignment
        );
    }

    #[test]
    fn test_allocate_fails_not_enough_space() {
        let mut pool = AddressAllocator::new(0x1000, 0x1000).unwrap();
        assert_eq!(
            pool.allocate(0x800, 0x100, AllocPolicy::LastMatch).unwrap(),
            RangeInclusive::new(0x1800, 0x1FFF).unwrap()
        );
        assert_eq!(
            pool.allocate(0x900, 0x100, AllocPolicy::FirstMatch)
                .unwrap_err(),
            Error::ResourceNotAvailable
        );
        assert_eq!(
            pool.allocate(0x400, 0x100, AllocPolicy::FirstMatch)
                .unwrap(),
            RangeInclusive::new(0x1000, 0x13FF).unwrap()
        );
    }

    #[test]
    fn test_allocate_with_alignment_first_ok() {
        let mut pool = AddressAllocator::new(0x1000, 0x1000).unwrap();
        assert_eq!(
            pool.allocate(0x110, 0x100, AllocPolicy::FirstMatch)
                .unwrap(),
            RangeInclusive::new(0x1000, 0x110F).unwrap()
        );
        assert_eq!(
            pool.allocate(0x100, 0x100, AllocPolicy::FirstMatch)
                .unwrap(),
            RangeInclusive::new(0x1200, 0x12FF).unwrap()
        );
        assert_eq!(
            pool.allocate(0x10, 0x100, AllocPolicy::FirstMatch).unwrap(),
            RangeInclusive::new(0x1300, 0x130F).unwrap()
        );
    }

    #[test]
    fn test_allocate_with_alignment_last_ok() {
        let mut pool_reverse = AddressAllocator::new(0x1000, 0x10000).unwrap();
        assert_eq!(
            pool_reverse
                .allocate(0x110, 0x100, AllocPolicy::LastMatch)
                .unwrap(),
            RangeInclusive::new(0x10E00, 0x10F0F).unwrap()
        );
        assert_eq!(
            pool_reverse
                .allocate(0x100, 0x100, AllocPolicy::LastMatch)
                .unwrap(),
            RangeInclusive::new(0x10D00, 0x10DFF).unwrap()
        );
        assert_eq!(
            pool_reverse
                .allocate(0x10, 0x100, AllocPolicy::LastMatch)
                .unwrap(),
            RangeInclusive::new(0x10C00, 0x10C0F).unwrap()
        );
    }

    #[test]
    fn test_allocate_address_not_enough_space() {
        let mut pool = AddressAllocator::new(0x1000, 0x1000).unwrap();
        // First range is [0x1000:0x17FF]
        assert_eq!(
            pool.allocate(0x800, 0x100, AllocPolicy::FirstMatch)
                .unwrap(),
            RangeInclusive::new(0x1000, 0x17FF).unwrap()
        );
        // Second range is [0x1A00:0x1BFF]
        assert_eq!(
            pool.allocate(0x200, 0x100, AllocPolicy::ExactMatch(0x1A00))
                .unwrap(),
            RangeInclusive::new(0x1A00, 0x1BFF).unwrap()
        );
        // There is 0x200 between the first 2 ranges.
        // We ask for an available address but the range is too big
        assert_eq!(
            pool.allocate(0x800, 0x100, AllocPolicy::ExactMatch(0x1800))
                .unwrap_err(),
            Error::ResourceNotAvailable
        );
        // We ask for an available address, with a small enough range
        assert_eq!(
            pool.allocate(0x100, 0x100, AllocPolicy::ExactMatch(0x1800))
                .unwrap(),
            RangeInclusive::new(0x1800, 0x18FF).unwrap()
        );
    }

    #[test]
    fn test_tree_allocate_address_free_and_realloc() {
        let mut pool = AddressAllocator::new(0x1000, 0x1000).unwrap();
        assert_eq!(
            pool.allocate(0x800, 0x100, AllocPolicy::FirstMatch)
                .unwrap(),
            RangeInclusive::new(0x1000, 0x17FF).unwrap()
        );

        let _ = pool.free(&RangeInclusive::new(0x1000, 0x17FF).unwrap());
        assert_eq!(
            pool.allocate(0x800, 0x100, AllocPolicy::FirstMatch)
                .unwrap(),
            RangeInclusive::new(0x1000, 0x17FF).unwrap()
        );
    }

    #[test]
    fn test_allow_range_size_one_left() {
        let mut pool = AddressAllocator::new(1, 1000).unwrap();
        assert_eq!(
            pool.allocate(10, 2, AllocPolicy::FirstMatch).unwrap(),
            RangeInclusive::new(2, 11).unwrap()
        );
        assert_eq!(
            pool.allocate(1, 1, AllocPolicy::FirstMatch).unwrap(),
            RangeInclusive::new(1, 1).unwrap()
        );
    }

    #[test]
    fn test_allocate_address_fail_free_and_realloc() {
        let mut pool = AddressAllocator::new(0x0, 0x1000).unwrap();
        // First allocation fails
        assert_eq!(
            pool.allocate(0x2000, 0x100, AllocPolicy::FirstMatch)
                .unwrap_err(),
            Error::ResourceNotAvailable
        );
        // We try to free a range that was not allocated.
        assert_eq!(
            pool.free(&RangeInclusive::new(0x1200, 0x3200).unwrap())
                .unwrap_err(),
            Error::ResourceNotAvailable
        );
        // Now we try an allocation that should succeed.
        assert_eq!(
            pool.allocate(0x4FE, 0x100, AllocPolicy::ExactMatch(0x500))
                .unwrap(),
            RangeInclusive::new(0x500, 0x9FD).unwrap()
        );
        assert!(
            pool.free(&RangeInclusive::new(0x500, 0x9FD).unwrap())
                .is_ok()
        );
    }
}

// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright © 2019 Intel Corporation. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

//! Provides allocation and releasing strategy for IDs.
//!
//! This module implements an allocation strategies for all resource types
//! that can be abstracted to an integer.

use alloc::collections::BTreeSet;

use super::{Error, Result};

/// An unique ID allocator that allows management of IDs in a given interval.
// Internal representation of IdAllocator. Contains the ends of the interval
// that is managed, a field that points at the next available ID, and a
// BTreeSet used to store the released IDs. The reason we chose a
// BTreeSet is that the average complexity for deletion and insertion is
// O(logN) compared to Vec for example, another benefit is that the entries
// are sorted so we will always use the first available ID.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IdAllocator {
    // Beginning of the range of IDs that we want to manage.
    range_base: u32,
    // First available id that was never allocated.
    next_id: Option<u32>,
    // End of the range of IDs that we want to manage.
    range_end: u32,
    // Set of all freed ids that can be reused at subsequent allocations.
    freed_ids: BTreeSet<u32>,
}

#[allow(dead_code)]
impl IdAllocator {
    /// Creates a new instance of IdAllocator that will be used to manage the
    /// allocation and release of ids from the interval specified by
    /// `range_base` and `range_end`
    pub fn new(range_base: u32, range_end: u32) -> Result<Self> {
        if range_end < range_base {
            return Err(Error::InvalidRange(range_base.into(), range_end.into()));
        }
        Ok(IdAllocator {
            range_base,
            next_id: Some(range_base),
            range_end,
            freed_ids: BTreeSet::<u32>::new(),
        })
    }

    fn id_in_range(&self, id: u32) -> bool {
        // Check for out of range.
        self.range_base <= id && id <= self.range_end
    }

    /// Allocate an ID from the managed range.
    /// We first try to reuse one of the IDs released before. If there is no
    /// ID to reuse we return the next available one from the managed range.
    pub fn allocate_id(&mut self) -> Result<u32> {
        // If the set containing all freed ids is not empty we extract the
        // first entry from that set and return it.
        if !self.freed_ids.is_empty() {
            let ret_value = *self.freed_ids.iter().next().unwrap();
            self.freed_ids.remove(&ret_value);
            return Ok(ret_value);
        }
        // If no id was freed before we return the next available id.
        if let Some(next_id) = self.next_id {
            if next_id > self.range_end {
                return Err(Error::ResourceNotAvailable);
            }
            // Prepare the next available id. If the addition overflows we
            // set the next_id field to None and return Overflow at the next
            // allocation request.
            self.next_id = next_id.checked_add(1);
            return Ok(next_id);
        }
        Err(Error::Overflow)
    }

    /// Frees an id from the managed range.
    pub fn free_id(&mut self, id: u32) -> Result<u32> {
        // Check if the id belongs to the managed range and if it was not
        // released before. Return error if any of the conditions is not met.
        if !self.id_in_range(id) {
            return Err(Error::OutOfRange(id));
        }
        if let Some(next_id) = self.next_id
            && next_id < id
        {
            return Err(Error::NeverAllocated(id));
        }

        // Insert the released id in the set of released id to avoid releasing
        // it in next iterations.
        self.freed_ids
            .insert(id)
            .then_some(id)
            .ok_or(Error::AlreadyReleased(id))
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, IdAllocator};

    #[test]
    fn test_slot_id_allocation() {
        let faulty_allocator = IdAllocator::new(23, 5);
        assert_eq!(faulty_allocator.unwrap_err(), Error::InvalidRange(23, 5));
        let mut legacy_irq_allocator = IdAllocator::new(5, 23).unwrap();
        assert_eq!(legacy_irq_allocator.range_base, 5);
        assert_eq!(legacy_irq_allocator.range_end, 23);

        let id = legacy_irq_allocator.allocate_id().unwrap();
        assert_eq!(id, 5);
        assert_eq!(legacy_irq_allocator.next_id.unwrap(), 6);

        for _ in 1..19 {
            assert!(legacy_irq_allocator.allocate_id().is_ok());
        }

        assert_eq!(
            legacy_irq_allocator.allocate_id().unwrap_err(),
            Error::ResourceNotAvailable
        );
    }

    #[test]
    fn test_u32_overflow() {
        let mut allocator = IdAllocator::new(u32::MAX - 1, u32::MAX).unwrap();
        assert_eq!(allocator.allocate_id().unwrap(), u32::MAX - 1);
        assert_eq!(allocator.allocate_id().unwrap(), u32::MAX);
        let res = allocator.allocate_id();
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), Error::Overflow);
    }

    #[test]
    fn test_slot_id_free() {
        let mut legacy_irq_allocator = IdAllocator::new(5, 23).unwrap();
        assert_eq!(
            legacy_irq_allocator.free_id(3).unwrap_err(),
            Error::OutOfRange(3)
        );
        assert_eq!(legacy_irq_allocator.freed_ids.len(), 0);

        for _ in 1..10 {
            let _id = legacy_irq_allocator.allocate_id().unwrap();
        }

        let irq = 10;
        legacy_irq_allocator.free_id(irq).unwrap();
        assert!(legacy_irq_allocator.freed_ids.contains(&irq));
        assert_eq!(
            legacy_irq_allocator.free_id(10).unwrap_err(),
            Error::AlreadyReleased(10)
        );
        let irq = 9;
        legacy_irq_allocator.free_id(irq).unwrap();
        assert_eq!(legacy_irq_allocator.freed_ids.len(), 2);
        assert_eq!(*legacy_irq_allocator.freed_ids.iter().next().unwrap(), 9);

        let irq = legacy_irq_allocator.allocate_id().unwrap();
        assert_eq!(irq, 9);
        assert!(!legacy_irq_allocator.freed_ids.contains(&irq));
        assert_eq!(legacy_irq_allocator.freed_ids.len(), 1);
        assert_eq!(
            legacy_irq_allocator.free_id(21).unwrap_err(),
            Error::NeverAllocated(21)
        );
    }

    #[test]
    fn test_id_sanity_checks() {
        let legacy_irq_allocator = IdAllocator::new(5, 23).unwrap();

        assert!(!legacy_irq_allocator.id_in_range(4));
        assert!(legacy_irq_allocator.id_in_range(6));
        assert!(!legacy_irq_allocator.id_in_range(25));
    }
}

//! Bounded ownership of arena intervals, independent of firmware and locks.
use super::{
    AllocationSnapshot, MAX_ALLOCATIONS, MAX_RESOURCES, PhysicalRange, ResourceError, ResourceId,
    ResourceSnapshot,
};

#[derive(Clone, Copy)]
struct Slot {
    allocation: AllocationSnapshot,
    generation: u64,
}

pub(super) struct Arenas {
    pub resources: [ResourceSnapshot; MAX_RESOURCES],
    slots: [Slot; MAX_ALLOCATIONS],
    generation: u64,
}
impl Arenas {
    pub const fn new() -> Self {
        Self {
            resources: [ResourceSnapshot {
                id: 0,
                range: PhysicalRange::new(0, 0),
                initialized: 0,
            }; MAX_RESOURCES],
            slots: [Slot {
                allocation: AllocationSnapshot {
                    resource_id: 0,
                    range: PhysicalRange::new(0, 0),
                },
                generation: 0,
            }; MAX_ALLOCATIONS],
            generation: 0,
        }
    }

    pub fn restore(
        &mut self,
        resources: &[ResourceSnapshot],
        live: &[AllocationSnapshot],
    ) -> Result<(), ResourceError> {
        if resources.len() > MAX_RESOURCES || live.len() > MAX_ALLOCATIONS {
            return Err(ResourceError::Capacity);
        }
        let mut next = Self::new();
        // Never reuse a generation even across restoration of the same arena.
        next.generation = self.generation;
        for (i, resource) in resources.iter().enumerate() {
            resource.validate()?;
            if resources[..i]
                .iter()
                .any(|r| r.id == resource.id || r.range.overlaps(resource.range))
            {
                return Err(ResourceError::Overlap);
            }
            next.resources[i] = *resource;
        }
        for allocation in live {
            if allocation.resource_id == 0
                || !allocation.range.valid()
                || !allocation.range.start.is_multiple_of(super::PAGE_SIZE)
                || !allocation.range.len.is_multiple_of(super::PAGE_SIZE)
            {
                return Err(ResourceError::InvalidSnapshot);
            }
            if let Some(resource) = resources.iter().find(|r| r.id == allocation.resource_id)
                && resource.range.overlaps(allocation.range)
                && allocation.range.start < resource.range.start + resource.initialized
            {
                return Err(ResourceError::InvalidSnapshot);
            }
            // Detached or truncated arenas still have live objects. Keep those
            // slots independently of capacity so they can be forwarded/retired.
            next.insert(*allocation)?;
        }
        *self = next;
        Ok(())
    }

    fn insert(&mut self, allocation: AllocationSnapshot) -> Result<(usize, u64), ResourceError> {
        if self
            .slots
            .iter()
            .any(|s| s.allocation.range.overlaps(allocation.range))
        {
            return Err(ResourceError::Overlap);
        }
        let index = self
            .slots
            .iter()
            .position(|s| s.allocation.resource_id == 0)
            .ok_or(ResourceError::Capacity)?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ResourceError::Capacity)?;
        self.slots[index] = Slot {
            allocation,
            generation: self.generation,
        };
        Ok((index, self.generation))
    }

    pub fn allocate(
        &mut self,
        id: ResourceId,
        bytes: u64,
        alignment: u64,
    ) -> Result<(PhysicalRange, usize, u64), ResourceError> {
        if id.0 == 0 || bytes == 0 || !alignment.is_power_of_two() {
            return Err(ResourceError::InvalidRequest);
        }
        let bytes = bytes
            .checked_next_multiple_of(super::PAGE_SIZE)
            .ok_or(ResourceError::Capacity)?;
        let alignment = alignment.max(super::PAGE_SIZE);
        let resource = *self
            .resources
            .iter()
            .find(|r| r.id == id.0)
            .ok_or(ResourceError::Unavailable)?;
        let mut cursor = resource.range.start + resource.initialized;
        loop {
            let start = cursor
                .checked_add(alignment - 1)
                .ok_or(ResourceError::Capacity)?
                & !(alignment - 1);
            let range = PhysicalRange::new(start, bytes);
            if !resource.range.contains(range) {
                return Err(ResourceError::Capacity);
            }
            if let Some(blocker) = self
                .slots
                .iter()
                .find(|s| s.allocation.range.overlaps(range))
            {
                cursor = blocker
                    .allocation
                    .range
                    .end()
                    .ok_or(ResourceError::InvalidSnapshot)?;
                continue;
            }
            let (index, generation) = self.insert(AllocationSnapshot {
                resource_id: id.0,
                range,
            })?;
            return Ok((range, index, generation));
        }
    }

    pub fn release(&mut self, index: usize, generation: u64) -> Result<(), ResourceError> {
        let slot = self
            .slots
            .get_mut(index)
            .ok_or(ResourceError::InvalidSnapshot)?;
        if slot.generation != generation || slot.allocation.resource_id == 0 {
            return Err(ResourceError::InvalidSnapshot);
        }
        slot.allocation = AllocationSnapshot::default();
        Ok(())
    }

    pub fn retire(&mut self, range: PhysicalRange) -> Option<PhysicalRange> {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.allocation.range == range) {
            let resource_id = slot.allocation.resource_id;
            slot.allocation = AllocationSnapshot::default();
            self.resources
                .iter()
                .find(|r| r.id == resource_id)
                .map(|r| r.range)
        } else {
            None
        }
    }

    #[cfg(any(test, all(efi, target_arch = "loongarch64", feature = "efi-handoff")))]
    pub fn live(&self) -> [AllocationSnapshot; MAX_ALLOCATIONS] {
        core::array::from_fn(|i| self.slots[i].allocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_reuses_holes_retires_dead_stages_and_rejects_stale_releases() {
        let resource = ResourceSnapshot {
            id: 1,
            range: PhysicalRange::new(4096, 256 * 4096),
            initialized: 3,
        };
        let mut arenas = Arenas::new();
        arenas.restore(&[resource], &[]).unwrap();
        let (a, ai, ag) = arenas.allocate(ResourceId(1), 4096, 4096).unwrap();
        let (b, ..) = arenas.allocate(ResourceId(1), 4096, 4096).unwrap();
        arenas.release(ai, ag).unwrap();
        let (c, ci, cg) = arenas.allocate(ResourceId(1), 4096, 4096).unwrap();
        assert_eq!(a, c);
        assert!(arenas.release(ai, ag).is_err());
        arenas.release(ci, cg).unwrap();
        // Only b survives. The previous a/c allocation is available again.
        arenas
            .restore(
                &[resource],
                &[AllocationSnapshot {
                    resource_id: 1,
                    range: b,
                }],
            )
            .unwrap();
        assert_eq!(arenas.allocate(ResourceId(1), 4096, 4096).unwrap().0, a);
        assert!(arenas.retire(b).is_some());
        assert!(arenas.retire(b).is_none());
        for _ in 1..MAX_ALLOCATIONS {
            arenas.allocate(ResourceId(1), 1, 1).unwrap();
        }
        assert_eq!(
            arenas.allocate(ResourceId(1), 1, 1),
            Err(ResourceError::Capacity)
        );
        let survivor = AllocationSnapshot {
            resource_id: 1,
            range: b,
        };
        arenas.restore(&[], &[survivor]).unwrap();
        assert_eq!(
            arenas.allocate(ResourceId(1), 4096, 4096),
            Err(ResourceError::Unavailable)
        );
        assert!(arenas.live().contains(&survivor));
        let smaller = ResourceSnapshot {
            range: PhysicalRange::new(resource.range.start, 4096),
            initialized: 0,
            ..resource
        };
        arenas.restore(&[smaller], &[survivor]).unwrap();
        assert!(arenas.live().contains(&survivor));
        assert_eq!(
            arenas.allocate(ResourceId(1), 1, 1).unwrap().0,
            smaller.range
        );
        arenas.retire(b);
        assert!(!arenas.live().contains(&survivor));
        assert!(arenas.retire(PhysicalRange::default()).is_none());
    }
}

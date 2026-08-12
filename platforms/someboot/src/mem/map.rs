use core::{
    cell::UnsafeCell,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use heapless::Vec;

use super::{MemoryDescriptor, MemoryType};

pub(super) const MEMORY_MAP_CAPACITY: usize = 512;

/// BSP-owned memory-map state, published immutably before runtime entry.
pub(super) struct BootMemoryMap {
    state: UnsafeCell<BootMemoryMapState>,
    published: AtomicBool,
}

struct BootMemoryMapState {
    map: MemoryMap,
    frozen: bool,
}

// SAFETY: only the boot processor accesses the state while it is mutable.
// `freeze` runs before runtime entry and no mutation API accepts updates after
// that transition, so runtime CPUs can only observe the immutable slice.
unsafe impl Sync for BootMemoryMap {}

impl BootMemoryMap {
    pub(super) const fn new() -> Self {
        Self {
            state: UnsafeCell::new(BootMemoryMapState {
                map: MemoryMap::new(),
                frozen: false,
            }),
            published: AtomicBool::new(false),
        }
    }

    pub(super) fn with_slice<R>(&self, f: impl FnOnce(&[MemoryDescriptor]) -> R) -> R {
        // SAFETY: boot-time callers run only on the BSP. The callback cannot
        // return a reference tied to this temporary borrow, and runtime calls
        // happen only after the state is frozen.
        let state = unsafe { &*self.state.get() };
        f(state.map.as_slice())
    }

    pub(super) fn published_slice(&self) -> &[MemoryDescriptor] {
        // SAFETY: `freeze` permanently ends mutation before this public view is
        // made available to runtime consumers.
        assert!(
            self.published.load(Ordering::Acquire),
            "boot memory map is not frozen"
        );
        // SAFETY: the acquire load above observes the release publication at
        // the end of `freeze`, after which the map is immutable.
        let state = unsafe { &*self.state.get() };
        state.map.as_slice()
    }

    pub(super) fn sort_by_physical_start(&self) {
        self.with_mutable_state(|state| state.map.sort_by_physical_start())
            .expect("boot memory map is already frozen");
    }

    pub(super) fn insert(&self, descriptor: MemoryDescriptor) -> Result<(), MemoryMapError> {
        self.with_mutable_state(|state| state.map.insert(descriptor))?
    }

    pub(super) fn freeze(&self) {
        self.with_mutable_state(|state| {
            state.map.sort_by_physical_start();
            state.frozen = true;
        })
        .expect("boot memory map is already frozen");
        self.published.store(true, Ordering::Release);
    }

    fn with_mutable_state<R>(
        &self,
        f: impl FnOnce(&mut BootMemoryMapState) -> R,
    ) -> Result<R, MemoryMapError> {
        let state = self.state.get();
        // SAFETY: reading the phase is restricted to the boot processor until
        // freeze. Afterwards it is immutable for the system lifetime.
        if unsafe { (*state).frozen } {
            Err(MemoryMapError::Frozen)
        } else {
            // SAFETY: no published slice exists before freeze, and all mutable
            // access is restricted to the single boot processor.
            Ok(f(unsafe { &mut *state }))
        }
    }
}

/// The boot-time physical memory map.
pub(super) struct MemoryMap {
    descriptors: Vec<MemoryDescriptor, MEMORY_MAP_CAPACITY>,
}

impl MemoryMap {
    pub(super) const fn new() -> Self {
        Self {
            descriptors: Vec::new(),
        }
    }

    pub(super) fn as_slice(&self) -> &[MemoryDescriptor] {
        self.descriptors.as_slice()
    }

    pub(super) fn sort_by_physical_start(&mut self) {
        self.descriptors
            .sort_by_key(|descriptor| descriptor.physical_start);
    }

    pub(super) fn insert(&mut self, new: MemoryDescriptor) -> Result<(), MemoryMapError> {
        let new_range = descriptor_range(&new)?;
        let resulting_len = self.validate_insert(&new, &new_range)?;
        if resulting_len > self.descriptors.capacity() {
            return Err(MemoryMapError::Capacity);
        }

        self.apply_insert(new, new_range);
        self.merge_same_type();
        Ok(())
    }

    fn validate_insert(
        &self,
        new: &MemoryDescriptor,
        new_range: &Range<usize>,
    ) -> Result<usize, MemoryMapError> {
        let mut resulting_len = self.descriptors.len() + 1;

        for existing in &self.descriptors {
            let existing_range = descriptor_range(existing)?;
            if !ranges_overlap(new_range, &existing_range) {
                continue;
            }
            if existing.memory_type != MemoryType::Free && existing.memory_type != new.memory_type {
                return Err(MemoryMapError::Conflict {
                    new: new.clone(),
                    existing: existing.clone(),
                });
            }

            resulting_len = resulting_len - 1 + retained_fragment_count(new_range, &existing_range);
        }

        Ok(resulting_len)
    }

    fn apply_insert(&mut self, new: MemoryDescriptor, new_range: Range<usize>) {
        let mut index = 0;
        while index < self.descriptors.len() {
            let existing_range = descriptor_range(&self.descriptors[index])
                .expect("memory descriptors were validated before insertion");
            if !ranges_overlap(&new_range, &existing_range) {
                index += 1;
                continue;
            }

            let existing = self.descriptors.remove(index);
            if existing_range.start < new_range.start {
                let left_end = new_range.start.min(existing_range.end);
                self.descriptors
                    .insert(
                        index,
                        descriptor_with_range(&existing, existing_range.start..left_end),
                    )
                    .expect("memory map capacity was validated before insertion");
                index += 1;
            }
            if existing_range.end > new_range.end {
                let right_start = new_range.end.max(existing_range.start);
                self.descriptors
                    .insert(
                        index,
                        descriptor_with_range(&existing, right_start..existing_range.end),
                    )
                    .expect("memory map capacity was validated before insertion");
                index += 1;
            }
        }

        self.descriptors
            .push(new)
            .expect("memory map capacity was validated before insertion");
    }

    fn merge_same_type(&mut self) {
        self.sort_by_physical_start();
        let mut index = 0;
        while index + 1 < self.descriptors.len() {
            let current = &self.descriptors[index];
            let next = &self.descriptors[index + 1];
            let current_end = current
                .checked_end()
                .expect("memory descriptors were validated before merging");

            if current.memory_type == next.memory_type && current_end >= next.physical_start {
                let next = self.descriptors.remove(index + 1);
                let current = &mut self.descriptors[index];
                let next_end = next
                    .checked_end()
                    .expect("memory descriptors were validated before merging");
                let merged_end = current_end.max(next_end);
                current.size_in_bytes = merged_end - current.physical_start;
            } else {
                index += 1;
            }
        }
    }
}

/// An error encountered while updating the boot memory map.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum MemoryMapError {
    /// Runtime publication has permanently ended boot-time updates.
    #[error("boot memory map is already frozen")]
    Frozen,

    /// The fixed-capacity map cannot hold all resulting descriptors.
    #[error("boot memory map capacity exceeded")]
    Capacity,

    /// The descriptor overlaps memory that cannot be replaced.
    #[error("new descriptor {new:?} conflicts with existing descriptor {existing:?}")]
    Conflict {
        /// The descriptor being inserted.
        new: MemoryDescriptor,
        /// The descriptor that prevents insertion.
        existing: MemoryDescriptor,
    },

    /// A descriptor's end address cannot be represented by `usize`.
    #[error("memory descriptor range overflows: {descriptor:?}")]
    InvalidRange {
        /// The invalid descriptor.
        descriptor: MemoryDescriptor,
    },
}

fn descriptor_range(descriptor: &MemoryDescriptor) -> Result<Range<usize>, MemoryMapError> {
    let Some(end) = descriptor.checked_end() else {
        return Err(MemoryMapError::InvalidRange {
            descriptor: descriptor.clone(),
        });
    };
    Ok(descriptor.physical_start..end)
}

fn descriptor_with_range(descriptor: &MemoryDescriptor, range: Range<usize>) -> MemoryDescriptor {
    MemoryDescriptor::new_with_range(range, descriptor.memory_type)
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && left.end > right.start
}

fn retained_fragment_count(new: &Range<usize>, existing: &Range<usize>) -> usize {
    usize::from(existing.start < new.start) + usize::from(existing.end > new.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(range: Range<usize>, memory_type: MemoryType) -> MemoryDescriptor {
        MemoryDescriptor::new_with_range(range, memory_type)
    }

    #[test]
    fn reserved_memory_splits_free_memory() {
        let mut map = MemoryMap::new();
        map.insert(descriptor(0..30, MemoryType::Free)).unwrap();

        map.insert(descriptor(10..20, MemoryType::Reserved))
            .unwrap();

        assert_eq!(
            map.as_slice(),
            &[
                descriptor(0..10, MemoryType::Free),
                descriptor(10..20, MemoryType::Reserved),
                descriptor(20..30, MemoryType::Free),
            ]
        );
    }

    #[test]
    fn conflicting_insert_preserves_memory_map() {
        let mut map = MemoryMap::new();
        map.insert(descriptor(0..10, MemoryType::Free)).unwrap();
        map.insert(descriptor(10..20, MemoryType::Reserved))
            .unwrap();
        let original = map.as_slice().to_vec();

        let result = map.insert(descriptor(5..15, MemoryType::Mmio));

        assert!(matches!(result, Err(MemoryMapError::Conflict { .. })));
        assert_eq!(map.as_slice(), original);
    }

    #[test]
    fn capacity_failure_preserves_memory_map() {
        let mut map = MemoryMap::new();
        for index in 0..(MEMORY_MAP_CAPACITY - 1) {
            let start = index * 2;
            map.insert(descriptor(start..start + 1, MemoryType::Reserved))
                .unwrap();
        }
        map.insert(descriptor(2000..2030, MemoryType::Free))
            .unwrap();
        let original = map.as_slice().to_vec();

        let result = map.insert(descriptor(2010..2020, MemoryType::Mmio));

        assert_eq!(result, Err(MemoryMapError::Capacity));
        assert_eq!(map.as_slice(), original);
    }

    #[test]
    fn adjacent_descriptors_of_same_type_are_merged() {
        let mut map = MemoryMap::new();
        map.insert(descriptor(10..20, MemoryType::Free)).unwrap();
        map.insert(descriptor(0..10, MemoryType::Free)).unwrap();

        assert_eq!(map.as_slice(), &[descriptor(0..20, MemoryType::Free)]);
    }

    #[test]
    fn frozen_boot_memory_map_rejects_updates() {
        let map = BootMemoryMap::new();
        map.insert(descriptor(0..10, MemoryType::Free)).unwrap();

        map.freeze();

        assert_eq!(
            map.insert(descriptor(10..20, MemoryType::Reserved)),
            Err(MemoryMapError::Frozen)
        );
        assert_eq!(
            map.published_slice(),
            &[descriptor(0..10, MemoryType::Free)]
        );
    }
}

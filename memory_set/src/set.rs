use alloc::{collections::BTreeMap, vec::Vec};
use core::fmt;

use memory_addr::{VirtAddr, VirtAddrRange};

use crate::{MappingBackend, MappingError, MappingResult, MemoryArea};

/// A container that maintains memory mappings ([`MemoryArea`]).
pub struct MemorySet<F: Copy, P, B: MappingBackend<F, P>> {
    areas: BTreeMap<VirtAddr, MemoryArea<F, P, B>>,
}

impl<F: Copy, P, B: MappingBackend<F, P>> MemorySet<F, P, B> {
    /// Creates a new memory set.
    pub const fn new() -> Self {
        Self {
            areas: BTreeMap::new(),
        }
    }

    /// Returns the number of memory areas in the memory set.
    pub fn len(&self) -> usize {
        self.areas.len()
    }

    /// Returns `true` if the memory set contains no memory areas.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }

    /// Returns the iterator over all memory areas.
    pub fn iter(&self) -> impl Iterator<Item = &MemoryArea<F, P, B>> {
        self.areas.values()
    }

    /// Returns whether the given address range overlaps with any existing area.
    pub fn overlaps(&self, range: VirtAddrRange) -> bool {
        if let Some((_, before)) = self.areas.range(..range.start).last() {
            if before.va_range().overlaps(range) {
                return true;
            }
        }
        if let Some((_, after)) = self.areas.range(range.start..).next() {
            if after.va_range().overlaps(range) {
                return true;
            }
        }
        false
    }

    /// Finds the memory area that contains the given address.
    pub fn find(&self, addr: VirtAddr) -> Option<&MemoryArea<F, P, B>> {
        let candidate = self.areas.range(..=addr).last().map(|(_, a)| a);
        candidate.filter(|a| a.va_range().contains(addr))
    }

    /// Finds a free area that can accommodate the given size.
    ///
    /// The search starts from the given `hint` address, and the area should be
    /// within the given `limit` range.
    ///
    /// Returns the start address of the free area. Returns `None` if no such
    /// area is found.
    pub fn find_free_area(
        &self,
        hint: VirtAddr,
        size: usize,
        limit: VirtAddrRange,
    ) -> Option<VirtAddr> {
        // brute force: try each area's end address as the start.
        let mut last_end = hint.max(limit.start);
        for (addr, area) in self.areas.iter() {
            if last_end + size <= *addr {
                return Some(last_end);
            }
            last_end = area.end();
        }
        if last_end + size <= limit.end {
            Some(last_end)
        } else {
            None
        }
    }

    /// Add a new memory mapping.
    ///
    /// The mapping is represented by a [`MemoryArea`].
    ///
    /// If the new area overlaps with any existing area, the behavior is
    /// determined by the `unmap_overlap` parameter. If it is `true`, the
    /// overlapped regions will be unmapped first. Otherwise, it returns an
    /// error.
    pub fn map(
        &mut self,
        area: MemoryArea<F, P, B>,
        page_table: &mut P,
        unmap_overlap: bool,
    ) -> MappingResult {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }

        if self.overlaps(area.va_range()) {
            if unmap_overlap {
                self.unmap(area.start(), area.size(), page_table)?;
            } else {
                return Err(MappingError::AlreadyExists);
            }
        }

        area.map_area(page_table)?;
        assert!(self.areas.insert(area.start(), area).is_none());
        Ok(())
    }

    /// Remove memory mappings within the given address range.
    ///
    /// All memory areas that are fully contained in the range will be removed
    /// directly. If the area intersects with the boundary, it will be shrinked.
    /// If the unmapped range is in the middle of an existing area, it will be
    /// split into two areas.
    pub fn unmap(&mut self, start: VirtAddr, size: usize, page_table: &mut P) -> MappingResult {
        let range = VirtAddrRange::from_start_size(start, size);
        let end = range.end;
        if range.is_empty() {
            return Ok(());
        }

        // Unmap entire areas that are contained by the range.
        self.areas.retain(|_, area| {
            if area.va_range().contained_in(range) {
                area.unmap_area(page_table).unwrap();
                false
            } else {
                true
            }
        });

        // Shrink right if the area intersects with the left boundary.
        if let Some((before_start, before)) = self.areas.range_mut(..start).last() {
            let before_end = before.end();
            if before_end > start {
                if before_end <= end {
                    // the unmapped area is at the end of `before`.
                    before.shrink_right(start.as_usize() - before_start.as_usize(), page_table)?;
                } else {
                    // the unmapped area is in the middle `before`, need to split.
                    let right_part = before.split(end).unwrap();
                    before.shrink_right(start.as_usize() - before_start.as_usize(), page_table)?;
                    assert_eq!(right_part.start(), end);
                    self.areas.insert(end, right_part);
                }
            }
        }

        // Shrink left if the area intersects with the right boundary.
        if let Some((&after_start, after)) = self.areas.range_mut(start..).next() {
            let after_end = after.end();
            if after_start < end {
                // the unmapped area is at the start of `after`.
                let mut new_area = self.areas.remove(&after_start).unwrap();
                new_area.shrink_left(after_end.as_usize() - end.as_usize(), page_table)?;
                assert_eq!(new_area.start(), end);
                self.areas.insert(end, new_area);
            }
        }

        Ok(())
    }

    /// Remove all memory areas and the underlying mappings.
    pub fn clear(&mut self, page_table: &mut P) -> MappingResult {
        for (_, area) in self.areas.iter() {
            area.unmap_area(page_table)?;
        }
        self.areas.clear();
        Ok(())
    }

    /// Change the flags of memory mappings within the given address range.
    ///
    /// `update_flags` is a function that receives old flags and processes
    /// new flags (e.g., some flags can not be changed through this interface).
    /// It returns [`None`] if there is no bit to change.
    ///
    /// Memory areas will be skipped according to `update_flags`. Memory areas
    /// that are fully contained in the range or contains the range or intersects
    /// with the boundary will be handled similarly to `munmap`.
    pub fn protect(
        &mut self,
        start: VirtAddr,
        size: usize,
        update_flags: impl Fn(F) -> Option<F>,
        page_table: &mut P,
    ) -> MappingResult {
        let end = start + size;
        let mut to_insert = Vec::new();
        for (_, area) in self.areas.iter_mut() {
            if let Some(new_flags) = update_flags(area.flags()) {
                if area.start() >= end {
                    // [ prot ]
                    //          [ area ]
                    break;
                } else if area.end() <= start {
                    //          [ prot ]
                    // [ area ]
                    // Do nothing
                } else if area.start() >= start && area.end() <= end {
                    // [   prot   ]
                    //   [ area ]
                    area.protect_area(new_flags, page_table)?;
                    area.set_flags(new_flags);
                } else if area.start() < start && area.end() > end {
                    //        [ prot ]
                    // [ left | area | right ]
                    let right_part = area.split(end).unwrap();
                    area.set_end(start);

                    let mut middle_part =
                        MemoryArea::new(start, size, area.flags(), area.backend().clone());
                    middle_part.protect_area(new_flags, page_table)?;
                    middle_part.set_flags(new_flags);

                    to_insert.push((right_part.start(), right_part));
                    to_insert.push((middle_part.start(), middle_part));
                } else if area.end() > end {
                    // [    prot ]
                    //   [  area | right ]
                    let right_part = area.split(end).unwrap();
                    area.protect_area(new_flags, page_table)?;
                    area.set_flags(new_flags);

                    to_insert.push((right_part.start(), right_part));
                } else {
                    //        [ prot    ]
                    // [ left |  area ]
                    let mut right_part = area.split(start).unwrap();
                    right_part.protect_area(new_flags, page_table)?;
                    right_part.set_flags(new_flags);

                    to_insert.push((right_part.start(), right_part));
                }
            }
        }
        self.areas.extend(to_insert.into_iter());
        Ok(())
    }
}

impl<F: Copy + fmt::Debug, P, B: MappingBackend<F, P>> fmt::Debug for MemorySet<F, P, B> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.areas.values()).finish()
    }
}

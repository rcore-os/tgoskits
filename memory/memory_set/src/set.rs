use alloc::collections::BTreeMap;
#[allow(unused_imports)] // this is a weird false alarm
use alloc::vec::Vec;
use core::fmt;

use ax_memory_addr::{AddrRange, MemoryAddr};

use crate::{MappingBackend, MappingError, MappingResult, MemoryArea};

/// A container that maintains memory mappings ([`MemoryArea`]).
pub struct MemorySet<B: MappingBackend> {
    areas: BTreeMap<B::Addr, MemoryArea<B>>,
}

impl<B: MappingBackend> MemorySet<B> {
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
    pub fn iter(&self) -> impl Iterator<Item = &MemoryArea<B>> {
        self.areas.values()
    }

    /// Returns whether the given address range overlaps with any existing area.
    pub fn overlaps(&self, range: AddrRange<B::Addr>) -> bool {
        if let Some((_, before)) = self.areas.range(..range.start).last()
            && before.va_range().overlaps(range)
        {
            return true;
        }
        if let Some((_, after)) = self.areas.range(range.start..).next()
            && after.va_range().overlaps(range)
        {
            return true;
        }
        false
    }

    /// Finds the memory area that contains the given address.
    pub fn find(&self, addr: B::Addr) -> Option<&MemoryArea<B>> {
        let candidate = self.areas.range(..=addr).last().map(|(_, a)| a);
        candidate.filter(|a| a.va_range().contains(addr))
    }

    /// Finds a free area that can accommodate the given size.
    ///
    /// The search starts from the given `hint` address, and the area should be
    /// within the given `limit` range.
    ///
    /// # Notes
    /// The `align` parameter specifies the alignment of the start address and
    /// the size of the area. The start address of the resulting area will
    /// be aligned to this value. Also, the size of the area must be a multiple
    /// of this value.
    ///
    /// # Returns
    /// Returns the start address of the free area. Returns `None` if no such
    /// area is found.
    pub fn find_free_area(
        &self,
        hint: B::Addr,
        size: usize,
        limit: AddrRange<B::Addr>,
        align: usize,
    ) -> Option<B::Addr> {
        if !size.is_multiple_of(align) {
            // size must be a multiple of align.
            return None;
        }
        // brute force: try each area's end address as the start.
        let mut last_end: <B as MappingBackend>::Addr = hint.max(limit.start).align_up(align);
        if let Some((_, area)) = self.areas.range(..last_end).last() {
            last_end = last_end.max(area.end()).align_up(align);
        }
        for (&addr, area) in self.areas.range(last_end..) {
            if last_end.checked_add(size).is_some_and(|end| end <= addr) {
                return Some(last_end);
            }
            last_end = area.end().align_up(align);
        }
        if last_end
            .checked_add(size)
            .is_some_and(|end| end <= limit.end)
        {
            Some(last_end)
        } else {
            None
        }
    }

    /// Grows the area containing `addr` by `additional_size` at its end.
    pub fn extend_area(
        &mut self,
        addr: B::Addr,
        additional_size: usize,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        if additional_size == 0 {
            return Ok(());
        }

        // Find the area containing addr.
        let area_start = self
            .areas
            .range(..=addr)
            .last()
            .filter(|(_, a)| a.va_range().contains(addr))
            .map(|(&start, _)| start)
            .ok_or(MappingError::InvalidParam)?;

        // Only the next area can conflict with a rightward extension.
        let area_end = self.areas[&area_start].end();
        let new_end = area_end
            .checked_add(additional_size)
            .ok_or(MappingError::InvalidParam)?;
        if let Some((_, next)) = self.areas.range(area_end..).next()
            && new_end > next.start()
        {
            return Err(MappingError::AlreadyExists);
        }

        self.areas.get_mut(&area_start).unwrap().grow_right(
            additional_size,
            context,
            page_table,
        )?;
        Ok(())
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
        area: MemoryArea<B>,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
        unmap_overlap: bool,
    ) -> MappingResult {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }

        if self.overlaps(area.va_range()) {
            if unmap_overlap {
                self.unmap(area.start(), area.size(), context, page_table)?;
            } else {
                return Err(MappingError::AlreadyExists);
            }
        }

        area.map_area(context, page_table)?;
        assert!(self.areas.insert(area.start(), area).is_none());
        Ok(())
    }

    /// Remove memory mappings within the given address range.
    ///
    /// All memory areas that are fully contained in the range will be removed
    /// directly. If the area intersects with the boundary, it will be shrinked.
    /// If the unmapped range is in the middle of an existing area, it will be
    /// split into two areas.
    pub fn unmap(
        &mut self,
        start: B::Addr,
        size: usize,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        let range =
            AddrRange::try_from_start_size(start, size).ok_or(MappingError::InvalidParam)?;
        if range.is_empty() {
            return Ok(());
        }

        self.validate_unmap(start, size, page_table)?;

        // Publish every backend transition before changing any owner metadata.
        // A later backend may still report resource pressure after an earlier
        // one removed PTEs. Keeping the complete VMA set makes that state
        // retryable and, more importantly, retains every backend until the
        // caller's invalidation transaction has confirmed stale translations.
        for area in self.areas.values() {
            if area.start() >= range.end {
                break;
            }
            if area.end() <= range.start {
                continue;
            }
            let unmap_start = area.start().max(range.start);
            let unmap_end = area.end().min(range.end);
            area.unmap_range(
                unmap_start,
                unmap_end.sub_addr(unmap_start),
                context,
                page_table,
            )?;
        }

        // Validation above proves the range shape. This metadata-only commit
        // performs no backend operation and cannot fail due to page-table or
        // resource state.
        self.unmap_metadata(start, size)
    }

    /// Preflights every backend touched by an unmap without changing state.
    pub fn validate_unmap(
        &self,
        start: B::Addr,
        size: usize,
        page_table: &B::PageTable,
    ) -> MappingResult {
        let range =
            AddrRange::try_from_start_size(start, size).ok_or(MappingError::InvalidParam)?;
        if range.is_empty() {
            return Ok(());
        }

        // Reject predictable mapping-shape and ownership failures before the
        // first PTE is removed. Commit still retains every backend owner until
        // all disjoint subranges complete or the caller quarantines a partial
        // published mutation.
        for area in self.areas.values() {
            if area.start() >= range.end {
                break;
            }
            if area.end() <= range.start {
                continue;
            }
            let unmap_start = area.start().max(range.start);
            let unmap_end = area.end().min(range.end);
            area.validate_unmap_range(unmap_start, unmap_end.sub_addr(unmap_start), page_table)?;
        }
        Ok(())
    }

    /// Remove memory area metadata without calling the backend's unmap hook.
    ///
    /// This is intended for callers that have already moved or detached the
    /// affected page-table entries and only need to update VMA bookkeeping.
    pub fn unmap_metadata(&mut self, start: B::Addr, size: usize) -> MappingResult {
        let range =
            AddrRange::try_from_start_size(start, size).ok_or(MappingError::InvalidParam)?;
        if range.is_empty() {
            return Ok(());
        }

        let end = range.end;

        self.areas
            .retain(|_, area| !area.va_range().contained_in(range));

        if let Some((&before_start, before)) = self.areas.range_mut(..start).last() {
            let before_end = before.end();
            if before_end > start {
                if before_end <= end {
                    before.shrink_right_metadata(start.sub_addr(before_start));
                } else {
                    let right_part = before.split(end).unwrap();
                    before.shrink_right_metadata(start.sub_addr(before_start));
                    assert_eq!(right_part.start().into(), Into::<usize>::into(end));
                    self.areas.insert(end, right_part);
                }
            }
        }

        if let Some((&after_start, _)) = self.areas.range(start..).next()
            && after_start < end
        {
            let mut new_area = self.areas.remove(&after_start).unwrap();
            let after_end = new_area.end();
            new_area.shrink_left_metadata(after_end.sub_addr(end));
            assert_eq!(new_area.start().into(), Into::<usize>::into(end));
            self.areas.insert(end, new_area);
        }

        Ok(())
    }

    /// Validates that `area` can replace one contained metadata range.
    pub fn validate_area_metadata_replacement(&self, area: &MemoryArea<B>) -> MappingResult {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }

        let start = area.start();
        let end = area.end();

        self.areas
            .range(..=start)
            .last()
            .filter(|(_, old)| old.start() <= start && end <= old.end())
            .map(|_| ())
            .ok_or(MappingError::InvalidParam)
    }

    /// Replaces area metadata without touching page-table entries.
    pub fn replace_area_metadata(&mut self, area: MemoryArea<B>) -> MappingResult {
        self.validate_area_metadata_replacement(&area)?;

        let start = area.start();
        let end = area.end();
        let old_start = self
            .areas
            .range(..=start)
            .next_back()
            .map(|(&old_start, _)| old_start)
            .expect("validated metadata replacement lost its containing area");

        let mut old_area = self.areas.remove(&old_start).unwrap();
        if old_start < start {
            let right_part = old_area.split(start).unwrap();
            self.areas.insert(old_start, old_area);
            old_area = right_part;
        }
        if old_area.end() > end {
            let right_part = old_area.split(end).unwrap();
            self.areas.insert(right_part.start(), right_part);
        }
        assert!(self.areas.insert(start, area).is_none());
        Ok(())
    }

    /// Remove all memory areas and the underlying mappings.
    pub fn clear(
        &mut self,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        for area in self.areas.values() {
            area.validate_unmap_range(area.start(), area.size(), page_table)?;
        }
        for area in self.areas.values() {
            area.unmap_area(context, page_table)?;
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
    /// that are fully contained in the range or contains the range or
    /// intersects with the boundary will be handled similarly to `munmap`.
    pub fn protect(
        &mut self,
        start: B::Addr,
        size: usize,
        update_flags: impl Fn(B::Flags) -> Option<B::Flags>,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        self.protect_with_reported_flags(
            start,
            size,
            |flags, _reported_flags| update_flags(flags).map(|new_flags| (new_flags, new_flags)),
            context,
            page_table,
        )
    }

    /// Change backend/page-table flags and reported flags within the given range.
    pub fn protect_with_reported_flags(
        &mut self,
        start: B::Addr,
        size: usize,
        update_flags: impl Fn(B::Flags, B::Flags) -> Option<(B::Flags, B::Flags)>,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        let mut operations = Vec::new();
        for (&area_start, area) in &self.areas {
            let area_end = area.end();
            if area_start >= end {
                break;
            }
            if area_end <= start {
                continue;
            }
            if let Some((new_flags, new_reported_flags)) =
                update_flags(area.flags(), area.reported_flags())
            {
                let protect_start = area_start.max(start);
                let protect_end = area_end.min(end);
                operations.push((
                    area_start,
                    protect_start,
                    protect_end,
                    area.flags(),
                    new_flags,
                    new_reported_flags,
                ));
            }
        }

        // Page-table/backend work is the fallible prepare phase. Metadata is
        // not split until every range succeeds. If a later range fails, roll
        // back every attempted range while the original area topology still
        // identifies the same backends.
        for (index, &(area_start, protect_start, protect_end, _, new_flags, _)) in
            operations.iter().enumerate()
        {
            let result = self.areas[&area_start].protect_range(
                protect_start,
                protect_end.sub_addr(protect_start),
                new_flags,
                context,
                page_table,
            );
            if let Err(error) = result {
                for &(rollback_area_start, rollback_start, rollback_end, old_flags, ..) in
                    operations[..=index].iter().rev()
                {
                    self.areas[&rollback_area_start]
                        .protect_range(
                            rollback_start,
                            rollback_end.sub_addr(rollback_start),
                            old_flags,
                            context,
                            page_table,
                        )
                        .expect(
                            "a failed protection prepare must remain rollbackable before metadata \
                             commit",
                        );
                }
                return Err(error);
            }
        }

        // All fallible work is complete. Commit VMA splits and flags without
        // touching the backend or page table again.
        let mut to_insert = Vec::new();
        for (area_start, protect_start, protect_end, _, new_flags, new_reported_flags) in operations
        {
            let area = self.areas.get_mut(&area_start).unwrap();
            let area_end = area.end();
            if protect_start == area_start && protect_end == area_end {
                area.set_flags_with_reported_flags(new_flags, new_reported_flags);
            } else if area_start < protect_start && protect_end < area_end {
                let mut middle_part = area.split(protect_start).unwrap();
                let right_part = middle_part.split(protect_end).unwrap();
                middle_part.set_flags_with_reported_flags(new_flags, new_reported_flags);
                to_insert.push((right_part.start(), right_part));
                to_insert.push((middle_part.start(), middle_part));
            } else if protect_start == area_start {
                let right_part = area.split(protect_end).unwrap();
                area.set_flags_with_reported_flags(new_flags, new_reported_flags);
                to_insert.push((right_part.start(), right_part));
            } else {
                debug_assert!(protect_end == area_end);
                let mut right_part = area.split(protect_start).unwrap();
                right_part.set_flags_with_reported_flags(new_flags, new_reported_flags);
                to_insert.push((right_part.start(), right_part));
            }
        }
        self.areas.extend(to_insert);
        Ok(())
    }
}

impl<B: MappingBackend> Default for MemorySet<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: MappingBackend> fmt::Debug for MemorySet<B>
where
    B::Addr: fmt::Debug,
    B::Flags: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.areas.values()).finish()
    }
}

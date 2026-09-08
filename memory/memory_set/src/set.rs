use alloc::{collections::BTreeMap, vec::Vec};
use core::fmt;

use ax_memory_addr::{AddrRange, MemoryAddr};

use crate::{MappingBackend, MappingError, MappingResult, MemoryArea};

/// Reinstalls the portions of a preimage that were removed by an overlapping
/// map.  This is deliberately backend-driven: `MemorySet` does not assume
/// that physical pages are contiguous or that a page-table clone is
/// available.  A `false` result means the materialized state is indeterminate
/// and callers must quarantine/repair the range.
fn restore_overlapped_mappings<B: MappingBackend>(
    old: &BTreeMap<B::Addr, MemoryArea<B>>,
    range: AddrRange<B::Addr>,
    context: &mut B::MutationContext,
    page_table: &mut B::PageTable,
) -> bool {
    for area in old.values() {
        if area.start() >= range.end {
            break;
        }
        if area.end() <= range.start {
            continue;
        }
        let start = area.start().max(range.start);
        let end = area.end().min(range.end);
        let Some(fragment) = AddrRange::try_new(start, end) else {
            return false;
        };
        if !area.backend().map(
            fragment.start,
            fragment.size(),
            area.flags(),
            context,
            page_table,
        ) {
            return false;
        }
    }
    true
}

/// A container that maintains memory mappings ([`MemoryArea`]).
#[derive(Clone)]
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

    /// Restores a metadata preimage after the caller has reverted every
    /// materialized PTE and backend ownership change.
    ///
    /// This method intentionally does not touch the page table: callers must
    /// first prove that the current mapping was detached and that every old
    /// leaf/frame reference was restored.  Keeping that ordering explicit
    /// prevents a metadata rollback from masquerading as a complete rollback.
    pub fn restore_metadata_preimage(&mut self, preimage: Self) {
        *self = preimage;
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
        // `MemoryAddr::align_up` is intentionally a low-level, infallible
        // primitive.  This public allocator-facing API must reject malformed
        // alignment values before calling it; otherwise `align == 0` underflows
        // and an address near `usize::MAX` can wrap into the search range.
        if align == 0 || !align.is_power_of_two() || size == 0 || limit.start >= limit.end {
            return None;
        }
        if !size.is_multiple_of(align) {
            // size must be a multiple of align.
            return None;
        }
        // brute force: try each area's end address as the start.
        let align_up = |address: B::Addr| {
            address
                .into()
                .checked_add(align - 1)
                .map(|value| B::Addr::from(value & !(align - 1)))
        };
        let mut last_end: <B as MappingBackend>::Addr = align_up(hint.max(limit.start))?;
        if last_end < limit.start || last_end >= limit.end {
            return None;
        }
        if let Some((_, area)) = self.areas.range(..last_end).last() {
            last_end = align_up(last_end.max(area.end()))?;
            if last_end >= limit.end {
                return None;
            }
        }
        for (&addr, area) in self.areas.range(last_end..) {
            if addr >= limit.end {
                break;
            }
            if last_end.checked_add(size).is_some_and(|end| end <= addr) {
                if last_end
                    .checked_add(size)
                    .is_some_and(|end| end <= limit.end)
                {
                    return Some(last_end);
                }
                return None;
            }
            last_end = align_up(area.end().max(limit.start))?;
            if last_end >= limit.end {
                return None;
            }
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

        self.areas
            .get_mut(&area_start)
            .ok_or(MappingError::BadState)?
            .grow_right(additional_size, context, page_table)?;
        Ok(())
    }

    /// Reverts a successful [`Self::extend_area`] before its surrounding
    /// mutation is published.  The newly materialized suffix is unmapped
    /// first, then the metadata is shortened.  A backend is allowed to report
    /// that only a prefix was unmapped; in that case the caller receives
    /// `NeedsRepair` and must not pretend that the preimage was restored.
    pub fn rollback_extend_area(
        &mut self,
        addr: B::Addr,
        additional_size: usize,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        if additional_size == 0 {
            return Ok(());
        }
        let area_start = self
            .areas
            .range(..=addr)
            .last()
            .filter(|(_, area)| area.va_range().contains(addr))
            .map(|(&start, _)| start)
            .ok_or(MappingError::InvalidParam)?;
        let area = self.areas.get(&area_start).ok_or(MappingError::BadState)?;
        if additional_size >= area.size() {
            return Err(MappingError::InvalidParam);
        }
        let suffix_start = area
            .end()
            .checked_sub(additional_size)
            .ok_or(MappingError::InvalidParam)?;
        let backend = area.backend().clone();
        if !backend.validate_unmap(suffix_start, additional_size, page_table) {
            return Err(MappingError::BadState);
        }
        if !backend.unmap(suffix_start, additional_size, context, page_table) {
            return Err(MappingError::NeedsRepair);
        }
        let area = self
            .areas
            .get_mut(&area_start)
            .ok_or(MappingError::BadState)?;
        let old_size = area.size();
        area.shrink_right_metadata(old_size - additional_size)
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

        let overlaps = self.overlaps(area.va_range());
        let backup = overlaps.then(|| self.areas.clone());
        if overlaps {
            if unmap_overlap {
                self.unmap(area.start(), area.size(), context, page_table)?;
            } else {
                return Err(MappingError::AlreadyExists);
            }
        } else {
            // Give the backend a read-only chance to reject a fresh mapping
            // before any PTE is written.  Overlapping MAP_FIXED replacement
            // intentionally skips this check because the existing leaves are
            // expected to be present until the unmap phase above completes.
            area.validate_map(page_table)?;
        }

        let area_start = area.start();
        let area_size = area.size();
        let area_backend = area.backend().clone();
        if let Err(error) = area.map_area(context, page_table) {
            // `map` is allowed to fail after writing a prefix.  Try the
            // backend's inverse first; if that cannot prove a complete
            // rollback, preserve the explicit NeedsRepair state instead of
            // returning an ordinary error with a dangling PTE.
            let reverted_new = area_backend.unmap(area_start, area_size, context, page_table);
            let restored_old = backup.as_ref().is_none_or(|old| {
                restore_overlapped_mappings(old, area.va_range(), context, page_table)
            });
            if !reverted_new || !restored_old {
                if let Some(old) = backup {
                    self.areas = old;
                }
                return Err(MappingError::NeedsRepair);
            }
            if let Some(old) = backup {
                self.areas = old;
            }
            return Err(error);
        }

        if self.areas.insert(area_start, area).is_some() {
            // This should be impossible after the overlap removal, but avoid
            // an assertion in a recovery path.  Restore both the newly mapped
            // range and the old metadata if an allocator/tree invariant is
            // violated.
            let reverted_new = area_backend.unmap(area_start, area_size, context, page_table);
            let restored_old = backup.as_ref().is_none_or(|old| {
                restore_overlapped_mappings(
                    old,
                    AddrRange::from_start_size(area_start, area_size),
                    context,
                    page_table,
                )
            });
            if let Some(old) = backup {
                self.areas = old;
            }
            return Err(if reverted_new && restored_old {
                MappingError::BadState
            } else {
                MappingError::NeedsRepair
            });
        }
        Ok(())
    }

    /// Publishes metadata without invoking the backend's mapping operation.
    ///
    /// The caller either owns an unpublished address space with prepared PTEs,
    /// or retains a reservation token that retires partially installed leaves
    /// if the subsequent page-table apply fails.
    ///
    /// This is intentionally separate from [`Self::map`]: replaying `map`
    /// after a fork clone has installed child PTEs would reject those exact
    /// leaves as an overlap, while silently skipping the normal map preflight
    /// would weaken every ordinary caller. The caller must retain rollback
    /// ownership for the prepared backend state until this insertion and its
    /// surrounding address-space publication complete.
    pub fn insert_prepared_area(&mut self, area: MemoryArea<B>) -> MappingResult {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }
        if self.overlaps(area.va_range()) {
            return Err(MappingError::AlreadyExists);
        }
        if self.areas.insert(area.start(), area).is_some() {
            return Err(MappingError::BadState);
        }
        Ok(())
    }

    /// Replaces the backend of one exact area without allocating or touching
    /// its materialized page-table state.
    ///
    /// This is used by typed owners that need to publish a lifecycle state
    /// transition (for example `Present -> Quarantined`) before a TLB
    /// acknowledgement. Requiring an exact range prevents a backend that does
    /// not support split ownership from being installed on a fragment.
    pub fn replace_exact_backend(
        &mut self,
        start: B::Addr,
        size: usize,
        backend: B,
    ) -> MappingResult<B> {
        let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        let area = self
            .areas
            .get_mut(&start)
            .filter(|area| area.end() == end)
            .ok_or(MappingError::InvalidParam)?;
        Ok(area.replace_backend(backend))
    }

    /// Removes one exact area without constructing the general unmap
    /// operation vector.
    ///
    /// The exact form is useful in allocation-free retire paths whose owner
    /// already proved that the mapping cannot be split. Metadata is removed
    /// only after the backend has detached every materialized entry. The
    /// returned owner lets the caller release resources outside its lock.
    pub fn unmap_exact(
        &mut self,
        start: B::Addr,
        size: usize,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult<MemoryArea<B>> {
        let end = start.checked_add(size).ok_or(MappingError::InvalidParam)?;
        let area = self
            .areas
            .get(&start)
            .filter(|area| area.end() == end)
            .ok_or(MappingError::InvalidParam)?;
        if area.validate_unmap_range(start, size, page_table).is_err() {
            return Err(MappingError::BadState);
        }
        let backend = area.backend().clone();
        if !backend.unmap(start, size, context, page_table) {
            return Err(MappingError::BadState);
        }
        self.areas.remove(&start).ok_or(MappingError::BadState)
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
        let prepared = self.prepare_unmap_metadata(range)?;

        // Publish every backend transition before changing any owner metadata.
        // A later backend may still report resource pressure after an earlier
        // one removed PTEs. Keeping the complete VMA set makes that state
        // retryable and, more importantly, retains every backend until the
        // caller's invalidation transaction has confirmed stale translations.
        self.for_each_intersecting_area(range, |area, unmap_start, unmap_size| {
            area.unmap_range(unmap_start, unmap_size, context, page_table)
        })?;

        // All fallible metadata surgery completed before the first PTE
        // mutation. Publish the prepared ownership tree only after every
        // backend accepted the detach.
        self.areas = prepared;
        Ok(())
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
        self.for_each_intersecting_area(range, |area, unmap_start, unmap_size| {
            area.validate_unmap_range(unmap_start, unmap_size, page_table)
        })
    }

    /// Visits each VMA intersecting `range` with its clipped unmap interval.
    ///
    /// Keeping the range clipping in one place is important because both the
    /// fallible preflight and the backend commit must describe exactly the same
    /// subranges. The callback may fail; no metadata is changed by this helper.
    fn for_each_intersecting_area(
        &self,
        range: AddrRange<B::Addr>,
        mut visit: impl FnMut(&MemoryArea<B>, B::Addr, usize) -> MappingResult,
    ) -> MappingResult {
        for area in self.areas.values() {
            if area.start() >= range.end {
                break;
            }
            if area.end() <= range.start {
                continue;
            }
            let unmap_start = area.start().max(range.start);
            let unmap_end = area.end().min(range.end);
            visit(area, unmap_start, unmap_end.sub_addr(unmap_start))?;
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

        self.areas = self.prepare_unmap_metadata(range)?;
        Ok(())
    }

    /// Stages fallible splits in an unpublished tree, retaining the complete
    /// preimage until the page-table operation has committed.
    fn prepare_unmap_metadata(
        &self,
        range: AddrRange<B::Addr>,
    ) -> MappingResult<BTreeMap<B::Addr, MemoryArea<B>>> {
        let mut areas = self.areas.clone();
        let start = range.start;
        let end = range.end;
        areas.retain(|_, area| !area.va_range().contained_in(range));

        if let Some((&before_start, before)) = areas.range_mut(..start).last() {
            let before_end = before.end();
            if before_end > start {
                if before_end <= end {
                    before.shrink_right_metadata(start.sub_addr(before_start))?;
                } else {
                    let right_part = before.split(end)?.ok_or(MappingError::BadState)?;
                    before.shrink_right_metadata(start.sub_addr(before_start))?;
                    if right_part.start() != end {
                        return Err(MappingError::BadState);
                    }
                    areas.insert(end, right_part);
                }
            }
        }

        if let Some((&after_start, _)) = areas.range(start..).next()
            && after_start < end
        {
            let mut new_area = areas.remove(&after_start).ok_or(MappingError::BadState)?;
            let after_end = new_area.end();
            new_area.shrink_left_metadata(after_end.sub_addr(end))?;
            if new_area.start() != end {
                return Err(MappingError::BadState);
            }
            areas.insert(end, new_area);
        }
        Ok(areas)
    }

    /// Finds the existing area that contains the replacement range.
    fn containing_area_for_metadata_replacement(
        &self,
        area: &MemoryArea<B>,
    ) -> MappingResult<B::Addr> {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }

        let start = area.start();
        let end = area.end();

        self.areas
            .range(..=start)
            .last()
            .filter(|(_, old)| old.start() <= start && end <= old.end())
            .map(|(&old_start, _)| old_start)
            .ok_or(MappingError::InvalidParam)
    }

    /// Validates that `area` can replace one contained metadata range.
    pub fn validate_area_metadata_replacement(&self, area: &MemoryArea<B>) -> MappingResult {
        self.containing_area_for_metadata_replacement(area)
            .map(|_| ())
    }

    /// Replaces area metadata without touching page-table entries.
    pub fn replace_area_metadata(&mut self, area: MemoryArea<B>) -> MappingResult {
        let start = area.start();
        let end = area.end();
        let old_start = self.containing_area_for_metadata_replacement(&area)?;

        let backup = self.areas.clone();
        let result = (|| {
            let Some(mut old_area) = self.areas.remove(&old_start) else {
                return Err(MappingError::BadState);
            };
            if old_start < start {
                let right_part = old_area.split(start)?.ok_or(MappingError::BadState)?;
                self.areas.insert(old_start, old_area);
                old_area = right_part;
            }
            if old_area.end() > end {
                let right_part = old_area.split(end)?.ok_or(MappingError::BadState)?;
                self.areas.insert(right_part.start(), right_part);
            }
            if self.areas.insert(start, area).is_some() {
                return Err(MappingError::AlreadyExists);
            }
            Ok(())
        })();
        if result.is_err() {
            self.areas = backup;
        }
        result
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
        for (index, area) in self.areas.values().enumerate() {
            if let Err(error) = area.unmap_area(context, page_table) {
                return Err(if index == 0 {
                    error
                } else {
                    MappingError::NeedsRepair
                });
            }
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
        if size == 0 {
            return Ok(());
        }
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

        // Splitting a backend is fallible. Prepare the complete metadata tree
        // before publishing any PTE, retaining the original owners for rollback.
        let mut prepared = self.areas.clone();
        for &(area_start, protect_start, protect_end, _, new_flags, new_reported_flags) in
            &operations
        {
            let original = &self.areas[&area_start];
            if !original.backend().validate_protect(
                protect_start,
                protect_end.sub_addr(protect_start),
                new_flags,
                page_table,
            ) {
                return Err(MappingError::BadState);
            }
            let mut middle = prepared.remove(&area_start).ok_or(MappingError::BadState)?;
            if area_start < protect_start {
                let right = middle.split(protect_start)?.ok_or(MappingError::BadState)?;
                prepared.insert(area_start, middle);
                middle = right;
            }
            if protect_end < middle.end() {
                let right = middle.split(protect_end)?.ok_or(MappingError::BadState)?;
                prepared.insert(right.start(), right);
            }
            middle.set_flags_with_reported_flags(new_flags, new_reported_flags);
            prepared.insert(middle.start(), middle);
        }

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
                let mut restored = true;
                for &(rollback_area_start, rollback_start, rollback_end, old_flags, ..) in
                    operations[..=index].iter().rev()
                {
                    restored &= self.areas[&rollback_area_start]
                        .protect_range(
                            rollback_start,
                            rollback_end.sub_addr(rollback_start),
                            old_flags,
                            context,
                            page_table,
                        )
                        .is_ok();
                }
                return Err(if restored {
                    error
                } else {
                    MappingError::NeedsRepair
                });
            }
        }
        self.areas = prepared;
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

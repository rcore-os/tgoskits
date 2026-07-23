use alloc::collections::BTreeMap;
#[allow(unused_imports)] // this is a weird false alarm
use alloc::vec::Vec;
use core::fmt;

use ax_memory_addr::{AddrRange, MemoryAddr};

use crate::{
    MapPrecondition, MappingBackend, MappingError, MappingOperation, MappingResult, MemoryArea,
};

type BackendOperation<B> = (
    B,
    MappingOperation<<B as MappingBackend>::Addr, <B as MappingBackend>::Flags>,
);

struct MetadataPlan<B: MappingBackend> {
    remove: Vec<B::Addr>,
    insert: Vec<MemoryArea<B>>,
}

impl<B: MappingBackend> MetadataPlan<B> {
    fn apply(self, areas: &mut BTreeMap<B::Addr, MemoryArea<B>>) {
        for start in self.remove {
            assert!(areas.remove(&start).is_some());
        }
        for area in self.insert {
            assert!(areas.insert(area.start(), area).is_none());
        }
    }
}

/// A container that maintains memory mappings ([`MemoryArea`]).
#[derive(Clone)]
pub struct MemorySet<B: MappingBackend> {
    areas: BTreeMap<B::Addr, MemoryArea<B>>,
}

impl<B: MappingBackend> MemorySet<B> {
    fn execute(
        operations: Vec<BackendOperation<B>>,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(operations.len())
            .map_err(|_| MappingError::NoMemory)?;
        for (backend, operation) in operations {
            match backend.prepare(operation, page_table) {
                Ok(plan) => prepared.push((backend, plan)),
                Err(error) => {
                    for (backend, plan) in prepared.into_iter().rev() {
                        backend.abort(plan, page_table);
                    }
                    return Err(error);
                }
            }
        }

        let mut committed = Vec::new();
        if committed.try_reserve_exact(prepared.len()).is_err() {
            for (backend, plan) in prepared.into_iter().rev() {
                backend.abort(plan, page_table);
            }
            return Err(MappingError::NoMemory);
        }
        let mut prepared = prepared.into_iter();
        while let Some((backend, plan)) = prepared.next() {
            match backend.commit(plan, page_table) {
                Ok(state) => committed.push((backend, state)),
                Err(error) => {
                    for (backend, plan) in prepared.rev() {
                        backend.abort(plan, page_table);
                    }
                    let mut rollback_failed = false;
                    for (backend, state) in committed.into_iter().rev() {
                        if backend.rollback(state, page_table).is_err() {
                            rollback_failed = true;
                        }
                    }
                    return Err(if rollback_failed {
                        MappingError::BadState
                    } else {
                        error
                    });
                }
            }
        }

        for (backend, state) in committed {
            backend.finalize(state, page_table);
        }
        Ok(())
    }

    fn affected_area_starts(&self, range: AddrRange<B::Addr>) -> MappingResult<Vec<B::Addr>> {
        let preceding = self
            .areas
            .range(..range.start)
            .next_back()
            .filter(|(_, area)| area.end() > range.start)
            .map(|(&start, _)| start);
        let starts_in_range = self.areas.range(range.start..range.end).count();
        let mut starts = Vec::new();
        starts
            .try_reserve_exact(starts_in_range + usize::from(preceding.is_some()))
            .map_err(|_| MappingError::NoMemory)?;
        starts.extend(preceding);
        starts.extend(
            self.areas
                .range(range.start..range.end)
                .map(|(&start, _)| start),
        );
        Ok(starts)
    }

    fn plan_unmap(
        &self,
        range: AddrRange<B::Addr>,
    ) -> MappingResult<(MetadataPlan<B>, Vec<BackendOperation<B>>)> {
        let affected = self.affected_area_starts(range)?;
        let mut remove = Vec::new();
        let mut insert = Vec::new();
        let mut operations = Vec::new();
        remove
            .try_reserve_exact(affected.len())
            .map_err(|_| MappingError::NoMemory)?;
        insert
            .try_reserve_exact(affected.len().saturating_mul(2))
            .map_err(|_| MappingError::NoMemory)?;
        operations
            .try_reserve_exact(affected.len())
            .map_err(|_| MappingError::NoMemory)?;

        for area_start in affected {
            let source = self.areas.get(&area_start).ok_or(MappingError::BadState)?;
            let start = source.start().max(range.start);
            let end = source.end().min(range.end);
            operations.push((
                source.backend().clone(),
                MappingOperation::Unmap {
                    start,
                    size: end.sub_addr(start),
                    old_flags: source.flags(),
                },
            ));
            remove.push(area_start);

            let mut area = source.clone();
            if area.start() < range.start {
                let right = area.split(range.start).ok_or(MappingError::BadState)?;
                insert.push(area);
                area = right;
            }
            if area.end() > range.end {
                let right = area.split(range.end).ok_or(MappingError::BadState)?;
                insert.push(right);
            }
        }

        Ok((MetadataPlan { remove, insert }, operations))
    }

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

        let area = &self.areas[&area_start];
        let operation = MappingOperation::Map {
            start: area.end(),
            size: additional_size,
            flags: area.flags(),
            precondition: MapPrecondition::Vacant,
        };
        let backend = area.backend().clone();
        let mut grown = area.clone();
        grown.grow_right_metadata(additional_size)?;
        let mut remove = Vec::new();
        let mut insert = Vec::new();
        remove
            .try_reserve_exact(1)
            .map_err(|_| MappingError::NoMemory)?;
        insert
            .try_reserve_exact(1)
            .map_err(|_| MappingError::NoMemory)?;
        remove.push(area_start);
        insert.push(grown);
        let metadata = MetadataPlan { remove, insert };

        Self::execute(alloc::vec![(backend, operation)], page_table)?;
        metadata.apply(&mut self.areas);
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
        page_table: &mut B::PageTable,
        unmap_overlap: bool,
    ) -> MappingResult {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }

        let mut operations = Vec::new();
        operations
            .try_reserve(1)
            .map_err(|_| MappingError::NoMemory)?;
        let overlaps = self.overlaps(area.va_range());
        let mut metadata = MetadataPlan {
            remove: Vec::new(),
            insert: Vec::new(),
        };
        if overlaps {
            if unmap_overlap {
                (metadata, operations) = self.plan_unmap(area.va_range())?;
                operations
                    .try_reserve(1)
                    .map_err(|_| MappingError::NoMemory)?;
            } else {
                return Err(MappingError::AlreadyExists);
            }
        }

        operations.push((
            area.backend().clone(),
            MappingOperation::Map {
                start: area.start(),
                size: area.size(),
                flags: area.flags(),
                precondition: if overlaps {
                    MapPrecondition::Replacing
                } else {
                    MapPrecondition::Vacant
                },
            },
        ));
        metadata
            .insert
            .try_reserve(1)
            .map_err(|_| MappingError::NoMemory)?;
        metadata.insert.push(area);
        Self::execute(operations, page_table)?;
        metadata.apply(&mut self.areas);
        Ok(())
    }

    /// Inserts area metadata for mappings already installed by the caller.
    ///
    /// This operation never invokes the backend or changes the page table. It
    /// is intended for ownership transfers such as a fork operation that
    /// installs child PTEs and their resource references atomically before
    /// publishing the corresponding VMA.
    pub fn map_metadata(&mut self, area: MemoryArea<B>) -> MappingResult {
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

    /// Replaces every mapping in `replace_range` and installs `area` in one
    /// backend transaction.
    ///
    /// The new area must be fully contained in the replacement range. This is
    /// useful when a device mapping is shorter than the user-requested
    /// replacement span: the whole requested span is removed, while only the
    /// validated device range is installed.
    pub fn replace(
        &mut self,
        replace_range: AddrRange<B::Addr>,
        area: MemoryArea<B>,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        if replace_range.is_empty()
            || area.va_range().is_empty()
            || !area.va_range().contained_in(replace_range)
        {
            return Err(MappingError::InvalidParam);
        }

        let (mut metadata, mut operations) = self.plan_unmap(replace_range)?;
        operations
            .try_reserve(1)
            .map_err(|_| MappingError::NoMemory)?;
        operations.push((
            area.backend().clone(),
            MappingOperation::Map {
                start: area.start(),
                size: area.size(),
                flags: area.flags(),
                precondition: MapPrecondition::Replacing,
            },
        ));

        metadata
            .insert
            .try_reserve(1)
            .map_err(|_| MappingError::NoMemory)?;
        metadata.insert.push(area);

        Self::execute(operations, page_table)?;
        metadata.apply(&mut self.areas);
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
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        let range =
            AddrRange::try_from_start_size(start, size).ok_or(MappingError::InvalidParam)?;
        if range.is_empty() {
            return Ok(());
        }

        let (metadata, operations) = self.plan_unmap(range)?;
        Self::execute(operations, page_table)?;
        metadata.apply(&mut self.areas);
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

        let (metadata, _operations) = self.plan_unmap(range)?;
        metadata.apply(&mut self.areas);
        Ok(())
    }

    /// Replaces area metadata without touching page-table entries.
    pub fn replace_area_metadata(&mut self, area: MemoryArea<B>) -> MappingResult {
        if area.va_range().is_empty() {
            return Err(MappingError::InvalidParam);
        }

        let start = area.start();
        let end = area.end();

        let old_start = self
            .areas
            .range(..=start)
            .last()
            .filter(|(_, old)| old.start() <= start && end <= old.end())
            .map(|(&old_start, _)| old_start)
            .ok_or(MappingError::InvalidParam)?;

        let mut old_area = self
            .areas
            .remove(&old_start)
            .ok_or(MappingError::BadState)?;
        if old_start < start {
            let right_part = old_area.split(start).ok_or(MappingError::BadState)?;
            self.areas.insert(old_start, old_area);
            old_area = right_part;
        }
        if old_area.end() > end {
            let right_part = old_area.split(end).ok_or(MappingError::BadState)?;
            self.areas.insert(right_part.start(), right_part);
        }
        assert!(self.areas.insert(start, area).is_none());
        Ok(())
    }

    /// Remove all memory areas and the underlying mappings.
    pub fn clear(&mut self, page_table: &mut B::PageTable) -> MappingResult {
        let mut operations = Vec::new();
        operations
            .try_reserve_exact(self.areas.len())
            .map_err(|_| MappingError::NoMemory)?;
        operations.extend(self.areas.values().map(|area| {
            (
                area.backend().clone(),
                MappingOperation::Unmap {
                    start: area.start(),
                    size: area.size(),
                    old_flags: area.flags(),
                },
            )
        }));
        Self::execute(operations, page_table)?;
        self.areas = BTreeMap::new();
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
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        self.protect_with_reported_flags(
            start,
            size,
            |flags, _reported_flags| update_flags(flags).map(|new_flags| (new_flags, new_flags)),
            page_table,
        )
    }

    /// Change backend/page-table flags and reported flags within the given range.
    pub fn protect_with_reported_flags(
        &mut self,
        start: B::Addr,
        size: usize,
        update_flags: impl Fn(B::Flags, B::Flags) -> Option<(B::Flags, B::Flags)>,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        let range =
            AddrRange::try_from_start_size(start, size).ok_or(MappingError::InvalidParam)?;
        if range.is_empty() {
            return Ok(());
        }
        let (metadata, operations) = self.plan_protect(range, update_flags)?;
        Self::execute(operations, page_table)?;
        metadata.apply(&mut self.areas);
        Ok(())
    }

    fn plan_protect(
        &self,
        range: AddrRange<B::Addr>,
        update_flags: impl Fn(B::Flags, B::Flags) -> Option<(B::Flags, B::Flags)>,
    ) -> MappingResult<(MetadataPlan<B>, Vec<BackendOperation<B>>)> {
        let affected = self.affected_area_starts(range)?;
        let mut remove = Vec::new();
        let mut insert = Vec::new();
        let mut operations = Vec::new();
        remove
            .try_reserve_exact(affected.len())
            .map_err(|_| MappingError::NoMemory)?;
        insert
            .try_reserve_exact(affected.len().saturating_mul(3))
            .map_err(|_| MappingError::NoMemory)?;
        operations
            .try_reserve_exact(affected.len())
            .map_err(|_| MappingError::NoMemory)?;

        for area_start in affected {
            let source = self.areas.get(&area_start).ok_or(MappingError::BadState)?;
            let Some((new_flags, new_reported_flags)) =
                update_flags(source.flags(), source.reported_flags())
            else {
                continue;
            };

            remove.push(area_start);
            let mut protected = source.clone();
            if protected.start() < range.start {
                let right = protected.split(range.start).ok_or(MappingError::BadState)?;
                insert.push(protected);
                protected = right;
            }
            if protected.end() > range.end {
                let right = protected.split(range.end).ok_or(MappingError::BadState)?;
                insert.push(right);
            }
            operations.push((
                protected.backend().clone(),
                MappingOperation::Protect {
                    start: protected.start(),
                    size: protected.size(),
                    old_flags: protected.flags(),
                    new_flags,
                },
            ));
            protected.set_flags_with_reported_flags(new_flags, new_reported_flags);
            insert.push(protected);
        }

        Ok((MetadataPlan { remove, insert }, operations))
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

//! Range walking and bounded retirement within the metadata flush domain.

use core::ops::Range;

use crate::{
    DeferredPageTableFrames, Frame, FrameAllocator, PageTableEntry, PageTableRef, PagingError,
    PagingResult, PhysAddr, TableMeta, VirtAddr,
};

struct RangeRemoval {
    empty: bool,
    changed: bool,
}

impl<T: TableMeta, A: FrameAllocator> PageTableRef<T, A> {
    /// Removes complete occupied leaves without allocation or TLB invalidation.
    ///
    /// The caller retains every data-frame owner before entry and transfers
    /// returned table batches into its preallocated stage-1 retirement gather.
    /// At most one nonempty batch per cleared leaf is emitted. Existing empty
    /// paths and retained shared root entries remain installed. `retire` must
    /// not allocate, panic, reenter the table, or reclaim unconfirmed frames.
    ///
    /// Errors leave the failing leaf intact and still transfer detached table
    /// batches from the successful prefix. Return or callback completion does
    /// not revoke hardware access; only the caller's TLB receipt does so.
    pub fn unmap_range_deferred(
        &mut self,
        range: Range<VirtAddr>,
        retire: impl FnMut(DeferredPageTableFrames<A>),
    ) -> PagingResult<usize> {
        self.validate_owned_unmap_range(&range)?;
        if range.is_empty() {
            return Ok(0);
        }
        if Frame::<T, A>::PT_LEVEL > crate::table::MAX_DEFERRED_PAGE_TABLE_LEVELS {
            return Err(PagingError::hierarchy_error(
                "Page-table depth exceeds deferred reclaim capacity",
            ));
        }
        let retained = self.retained_root_entry_range();
        let mut gather = DeferredRetirement {
            tables: DeferredPageTableFrames::new(self.root.allocator.clone()),
            retire,
        };
        let mut removed = 0;
        let result = remove_range(
            &mut self.root,
            range,
            Frame::<T, A>::PT_LEVEL,
            retained,
            &mut removed,
            &mut gather,
        );
        // An empty token still reports leaf changes when no intermediate table
        // became empty. A range containing only holes changed no descriptors.
        // No table is freed at this boundary, including an error prefix.
        if removed != 0 {
            gather.finish();
        }
        result.map(|_| removed)
    }
}

struct DeferredRetirement<A: FrameAllocator, R: FnMut(DeferredPageTableFrames<A>)> {
    tables: DeferredPageTableFrames<A>,
    retire: R,
}

impl<A: FrameAllocator, R: FnMut(DeferredPageTableFrames<A>)> DeferredRetirement<A, R> {
    fn finish(&mut self) {
        let empty = DeferredPageTableFrames::new(self.tables.allocator_clone());
        (self.retire)(core::mem::replace(&mut self.tables, empty));
    }

    fn reserve(&mut self) {
        if self.tables.is_full() {
            self.finish();
        }
    }

    fn table_removed(&mut self, frame: PhysAddr) {
        self.tables.push(frame);
    }
}

fn remove_range<T, A, R>(
    frame: &mut Frame<T, A>,
    range: Range<VirtAddr>,
    level: usize,
    retained_root_entries: Option<(usize, usize)>,
    removed: &mut usize,
    gather: &mut DeferredRetirement<A, R>,
) -> PagingResult<RangeRemoval>
where
    T: TableMeta,
    A: FrameAllocator,
    R: FnMut(DeferredPageTableFrames<A>),
{
    let allocator = frame.allocator.clone();
    let entries = frame.as_slice_mut();
    let level_size = Frame::<T, A>::level_size(level);
    let mut address = range.start;
    let mut changed = false;

    while address < range.end {
        let index = Frame::<T, A>::virt_to_index(address, level);
        let to_boundary = level_size - address.as_usize() % level_size;
        let next = address + to_boundary.min(range.end - address);
        let entry = &mut entries[index];
        if entry.unused() {
            address = next;
            continue;
        }

        if level == 1 || entry.huge(true) {
            if !address.as_usize().is_multiple_of(level_size) || next - address != level_size {
                return Err(PagingError::invalid_range(
                    "Unmap range intersects a partial huge leaf",
                ));
            }
            gather.reserve();
            entry.clear();
            changed = true;
            *removed += 1;
        } else {
            if !entry.present() {
                return Err(PagingError::hierarchy_error(
                    "Non-present intermediate entry is not a leaf",
                ));
            }
            let child_paddr = entry.paddr(true);
            let mut child = Frame::<T, A>::from_paddr(child_paddr, allocator.clone());
            let child_removed =
                remove_range(&mut child, address..next, level - 1, None, removed, gather)?;
            changed |= child_removed.changed;
            if child_removed.empty
                && child_removed.changed
                && !retained_root_entries.is_some_and(|(start, end)| start <= index && index < end)
            {
                gather.reserve();
                // Retain the detached frame until this parent update is covered
                // by a completed invalidation, including capacity-driven drains.
                entry.clear();
                changed = true;
                gather.table_removed(child_paddr);
            }
        }
        address = next;
    }

    // Once per visited table, not once per removed page from the VMA.
    Ok(RangeRemoval {
        empty: entries.iter().all(PageTableEntry::unused),
        changed,
    })
}

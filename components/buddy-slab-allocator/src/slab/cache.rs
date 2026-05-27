/// Per-size-class slab cache.
///
/// Maintains three intrusive doubly-linked lists of slab pages:
/// - **partial**: some objects free (preferred for allocation)
/// - **full**: no objects free
/// - **empty**: all objects free (at most one cached; rest returned to buddy)
use super::page::{SlabListState, SlabPageHeader};
use super::size_class::SizeClass;

/// Intrusive list head (address of the first `SlabPageHeader`, 0 = empty).
#[derive(Debug, Clone, Copy)]
struct ListHead {
    first: usize,
    kind: SlabListState,
}

impl ListHead {
    const fn empty(kind: SlabListState) -> Self {
        Self { first: 0, kind }
    }

    fn is_empty(&self) -> bool {
        self.first == 0
    }

    /// Push a slab page onto the front of the list.
    ///
    /// # Safety
    /// `base` must point to a valid `SlabPageHeader`.
    unsafe fn push_front(&mut self, base: usize) {
        unsafe {
            let hdr = &mut *(base as *mut SlabPageHeader);
            debug_assert_eq!(
                hdr.list_state,
                SlabListState::None,
                "pushing slab onto {:?} while it is on {:?}",
                self.kind,
                hdr.list_state
            );
            debug_assert_eq!(hdr.list_prev, 0, "pushing slab with stale list_prev");
            debug_assert_eq!(hdr.list_next, 0, "pushing slab with stale list_next");
            hdr.list_state = self.kind;
            hdr.list_prev = 0;
            hdr.list_next = self.first;
            if self.first != 0 {
                let old = &mut *(self.first as *mut SlabPageHeader);
                debug_assert_eq!(
                    old.list_state, self.kind,
                    "list head points to slab with wrong membership"
                );
                old.list_prev = base;
            }
            self.first = base;
        }
    }

    /// Remove a slab page from this list.
    ///
    /// # Safety
    /// `base` must be in this list.
    unsafe fn remove(&mut self, base: usize) {
        unsafe {
            let hdr = &*(base as *const SlabPageHeader);
            debug_assert_eq!(
                hdr.list_state, self.kind,
                "removing slab from {:?} while it is on {:?}",
                self.kind, hdr.list_state
            );
            if hdr.list_prev == 0 {
                debug_assert_eq!(self.first, base, "front slab is not this list's head");
            } else {
                debug_assert_eq!(
                    (*(hdr.list_prev as *const SlabPageHeader)).list_next,
                    base,
                    "previous slab does not link back to removed slab"
                );
            }
            if hdr.list_next != 0 {
                debug_assert_eq!(
                    (*(hdr.list_next as *const SlabPageHeader)).list_prev,
                    base,
                    "next slab does not link back to removed slab"
                );
            }
            let prev = hdr.list_prev;
            let next = hdr.list_next;

            if prev != 0 {
                (*(prev as *mut SlabPageHeader)).list_next = next;
            } else {
                self.first = next;
            }
            if next != 0 {
                (*(next as *mut SlabPageHeader)).list_prev = prev;
            }
            // Clear links
            let hdr = &mut *(base as *mut SlabPageHeader);
            hdr.list_prev = 0;
            hdr.list_next = 0;
            hdr.list_state = SlabListState::None;
        }
    }

    /// Pop the first page from the list.  Returns 0 if empty.
    unsafe fn pop_front(&mut self) -> usize {
        unsafe {
            if self.first == 0 {
                return 0;
            }
            let base = self.first;
            self.remove(base);
            base
        }
    }
}

/// Cache for a single [`SizeClass`].
pub struct SlabCache {
    pub size_class: SizeClass,
    partial: ListHead,
    full: ListHead,
    empty: ListHead,
    /// Number of empty slabs cached (we keep at most 1).
    empty_count: usize,
}

/// Result of a per-cache deallocation.
pub enum CacheDeallocResult {
    /// Object freed, slab stays.
    Done,
    /// Slab became empty and should be returned to the page allocator.
    FreeSlab { base: usize, pages: usize },
}

impl SlabCache {
    pub const fn new(size_class: SizeClass) -> Self {
        Self {
            size_class,
            partial: ListHead::empty(SlabListState::Partial),
            full: ListHead::empty(SlabListState::Full),
            empty: ListHead::empty(SlabListState::Empty),
            empty_count: 0,
        }
    }

    /// Try to allocate one object.  Returns `Some(obj_addr)` or `None` if no slabs available.
    pub fn alloc_object<const PAGE_SIZE: usize>(&mut self) -> Option<usize> {
        // 1. Try the first partial slab (drain remote frees first).
        if let Some(addr) = self.try_alloc_from_partial::<PAGE_SIZE>() {
            return Some(addr);
        }

        // 2. A full slab may have gained free objects via lock-free remote frees.
        if let Some(base) = self.reclaim_full_with_remote_frees() {
            unsafe { self.partial.push_front(base) };
            return self.try_alloc_from_partial::<PAGE_SIZE>();
        }

        // 3. Try recycling an empty slab.
        if !self.empty.is_empty() {
            let base = unsafe { self.empty.pop_front() };
            self.empty_count -= 1;
            // Move to partial and alloc from it.
            unsafe { self.partial.push_front(base) };
            return self.try_alloc_from_partial::<PAGE_SIZE>();
        }

        None
    }

    /// Drain remote frees from the first full slab that has them and move it
    /// back to the partial list.
    fn reclaim_full_with_remote_frees(&mut self) -> Option<usize> {
        let mut base = self.full.first;
        while base != 0 {
            let next = unsafe { (*(base as *const SlabPageHeader)).list_next };
            let hdr = unsafe { &mut *(base as *mut SlabPageHeader) };
            if hdr.has_remote_frees() {
                hdr.drain_remote_frees(base);
                unsafe { self.full.remove(base) };
                return Some(base);
            }
            base = next;
        }
        None
    }

    /// Attempt allocation from the first partial slab.
    fn try_alloc_from_partial<const PAGE_SIZE: usize>(&mut self) -> Option<usize> {
        let base = self.partial.first;
        if base == 0 {
            return None;
        }

        let hdr = unsafe { &mut *(base as *mut SlabPageHeader) };

        // Drain any remote frees first.
        if hdr.has_remote_frees() {
            hdr.drain_remote_frees(base);
        }

        if let Some(idx) = hdr.local_alloc() {
            let obj_addr = hdr.object_addr(base, idx);
            // If slab is now full, move to full list.
            if hdr.is_local_full() && !hdr.has_remote_frees() {
                unsafe {
                    self.partial.remove(base);
                    self.full.push_front(base);
                }
            }
            return Some(obj_addr);
        }
        None
    }

    /// Free an object back to this cache (local CPU path — under lock).
    ///
    /// Returns whether the slab should be returned to the page allocator.
    pub fn dealloc_object<const PAGE_SIZE: usize>(
        &mut self,
        obj_addr: usize,
    ) -> CacheDeallocResult {
        let slab_bytes = self.size_class.slab_pages(PAGE_SIZE) * PAGE_SIZE;
        let base = SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(obj_addr, slab_bytes);
        let hdr = unsafe { &mut *(base as *mut SlabPageHeader) };
        let was_full = hdr.list_state == SlabListState::Full;

        let idx = hdr.object_index(base, obj_addr);
        hdr.local_free(idx);

        if was_full {
            // Move from full to partial.
            unsafe {
                self.full.remove(base);
                self.partial.push_front(base);
            }
        }

        // Check if slab is now completely empty.
        // First drain remote frees so we have an accurate count.
        if hdr.has_remote_frees() {
            hdr.drain_remote_frees(base);
        }

        if hdr.is_all_free() {
            if self.empty_count == 0 {
                // Cache one empty slab for reuse.
                unsafe {
                    self.partial.remove(base);
                    self.empty.push_front(base);
                }
                self.empty_count += 1;
                CacheDeallocResult::Done
            } else {
                // Already have a cached empty slab — return this one.
                unsafe { self.partial.remove(base) };
                let hdr = unsafe { &mut *(base as *mut SlabPageHeader) };
                hdr.prepare_for_buddy_free();
                CacheDeallocResult::FreeSlab {
                    base,
                    pages: self.size_class.slab_pages(PAGE_SIZE),
                }
            }
        } else {
            CacheDeallocResult::Done
        }
    }

    /// Register a newly allocated slab page (from the buddy allocator).
    pub fn add_slab(&mut self, base: usize, bytes: usize, owner_cpu: u16) {
        let hdr = unsafe { &mut *(base as *mut SlabPageHeader) };
        hdr.init(self.size_class, bytes, owner_cpu);
        unsafe { self.partial.push_front(base) };
    }
}

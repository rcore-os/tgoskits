/// Slab page header, bitmap-based object tracking, and lock-free remote free.
///
/// Each slab page starts with a [`SlabPageHeader`] followed by the object array.
/// Local (owner-CPU) operations use a bitmap under the slab lock.
/// Remote (cross-CPU) frees use an atomic CAS stack — no lock required.
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::size_class::SizeClass;

/// Magic number written at the start of every slab page header.
pub const SLAB_MAGIC: u32 = 0x534C_4142; // "SLAB"

/// Maximum objects per slab page (512 = 8 × u64 bitmap words).
pub const MAX_OBJECTS_PER_SLAB: usize = 512;

/// Number of u64 words in the local bitmap.
pub const BITMAP_WORDS: usize = MAX_OBJECTS_PER_SLAB / 64; // 8

/// Maximum number of pages a slab may span.
pub const MAX_SLAB_PAGES: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SlabListState {
    None,
    Partial,
    Full,
    Empty,
}

/// Header placed at the very start of each slab page.
///
/// Object data starts at `header_end`, aligned to `size_class.size()`.
#[repr(C)]
pub struct SlabPageHeader {
    /// Magic number for integrity checks.
    pub magic: u32,
    /// Which size class this slab serves.
    pub size_class: SizeClass,
    /// Total number of objects that fit.
    pub object_count: u16,
    /// Number of objects free in `local_bitmap`.
    pub local_free_count: u16,
    /// The CPU that owns this slab page (local alloc/dealloc go through this CPU's lock).
    pub owner_cpu: u16,
    _pad: u16,
    /// Total usable bytes (slab pages × PAGE_SIZE).
    pub slab_bytes: u32,

    // --- Intrusive doubly-linked list pointers (used by SlabCache) ---
    pub list_prev: usize,
    pub list_next: usize,
    pub(crate) list_state: SlabListState,

    // --- Local bitmap (under slab lock, owner CPU only) ---
    // A set bit means the slot is FREE.
    pub local_bitmap: [u64; BITMAP_WORDS],

    // --- Lock-free remote free stack (any CPU) ---
    /// Head of the remote-free linked list (object virtual address, 0 = empty).
    /// Each freed object stores `next` at its start (reuses object memory).
    pub remote_free_head: AtomicUsize,
    /// Number of objects in the remote-free stack.
    pub remote_free_count: AtomicU32,
}

impl SlabPageHeader {
    /// Size of the header in bytes.
    pub const HEADER_SIZE: usize = core::mem::size_of::<Self>();

    /// Initialise a slab page header.  All objects are marked free in the bitmap.
    ///
    /// `base` is the virtual address of the page, `bytes` is the total slab size.
    pub fn init(&mut self, size_class: SizeClass, bytes: usize, owner_cpu: u16) {
        let obj_size = size_class.size();
        let data_start = Self::data_offset(obj_size);
        let usable = bytes.saturating_sub(data_start);
        let count = (usable / obj_size).min(MAX_OBJECTS_PER_SLAB);

        self.magic = SLAB_MAGIC;
        self.size_class = size_class;
        self.object_count = count as u16;
        self.local_free_count = count as u16;
        self.owner_cpu = owner_cpu;
        self._pad = 0;
        self.slab_bytes = bytes as u32;
        self.list_prev = 0;
        self.list_next = 0;
        self.list_state = SlabListState::None;
        self.local_bitmap = [0u64; BITMAP_WORDS];
        self.remote_free_head = AtomicUsize::new(0);
        self.remote_free_count = AtomicU32::new(0);

        // Mark first `count` bits as 1 (free).
        let full_words = count / 64;
        let remaining_bits = count % 64;
        for w in self.local_bitmap.iter_mut().take(full_words) {
            *w = u64::MAX;
        }
        if remaining_bits > 0 {
            self.local_bitmap[full_words] = (1u64 << remaining_bits) - 1;
        }
    }

    /// Byte offset from the start of the page to the first object.
    /// Aligned up to `obj_size` for natural alignment.
    pub fn data_offset(obj_size: usize) -> usize {
        let raw = Self::HEADER_SIZE;
        // Align to obj_size (which is always a power of two).
        (raw + obj_size - 1) & !(obj_size - 1)
    }

    /// Virtual address of the object region start (given `base` = page start).
    #[inline]
    pub fn data_start(&self, base: usize) -> usize {
        base + Self::data_offset(self.size_class.size())
    }

    /// Virtual address of object at `index`.
    #[inline]
    pub fn object_addr(&self, base: usize, index: usize) -> usize {
        self.data_start(base) + index * self.size_class.size()
    }

    /// Index of the object whose address is `addr`.
    #[inline]
    pub fn object_index(&self, base: usize, addr: usize) -> usize {
        (addr - self.data_start(base)) / self.size_class.size()
    }

    /// Base address (slab start) from an object address.
    ///
    /// Searches backward by page because the slab base may no longer have
    /// absolute `slab_bytes` alignment after metadata is carved from a region
    /// prefix by [`GlobalAllocator`](crate::GlobalAllocator).
    #[inline]
    pub fn base_from_obj_addr<const PAGE_SIZE: usize>(addr: usize, slab_bytes: usize) -> usize {
        let slab_pages = slab_bytes / PAGE_SIZE;
        debug_assert!(slab_pages > 0);

        let page_base = addr & !(PAGE_SIZE - 1);
        for page_idx in 0..slab_pages {
            let Some(candidate) = page_base.checked_sub(page_idx * PAGE_SIZE) else {
                break;
            };
            let hdr = unsafe { &*(candidate as *const SlabPageHeader) };
            if hdr.magic == SLAB_MAGIC
                && hdr.slab_bytes as usize == slab_bytes
                && addr >= candidate
                && addr < candidate + slab_bytes
            {
                return candidate;
            }
        }

        debug_assert!(false, "object address does not belong to a live slab");
        page_base
    }

    fn base_from_obj_addr_unknown_with_page_size(addr: usize, page_size: usize) -> Option<usize> {
        if page_size == 0 || !page_size.is_power_of_two() {
            return None;
        }
        let page_base = addr & !(page_size - 1);
        for page_idx in 0..MAX_SLAB_PAGES {
            let Some(candidate) = page_base.checked_sub(page_idx * page_size) else {
                break;
            };
            let hdr = unsafe { &*(candidate as *const SlabPageHeader) };
            let slab_bytes = hdr.slab_bytes as usize;
            if hdr.magic != SLAB_MAGIC
                || slab_bytes == 0
                || !slab_bytes.is_multiple_of(page_size)
                || slab_bytes / page_size > MAX_SLAB_PAGES
            {
                continue;
            }
            if addr >= candidate && addr < candidate + slab_bytes {
                return Some(candidate);
            }
        }
        None
    }

    /// Base address (slab start) from an object address without knowing the slab size.
    ///
    /// Searches backward up to [`MAX_SLAB_PAGES`] pages and validates each candidate
    /// against the header's `slab_bytes`.
    #[inline]
    pub fn base_from_obj_addr_unknown<const PAGE_SIZE: usize>(addr: usize) -> Option<usize> {
        Self::base_from_obj_addr_unknown_with_page_size(addr, PAGE_SIZE)
    }

    /// Queue the object on its owner's remote-free list.
    ///
    /// # Safety
    /// - `ptr` must point to a valid live slab object.
    /// - `owner_cpu` must match the slab header's owner CPU.
    /// - `page_size` must be the slab allocator's page size.
    pub unsafe fn remote_free_object(ptr: NonNull<u8>, owner_cpu: u16, page_size: usize) {
        let obj_addr = ptr.as_ptr() as usize;
        let Some(base) = Self::base_from_obj_addr_unknown_with_page_size(obj_addr, page_size)
        else {
            debug_assert!(false, "object address does not belong to a live slab");
            return;
        };
        let hdr = unsafe { &*(base as *const SlabPageHeader) };
        debug_assert_eq!(hdr.magic, SLAB_MAGIC);
        debug_assert_eq!(hdr.owner_cpu, owner_cpu);
        unsafe { hdr.remote_free(obj_addr) };
    }

    // ------------------------------------------------------------------
    // Local allocation (under slab lock)
    // ------------------------------------------------------------------

    /// Allocate one object from the local bitmap.  Returns the slot index or `None`.
    pub fn local_alloc(&mut self) -> Option<usize> {
        for (wi, word) in self.local_bitmap.iter_mut().enumerate() {
            if *word != 0 {
                let bit = word.trailing_zeros() as usize;
                *word &= !(1u64 << bit);
                self.local_free_count -= 1;
                return Some(wi * 64 + bit);
            }
        }
        None
    }

    /// Free an object back to the local bitmap.
    pub fn local_free(&mut self, index: usize) {
        let wi = index / 64;
        let bit = index % 64;
        debug_assert!(self.local_bitmap[wi] & (1u64 << bit) == 0, "double free");
        self.local_bitmap[wi] |= 1u64 << bit;
        self.local_free_count += 1;
    }

    /// Whether this slab has any free objects (local bitmap only).
    #[inline]
    pub fn has_local_free(&self) -> bool {
        self.local_free_count > 0
    }

    /// Whether every object in this slab is free (local bitmap only).
    #[inline]
    pub fn is_all_free(&self) -> bool {
        self.local_free_count == self.object_count
    }

    /// Whether the local bitmap is completely full (zero free objects locally).
    #[inline]
    pub fn is_local_full(&self) -> bool {
        self.local_free_count == 0
    }

    pub(crate) fn prepare_for_buddy_free(&mut self) {
        assert_eq!(
            self.remote_free_head.load(Ordering::Acquire),
            0,
            "returning slab with pending remote frees"
        );
        self.list_prev = 0;
        self.list_next = 0;
        self.list_state = SlabListState::None;
        self.magic = 0;
    }

    // ------------------------------------------------------------------
    // Remote free (lock-free, any CPU)
    // ------------------------------------------------------------------

    /// Push `obj_addr` onto the remote-free stack (lock-free CAS).
    ///
    /// # Safety
    /// - `obj_addr` must point to a previously allocated object within this slab.
    /// - The object's first `size_of::<usize>()` bytes will be overwritten with
    ///   the next-pointer.
    pub unsafe fn remote_free(&self, obj_addr: usize) {
        unsafe {
            loop {
                let old_head = self.remote_free_head.load(Ordering::Acquire);
                // Store "next" pointer inside the freed object.
                (obj_addr as *mut usize).write(old_head);
                if self
                    .remote_free_head
                    .compare_exchange_weak(old_head, obj_addr, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    self.remote_free_count.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }

    /// Drain all remote frees back into the local bitmap.
    ///
    /// Must be called under the owner-CPU slab lock.
    pub fn drain_remote_frees(&mut self, base: usize) {
        let head = self.remote_free_head.swap(0, Ordering::AcqRel);
        if head == 0 {
            return;
        }
        // Also zero the count (we'll re-add to local).
        self.remote_free_count.store(0, Ordering::Relaxed);

        let mut ptr = head;
        while ptr != 0 {
            let next = unsafe { *(ptr as *const usize) };
            let idx = self.object_index(base, ptr);
            let wi = idx / 64;
            let bit = idx % 64;
            debug_assert!(
                self.local_bitmap[wi] & (1u64 << bit) == 0,
                "remote double free"
            );
            self.local_bitmap[wi] |= 1u64 << bit;
            self.local_free_count += 1;
            ptr = next;
        }
    }

    /// Whether any remote frees are pending.
    #[inline]
    pub fn has_remote_frees(&self) -> bool {
        self.remote_free_head.load(Ordering::Acquire) != 0
    }
}

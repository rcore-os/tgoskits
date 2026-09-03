use core::ops::{Deref, DerefMut, Range};

use ax_memory_addr::MemoryAddr;

use crate::{
    FrameAllocator, PageTableEntry, PagingError, PagingResult, PhysAddr, PteConfigOf, TableMeta,
    VirtAddr,
    frame::{DetachedPageTableFrame, Frame, HugeSplitFill},
    map::{MapConfig, MapRecursiveConfig, UnmapConfig, UnmapRecursiveConfig},
    walk::{PageTableWalker, WalkConfig},
};

const TARGETED_FLUSH_LIMIT: usize = 32;

#[derive(Clone, Copy)]
enum RegionPageSelection {
    BasePages,
    Linear { allow_huge: bool },
}

/// A move-only, pre-zeroed child-table reservation.
///
/// This raw allocation token never crosses the public API.  Callers receive a
/// [`HugeSplitDeposit`] that also binds the frame to the huge leaf observed
/// during prepare.
struct ReservedTable<T: TableMeta, A: FrameAllocator> {
    frame: Option<Frame<T, A>>,
}

/// A child-table deposit bound to one observed huge leaf.
///
/// This is the page-table-generic equivalent of Linux's deposited PTE page:
/// allocation happens before the mutation critical section, dropping an
/// unpublished deposit releases the frame, and apply consumes it only if the
/// root and huge-leaf identity still match.  The target address is deliberately
/// not accepted by apply, so a deposit cannot be redirected to another leaf.
pub struct HugeSplitDeposit<T: TableMeta, A: FrameAllocator> {
    table: ReservedTable<T, A>,
    root_paddr: PhysAddr,
    block_vaddr: VirtAddr,
    block_paddr: PhysAddr,
    block_config: PteConfigOf<T>,
    block_size: usize,
}

/// Failed structural apply that returns the still-unpublished deposit to its
/// caller.  Transactional users must not lose the only child-table owner merely
/// because the observed huge leaf became stale before apply.
pub struct HugeSplitApplyError<T: TableMeta, A: FrameAllocator> {
    error: PagingError,
    deposit: HugeSplitDeposit<T, A>,
}

impl<T: TableMeta, A: FrameAllocator> HugeSplitApplyError<T, A> {
    pub const fn error(&self) -> &PagingError {
        &self.error
    }

    pub fn into_parts(self) -> (PagingError, HugeSplitDeposit<T, A>) {
        (self.error, self.deposit)
    }
}

impl<T: TableMeta, A: FrameAllocator> core::fmt::Debug for HugeSplitApplyError<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HugeSplitApplyError")
            .field("error", &self.error)
            .field("deposit", &self.deposit)
            .finish()
    }
}

impl<T: TableMeta, A: FrameAllocator> HugeSplitDeposit<T, A> {
    pub const fn block_vaddr(&self) -> VirtAddr {
        self.block_vaddr
    }

    pub const fn block_paddr(&self) -> PhysAddr {
        self.block_paddr
    }

    pub const fn block_size(&self) -> usize {
        self.block_size
    }

    pub const fn block_config(&self) -> PteConfigOf<T> {
        self.block_config
    }
}

impl<T: TableMeta, A: FrameAllocator> core::fmt::Debug for HugeSplitDeposit<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HugeSplitDeposit")
            .field("root_paddr", &self.root_paddr)
            .field("block_vaddr", &self.block_vaddr)
            .field("block_paddr", &self.block_paddr)
            .field("block_size", &self.block_size)
            .finish_non_exhaustive()
    }
}

/// Receipt proving that one deposited child table is now reachable from the
/// page-table tree.
///
/// The receipt does not own mapped data frames.  Its metadata is retained by a
/// higher-level mutation receipt when rollback, reverse mappings, or delayed
/// page-table-frame reclamation must be coordinated with a TLB obligation.
pub struct InstalledHugeSplit<T: TableMeta> {
    root_paddr: PhysAddr,
    block_vaddr: VirtAddr,
    block_paddr: PhysAddr,
    block_config: PteConfigOf<T>,
    block_size: usize,
    child_table_paddr: PhysAddr,
}

impl<T: TableMeta> InstalledHugeSplit<T> {
    pub const fn root_paddr(&self) -> PhysAddr {
        self.root_paddr
    }

    pub const fn block_vaddr(&self) -> VirtAddr {
        self.block_vaddr
    }

    pub const fn block_paddr(&self) -> PhysAddr {
        self.block_paddr
    }

    pub const fn block_config(&self) -> PteConfigOf<T> {
        self.block_config
    }

    pub const fn block_size(&self) -> usize {
        self.block_size
    }

    pub const fn child_table_paddr(&self) -> PhysAddr {
        self.child_table_paddr
    }
}

impl<T: TableMeta> core::fmt::Debug for InstalledHugeSplit<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InstalledHugeSplit")
            .field("root_paddr", &self.root_paddr)
            .field("block_vaddr", &self.block_vaddr)
            .field("block_paddr", &self.block_paddr)
            .field("block_size", &self.block_size)
            .field("child_table_paddr", &self.child_table_paddr)
            .finish_non_exhaustive()
    }
}

impl<T: TableMeta, A: FrameAllocator> ReservedTable<T, A> {
    fn frame(&self) -> Frame<T, A> {
        match self.frame.as_ref() {
            Some(frame) => frame.clone(),
            None => unreachable!("a reserved table is consumed at most once"),
        }
    }

    fn disarm(&mut self) {
        self.frame = None;
    }
}

impl<T: TableMeta, A: FrameAllocator> Drop for ReservedTable<T, A> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            frame.allocator.dealloc_frame(frame.paddr);
        }
    }
}

pub struct PageTable<T: TableMeta, A: FrameAllocator> {
    inner: PageTableRef<T, A>,
    /// Set once ownership of all page-table frames has been transferred to
    /// detached tokens.  `Drop` must not release them a second time.
    detached: bool,
    #[cfg(feature = "copy-from")]
    borrowed_root_entries: Option<Range<usize>>,
}

impl<T: TableMeta, A: FrameAllocator> PageTable<T, A> {
    pub const VALID_BITS: usize = Frame::<T, A>::PT_VALID_BITS;

    /// 创建一个新的页表
    pub fn new(allocator: A) -> PagingResult<Self> {
        let inner = unsafe { PageTableRef::new(allocator) }?;
        Ok(Self {
            inner,
            detached: false,
            #[cfg(feature = "copy-from")]
            borrowed_root_entries: None,
        })
    }

    pub const fn root_paddr(&self) -> PhysAddr {
        self.inner.root.paddr
    }

    /// Preallocates and retains the root directories covering `range`.
    ///
    /// This is the page-table analogue of Linux preallocating the vmalloc
    /// directory levels before process roots copy the kernel half. A process
    /// root may subsequently borrow these entries once; all later mappings
    /// are published below the stable shared directories.
    ///
    /// Callers must finish this operation before sharing the affected root
    /// entries or otherwise publishing this page table. Allocation failure may
    /// leave a prefix installed, but that private prefix remains owned by this
    /// page table and is reclaimed by its normal destructor.
    pub fn preallocate_shared_root_entries(
        &mut self,
        start_vaddr: VirtAddr,
        size: usize,
    ) -> PagingResult
    where
        PteConfigOf<T>: PartialEq,
    {
        let Some(entries) = Self::root_entry_range(start_vaddr, size)? else {
            return Ok(());
        };
        let span = RootEntrySpan {
            start: entries.start,
            end: entries.end,
        };
        if self
            .inner
            .retained_root_entries
            .is_some_and(|retained| retained != span)
        {
            return Err(PagingError::hierarchy_error(
                "Page table already retains a different root-entry range",
            ));
        }
        self.inner.retained_root_entries = Some(span);

        let root_entry_size = Frame::<T, A>::level_size(Frame::<T, A>::PT_LEVEL);
        let first_entry_vaddr = start_vaddr.align_down(root_entry_size);
        for (entry_offset, index) in entries.enumerate() {
            let current = self.inner.root.as_slice()[index];
            if current.unused() {
                let child = Frame::<T, A>::new(self.inner.root.allocator.clone())?;
                self.inner.root.as_slice_mut()[index] = T::P::new_table(child.paddr);
                continue;
            }
            if !current.present() {
                return Err(PagingError::hierarchy_error(
                    "Shared root entry is not a child page table",
                ));
            }
            if current.huge(true) {
                let offset = entry_offset.checked_mul(root_entry_size).ok_or_else(|| {
                    PagingError::address_overflow("shared root entry virtual address")
                })?;
                let entry_vaddr = first_entry_vaddr
                    .as_usize()
                    .checked_add(offset)
                    .map(VirtAddr::from_usize)
                    .ok_or_else(|| {
                        PagingError::address_overflow("shared root entry virtual address")
                    })?;
                self.split_huge_page(entry_vaddr)?;
            }
        }
        Ok(())
    }

    /// Detaches every page-table frame owned by this table and transfers the
    /// release capability to `release`.  Mapped data frames are never touched.
    ///
    /// # Safety
    ///
    /// The caller must have stopped all page-table users and completed the
    /// required local/remote TLB invalidations before reclaiming the returned
    /// tokens.  The table is unusable after this call; its `Drop` implementation
    /// intentionally skips frame release to prevent a double free.
    pub unsafe fn detach(&mut self, mut release: impl FnMut(DetachedPageTableFrame<A>)) {
        if self.detached {
            return;
        }
        #[cfg(feature = "copy-from")]
        self.detach_borrowed_root_entries();
        // Publish the inert state before invoking caller code. If a callback
        // unwinds after consuming a prefix of tokens, Drop must leak the
        // undispatched suffix rather than recursively double-free that prefix.
        self.detached = true;
        self.inner
            .root
            .detach_recursive(Frame::<T, A>::PT_LEVEL, &mut release);
    }

    /// Releases the owning table exactly once.  `PageTableRef` remains a
    /// copyable view for the legacy walker API, so the detached guard belongs
    /// to this owning wrapper rather than to the view itself.
    unsafe fn deallocate_inner(&mut self) {
        if self.detached {
            return;
        }
        // SAFETY: callers of this helper are the owning `Drop` path; the
        // caller has exclusive access to the page-table tree.
        self.inner
            .root
            .deallocate_recursive(Frame::<T, A>::PT_LEVEL);
        self.detached = true;
    }

    /// Releases all page-table frames and permanently invalidates this
    /// owning table.  Calling it more than once is harmless; the detached bit
    /// makes the operation idempotent for teardown/recovery code.
    ///
    /// # Safety
    ///
    /// No CPU or walker may still use the table when this method is called.
    pub unsafe fn deallocate(&mut self) {
        // SAFETY: the precondition is carried by this public unsafe API.
        unsafe {
            self.deallocate_inner();
        }
    }

    /// Consumes the owning table after releasing its page-table frames.
    /// Mapped data frames are intentionally left to the mapping owner.
    ///
    /// # Safety
    ///
    /// The caller must establish the same quiescence requirements as
    /// [`Self::deallocate`].
    pub unsafe fn destroy(mut self) {
        // SAFETY: forwarded from the method's quiescence contract.
        unsafe {
            self.deallocate_inner();
        }
    }

    /// Abandons the allocator capability for this table without attempting a
    /// fallible teardown.
    ///
    /// This is intentionally an explicit leak used only when an owning
    /// address-space destructor discovers that mappings or a TLB quarantine
    /// are still live.  `Drop` must not reclaim page-table frames in that
    /// state: doing so could let a stale CPU walk a frame that has already
    /// been reused.  The caller must retain an out-of-band repair record if
    /// those frames are to be reclaimed after the missing quiescence is fixed.
    pub fn leak(&mut self) {
        #[cfg(feature = "copy-from")]
        self.detach_borrowed_root_entries();
        self.detached = true;
    }

    /// Returns the occupied leaf PTE and the level at which it was found.
    /// Unlike `translate_recursive_with_level`, this also reports a retained
    /// non-present leaf, which is needed by rollback and quarantine code.
    pub fn query_occupied(&self, vaddr: VirtAddr) -> PagingResult<(T::P, usize)> {
        self.inner
            .root
            .find_occupied_leaf(vaddr, Frame::<T, A>::PT_LEVEL)
    }

    /// Convenience wrapper for a VA→PA mapping.  The endpoint arithmetic is
    /// checked before any PTE is written.  Contiguous ranges may use block
    /// descriptors; sparse/device ranges are represented by base-page leaves.
    pub fn map_linear_pages(
        &mut self,
        start_vaddr: VirtAddr,
        start_paddr: PhysAddr,
        size: usize,
        config: PteConfigOf<T>,
        allow_huge: bool,
    ) -> PagingResult {
        if size == 0 || !size.is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::invalid_size(
                "Linear mapping size must be base-page aligned",
            ));
        }
        start_vaddr.as_usize().checked_add(size).ok_or_else(|| {
            PagingError::address_overflow("Virtual address overflow in map_linear_pages")
        })?;
        start_paddr.as_usize().checked_add(size).ok_or_else(|| {
            PagingError::address_overflow("Physical address overflow in map_linear_pages")
        })?;
        self.map_region_with_selection(
            start_vaddr,
            |vaddr| {
                let offset = vaddr
                    .as_usize()
                    .checked_sub(start_vaddr.as_usize())
                    .ok_or_else(|| {
                        PagingError::address_overflow(
                            "Virtual address precedes linear mapping start",
                        )
                    })?;
                let paddr = start_paddr.as_usize().checked_add(offset).ok_or_else(|| {
                    PagingError::address_overflow("Physical address overflow in linear mapping")
                })?;
                Ok(PhysAddr::from_usize(paddr))
            },
            size,
            config,
            RegionPageSelection::Linear { allow_huge },
        )
    }

    /// Deep-copies source root entries that are absent from this page table.
    ///
    /// Leaf mappings keep referring to the same physical memory, while every
    /// copied intermediate page-table frame is independently owned by this
    /// page table. Existing destination root entries are left unchanged.
    ///
    /// If allocation fails, entries copied before the failure remain installed
    /// and are reclaimed normally when this page table is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the range overflows or wraps around the root table,
    /// or if an intermediate page-table frame cannot be allocated.
    pub fn clone_missing_root_entries_from(
        &mut self,
        other: &PageTableRef<T, A>,
        start_vaddr: VirtAddr,
        size: usize,
    ) -> PagingResult {
        let Some(entries) = Self::root_entry_range(start_vaddr, size)? else {
            return Ok(());
        };

        let root_level = Frame::<T, A>::PT_LEVEL;
        let mut changed = false;
        for index in entries {
            changed |= self
                .inner
                .root
                .clone_entry_from(&other.root, index, root_level)?;
        }
        if changed {
            T::flush(None);
        }
        Ok(())
    }

    /// Shares root page-table entries from another page table.
    ///
    /// Mappings below the shared root entries remain owned by the source and
    /// changes made there are visible through both page tables.
    ///
    /// # Safety
    ///
    /// The source page table must outlive this page table. The caller must
    /// also prevent this page table from modifying or unmapping the shared
    /// virtual-address range.
    #[cfg(feature = "copy-from")]
    pub unsafe fn share_root_entries_from(
        &mut self,
        other: &Self,
        start_vaddr: VirtAddr,
        size: usize,
    ) -> PagingResult {
        if size == 0 {
            return Ok(());
        }
        if self.borrowed_root_entries.is_some() {
            return Err(PagingError::hierarchy_error(
                "Page table already contains shared root entries",
            ));
        }

        let Some(entries) = Self::root_entry_range(start_vaddr, size)? else {
            return Ok(());
        };
        let root_level = Frame::<T, A>::PT_LEVEL;

        for index in entries.clone() {
            self.inner.root.dealloc_entry_recursive(index, root_level);
            self.inner.root.as_slice_mut()[index] = other.inner.root.as_slice()[index];
        }
        self.borrowed_root_entries = Some(entries);
        T::flush(None);
        Ok(())
    }

    fn root_entry_range(start_vaddr: VirtAddr, size: usize) -> PagingResult<Option<Range<usize>>> {
        if size == 0 {
            return Ok(None);
        }
        let end_vaddr = start_vaddr
            .as_usize()
            .checked_add(size)
            .ok_or_else(|| PagingError::address_overflow("root_entry_range"))?;
        let root_level = Frame::<T, A>::PT_LEVEL;
        let start_index = Frame::<T, A>::virt_to_index(start_vaddr, root_level);
        let end_index =
            Frame::<T, A>::virt_to_index(VirtAddr::from_usize(end_vaddr - 1), root_level) + 1;
        if start_index >= end_index {
            return Err(PagingError::invalid_range(
                "Range must be contiguous in the root page table",
            ));
        }
        Ok(Some(start_index..end_index))
    }

    #[cfg(feature = "copy-from")]
    fn detach_borrowed_root_entries(&mut self) {
        let Some(entries) = self.borrowed_root_entries.take() else {
            return;
        };
        for index in entries {
            self.inner.root.as_slice_mut()[index].clear();
        }
    }
}

impl<T: TableMeta, A: FrameAllocator> Drop for PageTable<T, A> {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        #[cfg(feature = "copy-from")]
        self.detach_borrowed_root_entries();
        unsafe {
            // 释放所有页表帧，但不释放映射的物理页
            self.deallocate_inner();
        }
    }
}

impl<T: TableMeta, A: FrameAllocator> Deref for PageTable<T, A> {
    type Target = PageTableRef<T, A>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: TableMeta, A: FrameAllocator> DerefMut for PageTable<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

pub struct PageTableRef<T: TableMeta, A: FrameAllocator> {
    pub(crate) root: Frame<T, A>,
    /// Root directories that must remain installed after their last leaf is
    /// removed. Kernel page tables use this for ranges shared into process
    /// roots: later mappings remain visible through the already-shared child
    /// directory instead of requiring root-entry propagation.
    retained_root_entries: Option<RootEntrySpan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootEntrySpan {
    start: usize,
    end: usize,
}

impl<T: TableMeta, A: FrameAllocator> core::fmt::Debug for PageTableRef<T, A>
where
    T::P: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageTable")
            .field(
                "root_paddr",
                &format_args!("{:#x}", self.root.paddr.as_usize()),
            )
            .field("table_levels", &T::LEVEL_BITS.len())
            .field("max_block_level", &T::MAX_BLOCK_LEVEL)
            .field("page_size", &format_args!("{:#x}", T::PAGE_SIZE))
            .finish()
    }
}

impl<T: TableMeta, A: FrameAllocator> PageTableRef<T, A> {
    /// 创建一个新的页表
    ///
    /// # Safety
    ///
    /// 调用者必须确保提供的FrameAllocator是有效的，并且在页表生命周期内保持有效
    pub(crate) unsafe fn new(allocator: A) -> PagingResult<Self> {
        let root = Frame::new_root(allocator)?;
        Ok(Self {
            root,
            retained_root_entries: None,
        })
    }

    /// Creates a non-owning view of an existing page-table root.
    ///
    /// The returned view may inspect and mutate page-table entries, but it
    /// never owns or releases any page-table frame. Only [`PageTable`] carries
    /// the corresponding frame-reclamation capability.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `paddr` names an aligned, initialized root
    /// table for `T`, that every table frame reachable from it remains mapped
    /// by `allocator` for the entire use of this value, and that all mutable
    /// access is serialized with hardware walkers and other page-table users.
    /// The caller must also ensure that the owning page table outlives this
    /// view.
    pub unsafe fn from_paddr(paddr: PhysAddr, allocator: A) -> Self {
        let root = Frame::from_root_paddr(paddr, allocator);
        Self {
            root,
            retained_root_entries: None,
        }
    }

    /// Maps one page with the requested page size.
    pub fn map_page(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        page_size: usize,
        config: PteConfigOf<T>,
    ) -> PagingResult {
        let Some(level) = Frame::<T, A>::level_for_page_size(page_size) else {
            return Err(PagingError::invalid_size(
                "Page size is not represented by the page-table levels",
            ));
        };
        if level > 1 && level > T::MAX_BLOCK_LEVEL {
            return Err(PagingError::invalid_size(
                "Page size exceeds the architecture's block-mapping level",
            ));
        }
        self.map(&MapConfig {
            vaddr: vaddr.align_down(page_size),
            paddr: paddr.align_down(page_size),
            size: page_size,
            pte: config,
            allow_huge: level > 1,
            flush: true,
        })
    }

    /// Maps a virtual region from a per-base-page physical resolver.
    ///
    /// The resolver may return a non-contiguous physical page sequence, so this
    /// API deliberately installs only base-page leaves. Use
    /// [`Self::map_linear_pages`] when the physical range is known to be
    /// contiguous and block mappings are allowed.
    ///
    /// Mappings installed by this call are rolled back if a later page fails.
    /// TLB invalidation is deferred and batched until the region has been
    /// updated.
    pub fn map_region(
        &mut self,
        start_vaddr: VirtAddr,
        get_paddr: impl Fn(VirtAddr) -> PhysAddr,
        size: usize,
        config: PteConfigOf<T>,
    ) -> PagingResult {
        self.map_region_checked(start_vaddr, |vaddr| Ok(get_paddr(vaddr)), size, config)
    }

    /// Maps a virtual region using a fallible physical-address resolver.
    ///
    /// The resolver is evaluated before each PTE write.  If it rejects a
    /// later page, mappings already installed by this invocation are rolled
    /// back and the resolver error is returned. This is the capability used
    /// by allocation-backed or sparse device mappings: address resolution
    /// must remain checked all the way through the page-table walker. Because
    /// the resolver does not prove contiguity, this API uses base-page leaves.
    pub fn map_region_checked(
        &mut self,
        start_vaddr: VirtAddr,
        get_paddr: impl FnMut(VirtAddr) -> PagingResult<PhysAddr>,
        size: usize,
        config: PteConfigOf<T>,
    ) -> PagingResult {
        self.map_region_with_selection(
            start_vaddr,
            get_paddr,
            size,
            config,
            RegionPageSelection::BasePages,
        )
    }

    fn map_region_with_selection(
        &mut self,
        start_vaddr: VirtAddr,
        mut get_paddr: impl FnMut(VirtAddr) -> PagingResult<PhysAddr>,
        size: usize,
        config: PteConfigOf<T>,
        page_selection: RegionPageSelection,
    ) -> PagingResult {
        if size == 0 {
            return Err(PagingError::invalid_size("Region size cannot be zero"));
        }
        if !start_vaddr.as_usize().is_multiple_of(T::PAGE_SIZE)
            || !size.is_multiple_of(T::PAGE_SIZE)
        {
            return Err(PagingError::alignment_error(
                "Region start and size must be base-page aligned",
            ));
        }
        start_vaddr.as_usize().checked_add(size).ok_or_else(|| {
            PagingError::address_overflow("Virtual address overflow in map_region")
        })?;
        self.validate_address_width(start_vaddr, size, "map_region")?;

        let mut offset = 0;
        let mut flush_addrs = heapless::Vec::<VirtAddr, TARGETED_FLUSH_LIMIT>::new();
        let mut full_flush = false;
        let result = loop {
            if offset >= size {
                break Ok(());
            }
            let vaddr =
                VirtAddr::from_usize(start_vaddr.as_usize().checked_add(offset).ok_or_else(
                    || PagingError::address_overflow("Virtual address overflow in map_region"),
                )?);
            let paddr = match get_paddr(vaddr) {
                Ok(paddr) => paddr,
                Err(error) => {
                    let rollback_result = if offset == 0 {
                        Ok(())
                    } else {
                        self.unmap_with_config(&UnmapConfig {
                            start_vaddr,
                            size: offset,
                            flush: false,
                        })
                    };
                    break match rollback_result {
                        Ok(()) => Err(error),
                        Err(rollback_err) => Err(rollback_err),
                    };
                }
            };
            if !paddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
                let rollback_result = if offset == 0 {
                    Ok(())
                } else {
                    self.unmap_with_config(&UnmapConfig {
                        start_vaddr,
                        size: offset,
                        flush: false,
                    })
                };
                break match rollback_result {
                    Ok(()) => Err(PagingError::alignment_error(
                        "Physical resolver returned an unaligned base page",
                    )),
                    Err(rollback_err) => Err(rollback_err),
                };
            }
            let page_size = match page_selection {
                RegionPageSelection::BasePages => T::PAGE_SIZE,
                RegionPageSelection::Linear { allow_huge } => {
                    largest_page_size::<T, A>(vaddr, paddr, size - offset, allow_huge)
                }
            };
            if let Err(err) = self.map(&MapConfig {
                vaddr: vaddr.align_down(page_size),
                paddr: paddr.align_down(page_size),
                size: page_size,
                pte: config,
                allow_huge: page_size > T::PAGE_SIZE,
                flush: false,
            }) {
                let rollback_result = if offset == 0 {
                    Ok(())
                } else {
                    self.unmap_with_config(&UnmapConfig {
                        start_vaddr,
                        size: offset,
                        flush: false,
                    })
                };
                break match rollback_result {
                    Ok(()) => Err(err),
                    Err(rollback_err) => Err(rollback_err),
                };
            }
            if !full_flush && flush_addrs.push(vaddr).is_err() {
                full_flush = true;
                flush_addrs.clear();
            }
            offset = match offset.checked_add(page_size) {
                Some(offset) => offset,
                None => {
                    let _ = self.unmap_with_config(&UnmapConfig {
                        start_vaddr,
                        size: offset,
                        flush: false,
                    });
                    break Err(PagingError::address_overflow(
                        "Mapping offset overflow in map_region",
                    ));
                }
            };
        };

        if full_flush {
            T::flush(None);
        } else {
            for vaddr in flush_addrs {
                T::flush(Some(vaddr));
            }
        }
        result
    }

    /// Unmaps one page and returns its physical address, flags, and page size.
    pub fn unmap_page(
        &mut self,
        vaddr: VirtAddr,
    ) -> PagingResult<(PhysAddr, PteConfigOf<T>, usize)> {
        let (pte, level) = self
            .root
            .find_occupied_leaf(vaddr, Frame::<T, A>::PT_LEVEL)?;
        let page_size = Frame::<T, A>::level_size(level);
        let is_dir = level > 1;
        let paddr = pte.paddr(is_dir);
        let config = pte.config(is_dir);
        self.unmap_with_config(&UnmapConfig {
            start_vaddr: vaddr.align_down(page_size),
            size: page_size,
            flush: true,
        })?;
        Ok((paddr, config, page_size))
    }

    /// Returns the huge block covering `vaddr` without changing the table.
    /// Retained non-present blocks are reported when the PTE format preserves
    /// their descriptor state.
    pub fn peek_huge_block(&self, vaddr: VirtAddr) -> Option<(PhysAddr, PteConfigOf<T>, usize)> {
        let (pte, level) = self
            .root
            .find_occupied_leaf(vaddr, Frame::<T, A>::PT_LEVEL)
            .ok()?;
        let is_dir = level > 1;
        pte.huge(is_dir).then(|| {
            (
                pte.paddr(is_dir),
                pte.config(is_dir),
                Frame::<T, A>::level_size(level),
            )
        })
    }

    /// Allocates a pre-zeroed child table and binds it to the currently
    /// observed huge leaf.
    ///
    /// The returned deposit may be stored by a mapping owner until a future
    /// partial operation needs to split the leaf.  Apply revalidates the root,
    /// virtual range, physical frame, configuration, and size before touching
    /// any descriptor.
    pub fn prepare_huge_split(&self, vaddr: VirtAddr) -> PagingResult<HugeSplitDeposit<T, A>> {
        let (block_paddr, block_config, block_size) = self
            .peek_huge_block(vaddr)
            .ok_or_else(PagingError::not_mapped)?;
        let block_vaddr = vaddr.align_down(block_size);
        let table = ReservedTable {
            frame: Some(Frame::<T, A>::new(self.root.allocator.clone())?),
        };
        Ok(HugeSplitDeposit {
            table,
            root_paddr: self.root_paddr(),
            block_vaddr,
            block_paddr,
            block_config,
            block_size,
        })
    }

    fn validate_huge_split_deposit(&self, deposit: &HugeSplitDeposit<T, A>) -> PagingResult
    where
        PteConfigOf<T>: PartialEq,
    {
        let current = self.peek_huge_block(deposit.block_vaddr);
        if self.root_paddr() != deposit.root_paddr
            || !current.is_some_and(|(paddr, config, size)| {
                paddr == deposit.block_paddr
                    && config == deposit.block_config
                    && size == deposit.block_size
            })
        {
            return Err(PagingError::stale_huge_split(deposit.block_vaddr));
        }
        Ok(())
    }

    fn try_apply_huge_split_deposit(
        &mut self,
        mut deposit: HugeSplitDeposit<T, A>,
        fill: HugeSplitFill,
    ) -> Result<InstalledHugeSplit<T>, HugeSplitApplyError<T, A>>
    where
        PteConfigOf<T>: PartialEq,
    {
        if let Err(error) = self.validate_huge_split_deposit(&deposit) {
            return Err(HugeSplitApplyError { error, deposit });
        }
        let child_table_paddr = deposit.table.frame().paddr;
        let frame = deposit.table.frame();
        let (block_paddr, block_config, block_size) = match self.root.split_huge_page_recursive(
            deposit.block_vaddr,
            Frame::<T, A>::PT_LEVEL,
            frame,
            fill,
        ) {
            Ok(installed) => installed,
            Err(error) => return Err(HugeSplitApplyError { error, deposit }),
        };
        // The child frame is now reachable from the tree.  Disarm immediately
        // after the structural apply so no later receipt construction can
        // accidentally free a live page-table frame.
        deposit.table.disarm();
        Ok(InstalledHugeSplit {
            root_paddr: deposit.root_paddr,
            block_vaddr: deposit.block_vaddr,
            block_paddr,
            block_config,
            block_size,
            child_table_paddr,
        })
    }

    /// Consumes a bound deposit and splits its huge block into inherited finer
    /// leaves.  No allocation occurs during apply.
    pub fn split_huge_page_with(
        &mut self,
        deposit: HugeSplitDeposit<T, A>,
    ) -> PagingResult<InstalledHugeSplit<T>>
    where
        PteConfigOf<T>: PartialEq,
    {
        self.try_split_huge_page_with(deposit)
            .map_err(|failure| failure.into_parts().0)
    }

    /// Transactional variant of [`Self::split_huge_page_with`].  On failure the
    /// caller receives the still-owned deposit and can put it back into its
    /// mapping slot without allocating during recovery.
    pub fn try_split_huge_page_with(
        &mut self,
        deposit: HugeSplitDeposit<T, A>,
    ) -> Result<InstalledHugeSplit<T>, HugeSplitApplyError<T, A>>
    where
        PteConfigOf<T>: PartialEq,
    {
        self.try_apply_huge_split_deposit(deposit, HugeSplitFill::Inherit)
    }

    /// Splits a huge block and installs an empty child table for a caller that
    /// will materialize non-contiguous finer leaves under the same mutation
    /// domain. The old block metadata is returned for rollback/accounting.
    pub fn split_huge_block_to_empty_table(
        &mut self,
        deposit: HugeSplitDeposit<T, A>,
    ) -> PagingResult<InstalledHugeSplit<T>>
    where
        PteConfigOf<T>: PartialEq,
    {
        self.try_apply_huge_split_deposit(deposit, HugeSplitFill::Empty)
            .map_err(|failure| failure.into_parts().0)
    }

    /// Rolls an installed split back to the exact huge descriptor captured by
    /// its receipt and returns ownership of the withdrawn child table.
    ///
    /// No allocation occurs.  The returned deposit is bound to the restored
    /// block and can either be retained for a retry or dropped to release the
    /// now-unpublished page-table frame.  This is the inverse of
    /// [`Self::split_huge_page_with`] used by unpublished transaction aborts.
    pub fn restore_huge_split(
        &mut self,
        installed: InstalledHugeSplit<T>,
    ) -> PagingResult<HugeSplitDeposit<T, A>> {
        if self.root_paddr() != installed.root_paddr {
            return Err(PagingError::stale_huge_split(installed.block_vaddr));
        }
        let frame = self.root.restore_huge_page_recursive(
            installed.block_vaddr,
            installed.block_paddr,
            installed.block_config,
            installed.block_size,
            installed.child_table_paddr,
            Frame::<T, A>::PT_LEVEL,
        )?;
        Ok(HugeSplitDeposit {
            table: ReservedTable { frame: Some(frame) },
            root_paddr: installed.root_paddr,
            block_vaddr: installed.block_vaddr,
            block_paddr: installed.block_paddr,
            block_config: installed.block_config,
            block_size: installed.block_size,
        })
    }

    /// Prepares and performs an inherited huge split. Transactional callers
    /// should retain [`HugeSplitDeposit`] from [`Self::prepare_huge_split`]
    /// before entering their mutation critical section.
    pub fn split_huge_page(&mut self, vaddr: VirtAddr) -> PagingResult<usize>
    where
        PteConfigOf<T>: PartialEq,
    {
        let deposit = self.prepare_huge_split(vaddr)?;
        self.split_huge_page_with(deposit)
            .map(|installed| installed.block_size())
    }

    /// Changes one existing mapping's flags and returns its page size.
    pub fn protect_page(&mut self, vaddr: VirtAddr, config: PteConfigOf<T>) -> PagingResult<usize> {
        let page_size = self
            .root
            .protect_recursive(vaddr, config, Frame::<T, A>::PT_LEVEL)?;
        T::flush(Some(vaddr));
        Ok(page_size)
    }

    /// Changes flags for a region. Unmapped base pages are skipped.
    pub fn protect_region(
        &mut self,
        start_vaddr: VirtAddr,
        size: usize,
        config: PteConfigOf<T>,
    ) -> PagingResult {
        let end = start_vaddr
            .as_usize()
            .checked_add(size)
            .ok_or_else(|| PagingError::address_overflow("protect_region"))?;
        if size == 0 {
            return Ok(());
        }

        // Linux splits only block mappings crossed by a protection boundary.
        // Interior blocks remain large mappings and can be updated as a unit.
        self.root
            .split_leaf_for_boundary(start_vaddr, start_vaddr, Frame::<T, A>::PT_LEVEL)?;
        self.root.split_leaf_for_boundary(
            VirtAddr::from_usize(end - 1),
            VirtAddr::from_usize(end),
            Frame::<T, A>::PT_LEVEL,
        )?;

        let mut vaddr = start_vaddr;
        while vaddr.as_usize() < end {
            match self.protect_page(vaddr, config) {
                Ok(page_size) => {
                    vaddr = vaddr
                        .as_usize()
                        .checked_add(page_size)
                        .map(VirtAddr::from_usize)
                        .ok_or_else(|| {
                            PagingError::address_overflow("protect_region address advance")
                        })?;
                }
                Err(PagingError::NotMapped) => {
                    vaddr = vaddr
                        .as_usize()
                        .checked_add(T::PAGE_SIZE)
                        .map(VirtAddr::from_usize)
                        .ok_or_else(|| {
                            PagingError::address_overflow("protect_region address advance")
                        })?;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Remaps one existing mapping and returns its page size.
    pub fn remap_page(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        config: PteConfigOf<T>,
    ) -> PagingResult<usize> {
        let page_size = self
            .root
            .remap_recursive(vaddr, paddr, config, Frame::<T, A>::PT_LEVEL)?;
        T::flush(Some(vaddr));
        Ok(page_size)
    }

    /// Queries one mapping and returns the translated physical address, flags, and page size.
    pub fn query(&self, vaddr: VirtAddr) -> PagingResult<(PhysAddr, PteConfigOf<T>, usize)> {
        let (paddr, pte, level) = self.translate_with_level(vaddr)?;
        Ok((
            paddr,
            pte.config(level > 1),
            Frame::<T, A>::level_size(level),
        ))
    }

    /// 映射虚拟地址范围到物理地址范围
    pub fn map(&mut self, config: &MapConfig<PteConfigOf<T>>) -> PagingResult {
        // 验证输入参数
        self.validate_map_config(config)?;

        // 检查大小溢出
        if config.vaddr.as_usize().checked_add(config.size).is_none()
            || config.paddr.as_usize().checked_add(config.size).is_none()
        {
            return Err(PagingError::address_overflow(
                "Virtual or physical address overflow",
            ));
        }
        self.validate_address_width(config.vaddr, config.size, "map")?;

        let end_vaddr = config
            .vaddr
            .as_usize()
            .checked_add(config.size)
            .map(VirtAddr::from_usize)
            .ok_or_else(|| PagingError::address_overflow("Virtual address overflow in map"))?;
        self.root.map_range_recursive(MapRecursiveConfig {
            start_vaddr: config.vaddr,
            start_paddr: config.paddr,
            end_vaddr,
            level: Frame::<T, A>::PT_LEVEL,
            allow_huge: config.allow_huge,
            flush: config.flush,
            pte_template: config.pte,
        })?;

        Ok(())
    }

    /// 取消映射虚拟地址范围
    ///
    /// # 参数
    /// - `start_vaddr`: 要取消映射的起始虚拟地址
    /// - `size`: 要取消映射的大小（字节）
    ///
    /// # 返回值
    /// - `Ok(())`: 取消映射成功
    /// - `Err(PagingError)`: 取消映射失败
    ///
    /// # 行为
    /// - 清除指定虚拟地址范围内的所有页表项
    /// - 自动回收空的子页表帧
    /// - 支持大页和普通页面的取消映射
    /// - 根据配置刷新TLB
    pub fn unmap(&mut self, start_vaddr: VirtAddr, size: usize) -> PagingResult<()> {
        // 验证输入参数
        self.validate_unmap_params(start_vaddr, size)?;

        // 检查大小溢出
        let end_vaddr: VirtAddr = match start_vaddr.as_usize().checked_add(size) {
            Some(end) => VirtAddr::from_usize(end),
            None => {
                return Err(PagingError::address_overflow(
                    "Virtual address overflow in unmap",
                ));
            }
        };
        self.validate_address_width(start_vaddr, size, "unmap")?;

        self.root.unmap_range_recursive(UnmapRecursiveConfig {
            start_vaddr,
            end_vaddr,
            level: Frame::<T, A>::PT_LEVEL,
            flush: true, // 默认刷新TLB确保一致性
            retained_root_entries: self
                .retained_root_entries
                .map(|entries| (entries.start, entries.end)),
        })?;

        Ok(())
    }

    /// 使用配置对象取消映射
    pub fn unmap_with_config(&mut self, config: &UnmapConfig) -> PagingResult<()> {
        self.validate_unmap_params(config.start_vaddr, config.size)?;

        let end_vaddr = match config.start_vaddr.as_usize().checked_add(config.size) {
            Some(end) => VirtAddr::from_usize(end),
            None => {
                return Err(PagingError::address_overflow(
                    "Virtual address overflow in unmap_with_config",
                ));
            }
        };
        self.validate_address_width(config.start_vaddr, config.size, "unmap_with_config")?;

        self.root.unmap_range_recursive(UnmapRecursiveConfig {
            start_vaddr: config.start_vaddr,
            end_vaddr,
            level: Frame::<T, A>::PT_LEVEL,
            flush: config.flush,
            retained_root_entries: self
                .retained_root_entries
                .map(|entries| (entries.start, entries.end)),
        })?;

        Ok(())
    }

    /// 验证取消映射参数的有效性
    fn validate_unmap_params(&self, start_vaddr: VirtAddr, size: usize) -> PagingResult<()> {
        if size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero in unmap"));
        }

        // 检查虚拟地址是否页对齐
        if !start_vaddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Start virtual address not page aligned in unmap",
            ));
        }

        // 检查大小是否页对齐
        if !size.is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Size not page aligned in unmap",
            ));
        }

        Ok(())
    }

    /// 创建页表遍历迭代器
    pub fn walk_all(&self, config: WalkConfig) -> PageTableWalker<'_, T, A> {
        PageTableWalker::new(self, config)
    }

    pub fn walk(
        &self,
        start_vaddr: VirtAddr,
        end_vaddr: VirtAddr,
    ) -> impl Iterator<Item = crate::walk::PteInfo<T::P>> + '_ {
        let config = WalkConfig {
            start_vaddr,
            end_vaddr,
        };
        PageTableWalker::new(self, config).filter(|p| p.pte.present())
    }

    /// 遍历所有有效的最终映射页表项（过滤掉无效项和中间级别的页表指针）
    pub fn walk_valid(&self) -> impl Iterator<Item = crate::walk::PteInfo<T::P>> + '_ {
        self.walk(0.into(), usize::MAX.into())
            .filter(|p| p.pte.present() && p.is_final_mapping)
    }

    /// Walks every occupied final leaf, including retained non-present leaves.
    ///
    /// Rollback and quarantine code must distinguish an empty page-table slot
    /// from a descriptor that still owns a physical mapping but has had its
    /// access permissions removed. The walk scales with allocated page-table
    /// frames rather than with the represented virtual address span.
    pub fn walk_occupied(&self) -> impl Iterator<Item = crate::walk::PteInfo<T::P>> + '_ {
        let config = WalkConfig {
            start_vaddr: 0.into(),
            end_vaddr: usize::MAX.into(),
        };
        PageTableWalker::new(self, config)
            .filter(|p| !p.pte.unused() && (p.level == 1 || p.pte.huge(p.level > 1)))
    }

    /// Returns the mapping size represented by one page-table level.
    pub fn mapping_size_for_level(&self, level: usize) -> Option<usize> {
        (level != 0 && level <= T::LEVEL_BITS.len()).then(|| Frame::<T, A>::level_size(level))
    }

    /// 验证映射配置的有效性
    fn validate_map_config(&self, config: &MapConfig<PteConfigOf<T>>) -> PagingResult {
        if config.size == 0 {
            return Err(PagingError::invalid_size("Size cannot be zero"));
        }

        // 检查虚拟地址和物理地址是否页对齐
        if !config.vaddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Virtual address not page aligned",
            ));
        }

        if !config.paddr.as_usize().is_multiple_of(T::PAGE_SIZE) {
            return Err(PagingError::alignment_error(
                "Physical address not page aligned",
            ));
        }

        Ok(())
    }

    fn validate_address_width(
        &self,
        start_vaddr: VirtAddr,
        size: usize,
        operation: &'static str,
    ) -> PagingResult<()> {
        if !T::STRICT_ADDRESS_WIDTH {
            return Ok(());
        }
        let Some(end) = start_vaddr.as_usize().checked_add(size) else {
            return Err(PagingError::address_overflow(
                "Virtual address range overflow",
            ));
        };
        let last = end - 1;
        if !Self::is_addr_in_width(start_vaddr.as_usize()) || !Self::is_addr_in_width(last) {
            return Err(PagingError::address_overflow(operation));
        }
        Ok(())
    }

    pub const fn page_size() -> usize {
        T::PAGE_SIZE
    }

    pub const fn table_levels() -> usize {
        T::LEVEL_BITS.len()
    }

    pub const fn valid_bits() -> usize {
        Frame::<T, A>::PT_VALID_BITS
    }

    fn is_addr_in_width(addr: usize) -> bool {
        let valid_bits = Self::valid_bits();
        if valid_bits >= usize::BITS as usize {
            return true;
        }
        addr < (1usize << valid_bits)
    }

    /// 通过虚拟地址查询页表项
    ///
    /// # 参数
    /// - `vaddr`: 要查询的虚拟地址
    ///
    /// # 返回值
    /// - `Ok(T::P)`: 找到的页表项，包含物理地址信息
    /// - `Err(PagingError)`: 查询失败，原因可能包括：
    ///   - 地址未映射
    ///   - 页表项无效
    ///   - 页表层次结构错误
    pub fn translate(&self, vaddr: VirtAddr) -> PagingResult<(PhysAddr, T::P)> {
        self.translate_with_level(vaddr)
            .map(|(phys_addr, pte, _)| (phys_addr, pte))
    }

    /// Translates a virtual address and returns the matched PTE level.
    pub fn translate_with_level(&self, vaddr: VirtAddr) -> PagingResult<(PhysAddr, T::P, usize)> {
        if T::STRICT_ADDRESS_WIDTH && !Self::is_addr_in_width(vaddr.as_usize()) {
            return Err(PagingError::address_overflow("translate"));
        }

        let (pte, level) = self
            .root
            .translate_recursive_with_level(vaddr, Frame::<T, A>::PT_LEVEL)?;

        let is_huge = pte.huge(level > 1);
        let pte_paddr = pte.paddr(level > 1);

        // 根据页表项类型计算正确的偏移
        let (phys_addr, _) = if is_huge {
            // 大页映射：需要使用实际级别的大小来计算偏移
            let level_size = Frame::<T, A>::level_size(level);
            let offset_in_page = vaddr.as_usize() % level_size;
            (
                PhysAddr::from_usize(pte_paddr.as_usize() + offset_in_page),
                level_size,
            )
        } else {
            // 普通页面映射：使用页面大小
            let offset_in_page = vaddr.as_usize() % T::PAGE_SIZE;
            (
                PhysAddr::from_usize(pte_paddr.as_usize() + offset_in_page),
                T::PAGE_SIZE,
            )
        };

        Ok((phys_addr, pte, level))
    }

    /// 通过虚拟地址查询物理地址（便利方法）
    ///
    /// # 参数
    /// - `vaddr`: 要查询的虚拟地址
    ///
    /// # 返回值
    /// - `Ok(PhysAddr)`: 找到的物理地址
    /// - `Err(PagingError)`: 查询失败
    pub fn translate_phys(&self, vaddr: VirtAddr) -> PagingResult<PhysAddr> {
        let (p, _) = self.translate(vaddr)?;
        Ok(p)
    }

    /// 检查虚拟地址是否已映射
    ///
    /// 这是一个便利方法，用于快速检查地址是否已映射而不需要获取页表项
    ///
    /// # 参数
    /// - `vaddr`: 要检查的虚拟地址
    ///
    /// # 返回值
    /// - `true`: 地址已映射
    /// - `false`: 地址未映射
    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_ok()
    }

    /// 获取页表的根帧物理地址
    pub fn root_paddr(&self) -> crate::PhysAddr {
        self.root.paddr
    }
}

fn largest_page_size<T: TableMeta, A: FrameAllocator>(
    vaddr: VirtAddr,
    paddr: PhysAddr,
    remaining: usize,
    allow_huge: bool,
) -> usize {
    if allow_huge {
        let max_level = Frame::<T, A>::PT_LEVEL.min(T::MAX_BLOCK_LEVEL);
        for level in (2..=max_level).rev() {
            let page_size = Frame::<T, A>::level_size(level);
            if vaddr.is_aligned(page_size) && paddr.is_aligned(page_size) && remaining >= page_size
            {
                return page_size;
            }
        }
    }
    T::PAGE_SIZE
}

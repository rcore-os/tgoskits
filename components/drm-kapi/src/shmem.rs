// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! LinuxKPI file-backed anonymous page cache for the source-order DRM GEM
//! translations.  The target kernel has no swap or tmpfs mount layer, so this
//! module owns zero-filled, page-aligned RAM pages until they are truncated or
//! their last reference is dropped.  Writeback cannot evict them: with no swap
//! device it leaves them dirty and reports the Linux no-swap result instead of
//! pretending that data was written.

use alloc::{
    alloc::{alloc, alloc_zeroed, dealloc},
    collections::BTreeMap,
};
use core::{
    cell::UnsafeCell,
    ffi::{CStr, c_char, c_int, c_long, c_ulong, c_void},
    hint::spin_loop,
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    config::{GFP_KERNEL, PAGE_SHIFT, PAGE_SIZE},
    kernel_services::ProviderSlot,
    print::{self, CFormatArg, DrmLogLevel},
    types::Page,
};

const ENOMEM: c_int = 12;
const EFAULT: c_int = 14;
const EINVAL: c_int = 22;
const EFBIG: c_int = 27;
const FMODE_READ: u32 = 1;
const FMODE_WRITE: u32 = 2;
const ITER_SOURCE: u32 = 1;
const WB_SYNC_ALL: c_int = 1;
const AOP_WRITEPAGE_ACTIVATE: c_int = 0x80000;
const MAX_FILE_SIZE: u64 = i64::MAX as u64;
const PAGE_BATCH_SIZE: usize = 15;
const PAGE_MAGIC: u64 = 0x544b_5041_4745_7631;

/// Kernel-owned address conversions used by the LinuxKPI page cache.
///
/// These are Rust-only callbacks: the provider is installed by the hosting
/// kernel, and is not part of the LinuxKPI C ABI.
pub struct MemoryAddressOps {
    /// Translate a kernel virtual address to its device-visible physical
    /// address. Return `None` when the address is not translatable.
    pub virt_to_phys: fn(address: *const u8) -> Option<u64>,
    /// Return the kernel direct-map alias for a physical frame, or `None` when
    /// the frame is outside the direct map.
    pub phys_to_virt: fn(physical: u64) -> Option<*mut u8>,
    /// Number of usable RAM pages reported by the hosting kernel.
    pub totalram_pages: fn() -> usize,
}

static MEMORY_ADDRESS: ProviderSlot<MemoryAddressOps> = ProviderSlot::new();

/// Install the hosting kernel's virtual/physical address operations.
pub fn install_memory_address_ops(ops: &'static MemoryAddressOps) -> Result<(), &'static str> {
    MEMORY_ADDRESS.install(ops)
}

const PAGE_DIRTY: u32 = 1 << 0;
const PAGE_ACCESSED: u32 = 1 << 1;
const PAGE_UNEVICTABLE: u32 = 1 << 2;
const PAGE_MAPPED: u32 = 1 << 3;
const PAGE_UPTODATE: u32 = 1 << 4;
const PAGE_LOCKED: u32 = 1 << 6;

const PAGE_LAYOUT: core::alloc::Layout =
    match core::alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE) {
        Ok(layout) => layout,
        Err(_) => panic!("invalid page allocation layout"),
    };

struct SpinLock<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `SpinLock` owns the inner value `T` exclusively; transferring ownership
// to another thread is safe as long as `T` can be sent across threads.
unsafe impl<T: Send> Send for SpinLock<T> {}
// SAFETY: Mutual exclusion is guaranteed by the `held` atomic flag with Acquire/Release
// ordering, granting exclusive `&mut T` access to at most one guard at any time.
// Thus sharing `&SpinLock<T>` across threads requires only `T: Send`.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn lock(&self) -> SpinLockGuard<'_, T> {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        SpinLockGuard { lock: self }
    }
}

struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> core::ops::Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The guard acquired the lock with Acquire ordering in `SpinLock::lock`
        // and holds it until dropped, ensuring exclusive read access to `value`.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> core::ops::DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The guard acquired the lock with Acquire ordering in `SpinLock::lock`
        // and holds it until dropped, ensuring exclusive mutable access to `value`.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.held.store(false, Ordering::Release);
    }
}

/// Ordinary page: owns its own `PAGE_LAYOUT` allocation.
const PAGE_KIND_OWNED: u8 = 0;
/// Head subpage of a contiguous `alloc_pages(order > 0)` block; owns `block`.
const PAGE_KIND_COMPOUND_HEAD: u8 = 1;
/// Non-head subpage of a compound block; memory belongs to the head.
const PAGE_KIND_COMPOUND_TAIL: u8 = 2;
/// Pinned frame of another owner (user memory); never freed here.
const PAGE_KIND_FOREIGN: u8 = 3;
/// Largest order accepted by `alloc_pages()` (Linux MAX_PAGE_ORDER on x86_64).
const MAX_PAGE_ORDER: u32 = 10;
/// Linux `___GFP_DIRECT_RECLAIM_BIT` in the reference configuration.
const GFP_DIRECT_RECLAIM: u32 = 1 << 10;

#[repr(C)]
struct PageRecord {
    magic: u64,
    address: *mut u8,
    physical: usize,
    refs: AtomicUsize,
    flags: AtomicU32,
    index: AtomicU64,
    mapping: AtomicPtr<AddressSpace>,
    next: *mut PageRecord,
    kind: u8,
    order: u32,
    block: *mut u8,
}

// SAFETY: `PageRecord` instances are synchronized via atomic fields (`refs`, `flags`,
// `index`, `mapping`) and protected by the `PAGE_REGISTRY` spinlock during list
// mutation. Raw pointers (`address`, `block`) refer to heap-allocated or pinned
// page frames that are thread-safe to transfer across threads.
unsafe impl Send for PageRecord {}

#[derive(Default)]
struct PageRegistry {
    first: *mut PageRecord,
    count: usize,
}

// SAFETY: `PageRegistry` is exclusively accessed while holding the global `PAGE_REGISTRY`
// spinlock, ensuring no data races when moving registry pointers across threads.
unsafe impl Send for PageRegistry {}

static PAGE_REGISTRY: SpinLock<PageRegistry> = SpinLock::new(PageRegistry {
    first: ptr::null_mut(),
    count: 0,
});

/// Linux `struct folio` uses `struct page` as its first member.  This target
/// keeps both as opaque pointer identities; `page_folio()` is therefore an
/// address-preserving conversion, while storage and accounting live in the
/// registered `PageRecord`.
#[repr(C)]
pub struct Folio {
    _opaque: [u8; 0],
}

/// The in-memory address-space state used by the anonymous shmem cache.
#[repr(C)]
pub struct AddressSpace {
    pages: SpinLock<BTreeMap<u64, usize>>,
    gfp_mask: AtomicU32,
    unevictable: AtomicBool,
    host: AtomicPtr<Inode>,
    writeback_index: AtomicU64,
}

impl AddressSpace {
    fn new() -> Self {
        Self {
            pages: SpinLock::new(BTreeMap::new()),
            gfp_mask: AtomicU32::new(GFP_KERNEL),
            unevictable: AtomicBool::new(false),
            host: AtomicPtr::new(ptr::null_mut()),
            writeback_index: AtomicU64::new(0),
        }
    }
}

/// Minimal inode owner for a LinuxKPI anonymous shmem file.
#[repr(C)]
pub struct Inode {
    pub mapping: *mut AddressSpace,
    pub size: AtomicU64,
    pub name: [u8; 32],
}

// SAFETY: `Inode` fields are either atomic (`size`) or point to an internally
// synchronized `AddressSpace`. It is safe to transfer ownership across threads.
unsafe impl Send for Inode {}

/// Linux `struct file` prefix consumed by the DRM GEM transcription.
#[repr(C)]
pub struct File {
    pub _f_lock: [u8; 4],
    pub f_mode: u32,
    pub f_op: *const FileOperations,
    pub f_mapping: *mut AddressSpace,
    pub _private_data: *mut c_void,
    pub f_inode: *mut Inode,
    pub f_flags: u32,
    pub f_iocb_flags: u32,
    refs: AtomicUsize,
    name: [u8; 32],
}

// SAFETY: `File` references internally synchronized structures (`AddressSpace`, `Inode`)
// and uses an atomic reference counter (`refs`). It is safe to transfer across threads.
unsafe impl Send for File {}

/// Linux `struct file_operations` prefix through `release` (v7.2.3 field
/// order, x86_64 offsets). Unused fields remain opaque pointer-sized slots so
/// that `release` sits at its Linux offset (120), matching the DRM GEM tables.
#[repr(C)]
pub struct FileOperations {
    pub owner: *mut c_void,
    pub fop_flags: u32,
    pub _pad: u32,
    pub _llseek: *const c_void,
    pub _read: *const c_void,
    pub _write: *const c_void,
    pub _read_iter: *const c_void,
    pub write_iter: Option<unsafe extern "C" fn(*mut Kiocb, *mut IovIter) -> isize>,
    pub _iopoll: *const c_void,
    pub _iterate_shared: *const c_void,
    /// Linux `->poll(file, poll_table)` dispatch.
    pub poll: Option<unsafe extern "C" fn(*mut File, *mut crate::wait::PollTable) -> u32>,
    /// Linux `->unlocked_ioctl(file, cmd, arg)` dispatch.
    pub unlocked_ioctl: Option<unsafe extern "C" fn(*mut File, u32, usize) -> c_long>,
    pub _compat_ioctl: *const c_void,
    pub _mmap: *const c_void,
    pub _open: *const c_void,
    pub _flush: *const c_void,
    /// Linux `->release(inode, file)`, run by the final `fput()`.
    pub release: Option<unsafe extern "C" fn(*mut Inode, *mut File) -> c_int>,
}

const _: [(); 120] = [(); offset_of!(FileOperations, release)];
const _: [(); 72] = [(); offset_of!(FileOperations, poll)];
const _: [(); 80] = [(); offset_of!(FileOperations, unlocked_ioctl)];
const _: [(); 128] = [(); size_of::<FileOperations>()];

impl FileOperations {
    /// A file table with every operation absent. Tables that define one
    /// operation spread this and set the field they need.
    pub const fn empty() -> Self {
        Self {
            owner: core::ptr::null_mut(),
            fop_flags: 0,
            _pad: 0,
            _llseek: core::ptr::null(),
            _read: core::ptr::null(),
            _write: core::ptr::null(),
            _read_iter: core::ptr::null(),
            write_iter: None,
            _iopoll: core::ptr::null(),
            _iterate_shared: core::ptr::null(),
            poll: None,
            unlocked_ioctl: None,
            _compat_ioctl: core::ptr::null(),
            _mmap: core::ptr::null(),
            _open: core::ptr::null(),
            _flush: core::ptr::null(),
            release: None,
        }
    }
}

// SAFETY: `FileOperations` is an immutable table of C ABI function pointers with
// no internal mutability, safe to share concurrently across threads.
unsafe impl Sync for FileOperations {}

/// Source-compatible writeback batch prefix.
#[repr(C)]
pub struct FolioBatch {
    pub nr: u8,
    pub i: u8,
    pub percpu_pvec_drained: bool,
    pub _pad: [u8; 5],
    pub folios: [*mut Folio; PAGE_BATCH_SIZE],
}

/// Fields of Linux `struct writeback_control` used by this DRM path.
#[repr(C)]
pub struct WritebackControl {
    pub nr_to_write: c_long,
    pub pages_skipped: c_long,
    pub range_start: u64,
    pub range_end: u64,
    pub sync_mode: c_int,
    pub _flags: u32,
    pub fbatch: FolioBatch,
    pub index: u64,
    pub saved_err: c_int,
    pub _cgroup_writeback: [u8; 64],
}

/// The `kiocb` prefix initialized by the synchronous DRM pwrite path.
#[repr(C)]
pub struct Kiocb {
    pub ki_filp: *mut File,
    pub ki_pos: i64,
    pub ki_complete: Option<unsafe extern "C" fn(*mut Kiocb, c_long)>,
    pub private: *mut c_void,
    pub ki_flags: c_int,
    pub ki_ioprio: u16,
    pub ki_write_stream: u8,
    pub _pad: u8,
    pub ki_waitq: *mut c_void,
}

/// Linux `iov_iter` storage used for an ubuf iterator.  The shape matches the
/// existing source prefix; the first four words carry the LinuxKPI state.
#[repr(C)]
pub struct IovIter {
    pub _opaque: [usize; 8],
}

/// Opaque vfsmount pointer accepted by shmem_file_setup_with_mnt().
#[repr(C)]
pub struct VfsMount {
    _opaque: [u8; 0],
}

const _: [(); 8] = [(); offset_of!(File, f_op)];
const _: [(); 16] = [(); offset_of!(File, f_mapping)];
const _: [(); 40] = [(); offset_of!(File, f_flags)];
const _: [(); 48] = [(); offset_of!(FileOperations, write_iter)];
const _: [(); 128] = [(); size_of::<FolioBatch>()];

static FILE_OPERATIONS: FileOperations = FileOperations {
    write_iter: Some(shmem_write_iter),
    ..FileOperations::empty()
};

/// Allocate and initialize a single heap object of type `T`.
///
/// # Safety
/// The caller assumes ownership of the raw allocation. The returned pointer,
/// if non-null, must eventually be deallocated with [`free_object`] using the
/// same type `T`.
unsafe fn alloc_object<T>(value: T) -> *mut T {
    let layout = core::alloc::Layout::new::<T>();
    // SAFETY: `layout` is non-zero-sized if `T` is not a ZST, and properly describes `T`'s size and alignment.
    let object = unsafe { alloc(layout) }.cast::<T>();
    if object.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `object` is verified non-null from `alloc`, properly aligned and writable for `T`.
    unsafe { object.write(value) };
    object
}

/// Drop and deallocate an object previously allocated with [`alloc_object`].
///
/// # Safety
/// If non-null, `object` must point to a live, valid, unaliased instance of `T`
/// allocated via [`alloc_object::<T>`].
unsafe fn free_object<T>(object: *mut T) {
    if object.is_null() {
        return;
    }
    // SAFETY: `object` is verified non-null and was allocated with `Layout::new::<T>()`.
    // Dropping in place runs destructors before deallocating the memory.
    unsafe {
        ptr::drop_in_place(object);
        dealloc(object.cast(), core::alloc::Layout::new::<T>());
    }
}

fn phys_addr(address: *mut u8) -> Option<usize> {
    if let Some(ops) = MEMORY_ADDRESS.get() {
        return (ops.virt_to_phys)(address).and_then(|physical| usize::try_from(physical).ok());
    }

    #[cfg(all(test, not(target_os = "none")))]
    {
        // Host-only unit models have no target page tables. The pointer is
        // used only as an identity within that model, never as hardware PFN
        // evidence.
        Some(address as usize)
    }
    #[cfg(not(all(test, not(target_os = "none"))))]
    {
        None
    }
}

/// Allocate a zeroed page and register it in the global page registry.
///
/// # Safety
/// If non-null, `mapping` must point to a valid, live `AddressSpace` that outlives
/// this page allocation. The returned `*mut Page` owns one reference and must eventually
/// be released with [`put_page`].
unsafe fn alloc_page(mapping: *mut AddressSpace, index: u64) -> *mut Page {
    // SAFETY: `PAGE_LAYOUT` specifies a valid page-sized and page-aligned allocation layout.
    let address = unsafe { alloc_zeroed(PAGE_LAYOUT) };
    if address.is_null() {
        return ptr::null_mut();
    }

    let Some(physical) = phys_addr(address) else {
        // SAFETY: `address` was allocated above with `PAGE_LAYOUT` and has not yet been used.
        unsafe { dealloc(address, PAGE_LAYOUT) };
        return ptr::null_mut();
    };
    // SAFETY: Allocating a new `PageRecord` object with owned heap backing.
    let record = unsafe {
        alloc_object(PageRecord {
            magic: PAGE_MAGIC,
            address,
            physical,
            refs: AtomicUsize::new(1), // address_space cache reference
            flags: AtomicU32::new(PAGE_UPTODATE),
            index: AtomicU64::new(index),
            mapping: AtomicPtr::new(mapping),
            next: ptr::null_mut(),
            kind: PAGE_KIND_OWNED,
            order: 0,
            block: ptr::null_mut(),
        })
    };
    if record.is_null() {
        // SAFETY: `address` was allocated above with `PAGE_LAYOUT` and must be deallocated on failure.
        unsafe { dealloc(address, PAGE_LAYOUT) };
        return ptr::null_mut();
    }

    {
        let mut registry = PAGE_REGISTRY.lock();
        let mut current = registry.first;
        while !current.is_null() {
            // SAFETY: `current` is non-null and points to a registered `PageRecord` while holding `PAGE_REGISTRY` lock.
            if unsafe { (*current).physical >> PAGE_SHIFT } == physical >> PAGE_SHIFT {
                drop(registry);
                // SAFETY: On duplicate detection, free both the newly allocated page frame and the record.
                unsafe {
                    dealloc(address, PAGE_LAYOUT);
                    free_object(record);
                }
                return ptr::null_mut();
            }
            // SAFETY: `current` is non-null and points to a valid `PageRecord` in the intrusive list.
            current = unsafe { (*current).next };
        }
        // SAFETY: `record` is a valid, freshly allocated `PageRecord` being linked into `PAGE_REGISTRY`.
        unsafe { (*record).next = registry.first };
        registry.first = record;
        registry.count += 1;
    }
    record.cast::<Page>()
}

fn find_record(registry: &PageRegistry, page: *const Page) -> *mut PageRecord {
    if page.is_null() {
        return ptr::null_mut();
    }
    let mut current = registry.first;
    while !current.is_null() {
        if current.cast::<Page>() == page.cast_mut() {
            return current;
        }
        // SAFETY: `current` is non-null and points to a valid `PageRecord` in the list guarded by `registry`.
        current = unsafe { (*current).next };
    }
    ptr::null_mut()
}

/// Try to resolve a page identity registered by the LinuxKPI shmem allocator.
///
/// # Safety
/// `page` must be null or a live page pointer from this module; its owning
/// reference must remain held for the duration of the lookup.
pub unsafe fn try_page_to_phys(page: *const Page) -> Option<usize> {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, page);
    if record.is_null() {
        None
    } else {
        // SAFETY: `record` was found in `PAGE_REGISTRY` for the valid `page`, so it is non-null and valid.
        Some(unsafe { (*record).physical })
    }
}

/// Bridge called by `linux::page::page_to_pfn` and the scatterlist iterator.
///
/// # Safety
/// `page` must be a live page pointer registered by this module, with an
/// owning reference held for the duration of the lookup.
pub unsafe fn page_to_phys(page: *const Page) -> usize {
    // SAFETY: Forwarding `page` to `try_page_to_phys` under the caller's guarantee of a live registered Page.
    unsafe { try_page_to_phys(page) }.expect("page_to_phys received an unregistered Page")
}

/// Bridge called by the source-derived PFN-to-page path.
pub fn pfn_to_page(pfn: c_ulong) -> *mut Page {
    let registry = PAGE_REGISTRY.lock();
    let mut current = registry.first;
    while !current.is_null() {
        // SAFETY: `current` is non-null and points to a registered `PageRecord` while holding `PAGE_REGISTRY` lock.
        if unsafe { ((*current).physical >> PAGE_SHIFT) as c_ulong } == pfn {
            return current.cast::<Page>();
        }
        // SAFETY: `current` is non-null and points to a valid `PageRecord` in the list.
        current = unsafe { (*current).next };
    }
    ptr::null_mut()
}

/// Bridge called by `linux::highmem::kmap_local_page`.
///
/// # Safety
/// `page` must be a live page pointer registered by this module, with an
/// owning reference held for the duration of the lookup.
pub unsafe fn page_address(page: *mut Page) -> *mut c_void {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, page);
    assert!(
        !record.is_null(),
        "page_address received an unregistered Page"
    );
    // SAFETY: `record` was verified non-null by the assertion above and points to a registered `PageRecord`.
    unsafe { (*record).address.cast() }
}

/// Increment the Linux page reference count.
///
/// # Safety
/// `page` must be a live page pointer registered by this module, and the
/// caller must already hold a reference that prevents its final release.
pub unsafe fn get_page(page: *mut Page) {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, page);
    assert!(!record.is_null(), "get_page received an unregistered Page");
    // SAFETY: `record` was verified non-null by the assertion above and points to a registered `PageRecord`; atomic refcount increment is safe.
    let old = unsafe { (*record).refs.fetch_add(1, Ordering::Relaxed) };
    assert!(
        old != 0 && old != usize::MAX,
        "invalid page reference count"
    );
}

/// Increment a folio reference; folio and page have the same identity here.
///
/// # Safety
/// `folio` must be a live folio pointer registered by this module, and the
/// caller must already hold a reference that prevents its final release.
pub unsafe fn folio_get(folio: *mut Folio) {
    // SAFETY: `folio` shares identity with `Page` here; forwarding caller's held reference.
    unsafe { get_page(folio.cast()) };
}

/// Decrement a Linux page reference and free backing storage on the last put.
///
/// # Safety
/// `page` must be null or a registered base-page pointer for which the caller
/// owns one reference. Do not call this for a compound-page member; release a
/// compound allocation with `free_pages` instead.
pub unsafe fn put_page(page: *mut Page) {
    if page.is_null() {
        return;
    }
    let mut free = false;
    let record = {
        let mut registry = PAGE_REGISTRY.lock();
        let mut link = &mut registry.first as *mut *mut PageRecord;
        // SAFETY: `link` points to `&mut registry.first`, a valid pointer while `PAGE_REGISTRY` is locked.
        let mut current = unsafe { *link };
        while !current.is_null() && current.cast::<Page>() != page {
            // SAFETY: `current` is non-null, so `(*current).next` is within the valid `PageRecord` allocation.
            link = unsafe { ptr::addr_of_mut!((*current).next) };
            // SAFETY: `link` points to `(*current).next`, valid to read.
            current = unsafe { *link };
        }
        assert!(!current.is_null(), "put_page received an unregistered Page");
        let record = current;
        // SAFETY: `record` is non-null, pointing to the registered `PageRecord` found in the list.
        let kind = unsafe { (*record).kind };
        assert!(
            kind == PAGE_KIND_OWNED || kind == PAGE_KIND_FOREIGN,
            "compound page subpages are released only by __free_pages"
        );
        // SAFETY: `record` is non-null and its atomic refcount is decremented.
        let old = unsafe { (*record).refs.fetch_sub(1, Ordering::AcqRel) };
        assert!(old != 0, "page reference underflow");
        if old == 1 {
            // SAFETY: Unlinking `record` from the registry list while holding `PAGE_REGISTRY` lock.
            unsafe { *link = (*record).next };
            registry.count -= 1;
            free = true;
        }
        record
    };
    if free {
        // SAFETY: `record` was unlinked above and the last reference was dropped.
        let kind = unsafe { (*record).kind };
        // SAFETY: Zeroing magic, deallocating owned page frame if `PAGE_KIND_OWNED`, and freeing the record object.
        unsafe {
            (*record).magic = 0;
            // A foreign record only describes frames owned by another subsystem.
            if kind == PAGE_KIND_OWNED {
                dealloc((*record).address, PAGE_LAYOUT);
            }
            free_object(record);
        }
    }
}

/// Linux `alloc_pages(gfp, order)`: returns `2^order` physically contiguous,
/// zeroed pages. Non-reclaiming allocations fail closed, as the global
/// allocator cannot honour them. Returns NULL when the block is not
/// physically contiguous, so the caller can fall back to a smaller order.
///
/// # Safety
/// Any non-null result is an owned allocation. Keep each returned page pointer
/// within the allocation's lifetime, and release the allocation exactly once
/// with [`free_pages`] using the same `order`; individual members of an
/// order-greater-than-zero allocation must not be released separately.
pub unsafe fn alloc_pages(gfp: u32, order: u32) -> *mut Page {
    if gfp & GFP_DIRECT_RECLAIM == 0 || order > MAX_PAGE_ORDER {
        return ptr::null_mut();
    }
    if order == 0 {
        // SAFETY: `mapping` is null for raw page allocations; order 0 allocates a single page.
        return unsafe { alloc_page(ptr::null_mut(), 0) };
    }
    let count = 1usize << order;
    let bytes = PAGE_SIZE << order;
    let Ok(layout) = core::alloc::Layout::from_size_align(bytes, PAGE_SIZE) else {
        return ptr::null_mut();
    };
    // SAFETY: `layout` has valid size (`bytes`) and alignment (`PAGE_SIZE`).
    let block = unsafe { alloc_zeroed(layout) };
    if block.is_null() {
        return ptr::null_mut();
    }
    let Some(base_physical) = phys_addr(block) else {
        // SAFETY: `block` was allocated above with `layout` and must be freed on failure.
        unsafe { dealloc(block, layout) };
        return ptr::null_mut();
    };
    let contiguous = (0..count).all(|index| {
        // SAFETY: `index < count`, so `index * PAGE_SIZE < bytes`, within the allocated `block`.
        phys_addr(unsafe { block.add(index * PAGE_SIZE) }).is_some_and(|physical| {
            base_physical
                .checked_add(index * PAGE_SIZE)
                .is_some_and(|expected| physical == expected)
        })
    });
    if !contiguous {
        // SAFETY: `block` was allocated above with `layout` and must be freed on non-contiguous frames.
        unsafe { dealloc(block, layout) };
        return ptr::null_mut();
    }

    let mut records = alloc::vec::Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: Allocating a `PageRecord` for each subpage within the contiguous allocation `block`.
        let record = unsafe {
            alloc_object(PageRecord {
                magic: PAGE_MAGIC,
                address: block.add(index * PAGE_SIZE),
                physical: base_physical + index * PAGE_SIZE,
                refs: AtomicUsize::new(1),
                flags: AtomicU32::new(PAGE_UPTODATE),
                index: AtomicU64::new(index as u64),
                mapping: AtomicPtr::new(ptr::null_mut()),
                next: ptr::null_mut(),
                kind: if index == 0 {
                    PAGE_KIND_COMPOUND_HEAD
                } else {
                    PAGE_KIND_COMPOUND_TAIL
                },
                order: if index == 0 { order } else { 0 },
                block: if index == 0 { block } else { ptr::null_mut() },
            })
        };
        if record.is_null() {
            for created in records {
                // SAFETY: Clean up previously created `PageRecord`s on allocation failure.
                unsafe { free_object(created) };
            }
            // SAFETY: Clean up the backing memory block on allocation failure.
            unsafe { dealloc(block, layout) };
            return ptr::null_mut();
        }
        records.push(record);
    }

    let mut registry = PAGE_REGISTRY.lock();
    let duplicate = records.iter().any(|record| {
        // SAFETY: `*record` is valid and points to a freshly allocated `PageRecord`.
        let physical = unsafe { (**record).physical };
        let mut current = registry.first;
        while !current.is_null() {
            // SAFETY: `current` is non-null and points to a registered `PageRecord` while holding `PAGE_REGISTRY` lock.
            if unsafe { (*current).physical } == physical {
                return true;
            }
            // SAFETY: `current` is non-null and points to a valid `PageRecord`.
            current = unsafe { (*current).next };
        }
        false
    });
    if duplicate {
        drop(registry);
        for record in records {
            // SAFETY: Clean up allocated `PageRecord` objects on duplicate detection.
            unsafe { free_object(record) };
        }
        // SAFETY: Clean up the backing memory block on duplicate detection.
        unsafe { dealloc(block, layout) };
        return ptr::null_mut();
    }
    for record in &records {
        // SAFETY: Linking the freshly allocated `record` into `PAGE_REGISTRY` while holding the lock.
        unsafe { (**record).next = registry.first };
        registry.first = *record;
        registry.count += 1;
    }
    records[0].cast::<Page>()
}

/// Linux `__free_pages(page, order)`. The page must be the head returned by
/// [`alloc_pages`] with no outstanding references besides the allocation's
/// own; the whole block is released together.
///
/// # Safety
/// `page` must be a live page from [`alloc_pages`] with the same `order`.
pub unsafe fn free_pages(page: *mut Page, order: u32) {
    if page.is_null() {
        return;
    }
    if order == 0 {
        // SAFETY: Order-0 pages are single pages freed via `put_page`.
        unsafe { put_page(page) };
        return;
    }
    let mut registry = PAGE_REGISTRY.lock();
    let head = find_record(&registry, page);
    assert!(
        !head.is_null(),
        "__free_pages received an unregistered Page"
    );
    assert!(
        // SAFETY: `head` is non-null and was verified present in `PAGE_REGISTRY`.
        unsafe { (*head).kind == PAGE_KIND_COMPOUND_HEAD && (*head).order == order },
        "__free_pages order does not match the allocation"
    );
    assert!(
        // SAFETY: `head` is non-null; reading atomic ref count is safe.
        unsafe { (*head).refs.load(Ordering::Acquire) } == 1,
        "__free_pages on a referenced compound page"
    );
    // SAFETY: `head` is non-null; `block` was stored when `alloc_pages` created the compound head.
    let block = unsafe { (*head).block };
    // SAFETY: `head` is non-null; reading `physical` address of compound allocation.
    let head_physical = unsafe { (*head).physical };
    let count = 1usize << order;
    let mut released = alloc::vec::Vec::with_capacity(count);
    for index in 0..count {
        let physical = head_physical + index * PAGE_SIZE;
        let mut link = &mut registry.first as *mut *mut PageRecord;
        // SAFETY: `link` points to `&mut registry.first`, a valid pointer while `PAGE_REGISTRY` is locked.
        let mut current = unsafe { *link };
        // SAFETY: `current` is non-null, pointing to a live `PageRecord` in `PAGE_REGISTRY`.
        while !current.is_null() && unsafe { (*current).physical } != physical {
            // SAFETY: `current` is non-null and points to a valid `PageRecord`.
            link = unsafe { ptr::addr_of_mut!((*current).next) };
            // SAFETY: `link` points to `(*current).next`.
            current = unsafe { *link };
        }
        assert!(
            !current.is_null(),
            "compound page subpage missing from registry"
        );
        // SAFETY: Unlinking `current` from the registry list while holding `PAGE_REGISTRY` lock.
        unsafe { *link = (*current).next };
        registry.count -= 1;
        released.push(current);
    }
    drop(registry);
    let layout = core::alloc::Layout::from_size_align(PAGE_SIZE << order, PAGE_SIZE)
        .expect("compound page layout");
    // SAFETY: `block` was allocated with `layout` in `alloc_pages`. All subpage records
    // in `released` have been unlinked from `PAGE_REGISTRY` and their references dropped.
    unsafe {
        dealloc(block, layout);
        for record in released {
            (*record).magic = 0;
            free_object(record);
        }
    }
}

/// Return the registered page for a physical frame, taking one reference. A
/// frame that has no record yet becomes a `FOREIGN` record for the duration of
/// the pin; the frame itself stays owned by its original allocator. Compound
/// allocation members are not pinnable individually and return null because
/// their references are released only by `free_pages` on the head.
///
/// # Safety
/// `physical` must identify a live RAM frame that remains allocated and
/// accessible through the installed direct map for as long as the returned
/// page reference is held. A non-null result is one owned page reference and
/// must be released with [`put_page`]; this function does not acquire ownership
/// of the underlying frame.
pub unsafe fn pin_physical_frame(physical: usize) -> *mut Page {
    let frame = physical & !(PAGE_SIZE - 1);
    {
        let registry = PAGE_REGISTRY.lock();
        let mut current = registry.first;
        while !current.is_null() {
            // SAFETY: `current` is non-null and points to a registered `PageRecord` while holding `PAGE_REGISTRY` lock.
            if unsafe { (*current).physical } == frame {
                // SAFETY: `current` is non-null and points to a registered `PageRecord`.
                let kind = unsafe { (*current).kind };
                if kind != PAGE_KIND_OWNED && kind != PAGE_KIND_FOREIGN {
                    return ptr::null_mut();
                }
                // SAFETY: Atomic refcount increment on a live registered `PageRecord`.
                unsafe { (*current).refs.fetch_add(1, Ordering::AcqRel) };
                return current.cast::<Page>();
            }
            // SAFETY: `current` is non-null and points to a valid `PageRecord`.
            current = unsafe { (*current).next };
        }
    }
    let address = direct_map_address(frame);
    if address.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: Allocating a `PageRecord` of kind `PAGE_KIND_FOREIGN` for the pinned frame.
    let record = unsafe {
        alloc_object(PageRecord {
            magic: PAGE_MAGIC,
            address,
            physical: frame,
            refs: AtomicUsize::new(1),
            flags: AtomicU32::new(PAGE_UPTODATE),
            index: AtomicU64::new(0),
            mapping: AtomicPtr::new(ptr::null_mut()),
            next: ptr::null_mut(),
            kind: PAGE_KIND_FOREIGN,
            order: 0,
            block: ptr::null_mut(),
        })
    };
    if record.is_null() {
        return ptr::null_mut();
    }
    let mut registry = PAGE_REGISTRY.lock();
    let mut current = registry.first;
    while !current.is_null() {
        // SAFETY: `current` is non-null and points to a registered `PageRecord` while holding `PAGE_REGISTRY` lock.
        if unsafe { (*current).physical } == frame {
            // Lost a race with another pin of the same frame; use that record.
            // SAFETY: Atomic refcount increment on the existing live `PageRecord`.
            unsafe { (*current).refs.fetch_add(1, Ordering::AcqRel) };
            drop(registry);
            // SAFETY: Free the duplicate `record` allocated above.
            unsafe { free_object(record) };
            return current.cast::<Page>();
        }
        // SAFETY: `current` is non-null and points to a valid `PageRecord`.
        current = unsafe { (*current).next };
    }
    // SAFETY: Linking the new `record` into `PAGE_REGISTRY` while holding the lock.
    unsafe { (*record).next = registry.first };
    registry.first = record;
    registry.count += 1;
    record.cast::<Page>()
}

/// Kernel direct-map alias of a physical frame.
fn direct_map_address(physical: usize) -> *mut u8 {
    if let Some(ops) = MEMORY_ADDRESS.get() {
        return u64::try_from(physical)
            .ok()
            .and_then(|physical| (ops.phys_to_virt)(physical))
            .filter(|address| !address.is_null())
            .unwrap_or(ptr::null_mut());
    }

    #[cfg(all(test, not(target_os = "none")))]
    {
        physical as *mut u8
    }
    #[cfg(not(all(test, not(target_os = "none"))))]
    {
        ptr::null_mut()
    }
}

/// Decrement a folio reference; folio and page have the same identity here.
///
/// # Safety
/// `folio` must be a registered base-page folio for which the caller owns one
/// reference. Release exactly one reference, and do not use the folio after
/// its final reference is dropped.
pub unsafe fn folio_put(folio: *mut Folio) {
    // SAFETY: `folio` shares identity with `Page` here; forwarding caller's owned reference.
    unsafe { put_page(folio.cast()) };
}

/// Linux `page_folio()` for the registered base-page folios used here.
///
/// # Safety
/// `page` must be a live page pointer registered by this module, and its
/// owning reference must remain held while the returned folio alias is used.
pub unsafe fn page_folio(page: *mut Page) -> *mut Folio {
    if page.is_null() {
        return ptr::null_mut();
    }
    let registry = PAGE_REGISTRY.lock();
    assert!(
        !find_record(&registry, page).is_null(),
        "page_folio received an unregistered Page"
    );
    page.cast()
}

/// Linux page lock used by userptr's notifier-safe invalidation path.
///
/// # Safety
/// `page` must be a live registered page with a reference held throughout the
/// call. If this returns `true`, the caller owns the page lock and must release
/// it with [`unlock_page`].
pub unsafe fn trylock_page(page: *mut Page) -> bool {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, page);
    if record.is_null() {
        return false;
    }
    // SAFETY: `record` was found in `PAGE_REGISTRY` and is live; atomic bit-test-and-set on `flags`.
    unsafe { (*record).flags.fetch_or(PAGE_LOCKED, Ordering::Acquire) & PAGE_LOCKED == 0 }
}

/// Unlock a page locked by `trylock_page` or writeback iteration.
///
/// # Safety
/// `page` must be a live registered page with a reference held throughout the
/// call, and the caller must own its page lock. Unlock it exactly once for
/// each successful lock operation.
pub unsafe fn unlock_page(page: *mut Page) {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, page);
    assert!(
        !record.is_null(),
        "unlock_page received an unregistered Page"
    );
    // SAFETY: `record` was verified non-null and points to a registered `PageRecord`; atomic bit-clear.
    let old = unsafe { (*record).flags.fetch_and(!PAGE_LOCKED, Ordering::Release) };
    assert!(old & PAGE_LOCKED != 0, "unlock_page on an unlocked page");
}

/// Acquire the registered page's Linux page lock, spinning until it is held.
///
/// # Safety
/// `page` must be a live page reference from this module's page registry.
pub unsafe fn lock_page(page: *mut Page) {
    // SAFETY: Caller guarantees `page` is a live registered page reference; spinning on `trylock_page`.
    while !unsafe { trylock_page(page) } {
        spin_loop();
    }
}

/// Linux `folio_batch_init()`.
pub fn folio_batch_init(batch: &mut FolioBatch) {
    batch.nr = 0;
    batch.i = 0;
    batch.percpu_pvec_drained = false;
}

/// Linux `folio_batch_add()`; returns remaining slots.
pub fn folio_batch_add(batch: &mut FolioBatch, folio: *mut Folio) -> u32 {
    if folio.is_null() || batch.nr as usize >= PAGE_BATCH_SIZE {
        return 0;
    }
    batch.folios[batch.nr as usize] = folio;
    batch.nr += 1;
    (PAGE_BATCH_SIZE - batch.nr as usize) as u32
}

/// Release page references held by a batch.
///
/// # Safety
/// `batch` must be null or point to an exclusively accessible, initialized
/// [`FolioBatch`]. Every non-null folio among its `nr` entries must be a live
/// registered folio with one reference owned by the batch; this function may
/// unlock and release those references and clears the batch.
pub unsafe fn __folio_batch_release(batch: *mut FolioBatch) {
    if batch.is_null() {
        return;
    }
    // SAFETY: `batch` is non-null and exclusively accessible per caller safety contract.
    let nr = unsafe { (*batch).nr as usize }.min(PAGE_BATCH_SIZE);
    for index in 0..nr {
        // SAFETY: `index < nr <= PAGE_BATCH_SIZE`, within `folios` array bounds.
        let folio = unsafe { (*batch).folios[index] };
        if !folio.is_null() {
            let locked = {
                let registry = PAGE_REGISTRY.lock();
                let record = find_record(&registry, folio.cast());
                !record.is_null()
                    // SAFETY: `record` is verified non-null and points to a registered `PageRecord` protected by `PAGE_REGISTRY`.
                    && unsafe { (*record).flags.load(Ordering::Acquire) & PAGE_LOCKED != 0 }
            };
            if locked {
                // SAFETY: `folio` was locked by writeback iteration and owned by this batch.
                unsafe { unlock_page(folio.cast()) };
            }
            // SAFETY: Releasing the batch-owned reference on `folio`.
            unsafe { folio_put(folio) };
            // SAFETY: Clearing the batch entry while exclusively owning `batch`.
            unsafe { (*batch).folios[index] = ptr::null_mut() };
        }
    }
    // SAFETY: Resetting batch state fields while exclusively owning `batch`.
    unsafe {
        (*batch).nr = 0;
        (*batch).i = 0;
        (*batch).percpu_pvec_drained = true;
    }
}

/// The hosting kernel has no reclaim LRU; this pass records the mapping's unevictable
/// state on each page rather than pretending to move pages between lists.
///
/// # Safety
/// `batch` must be null or point to an initialized batch that is not being
/// mutated concurrently. Each non-null folio in the batch must be live and
/// registered by this module.
pub unsafe fn check_move_unevictable_folios(batch: *mut FolioBatch) {
    if batch.is_null() {
        return;
    }
    // SAFETY: `batch` is non-null and not mutated concurrently per caller contract.
    let nr = unsafe { (*batch).nr as usize }.min(PAGE_BATCH_SIZE);
    for index in 0..nr {
        // SAFETY: `index < nr <= PAGE_BATCH_SIZE`, within `folios` array bounds.
        let folio = unsafe { (*batch).folios[index] };
        let registry = PAGE_REGISTRY.lock();
        let record = find_record(&registry, folio.cast());
        if record.is_null() {
            continue;
        }
        // SAFETY: `record` is non-null and points to a registered `PageRecord`.
        let mapping = unsafe { (*record).mapping.load(Ordering::Acquire) };
        // SAFETY: If `mapping` is non-null, reading its atomic `unevictable` flag.
        if !mapping.is_null() && unsafe { (*mapping).unevictable.load(Ordering::Acquire) } {
            // SAFETY: `record` is non-null; updating flags atomically.
            unsafe { (*record).flags.fetch_or(PAGE_UNEVICTABLE, Ordering::AcqRel) };
        } else {
            // SAFETY: `record` is non-null; updating flags atomically.
            unsafe {
                (*record)
                    .flags
                    .fetch_and(!PAGE_UNEVICTABLE, Ordering::AcqRel)
            };
        }
    }
}

/// Linux `mapping_set_unevictable()`.
///
/// # Safety
/// `mapping` must be null or point to a live, initialized `AddressSpace` that
/// remains allocated and whose page-map lock and entries are valid throughout
/// the call.
pub unsafe fn mapping_set_unevictable(mapping: *mut AddressSpace) {
    if mapping.is_null() {
        return;
    }
    // SAFETY: `mapping` is verified non-null and valid per caller safety contract.
    unsafe { (*mapping).unevictable.store(true, Ordering::Release) };
    {
        // SAFETY: Accessing `mapping.pages` lock on valid `mapping`.
        let pages = unsafe { (*mapping).pages.lock() };
        for page in pages.values().copied() {
            let registry = PAGE_REGISTRY.lock();
            let record = find_record(&registry, page as *const Page);
            if !record.is_null() {
                // SAFETY: `record` is non-null and points to a registered `PageRecord`.
                unsafe { (*record).flags.fetch_or(PAGE_UNEVICTABLE, Ordering::AcqRel) };
            }
        }
    }
}

/// Linux `mapping_clear_unevictable()`.
///
/// # Safety
/// `mapping` must be null or point to a live, initialized `AddressSpace` that
/// remains allocated and whose page-map lock and entries are valid throughout
/// the call.
pub unsafe fn mapping_clear_unevictable(mapping: *mut AddressSpace) {
    if mapping.is_null() {
        return;
    }
    // SAFETY: `mapping` is verified non-null and valid per caller safety contract.
    unsafe { (*mapping).unevictable.store(false, Ordering::Release) };
    {
        // SAFETY: Accessing `mapping.pages` lock on valid `mapping`.
        let pages = unsafe { (*mapping).pages.lock() };
        for page in pages.values().copied() {
            let registry = PAGE_REGISTRY.lock();
            let record = find_record(&registry, page as *const Page);
            if !record.is_null() {
                // SAFETY: `record` is non-null and points to a registered `PageRecord`.
                unsafe {
                    (*record)
                        .flags
                        .fetch_and(!PAGE_UNEVICTABLE, Ordering::AcqRel)
                };
            }
        }
    }
}

/// Linux `mapping_gfp_mask()`.
///
/// # Safety
/// `mapping` must be null or point to a live, initialized `AddressSpace` for
/// the duration of the read.
pub unsafe fn mapping_gfp_mask(mapping: *const AddressSpace) -> u32 {
    if mapping.is_null() {
        return GFP_KERNEL;
    }
    // SAFETY: `mapping` is non-null and valid for the duration of the read per caller contract.
    unsafe { (*mapping).gfp_mask.load(Ordering::Acquire) }
}

/// Linux `mapping_gfp_constraint()`.
///
/// # Safety
/// `mapping` must be null or point to a live, initialized `AddressSpace` for
/// the duration of the read.
pub unsafe fn mapping_gfp_constraint(mapping: *const AddressSpace, mask: u32) -> u32 {
    // SAFETY: Forwarding `mapping` to `mapping_gfp_mask` under the caller's safety contract.
    unsafe { mapping_gfp_mask(mapping) & mask }
}

/// Linux `mapping_set_gfp_mask()`.
///
/// # Safety
/// `mapping` must be null or point to a live, initialized `AddressSpace` for
/// the duration of the update.
pub unsafe fn mapping_set_gfp_mask(mapping: *mut AddressSpace, mask: u32) {
    if !mapping.is_null() {
        // SAFETY: `mapping` is non-null and valid for update per caller contract.
        unsafe { (*mapping).gfp_mask.store(mask, Ordering::Release) };
    }
}

/// Linux `folio_mark_dirty()` for an anonymous page-cache folio.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the call.
pub unsafe fn folio_mark_dirty(folio: *mut Folio) -> bool {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, folio.cast());
    assert!(
        !record.is_null(),
        "folio_mark_dirty received an unregistered folio"
    );
    // SAFETY: `record` is verified non-null and points to a registered `PageRecord`.
    unsafe { (*record).flags.fetch_or(PAGE_DIRTY, Ordering::AcqRel) & PAGE_DIRTY == 0 }
}

/// Linux `folio_mark_accessed()`.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the call.
pub unsafe fn folio_mark_accessed(folio: *mut Folio) {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, folio.cast());
    assert!(
        !record.is_null(),
        "folio_mark_accessed received an unregistered folio"
    );
    // SAFETY: `record` is verified non-null and points to a registered `PageRecord`.
    unsafe { (*record).flags.fetch_or(PAGE_ACCESSED, Ordering::Relaxed) };
}

/// Linux highmem API: account an access to a page-cache page.
///
/// # Safety
/// `page` must be a live registered page with a reference held throughout the
/// call.
pub unsafe fn mark_page_accessed(page: *mut Page) {
    // SAFETY: Caller guarantees `page` is a live registered page.
    let folio = unsafe { page_folio(page) };
    if !folio.is_null() {
        // SAFETY: `folio` is non-null and registered by this module.
        unsafe { folio_mark_accessed(folio) };
    }
}

/// Linux `folio_nr_pages()`; this KPI currently allocates order-0 pages only.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the call.
pub unsafe fn folio_nr_pages(folio: *const Folio) -> usize {
    let registry = PAGE_REGISTRY.lock();
    assert!(
        !find_record(&registry, folio.cast()).is_null(),
        "folio_nr_pages received an unregistered folio"
    );
    1
}

/// Linux `folio_mapped()` for this mapping implementation.  There is no
/// userspace mmap path in this KPI yet, so only explicit map-count updates can
/// report a folio mapped.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the call.
pub unsafe fn folio_mapped(folio: *const Folio) -> bool {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, folio.cast());
    // SAFETY: If `record` is non-null, reading atomic `flags` of the registered `PageRecord`.
    !record.is_null() && unsafe { (*record).flags.load(Ordering::Acquire) & PAGE_MAPPED != 0 }
}

/// Mark a page as mapped/unmapped for future address-space integrations.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the update.
pub unsafe fn set_folio_mapped(folio: *mut Folio, mapped: bool) {
    let registry = PAGE_REGISTRY.lock();
    let record = find_record(&registry, folio.cast());
    assert!(
        !record.is_null(),
        "set_folio_mapped received an unregistered folio"
    );
    if mapped {
        // SAFETY: `record` is verified non-null; atomic bitwise-OR on `flags`.
        unsafe { (*record).flags.fetch_or(PAGE_MAPPED, Ordering::AcqRel) };
    } else {
        // SAFETY: `record` is verified non-null; atomic bitwise-AND on `flags`.
        unsafe { (*record).flags.fetch_and(!PAGE_MAPPED, Ordering::AcqRel) };
    }
}

fn err_ptr<T>(error: c_int) -> *mut T {
    crate::config::err_ptr(error)
}

fn copy_file_name(name: *const c_char) -> [u8; 32] {
    let mut result = [0; 32];
    if !name.is_null() {
        // SAFETY: `name` is non-null and all callers pass a valid, readable NUL-terminated kernel string.
        let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
        let count = bytes.len().min(result.len() - 1);
        result[..count].copy_from_slice(&bytes[..count]);
    }
    result
}

/// Free all cached pages and deallocate the `AddressSpace` object.
///
/// # Safety
/// If non-null, `mapping` must point to an exclusive, live `AddressSpace` allocated
/// with [`alloc_object`] that is no longer accessed concurrently by other threads.
unsafe fn cleanup_mapping(mapping: *mut AddressSpace) {
    if mapping.is_null() {
        return;
    }
    loop {
        let page = {
            // SAFETY: `mapping` is non-null and exclusively owned during cleanup.
            let mut pages = unsafe { (*mapping).pages.lock() };
            let Some((&index, &page)) = pages.first_key_value() else {
                break;
            };
            pages.remove(&index);
            page as *mut Page
        };
        {
            let registry = PAGE_REGISTRY.lock();
            let record = find_record(&registry, page);
            if !record.is_null() {
                // SAFETY: `record` is non-null; clearing mapping pointer as page is unlinked.
                unsafe { (*record).mapping.store(ptr::null_mut(), Ordering::Release) };
            }
        }
        // SAFETY: Dropping the address-space cache reference on `page`.
        unsafe { put_page(page) };
    }
    // SAFETY: Deallocating the empty `mapping` object previously allocated with `alloc_object`.
    unsafe { free_object(mapping) };
}

/// Allocate and initialize a `File` object and its backing `Inode` and `AddressSpace`.
///
/// # Safety
/// `name` must point to a readable NUL-terminated C string. A successful result owns
/// one file reference and must eventually be released with [`fput_file`].
unsafe fn setup_file(name: *const c_char, size: u64) -> *mut File {
    if name.is_null() || size > MAX_FILE_SIZE {
        return err_ptr(-EINVAL);
    }
    let name = copy_file_name(name);
    // SAFETY: Allocating a new `AddressSpace` on the heap.
    let mapping = unsafe { alloc_object(AddressSpace::new()) };
    if mapping.is_null() {
        return err_ptr(-ENOMEM);
    }
    // SAFETY: Allocating a new `Inode` on the heap associated with `mapping`.
    let inode = unsafe {
        alloc_object(Inode {
            mapping,
            size: AtomicU64::new(size),
            name,
        })
    };
    if inode.is_null() {
        // SAFETY: Clean up `mapping` allocated above if inode allocation fails.
        unsafe { cleanup_mapping(mapping) };
        return err_ptr(-ENOMEM);
    }
    // SAFETY: `mapping` is non-null; linking its host inode.
    unsafe { (*mapping).host.store(inode, Ordering::Release) };
    // SAFETY: Allocating a new `File` structure referencing `mapping` and `inode`.
    let file = unsafe {
        alloc_object(File {
            _f_lock: [0; 4],
            f_mode: FMODE_READ | FMODE_WRITE,
            f_op: &FILE_OPERATIONS,
            f_mapping: mapping,
            _private_data: ptr::null_mut(),
            f_inode: inode,
            f_flags: 0,
            f_iocb_flags: 0,
            refs: AtomicUsize::new(1),
            name,
        })
    };
    if file.is_null() {
        // SAFETY: Clean up `inode` and `mapping` allocated above if file allocation fails.
        unsafe {
            free_object(inode);
            cleanup_mapping(mapping);
        }
        return err_ptr(-ENOMEM);
    }
    file
}

/// Create a zero-filled, file-backed anonymous LinuxKPI shmem object.
///
/// # Safety
/// `name` must point to a readable NUL-terminated string for the duration of
/// the call. A successful result owns one file reference and must eventually
/// be released with [`fput_file`]; an `err_ptr` result owns no reference.
pub unsafe fn shmem_file_setup(name: *const c_char, size: u64, _flags: c_ulong) -> *mut File {
    // SAFETY: Forwarding `name` and `size` to `setup_file` under caller's contract.
    unsafe { setup_file(name, size) }
}

/// The target has no tmpfs mount layer; a caller-supplied mount does not alter
/// the semantics of this anonymous page-cache owner.
///
/// # Safety
/// `name` must point to a readable NUL-terminated string for the duration of
/// the call. A successful result owns one file reference and must eventually
/// be released with [`fput_file`]; an `err_ptr` result owns no reference.
pub unsafe fn shmem_file_setup_with_mnt(
    _mnt: *mut VfsMount,
    name: *const c_char,
    size: u64,
    flags: c_ulong,
) -> *mut File {
    // SAFETY: Forwarding arguments to `shmem_file_setup` under caller's contract.
    unsafe { shmem_file_setup(name, size, flags) }
}

/// Acquire a reference to a LinuxKPI shmem file for a VMA or other owner.
///
/// # Safety
/// `file` must be null, an `err_ptr`, or a live LinuxKPI shmem file for which
/// the caller already owns a reference. A successful file pointer result owns
/// one additional reference that must later be released with [`fput_file`].
pub unsafe fn get_file_file(file: *mut File) -> *mut File {
    if file.is_null() || crate::config::is_err(file) {
        return file;
    }
    // SAFETY: Caller guarantees `file` is live and holds a reference; incrementing atomic refcount.
    let old = unsafe { (*file).refs.fetch_add(1, Ordering::Relaxed) };
    assert!(
        old != 0 && old != usize::MAX,
        "invalid file reference count"
    );
    file
}

/// Drop a shmem file reference and release its cache on the final put.
///
/// # Safety
/// `file` must be null, an `err_ptr`, or a live LinuxKPI shmem file for which
/// the caller owns exactly one reference. Its release callback, if any, must
/// be valid to invoke during the final put and must not free the file or its
/// mapping independently.
pub unsafe fn fput_file(file: *mut File) {
    if file.is_null() || crate::config::is_err(file) {
        return;
    }
    // SAFETY: `file` is non-null and not err_ptr; decrementing atomic refcount with AcqRel.
    let old = unsafe { (*file).refs.fetch_sub(1, Ordering::AcqRel) };
    assert!(old != 0, "file reference underflow");
    if old != 1 {
        return;
    }
    // SAFETY: The last reference was dropped; retrieving `f_mapping` pointer before releasing memory.
    let mapping = unsafe { (*file).f_mapping };
    // SAFETY: Retrieving `f_inode` pointer from `file` before releasing memory.
    let inode = unsafe { (*file).f_inode };
    // Linux `__fput()`: the last reference runs `f_op->release` before the
    // file is freed.
    // SAFETY: Reading `f_op` table pointer from `file`.
    let f_op = unsafe { (*file).f_op };
    // SAFETY: Dereferencing `f_op` to inspect optional `release` callback.
    if let Some(release) = unsafe { f_op.as_ref() }.and_then(|ops| ops.release) {
        // The result of `->release` is ignored by `__fput()` as well.
        // SAFETY: Invoking `f_op->release(inode, file)` per Linux `__fput` contract.
        unsafe { release(inode, file) };
    }
    // SAFETY: Deallocating mapping cache, inode, and file structure after last reference dropped.
    unsafe {
        cleanup_mapping(mapping);
        free_object(inode);
        free_object(file);
    }
}

/// Take one more reference to `file` (Linux `get_file()`).
///
/// # Safety
/// `file` must be a live file.
pub unsafe fn fget_file(file: *mut File) -> *mut File {
    // SAFETY: Caller guarantees `file` is live; incrementing atomic refcount.
    unsafe { (*file).refs.fetch_add(1, Ordering::AcqRel) };
    file
}

/// Linux-compatible `fput(void *)` wrapper for callers that keep the original
/// opaque file-pointer type.
///
/// # Safety
/// `file` must be null, an `err_ptr`, or a LinuxKPI shmem file pointer for
/// which the caller owns exactly one reference.
pub unsafe fn fput(file: *mut c_void) {
    // SAFETY: Forwarding `file` pointer to `fput_file` under caller's contract.
    unsafe { fput_file(file.cast()) };
}

/// Return the inode size for a LinuxKPI shmem file through its page-cache
/// mapping. This is the Rust-side equivalent of `file->f_mapping->host->i_size`.
///
/// # Safety
/// `file` must be a live LinuxKPI shmem file with a valid mapping and host
/// inode, and the caller must keep its file reference held during the read.
pub unsafe fn file_size(file: *mut File) -> u64 {
    assert!(!file.is_null());
    // SAFETY: `file` is non-null and verified live by caller.
    let mapping = unsafe { (*file).f_mapping };
    assert!(!mapping.is_null());
    // SAFETY: `mapping` is non-null; reading atomic `host` inode pointer.
    let inode = unsafe { (*mapping).host.load(Ordering::Acquire) };
    assert!(!inode.is_null());
    // SAFETY: `inode` is non-null; reading atomic `size`.
    unsafe { (*inode).size.load(Ordering::Acquire) }
}

/// Return the page-cache folio at @index, creating a zeroed page on a miss.
///
/// # Safety
/// `mapping` must be null or a live, initialized address space whose `host`
/// points to a live inode; it and the inode must remain valid throughout the
/// call. A non-error result owns one folio reference that the caller must
/// release with [`folio_put`].
pub unsafe fn shmem_read_folio_gfp(
    mapping: *mut AddressSpace,
    index: c_ulong,
    _gfp: u32,
) -> *mut Folio {
    if mapping.is_null() {
        return err_ptr(-EINVAL);
    }
    // SAFETY: `mapping` is non-null; reading atomic `host` inode pointer.
    let inode = unsafe { (*mapping).host.load(Ordering::Acquire) };
    if inode.is_null() {
        return err_ptr(-EINVAL);
    }
    // SAFETY: `inode` is non-null; reading atomic `size`.
    let size = unsafe { (*inode).size.load(Ordering::Acquire) };
    let Some(page_start) = index.checked_mul(PAGE_SIZE as u64) else {
        return err_ptr(-EFBIG);
    };
    if page_start >= size {
        return err_ptr(-EINVAL);
    }

    let page = {
        // SAFETY: Locking `mapping.pages` to look up or insert the cached page.
        let mut pages = unsafe { (*mapping).pages.lock() };
        let page = if let Some(page) = pages.get(&index).copied() {
            page as *mut Page
        } else {
            // SAFETY: Allocating a new page for `mapping` at `index` on page cache miss.
            let page = unsafe { alloc_page(mapping, index) };
            if page.is_null() {
                return err_ptr(-ENOMEM);
            }
            pages.insert(index, page as usize);
            page
        };
        // Take the caller's reference before releasing the mapping lock so a
        // concurrent truncate cannot remove the cache reference first.
        // SAFETY: `page` is a live registered page from cache or allocation; taking caller reference.
        unsafe { get_page(page) };
        page
    };
    // SAFETY: Locking the page before returning it to ensure initial state visibility.
    unsafe { lock_page(page) };
    // SAFETY: Unlocking the page after state is established.
    unsafe { unlock_page(page) };
    page.cast::<Folio>()
}

/// Linux `shmem_read_mapping_page_gfp()` page-returning wrapper.
///
/// # Safety
/// The requirements of [`shmem_read_folio_gfp`] apply. A non-error result owns
/// one page reference that the caller must release with [`put_page`].
pub unsafe fn shmem_read_mapping_page_gfp(
    mapping: *mut AddressSpace,
    index: c_ulong,
    gfp: u32,
) -> *mut Page {
    // SAFETY: Forwarding arguments to `shmem_read_folio_gfp` under caller's contract.
    unsafe { shmem_read_folio_gfp(mapping, index, gfp) }.cast()
}

/// Linux `shmem_read_mapping_page()` with the mapping's allocation mask.
///
/// # Safety
/// The requirements of [`shmem_read_mapping_page_gfp`] apply. A non-error
/// result owns one page reference that the caller must release with
/// [`put_page`].
pub unsafe fn shmem_read_mapping_page(mapping: *mut AddressSpace, index: c_ulong) -> *mut Page {
    // SAFETY: Reading gfp mask from `mapping` under caller's contract.
    let gfp = unsafe { mapping_gfp_mask(mapping) };
    // SAFETY: Forwarding arguments to `shmem_read_mapping_page_gfp`.
    unsafe { shmem_read_mapping_page_gfp(mapping, index, gfp) }
}

/// Drop all page-cache entries in the inclusive byte range.  The GEM caller
/// uses [0, -1], which releases every cached page while preserving file size.
///
/// # Safety
/// `inode` must be null or a live inode whose mapping remains allocated and
/// valid for the duration of the operation. Callers must synchronize file or
/// mapping destruction against this operation; references held by users of
/// removed pages must remain valid until those references are released.
pub unsafe fn shmem_truncate_range(inode: *mut Inode, start: i64, end: i64) {
    if inode.is_null() {
        return;
    }
    // SAFETY: `inode` is verified non-null and valid per caller safety contract.
    let mapping = unsafe { (*inode).mapping };
    if mapping.is_null() || start < 0 {
        return;
    }
    let first = (start as u64) >> PAGE_SHIFT;
    let last = if end < 0 {
        u64::MAX
    } else {
        (end as u64) >> PAGE_SHIFT
    };
    if first > last {
        return;
    }
    loop {
        let page = {
            // SAFETY: Accessing `mapping.pages` lock on valid `mapping`.
            let mut pages = unsafe { (*mapping).pages.lock() };
            let key = pages.range(first..=last).next().map(|(index, _)| *index);
            key.and_then(|index| pages.remove(&index))
                .map(|page| page as *mut Page)
        };
        let Some(page) = page else {
            break;
        };
        {
            let registry = PAGE_REGISTRY.lock();
            let record = find_record(&registry, page);
            if !record.is_null() {
                // SAFETY: `record` is non-null; disassociating page from `mapping` during truncate.
                unsafe { (*record).mapping.store(ptr::null_mut(), Ordering::Release) };
            }
        }
        // SAFETY: Releasing the address-space cache reference on the removed `page`.
        unsafe { put_page(page) };
    }
}

/// Set up a synchronous `kiocb` for a file write iterator.
///
/// # Safety
/// `kiocb` must be exclusively accessible for the write. `file` must be null
/// or a live LinuxKPI shmem file kept alive for the duration of the operation.
pub unsafe fn init_sync_kiocb(kiocb: &mut Kiocb, file: *mut File) {
    kiocb.ki_filp = file;
    kiocb.ki_pos = 0;
    kiocb.ki_complete = None;
    kiocb.private = ptr::null_mut();
    kiocb.ki_flags = if file.is_null() {
        0
    } else {
        // SAFETY: `file` is verified non-null and live per caller safety contract.
        unsafe { (*file).f_iocb_flags as c_int }
    };
    kiocb.ki_ioprio = 0;
    kiocb.ki_write_stream = 0;
    kiocb._pad = 0;
    kiocb.ki_waitq = ptr::null_mut();
}

/// Initialize a single-user-buffer iterator.  The backing words are private to
/// this KPI and keep the source-layout size of Linux `struct iov_iter`.
///
/// # Safety
/// `iter` must be exclusively accessible for the write. If `count` is
/// nonzero, `buffer` must remain valid for `count` bytes with access
/// appropriate to `direction` for every operation that consumes the
/// initialized iterator.
pub unsafe fn iov_iter_ubuf(iter: &mut IovIter, direction: u32, buffer: *mut c_void, count: usize) {
    iter._opaque = [0; 8];
    iter._opaque[0] = direction as usize;
    iter._opaque[1] = buffer as usize;
    iter._opaque[2] = count;
}

/// Synchronously write `count` bytes from `data` into `file` starting at `position`.
///
/// # Safety
/// - `file` must be a valid, live `File` pointer with valid `f_mapping` and `f_inode`.
/// - If `count > 0`, `data` must point to a readable buffer of at least `count` bytes
///   that does not overlap with any destination page.
unsafe fn write_at(file: *mut File, mut position: u64, data: *const u8, count: usize) -> isize {
    if file.is_null() || crate::config::is_err(file) {
        return -EINVAL as isize;
    }
    // SAFETY: `file` is verified non-null and not err_ptr.
    let mapping = unsafe { (*file).f_mapping };
    // SAFETY: `file` is verified non-null and not err_ptr.
    let inode = unsafe { (*file).f_inode };
    let Some(end) = position.checked_add(count as u64) else {
        return -EFBIG as isize;
    };
    if end > MAX_FILE_SIZE || mapping.is_null() || inode.is_null() {
        return -EFBIG as isize;
    }
    if count != 0 && data.is_null() {
        return -EFAULT as isize;
    }
    let mut written = 0usize;
    while written < count {
        let index = (position >> PAGE_SHIFT) as c_ulong;
        let offset = (position as usize) & (PAGE_SIZE - 1);
        let this = (PAGE_SIZE - offset).min(count - written);
        // SAFETY: Reading/allocating folio at `index` from valid `mapping`.
        let folio = unsafe { shmem_read_folio_gfp(mapping, index, mapping_gfp_mask(mapping)) };
        if crate::config::is_err(folio) {
            return if written != 0 {
                written as isize
            } else {
                crate::config::ptr_err(folio) as isize
            };
        }
        let page = folio.cast::<Page>();
        // SAFETY: `page` is a live registered page returned by `shmem_read_folio_gfp`.
        unsafe { lock_page(page) };
        // SAFETY: `page` is locked; computing destination byte pointer at `offset < PAGE_SIZE`.
        let target = unsafe { page_address(page).cast::<u8>().add(offset) };
        // SAFETY: `data.add(written)` is valid for `this` bytes, `target` has `PAGE_SIZE - offset >= this`
        // bytes in the page, and buffers do not overlap per caller safety contract.
        unsafe { ptr::copy_nonoverlapping(data.add(written), target, this) };
        {
            let registry = PAGE_REGISTRY.lock();
            let record = find_record(&registry, page);
            assert!(!record.is_null(), "write_at lost its page registration");
            // SAFETY: `record` is non-null; marking page uptodate after copying data.
            unsafe { (*record).flags.fetch_or(PAGE_UPTODATE, Ordering::Release) };
        }
        // SAFETY: Marking folio dirty after write.
        unsafe { folio_mark_dirty(folio) };
        // SAFETY: Marking folio accessed after write.
        unsafe { folio_mark_accessed(folio) };
        // SAFETY: Unlocking page after write completion.
        unsafe { unlock_page(page) };
        // SAFETY: Dropping caller reference acquired by `shmem_read_folio_gfp`.
        unsafe { put_page(page) };
        written += this;
        position += this as u64;
    }
    // SAFETY: Reading current inode size from atomic field.
    let size = unsafe { (*inode).size.load(Ordering::Acquire) };
    if end > size {
        // SAFETY: Updating inode size to reflect appended bytes.
        unsafe { (*inode).size.store(end, Ordering::Release) };
    }
    written as isize
}

/// Write iterator callback implementing `f_op->write_iter`.
///
/// # Safety
/// `kiocb` and `iter` must point to valid, initialized structures per Linux VFS contract.
unsafe extern "C" fn shmem_write_iter(kiocb: *mut Kiocb, iter: *mut IovIter) -> isize {
    if kiocb.is_null() || iter.is_null() {
        return -EINVAL as isize;
    }
    // SAFETY: `iter` is verified non-null and points to a valid `IovIter`.
    let state = unsafe { &(*iter)._opaque };
    if state[0] != ITER_SOURCE as usize {
        return -EINVAL as isize;
    }
    // SAFETY: `kiocb` is verified non-null.
    let file = unsafe { (*kiocb).ki_filp };
    // SAFETY: `kiocb` is verified non-null.
    let position = unsafe { (*kiocb).ki_pos };
    if position < 0 {
        return -EINVAL as isize;
    }
    // SAFETY: `file` is valid from `kiocb`, and `state[1]` points to a buffer of `state[2]` bytes.
    let result = unsafe { write_at(file, position as u64, state[1] as *const u8, state[2]) };
    if result > 0 {
        // SAFETY: Updating `ki_pos` on successful write.
        unsafe { (*kiocb).ki_pos = (*kiocb).ki_pos.saturating_add(result as i64) };
    }
    result
}

/// Write directly to the anonymous shmem page cache from a kernel buffer.
///
/// # Safety
/// `file` must be a live LinuxKPI shmem file with a reference held throughout
/// the call. If `count` is nonzero, `data` must be readable for `count` bytes
/// and must not overlap any destination page-cache range. `pos` must point to
/// a writable `i64` for the duration of the call.
pub unsafe fn kernel_write(
    file: *mut File,
    data: *const c_void,
    count: usize,
    pos: *mut i64,
) -> isize {
    // SAFETY: `pos` is checked for non-null before reading `*pos`.
    if pos.is_null() || unsafe { *pos < 0 } || (data.is_null() && count != 0) {
        return -EINVAL as isize;
    }
    // SAFETY: `file` is live, `*pos` is valid and non-negative, `data` is valid for `count` bytes.
    let result = unsafe { write_at(file, *pos as u64, data.cast(), count) };
    if result > 0 {
        // SAFETY: `pos` is verified non-null and writable per caller safety contract.
        unsafe { *pos = (*pos).saturating_add(result as i64) };
    }
    result
}

/// Files on x86_64 use large-file offsets by default.
pub fn force_o_largefile() -> bool {
    usize::BITS > 32
}

/// The target has no swap device; writeback retains dirty folios in memory.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the call. If it is page-locked, the caller must own that lock because this
/// function may release it. The ignored plug and list pointers are not
/// dereferenced.
pub unsafe fn shmem_writeout(
    folio: *mut Folio,
    _plug: *mut *mut c_void,
    _list: *mut c_void,
) -> c_int {
    if folio.is_null() {
        return -EINVAL;
    }
    // SAFETY: `folio` is verified non-null and live per caller safety contract.
    unsafe { folio_mark_dirty(folio) };
    // Linux reports AOP_WRITEPAGE_ACTIVATE when shmem cannot allocate swap.
    // This direct writeback caller has no VM writepage wrapper to unlock it.
    let page = folio.cast::<Page>();
    let locked = {
        let registry = PAGE_REGISTRY.lock();
        let record = find_record(&registry, page);
        // SAFETY: `record` is non-null and points to a registered `PageRecord`.
        !record.is_null() && unsafe { (*record).flags.load(Ordering::Acquire) & PAGE_LOCKED != 0 }
    };
    if locked {
        // SAFETY: `page` is locked and owned by the caller/writeback context.
        unsafe { unlock_page(page) };
    }
    AOP_WRITEPAGE_ACTIVATE
}

/// Redirty a folio declined by writeback and release the writeback lock.
///
/// # Safety
/// `folio` must be a live registered folio with a reference held throughout
/// the call. `wbc` must be null or point to a live, writable
/// [`WritebackControl`]. If the folio is locked, the caller must own the lock
/// because this function may release it.
pub unsafe fn folio_redirty_for_writepage(wbc: *mut WritebackControl, folio: *mut Folio) -> bool {
    if folio.is_null() {
        return false;
    }
    if !wbc.is_null() {
        // SAFETY: `wbc` is verified non-null and writable per caller safety contract.
        unsafe { (*wbc).pages_skipped = (*wbc).pages_skipped.saturating_add(1) };
    }
    // SAFETY: `folio` is verified non-null and live per caller safety contract.
    let changed = unsafe { folio_mark_dirty(folio) };
    let page = folio.cast::<Page>();
    let locked = {
        let registry = PAGE_REGISTRY.lock();
        let record = find_record(&registry, page);
        // SAFETY: `record` is non-null and points to a registered `PageRecord`.
        !record.is_null() && unsafe { (*record).flags.load(Ordering::Acquire) & PAGE_LOCKED != 0 }
    };
    if locked {
        // SAFETY: `page` is locked and owned by the writeback context.
        unsafe { unlock_page(page) };
    }
    changed
}

/// Iterate dirty folios in an address_space and lock each page for writeback.
/// The returned page reference is retained in the writeback batch until the
/// next iterator call, matching the Linux writeback_control lifetime.
///
/// # Safety
/// `mapping` and `wbc` must point to live, initialized objects that remain
/// valid throughout the call; `wbc` must be exclusively accessible. `error`
/// must point to a writable `c_int`. If non-null, `previous` must be the live
/// folio most recently returned by this iterator using the same `wbc`; do not
/// release the batch-owned reference before the next call. A non-null return
/// is a batch-owned folio reference that remains valid until the next call
/// releases that batch.
pub unsafe fn writeback_iter(
    mapping: *mut AddressSpace,
    wbc: *mut WritebackControl,
    previous: *mut Folio,
    error: *mut c_int,
) -> *mut Folio {
    if mapping.is_null() || wbc.is_null() || error.is_null() {
        return ptr::null_mut();
    }
    if previous.is_null() {
        // SAFETY: `wbc` is non-null and exclusively accessible per caller safety contract.
        folio_batch_init(unsafe { &mut (*wbc).fbatch });
        // SAFETY: Initializing writeback tracking fields on valid `wbc` and `error`.
        unsafe {
            (*wbc).index = (*wbc).range_start >> PAGE_SHIFT;
            (*wbc).saved_err = 0;
            *error = 0;
        }
    } else {
        // SAFETY: Releasing previous batch entries from `wbc.fbatch`.
        unsafe { __folio_batch_release(ptr::addr_of_mut!((*wbc).fbatch)) };
        let record = {
            let registry = PAGE_REGISTRY.lock();
            find_record(&registry, previous.cast())
        };
        if !record.is_null() {
            // SAFETY: `record` is non-null; reading atomic index.
            let next = unsafe { (*record).index.load(Ordering::Acquire).saturating_add(1) };
            // SAFETY: Updating `wbc.index` on valid `wbc`.
            unsafe { (*wbc).index = (*wbc).index.max(next) };
        }
        // SAFETY: Decrementing `nr_to_write` on valid `wbc`.
        unsafe { (*wbc).nr_to_write = (*wbc).nr_to_write.saturating_sub(1) };
        // SAFETY: Reading `*error` and saving error on valid `wbc`.
        if unsafe { *error != 0 && (*wbc).saved_err == 0 } {
            // SAFETY: Storing error code into `wbc.saved_err`.
            unsafe { (*wbc).saved_err = *error };
        }
        // SAFETY: Checking error condition and `nr_to_write` on valid `wbc` and `error`.
        if unsafe { (*error != 0 && (*wbc).sync_mode != WB_SYNC_ALL) || (*wbc).nr_to_write <= 0 } {
            // SAFETY: Writing saved error to `*error`.
            unsafe { *error = (*wbc).saved_err };
            return ptr::null_mut();
        }
    }

    loop {
        let candidate = {
            // SAFETY: Accessing `mapping.pages` lock on valid `mapping`.
            let pages = unsafe { (*mapping).pages.lock() };
            let candidate = pages
                // SAFETY: Reading `wbc.index` from valid `wbc`.
                .range(unsafe { (*wbc).index }..)
                .find(|(index, page)| {
                    let index = **index;
                    let page = **page as *const Page;
                    // SAFETY: Reading `wbc.range_end` from valid `wbc`.
                    (index << PAGE_SHIFT) <= unsafe { (*wbc).range_end } && {
                        let registry = PAGE_REGISTRY.lock();
                        let record = find_record(&registry, page);
                        !record.is_null()
                            // SAFETY: `record` is verified non-null and points to a registered `PageRecord`.
                            && unsafe { (*record).flags.load(Ordering::Acquire) & PAGE_DIRTY != 0 }
                    }
                })
                .map(|(index, page)| (*index, *page as *mut Page));
            if let Some((_, page)) = candidate {
                // Keep the page alive while dropping the address-space lock.
                // SAFETY: `page` is a live registered page found in `mapping.pages`.
                unsafe { get_page(page) };
            }
            candidate
        };
        let Some((index, page)) = candidate else {
            // SAFETY: Releasing batch and storing saved error when no more candidates exist.
            unsafe { __folio_batch_release(ptr::addr_of_mut!((*wbc).fbatch)) };
            // SAFETY: Writing saved error code to `*error`.
            unsafe { *error = (*wbc).saved_err };
            return ptr::null_mut();
        };
        // SAFETY: Updating `wbc.index` on valid `wbc`.
        unsafe { (*wbc).index = index.saturating_add(1) };
        // SAFETY: Locking `page` before inspecting and returning it.
        unsafe { lock_page(page) };
        let dirty = {
            let registry = PAGE_REGISTRY.lock();
            let record = find_record(&registry, page);
            !record.is_null()
                // SAFETY: `record` is verified non-null and points to a live registered `PageRecord`.
                && unsafe { (*record).flags.load(Ordering::Acquire) & PAGE_DIRTY != 0 }
        };
        if !dirty {
            // SAFETY: Page became clean; unlock and release temporary reference.
            unsafe { unlock_page(page) };
            // SAFETY: Drop reference acquired when candidate was selected.
            unsafe { put_page(page) };
            continue;
        }
        let folio = page.cast::<Folio>();
        // SAFETY: Adding `folio` to `wbc.fbatch` on valid `wbc`.
        folio_batch_add(unsafe { &mut (*wbc).fbatch }, folio);
        return folio;
    }
}

/// Number of available RAM pages on the target, or modeled live pages on a
/// host-only build.
pub fn totalram_pages() -> c_ulong {
    if let Some(ops) = MEMORY_ADDRESS.get() {
        return (ops.totalram_pages)() as c_ulong;
    }

    #[cfg(all(test, not(target_os = "none")))]
    {
        PAGE_REGISTRY.lock().count as c_ulong
    }
    #[cfg(not(all(test, not(target_os = "none"))))]
    {
        0
    }
}

/// The hosting kernel has no kswapd worker; there is no background-reclaim caller.
pub fn current_is_kswapd() -> bool {
    false
}

/// LinuxKPI `dev_warn()` adapter for the one-argument DRM callsite.
pub fn dev_warn<T, A: CFormatArg>(device: *mut T, format: &str, argument: A) {
    let _ = device;
    let args: [&dyn CFormatArg; 1] = [&argument];
    let message = print::format_c_message(format, &args);
    print::drm_log_at(
        DrmLogLevel::Warn,
        "drm LinuxKPI warning",
        file!(),
        line!(),
        &message,
    );
}

/// Linux `fput(file)` ABI entry.
///
/// # Safety
/// `file` must hold one reference taken by this owner.
#[unsafe(export_name = "fput")]
pub unsafe extern "C" fn fput_c(file: *mut c_void) {
    // SAFETY: Forwarding `file` pointer to `fput` under caller's contract.
    unsafe { fput(file) };
}

/// Linux `alloc_pages(gfp_mask, order)` ABI entry.
///
/// # Safety
/// Follows the Linux allocator contract; release with `__free_pages`.
#[unsafe(export_name = "alloc_pages")]
pub unsafe extern "C" fn alloc_pages_c(gfp_mask: u32, order: u32) -> *mut Page {
    // SAFETY: Forwarding allocation request to `alloc_pages`.
    unsafe { alloc_pages(gfp_mask, order) }
}

/// Linux `__free_pages(page, order)` ABI entry.
///
/// # Safety
/// `page` must come from `alloc_pages_c` with the same `order`.
#[unsafe(export_name = "__free_pages")]
pub unsafe extern "C" fn __free_pages_c(page: *mut Page, order: u32) {
    // SAFETY: Forwarding deallocation request to `free_pages`.
    unsafe { free_pages(page, order) };
}

/// Linux `page_address(page)` ABI entry for registered pages.
///
/// # Safety
/// `page` must be a page known to this allocator.
#[unsafe(export_name = "page_address")]
pub unsafe extern "C" fn page_address_c(page: *mut Page) -> *mut c_void {
    // SAFETY: Forwarding lookup request to `page_address`.
    unsafe { page_address(page) }
}

/// Linux `mark_page_accessed(page)` ABI entry.
///
/// # Safety
/// `page` must be a page known to this allocator.
#[unsafe(export_name = "mark_page_accessed")]
pub unsafe extern "C" fn mark_page_accessed_c(page: *mut Page) {
    // SAFETY: Forwarding access accounting to `mark_page_accessed`.
    unsafe { mark_page_accessed(page) };
}

/// Linux `unlock_page(page)` ABI entry.
///
/// # Safety
/// `page` must be locked by the caller.
#[unsafe(export_name = "unlock_page")]
pub unsafe extern "C" fn unlock_page_c(page: *mut Page) {
    // SAFETY: Forwarding page unlocking to `unlock_page`.
    unsafe { unlock_page(page) };
}

/// Linux `set_page_dirty(page)`: marks the page dirty and reports whether this
/// call changed its state.
///
/// # Safety
/// `page` must be a page known to this allocator.
#[unsafe(export_name = "set_page_dirty")]
pub unsafe extern "C" fn set_page_dirty_c(page: *mut Page) -> bool {
    // SAFETY: Forwarding dirty marking to `folio_mark_dirty`.
    unsafe { folio_mark_dirty(page.cast()) }
}

/// Linux `get_nr_swap_pages()`. The hosting kernel has no swap device, so the number of
/// free swap slots is zero by definition.
#[unsafe(export_name = "get_nr_swap_pages")]
pub extern "C" fn get_nr_swap_pages() -> c_long {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_physical_frame_rejects_compound_members() {
        // SAFETY: Unit test exercising module page allocation and physical frame pinning APIs.
        unsafe {
            let head = alloc_pages(GFP_DIRECT_RECLAIM, 1);
            assert!(!head.is_null());
            let base_physical = try_page_to_phys(head).unwrap();

            assert!(pin_physical_frame(base_physical).is_null());
            assert!(pin_physical_frame(base_physical + PAGE_SIZE).is_null());

            // Failed pins must not add references that prevent whole-block release.
            free_pages(head, 1);
        }
    }

    #[test]
    fn file_pages_survive_no_swap_writeback_and_truncate() {
        // SAFETY: Unit test exercising shmem file creation, page cache write, writeback, and truncation.
        unsafe {
            let file = shmem_file_setup(c"native-test".as_ptr(), (2 * PAGE_SIZE) as u64, 0);
            assert!(!file.is_null() && (file as isize) > 0);
            let payload = [0xa5u8; 32];
            let mut pos = (PAGE_SIZE - 16) as i64;
            assert_eq!(
                kernel_write(file, payload.as_ptr().cast(), payload.len(), &mut pos),
                32
            );
            assert_eq!(pos, (PAGE_SIZE + 16) as i64);
            let mapping = (*file).f_mapping;
            let folio = shmem_read_folio_gfp(mapping, 0, GFP_KERNEL);
            assert!(!folio.is_null() && (folio as isize) > 0);
            let page = folio.cast::<Page>();
            let addr = page_address(page).cast::<u8>();
            assert_eq!(*addr, 0);
            assert_eq!(*addr.add(PAGE_SIZE - 1), 0xa5);
            assert_eq!(
                shmem_writeout(folio, ptr::null_mut(), ptr::null_mut()),
                AOP_WRITEPAGE_ACTIVATE
            );
            assert_eq!(*addr.add(PAGE_SIZE - 1), 0xa5);
            shmem_truncate_range((*file).f_inode, 0, -1);
            // A caller reference survives removal of the mapping reference.
            assert_eq!(*addr.add(PAGE_SIZE - 1), 0xa5);
            folio_put(folio);
            let fresh = shmem_read_folio_gfp(mapping, 0, GFP_KERNEL);
            assert_eq!(
                *page_address(fresh.cast()).cast::<u8>().add(PAGE_SIZE - 1),
                0
            );
            folio_put(fresh);
            fput_file(file);
        }
    }
}

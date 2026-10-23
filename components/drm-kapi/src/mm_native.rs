// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Linux MM shim over the calling process's real address space. No Linux MM
//! implementation is copied. `current_mm()` resolves the current task's
//! process address space through a provider that the kernel installs; every
//! operation (usercopy, mapping, pinning, interval notifiers) goes through the
//! same `UserAddressSpace` that the core mm uses, so the core invalidation
//! hooks deliver to the interval subscribers registered here.
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    any::Any,
    cell::UnsafeCell,
    ffi::{c_int, c_long, c_ulong, c_void},
    mem::size_of,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use super::{
    mm::{VM_FAULT_SIGBUS, VM_WRITE, VmAreaStruct},
    shmem,
};
use crate::{
    config::PAGE_SIZE,
    mmu_notifier::{MmuIntervalNotifier, MmuIntervalNotifierOps, MmuNotifierRange},
    mmu_notifier_domain::{self as mmu_notifier, Event, Range as NotifierSpan, Subscriber},
    types::{File, Page},
};

/// Architecture-neutral page-table attributes used by the current process
/// address-space provider. These are internal LinuxKPI flags, not a user ABI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct VmFlags(u64);

impl VmFlags {
    pub const USER: Self = Self(1 << 0);
    pub const READ: Self = Self(1 << 1);
    pub const WRITE: Self = Self(1 << 2);
    pub const EXECUTE: Self = Self(1 << 3);
    pub const UNCACHED: Self = Self(1 << 4);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for VmFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for VmFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One process address space as the Linux mm shim sees it. The kernel
/// implements this over its per-process `AddrSpace` and holds it weakly: the
/// shim never keeps process memory alive. Once the process exits,
/// `alive()` is false, every operation fails closed, and the core
/// `mmu_notifier::release` has already invalidated the interval subscribers.
pub trait UserAddressSpace: Send + Sync {
    /// Core notifier-domain key (the address space's `AddressSpaceId`).
    fn id(&self) -> u64;
    /// False once the process address space has been destroyed.
    fn alive(&self) -> bool;
    /// `(base, end)` of the user virtual address range.
    fn bounds(&self) -> (usize, usize);
    fn contains_range(&self, addr: usize, size: usize) -> bool;
    /// First free area at or after `hint` (or the base when `hint` is 0).
    fn find_free_area(&self, hint: usize, size: usize) -> Option<usize>;
    /// Current 4 KiB-page leaf for `addr`: physical base and flags.
    fn query(&self, addr: usize) -> Option<(usize, VmFlags)>;
    /// Fault-checked read; `false` if any byte is unmapped or unreadable.
    fn read(&self, addr: usize, dst: &mut [u8]) -> bool;
    /// Fault-checked write; `false` if any byte is unmapped or unwritable.
    fn write(&self, addr: usize, src: &[u8]) -> bool;
    /// Install a leaf and perform any required TLB invalidation before return.
    fn map_linear(&self, addr: usize, paddr: usize, size: usize, flags: VmFlags) -> bool;
    /// Remove leaves and synchronously invalidate stale translations.
    fn unmap(&self, addr: usize, size: usize) -> bool;
    /// Faults in and pins the page-aligned range `[addr, addr + size)` with the
    /// core long-term pin owner. The returned frames are the physical pages the
    /// process maps. They stay allocated, and a COW break or unmap defers their
    /// release, until `hold` is dropped.
    fn pin(&self, addr: usize, size: usize, write: bool) -> Result<PinnedFrames, c_int>;
}

/// Physical 4 KiB frames of a pinned user range, plus the owner that keeps
/// them pinned.
pub struct PinnedFrames {
    pub frames: Vec<usize>,
    pub hold: Box<dyn Any + Send>,
}

/// Installed once by kernel glue: returns the calling task's process address
/// space, or `None` for kernel threads and uninitialised tasks.
pub type CurrentUserAddressSpace = fn() -> Option<Arc<dyn UserAddressSpace>>;

static CURRENT_USER_ADDRESS_SPACE: spin::Once<CurrentUserAddressSpace> = spin::Once::new();

/// Core-MM implementation of Linux `mapping_wrprotect_range()`.  The
/// LinuxKPI entry below intentionally delegates to the kernel's existing
/// shared-backing reverse map; it does not walk this shim's `MmStruct::vmas`.
///
/// # Safety
///
/// - `mapping` must point to the live shared-backing mapping whose pages are being write-protected.
/// - `pgoff`, `pfn` and `nr_pages` must describe a range that lies within that mapping.
pub type MappingWrprotectHandler = unsafe extern "C" fn(
    mapping: *mut c_void,
    pgoff: c_ulong,
    pfn: c_ulong,
    nr_pages: c_ulong,
) -> c_int;

static MAPPING_WRPROTECT_HANDLER: spin::Once<MappingWrprotectHandler> = spin::Once::new();

/// Core-MM implementation of Linux `vmf_insert_pfn()`. The provider validates
/// the active SharedPages VMA/backend before inserting a leaf into its owning
/// address space; this shim never creates a second VMA or page-table owner.
///
/// # Safety
///
/// - `vma` must point to a live `VmAreaStruct` whose SharedPages backing the provider has validated.
/// - `address` must lie inside the range of `vma`.
pub type VmfInsertPfnHandler =
    unsafe extern "C" fn(vma: *mut VmAreaStruct, address: c_ulong, pfn: c_ulong) -> u32;

static VMF_INSERT_PFN_HANDLER: spin::Once<VmfInsertPfnHandler> = spin::Once::new();

/// Installs the core-MM `vmf_insert_pfn()` provider.
pub fn install_vmf_insert_pfn_handler(handler: VmfInsertPfnHandler) -> Result<(), &'static str> {
    VMF_INSERT_PFN_HANDLER.call_once(|| handler);
    match VMF_INSERT_PFN_HANDLER.get() {
        Some(current) if *current as usize == handler as usize => Ok(()),
        _ => Err("vmf_insert_pfn provider already installed"),
    }
}

/// Linux `vmf_insert_pfn()`, delegated to the active core SharedPages fault
/// context. Missing provider or an unrelated VMA fails closed with SIGBUS.
///
/// # Safety
/// A non-null `vma` must point to a live, properly aligned `VmAreaStruct`
/// owned by the fault context and remain valid for the duration of the provider
/// call; null is rejected with SIGBUS. `address` must identify the faulting
/// address in that VMA, and `pfn` must identify a frame the VMA's backing
/// object may map and that remains owned for the mapping's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmf_insert_pfn(
    vma: *mut VmAreaStruct,
    address: c_ulong,
    pfn: c_ulong,
) -> u32 {
    if vma.is_null() {
        return VM_FAULT_SIGBUS;
    }
    let Some(handler) = VMF_INSERT_PFN_HANDLER.get() else {
        return VM_FAULT_SIGBUS;
    };
    // SAFETY: vma is verified non-null and satisfies caller validity and alignment contracts;
    // handler is the installed core SharedPages fault handler.
    unsafe { handler(vma, address, pfn) }
}

/// Installs the native core-MM provider for LinuxKPI deferred-I/O rearming.
///
/// This is process-global kernel plumbing, installed before framebuffer VMAs
/// are published.  A second, different installation is refused.
pub fn install_mapping_wrprotect_handler(
    handler: MappingWrprotectHandler,
) -> Result<(), &'static str> {
    MAPPING_WRPROTECT_HANDLER.call_once(|| handler);
    match MAPPING_WRPROTECT_HANDLER.get() {
        Some(current) if *current as usize == handler as usize => Ok(()),
        _ => Err("mapping_wrprotect_range provider already installed"),
    }
}

/// Returns whether the real core-MM write-protect provider is installed.
#[unsafe(no_mangle)]
pub extern "C" fn mapping_wrprotect_range_ready() -> c_int {
    if MAPPING_WRPROTECT_HANDLER.get().is_some() {
        1
    } else {
        0
    }
}

/// Linux `mapping_wrprotect_range()`, dispatched to the core shared-page
/// alias registry.  Returning an error before provider installation is
/// deliberately fail-closed: framebuffer deferred-I/O must not report a
/// successful rearm without revoking writable user translations.
///
/// # Safety
/// A non-null `mapping` must be the live address-space mapping identity
/// expected by the installed provider, with its backing object kept alive
/// until the call returns; null is rejected. `pgoff`, `pfn`, and `nr_pages`
/// must describe the corresponding valid page range in that mapping; the
/// provider may inspect it and revoke aliases synchronously.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mapping_wrprotect_range(
    mapping: *mut c_void,
    pgoff: c_ulong,
    pfn: c_ulong,
    nr_pages: c_ulong,
) -> c_int {
    if mapping.is_null() {
        return -crate::config::EINVAL;
    }
    if nr_pages == 0 {
        return 0;
    }
    let Some(handler) = MAPPING_WRPROTECT_HANDLER.get() else {
        return -crate::config::EOPNOTSUPP;
    };
    // SAFETY: mapping is non-null and matches the installed provider's expected identity;
    // caller guarantees backing object remains alive until return.
    unsafe { handler(mapping, pgoff, pfn, nr_pages) }
}

#[cfg(test)]
mod shared_mmap_provider_tests {
    use super::*;

    #[test]
    fn absent_shared_mmap_providers_fail_closed() {
        assert_eq!(mapping_wrprotect_range_ready(), 0);
        let mut mapping_cookie = 0u8;
        assert_eq!(
            // SAFETY: `mapping_cookie` is a live stack byte passed only as an opaque cookie; with the provider absent the call returns the fail-closed error before using it.
            unsafe {
                mapping_wrprotect_range(core::ptr::addr_of_mut!(mapping_cookie).cast(), 0, 0, 1)
            },
            -crate::config::EOPNOTSUPP,
        );
        let mut vma = VmAreaStruct::empty();
        // SAFETY: Test passes empty local VMA to verify SIGBUS when handler absent.
        assert_eq!(unsafe { vmf_insert_pfn(&mut vma, 0, 0) }, VM_FAULT_SIGBUS);
    }
}

/// Installs the provider behind [`current_mm`]. A second installation is
/// refused: the provider is process-global.
pub fn install_current_user_address_space(
    provider: CurrentUserAddressSpace,
) -> Result<(), &'static str> {
    let mut installed = false;
    CURRENT_USER_ADDRESS_SPACE.call_once(|| {
        installed = true;
        provider
    });
    if installed {
        Ok(())
    } else {
        Err("current user address space provider already installed")
    }
}

pub struct MmStruct {
    space: Arc<dyn UserAddressSpace>,
    vmas: UnsafeCell<BTreeMap<usize, NativeVma>>,
    locked: AtomicBool,
    user_access_owner: AtomicU64,
    user_access_start: AtomicUsize,
    user_access_end: AtomicUsize,
    /// Key of this address space's notifier domain in `tk_mmu_notifier`. The
    /// core mm raises its invalidations under the same key.
    aspace_key: u64,
}
struct NativeVma {
    area: Box<VmAreaStruct>,
    pages: Vec<usize>,
    file: usize,
}
impl Drop for NativeVma {
    fn drop(&mut self) {
        for page in &self.pages {
            // SAFETY: page was obtained by pinning and NativeVma owns this reference until dropped.
            unsafe { shmem::put_page(*page as *mut Page) };
        }
        if self.file != 0 {
            // SAFETY: self.file is non-zero, representing an owned file reference from shmem::get_file_file.
            unsafe { shmem::fput_file(self.file as *mut shmem::File) };
        }
    }
}
// SAFETY: All access to native MM/VMA storage is serialized by the internal atomic `locked` state
// and mmap_read_lock / mmap_write_lock protocols. The inner UserAddressSpace is thread-safe.
unsafe impl Send for MmStruct {}
// SAFETY: Concurrent operations on MmStruct fields are coordinated through atomic locks and
// mmap synchronization protocols, safely allowing shared references across threads.
unsafe impl Sync for MmStruct {}
/// One `MmStruct` per live process address space, keyed by its notifier
/// domain. An entry is pruned only after its address space has died and no
/// notifier owner holds it.
static MAPPINGS: spin::Mutex<BTreeMap<u64, Arc<MmStruct>>> = spin::Mutex::new(BTreeMap::new());

/// Notifier records hold their MM alive (Linux `mmgrab()` on
/// `notifier.mm`) until `mmu_interval_notifier_remove()`.
static NOTIFIER_OWNERS: spin::Mutex<BTreeMap<usize, Arc<MmStruct>>> =
    spin::Mutex::new(BTreeMap::new());

impl MmStruct {
    fn new(space: Arc<dyn UserAddressSpace>) -> Arc<Self> {
        let aspace_key = space.id();
        Arc::new(Self {
            space,
            vmas: UnsafeCell::new(BTreeMap::new()),
            locked: AtomicBool::new(false),
            user_access_owner: AtomicU64::new(u64::MAX),
            user_access_start: AtomicUsize::new(0),
            user_access_end: AtomicUsize::new(0),
            aspace_key,
        })
    }
}

/// What the calling task's user address space looks like.
enum UserMm {
    /// Kernel context: no provider is installed, or the task has no user
    /// address space. Pointers passed to the user-copy helpers are kernel
    /// pointers.
    Kernel,
    /// A user task whose address space is being torn down. Its user
    /// addresses must not be touched, not even through the kernel mapping.
    Dead,
    /// A live user address space, kept registered while this reference lives.
    Live(Arc<MmStruct>),
}

fn current_user_mm() -> UserMm {
    let Some(provider) = CURRENT_USER_ADDRESS_SPACE.get() else {
        return UserMm::Kernel;
    };
    let Some(space) = provider() else {
        return UserMm::Kernel;
    };
    if !space.alive() {
        return UserMm::Dead;
    }
    let key = space.id();
    let mut mappings = MAPPINGS.lock();
    mappings.retain(|_, mm| mm.space.alive() || Arc::strong_count(mm) > 1);
    UserMm::Live(
        mappings
            .entry(key)
            .or_insert_with(|| MmStruct::new(space))
            .clone(),
    )
}

/// The calling task's live MM, holding a reference so a concurrent registry
/// prune cannot free it while the caller uses it.
pub fn current_mm_ref() -> Option<Arc<MmStruct>> {
    match current_user_mm() {
        UserMm::Live(mm) => Some(mm),
        UserMm::Kernel | UserMm::Dead => None,
    }
}

/// Linux `current->mm`: the MM of the calling task's process, or null when
/// the task has no live user address space.
///
/// The pointer stays valid only while the task's address space is live. Code
/// in this module uses [`current_mm_ref`] so the entry cannot be pruned while
/// it is being used.
pub fn current_mm() -> *mut MmStruct {
    current_mm_ref().map_or(ptr::null_mut(), |mm| Arc::as_ptr(&mm).cast_mut())
}

fn mm_ptr(mm: &Option<Arc<MmStruct>>) -> *mut MmStruct {
    mm.as_ref()
        .map_or(ptr::null_mut(), |mm| Arc::as_ptr(mm).cast_mut())
}

/// Whether a kernel-context copy may dereference `addr` directly.
///
/// With an address-space provider installed this is a real kernel, where a
/// task without a user MM (a worker) may still run on a borrowed user page
/// table. A user address must then not be written or read through the
/// kernel mapping: that would bypass copy-on-write and the MM's page
/// ownership. Kernel addresses live in the upper half on every supported
/// target. Host builds have no provider and keep plain pointer copies.
fn kernel_context_may_access(addr: usize) -> bool {
    if CURRENT_USER_ADDRESS_SPACE.get().is_none() {
        return true;
    }
    if (addr as isize) < 0 {
        return true;
    }
    log::error!("refusing a kernel-context user copy at {addr:#x}");
    false
}

/// Acquire the native MM's serialized mmap lock.
///
/// # Safety
/// `mm` must point to a live `MmStruct` whose owner remains alive until the
/// matching unlock. The caller must not recursively acquire this lock and
/// must release it exactly once with [`mmap_read_unlock`] on the same `mm`
/// before allowing that owner to be destroyed.
pub unsafe fn mmap_read_lock(mm: *mut MmStruct) {
    assert!(!mm.is_null(), "mmap lock without a user address space");
    // SAFETY: mm is non-null and points to an aligned MmStruct.
    while unsafe {
        (*mm)
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
    }
    .is_err()
    {
        crate::clock::relax();
    }
}

/// Release a lock previously acquired with [`mmap_read_lock`] or
/// [`mmap_write_lock_killable`].
///
/// # Safety
/// `mm` must point to the same live `MmStruct` whose mmap lock this execution
/// context currently holds. Only the acquiring context may release the lock,
/// exactly once; no access to protected VMA state may continue after this call.
pub unsafe fn mmap_read_unlock(mm: *mut MmStruct) {
    assert!(!mm.is_null());
    assert!(
        // SAFETY: mm is non-null and points to an aligned MmStruct whose lock is held.
        unsafe { (*mm).locked.swap(false, Ordering::Release) },
        "unbalanced mmap unlock"
    );
}

fn mmap_read_trylock(mm: *mut MmStruct) -> bool {
    !mm.is_null()
        // SAFETY: mm is non-null and points to an aligned MmStruct.
        && unsafe {
            (*mm)
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        }
}
/// Native tasks have no killable-wait signal channel here; successful return
/// means the real MM lock was acquired, not that a missing lock was skipped.
///
/// # Safety
/// `mm` must point to a live `MmStruct` whose owner remains alive until the
/// matching [`mmap_write_unlock`]. The caller must not recursively acquire the
/// mmap lock and must release a successful acquisition exactly once.
pub unsafe fn mmap_write_lock_killable(mm: *mut MmStruct) -> c_int {
    if mm.is_null() {
        return -crate::config::ENODEV;
    }
    // SAFETY: mm is verified non-null and valid; delegates to mmap_read_lock.
    unsafe { mmap_read_lock(mm) };
    0
}

/// Release a lock previously acquired with [`mmap_write_lock_killable`].
///
/// # Safety
/// `mm` must point to the same live `MmStruct` whose mmap lock this execution
/// context currently holds. Only the acquiring context may release it, exactly
/// once and after all protected VMA accesses have ended.
pub unsafe fn mmap_write_unlock(mm: *mut MmStruct) {
    // SAFETY: mm is non-null and valid; delegates to mmap_read_unlock.
    unsafe { mmap_read_unlock(mm) }
}
/// This serialized native backend deliberately provides stronger exclusion
/// than a Linux read semaphore; it does not allow a writer past a reader.
///
/// # Safety
/// If `mm` is non-null, it must point to a live `MmStruct`, and the caller must
/// hold its mmap lock while calling this function and using any non-null result.
/// The
/// returned VMA pointer is borrowed from `mm` and becomes invalid when the
/// lock is released or the MM is destroyed.
pub unsafe fn find_vma(mm: *mut MmStruct, addr: c_ulong) -> *mut VmAreaStruct {
    if mm.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: caller holds mm's lock, granting exclusive access to the vmas UnsafeCell.
    unsafe {
        (&mut *(*mm).vmas.get())
            .values_mut()
            .find(|v| v.area.vm_end > addr)
            .map_or(ptr::null_mut(), |v| &mut *v.area)
    }
}
pub struct VmaIterator {
    mm: *mut MmStruct,
    next: c_ulong,
}
impl VmaIterator {
    pub fn new(mm: *mut MmStruct, addr: c_ulong) -> Self {
        Self { mm, next: addr }
    }
}

/// Find the next VMA in a locked native MM and advance `iter` past it.
///
/// # Safety
/// `iter` must be a valid, uniquely borrowed, aligned `VmaIterator` for the
/// duration of the call. Its `mm` must point to a live `MmStruct` whose mmap
/// lock is held by the caller. Any non-null returned VMA is borrowed from that
/// MM and must not be used after the lock is released.
pub unsafe fn vma_find(iter: *mut VmaIterator, end: c_ulong) -> *mut VmAreaStruct {
    // SAFETY: iter points to a valid, uniquely borrowed VmaIterator.
    let iter = unsafe { &mut *iter };
    if iter.next >= end {
        return ptr::null_mut();
    }
    // SAFETY: iter.mm is valid and caller holds its mmap lock.
    let vma = unsafe { find_vma(iter.mm, iter.next) };
    // SAFETY: vma is checked non-null before dereferencing vm_start.
    if vma.is_null() || unsafe { (*vma).vm_start >= end } {
        return ptr::null_mut();
    }
    // SAFETY: vma is non-null and points to a valid VmAreaStruct.
    iter.next = unsafe { (*vma).vm_end };
    vma
}
fn map_flags(prot: u64) -> VmFlags {
    let mut flags = VmFlags::USER | VmFlags::READ;
    if prot & 2 != 0 {
        flags |= VmFlags::WRITE;
    }
    if prot & (1u64 << 63) == 0 {
        flags |= VmFlags::EXECUTE;
    }
    if prot & 0x18 != 0 {
        flags |= VmFlags::UNCACHED;
    }
    flags
}
/// Install a real page-table mapping into the bound native address space.
/// The caller owns the mmap lock and the backing page/IO range lifetime.
///
/// # Safety
/// If `mm` is non-null, it must be a live `MmStruct` and its mmap lock must be
/// held by the caller; a null `mm` is rejected. `addr` must be a page-aligned
/// address in that MM, and `pfn` must describe a
/// valid, page-aligned physical frame that remains allocated or reserved until
/// the mapping is removed. The caller must keep the address-space owner alive
/// and serialize all VMA changes until this call returns.
pub unsafe fn map_pfn(mm: *mut MmStruct, addr: usize, pfn: usize, prot: u64) -> c_int {
    if mm.is_null() || addr & 4095 != 0 {
        return -crate::config::EINVAL;
    }
    // SAFETY: mm is non-null and points to an aligned, live MmStruct.
    let space = unsafe { &(*mm).space };
    if space.query(addr).is_some() {
        // SAFETY: mm is non-null; invalidates the old mapping across notifiers before replacement.
        let _invalidation = unsafe {
            mmu_notifier::invalidate_range_start((*mm).aspace_key, addr, addr + 4096, Event::Clear)
        };
        if !space.unmap(addr, 4096) {
            return -crate::config::EIO;
        }
    }
    if !space.map_linear(addr, pfn << 12, 4096, map_flags(prot)) {
        return -crate::config::ENOMEM;
    }
    0
}
/// Range record handed to interval-notifier callbacks. Field order follows
/// Linux `struct mmu_notifier_range` (CONFIG_MMU_NOTIFIER).
#[repr(C)]
struct NotifierRange {
    mm: *mut MmStruct,
    start: c_ulong,
    end: c_ulong,
    flags: u32,
    event: u32,
    owner: *mut c_void,
}

/// Adapts one `struct mmu_interval_notifier` to the core notifier domain. The
/// domain holds the subscriber's record address as its token; the driver's
/// `ops->invalidate` runs with the sequence the domain published.
struct IntervalSubscriber {
    notifier: *mut MmuIntervalNotifier,
}

// SAFETY: the notifier record is owned by its driver object and stays valid
// until `mmu_interval_notifier_remove()`, which waits for in-flight
// invalidations. Its fields are accessed through the domain's protocol.
unsafe impl Send for IntervalSubscriber {}
// SAFETY: `&IntervalSubscriber` only reads the `notifier` pointer, and the notifier's fields are reached through the domain protocol described above.
unsafe impl Sync for IntervalSubscriber {}

impl Subscriber for IntervalSubscriber {
    fn invalidate(&self, range: &NotifierSpan, seq: u64) -> bool {
        let notifier = self.notifier;
        // SAFETY: `self.notifier` is a valid, aligned pointer to an active `MmuIntervalNotifier`
        // registered with this domain, and remains valid while subscribed.
        let (ops, mm) = unsafe { ((*notifier).ops, (*notifier).mm) };
        let raw = NotifierRange {
            mm,
            start: range.start as c_ulong,
            end: range.end as c_ulong,
            flags: 0,
            event: range.event as u32,
            owner: ptr::null_mut(),
        };
        let invalidate =
            // SAFETY: `ops` was stored at subscription and is not changed until unsubscription, so reading its `invalidate` field here is valid.
            unsafe { (*ops).invalidate }.expect("interval notifier without invalidate");
        // SAFETY: `invalidate` is called with valid pointers to `notifier` and the temporary
        // stack-allocated `raw` range struct, matching the C mmu_interval_notifier callback signature.
        let blockable = unsafe {
            invalidate(
                notifier,
                ptr::addr_of!(raw).cast::<MmuNotifierRange>(),
                seq as c_ulong,
            )
        };
        assert!(
            blockable,
            "blockable interval-notifier invalidation refused"
        );
        blockable
    }

    fn seq(&self) -> u64 {
        // SAFETY: `self.notifier` points to an active, aligned `MmuIntervalNotifier` whose
        // `invalidate_seq` is read with volatile semantics to sample sequence updates concurrently.
        unsafe { ptr::addr_of!((*self.notifier).invalidate_seq).read_volatile() }
    }

    fn set_seq(&self, seq: u64) {
        // Source mmu_interval_set_seq() is WRITE_ONCE on invalidate_seq.
        // SAFETY: `self.notifier` points to an active, aligned `MmuIntervalNotifier` whose
        // `invalidate_seq` is updated with volatile write semantics.
        unsafe {
            ptr::addr_of_mut!((*self.notifier).invalidate_seq).write_volatile(seq as c_ulong)
        };
    }
}

/// Revoke every mapped leaf in a page-aligned range and notify MMU subscribers.
///
/// # Safety
/// `mm` must be non-null and point to a live `MmStruct`; its mmap lock must be
/// held by the caller for the entire operation. `addr` and `size` must describe a
/// page-aligned range within that address space whose end does not overflow.
/// Keep the address-space owner and any backing-page references alive until
/// invalidation callbacks and page-table removal have completed.
pub unsafe fn zap_range(mm: *mut MmStruct, addr: usize, size: usize) {
    assert!(!mm.is_null());
    let end = addr
        .checked_add(size)
        .expect("zap range wraps the address space");
    let _invalidation =
        // SAFETY: `mm` is non-null (asserted above) and live per the caller's contract; `aspace_key` is read from it while the caller holds the mmap lock.
        unsafe { mmu_notifier::invalidate_range_start((*mm).aspace_key, addr, end, Event::Unmap) };
    // SAFETY: caller guarantees `mm` points to a valid live `MmStruct` whose space is safe to reference.
    let space = unsafe { &(*mm).space };
    if size != 0 {
        let page_size = crate::config::PAGE_SIZE;
        let mut offset = 0usize;
        while offset < size {
            let page = addr + offset;
            if space.query(page).is_some() {
                assert!(
                    space.unmap(page, page_size),
                    "native MM lost a mapped leaf during zap"
                );
            }
            offset = offset.saturating_add(page_size);
        }
    }
}

/// Linux `unmap_mapping_range()` for native MM mappings created by this
/// LinuxKPI backend. `vm_mmap()` currently admits shared file mappings only,
/// so `even_cows` has no private-COW distinction to apply. The file/page refs
/// remain owned by each VMA until normal VMA teardown; this operation only
/// removes the affected PTEs, matching the source API's revocation semantics.
///
/// # Safety
/// If `mapping` is non-null, it must be the live opaque address-space mapping
/// identity used by these VMAs, and its backing object must remain alive until
/// this synchronous revocation returns; null is ignored. The function acquires
/// each MM lock itself, so callers must not hold one of those mmap locks while
/// calling it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unmap_mapping_range(
    mapping: *mut c_void,
    holebegin: c_long,
    holelen: c_long,
    _even_cows: i32,
) {
    if mapping.is_null() || holebegin < 0 || holelen < 0 {
        return;
    }
    let page_size = crate::config::PAGE_SIZE as u64;
    let hole_start = holebegin as u64;
    let hole_end = if holelen == 0 {
        u64::MAX
    } else {
        hole_start.saturating_add(holelen as u64)
    };

    // Clone the process MM owners before taking their sleepable mmap locks;
    // duplicate thread bindings are harmless because zapping is idempotent.
    let mms = {
        let registry = MAPPINGS.lock();
        let mut mms = Vec::new();
        mms.try_reserve_exact(registry.len())
            .expect("cannot snapshot native MM owners for mapping revocation");
        mms.extend(registry.values().cloned());
        mms
    };
    for mm_owner in mms {
        let mm = Arc::as_ptr(&mm_owner).cast_mut();
        // SAFETY: `mm` is derived from `Arc::as_ptr(&mm_owner)`, which is non-null, aligned, and kept alive by `mm_owner`.
        unsafe { mmap_read_lock(mm) };
        // SAFETY: `mm` points to a valid live `MmStruct`, and `mmap_read_lock` is held preventing concurrent mutation of VMAs.
        let vmas = unsafe { &*(*mm).vmas.get() };
        for vma in vmas.values() {
            let area = &vma.area;
            if area.vm_file.is_null() {
                continue;
            }
            let file = area.vm_file.cast::<shmem::File>();
            // SAFETY: `vm_file` is non-null and points to a live `shmem::File` associated with this VMA.
            if unsafe { (*file).f_mapping } != mapping.cast() {
                continue;
            }

            let file_start = area.vm_pgoff.saturating_mul(page_size);
            let address_start = area.vm_start;
            let vma_len = area.vm_end.saturating_sub(area.vm_start);
            let file_end = file_start.saturating_add(vma_len);
            let first = core::cmp::max(file_start, hole_start);
            let last = core::cmp::min(file_end, hole_end);
            if first >= last {
                continue;
            }

            let first_page = first / page_size * page_size;
            let last_page = last.saturating_add(page_size - 1) / page_size * page_size;
            let address = address_start.saturating_add(first_page.saturating_sub(file_start));
            let length = last_page.saturating_sub(first_page) as usize;
            // SAFETY: `mm` is valid, `mmap_read_lock` is held, and address/length specify a page-aligned subrange of the VMA.
            unsafe { zap_range(mm, address as usize, length) };
        }
        // SAFETY: releases the mmap read lock previously acquired on `mm`.
        unsafe { mmap_read_unlock(mm) };
    }
}
/// Create a shared mapping of a LinuxKPI file in the current process MM.
///
/// # Safety
/// If non-null, `file` must point to a live, initialized LinuxKPI file object
/// accepted by this backend, with a valid `f_mapping`, and the caller must keep
/// a file reference alive through the call; null is rejected. The current task's MM and the file's
/// backing mapping must remain alive while the new VMA and page references are
/// installed. The returned mapping then owns its own file/page references.
pub unsafe fn vm_mmap(
    file: *mut File,
    addr: c_ulong,
    size: c_ulong,
    prot: u32,
    flags: u32,
    offset: u64,
) -> c_ulong {
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    if mm.is_null() {
        return (-crate::config::ENODEV as isize) as c_ulong;
    }
    if file.is_null()
        || size == 0
        || (size | offset as c_ulong) & 4095 != 0
        || flags != 1
        || prot & !7 != 0
    {
        return (-crate::config::EINVAL as isize) as c_ulong;
    }
    // SAFETY: `mm` is verified non-null and points to the current live `MmStruct`.
    unsafe { mmap_read_lock(mm) };
    let result = (|| -> Result<usize, c_int> {
        // SAFETY: `mm` is valid and live, and `mmap_read_lock` is held.
        let space = unsafe { &(*mm).space };
        let start = space
            .find_free_area(addr as usize, size as usize)
            .ok_or(-crate::config::ENOMEM)?;
        let file = file.cast::<shmem::File>();
        // SAFETY: caller guarantees `file` is non-null and points to an initialized `shmem::File`.
        let mapping = unsafe { (*file).f_mapping };
        let mut pages = Vec::new();
        pages
            .try_reserve_exact((size >> 12) as usize)
            .map_err(|_| -crate::config::ENOMEM)?;
        let pgprot =
            5 | if prot & 2 != 0 { 2 } else { 0 } | if prot & 4 == 0 { 1u64 << 63 } else { 0 };
        let mut mapped = 0usize;
        for index in 0..(size >> 12) as usize {
            // SAFETY: `mapping` is valid and kept alive by the caller's file reference.
            let page = unsafe {
                shmem::shmem_read_mapping_page(
                    mapping,
                    (offset >> 12) as c_ulong + index as c_ulong,
                )
            };
            let result = if (page as isize) < 0 && (page as usize) >= usize::MAX - 4094 {
                page as isize as i32
            } else {
                pages.push(page as usize);
                // SAFETY: `mm` is valid, target address is within the allocated area, and `page` is a valid `Page`.
                unsafe {
                    map_pfn(
                        mm,
                        start + index * 4096,
                        shmem::page_to_phys(page) >> 12,
                        pgprot,
                    )
                }
            };
            if result != 0 {
                if mapped != 0 {
                    // SAFETY: cleans up partially mapped range under the held mmap lock.
                    unsafe { zap_range(mm, start, mapped) }
                }
                for page in pages {
                    // SAFETY: `page` was returned by `shmem_read_mapping_page` and holds a reference to be released.
                    unsafe { shmem::put_page(page as *mut Page) }
                }
                return Err(result);
            }
            mapped += 4096;
        }
        // SAFETY: zero-initializes C-compatible `VmAreaStruct` struct representation.
        let mut area: Box<VmAreaStruct> = Box::new(unsafe { core::mem::zeroed() });
        area.vm_start = start as c_ulong;
        area.vm_end = (start + size as usize) as c_ulong;
        area.vm_mm = mm.cast();
        area.vm_flags = if prot & 2 != 0 { VM_WRITE } else { 0 };
        area.vm_page_prot.pgprot = pgprot as c_ulong;
        area.vm_pgoff = (offset >> 12) as c_ulong;
        area.vm_file = file.cast();
        // SAFETY: increments reference count on the live `shmem::File` for the new VMA.
        unsafe { shmem::get_file_file(file) };
        // SAFETY: `mm` is valid and live, `mmap_read_lock` is held, safely inserting the new VMA.
        unsafe {
            (&mut *(*mm).vmas.get()).insert(
                start,
                NativeVma {
                    area,
                    pages,
                    file: file as usize,
                },
            )
        };
        Ok(start)
    })();
    // SAFETY: releases the mmap read lock previously acquired on `mm`.
    unsafe { mmap_read_unlock(mm) };
    result.unwrap_or_else(|err| err as isize as usize) as c_ulong
}

/// Fault-checked usercopy through the native address space, never a raw copy
/// from an untrusted user pointer. Linux returns the number of uncopied bytes.
///
/// # Safety
/// For nonzero `size`, `to` must point to `size` writable bytes of kernel memory
/// and must not overlap the user source range. `from` is interpreted as an address in the
/// current task's user address space; when no user MM is bound, it must instead
/// be directly readable for `size` bytes. The kernel destination must remain
/// exclusively accessible until the call returns.
pub unsafe fn copy_from_user(to: *mut c_void, from: *const c_void, size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    let mm_ref = match current_user_mm() {
        UserMm::Live(mm) => Some(mm),
        // A dying task's user pages may be shared copy-on-write with its
        // parent; never write or read them through the kernel mapping.
        UserMm::Dead => return size,
        UserMm::Kernel => None,
    };
    let mm = mm_ptr(&mm_ref);
    if mm.is_null() {
        if from.is_null()
            || to.is_null()
            || (from as usize) < 4096
            || !kernel_context_may_access(from as usize)
        {
            return size;
        }
        // SAFETY: caller guarantees non-null kernel buffers and valid readable memory when no MM is bound.
        unsafe {
            core::ptr::copy_nonoverlapping(from.cast::<u8>(), to.cast::<u8>(), size);
        }
        return 0;
    }
    // SAFETY: `mm` is non-null and valid.
    unsafe { mmap_read_lock(mm) };
    // SAFETY: caller guarantees `to` is a writable kernel buffer of `size` bytes; reading through native address space.
    let result = unsafe {
        (*mm).space.read(
            from as usize,
            core::slice::from_raw_parts_mut(to.cast(), size),
        )
    };
    // SAFETY: releases the mmap read lock previously acquired on `mm`.
    unsafe { mmap_read_unlock(mm) };
    if result { 0 } else { size }
}

/// Copy kernel bytes to the current task's user address space.
///
/// # Safety
/// For nonzero `size`, `from` must point to `size` readable bytes of kernel
/// memory and must not overlap the user destination range. `to` is interpreted as an address in
/// the current task's user address space; when no user MM is bound, it must
/// instead be directly writable for `size` bytes. The source bytes must remain
/// readable until the call returns.
pub unsafe fn copy_to_user(to: *mut c_void, from: *const c_void, size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    let mm_ref = match current_user_mm() {
        UserMm::Live(mm) => Some(mm),
        // A dying task's user pages may be shared copy-on-write with its
        // parent; never write or read them through the kernel mapping.
        UserMm::Dead => return size,
        UserMm::Kernel => None,
    };
    let mm = mm_ptr(&mm_ref);
    if mm.is_null() {
        if from.is_null()
            || to.is_null()
            || (to as usize) < 4096
            || !kernel_context_may_access(to as usize)
        {
            return size;
        }
        // SAFETY: caller guarantees non-null kernel buffers and valid writable memory when no MM is bound.
        unsafe {
            core::ptr::copy_nonoverlapping(from.cast::<u8>(), to.cast::<u8>(), size);
        }
        return 0;
    }
    // SAFETY: `mm` is non-null and valid.
    unsafe { mmap_read_lock(mm) };
    // SAFETY: caller guarantees `from` is a readable kernel buffer of `size` bytes; writing through native address space.
    let result = unsafe {
        (*mm)
            .space
            .write(to as usize, core::slice::from_raw_parts(from.cast(), size))
    };
    // SAFETY: releases the mmap read lock previously acquired on `mm`.
    unsafe { mmap_read_unlock(mm) };
    if result { 0 } else { size }
}

/// Linux `copy_from_user()` ABI entry; see [`copy_from_user`].
///
/// # Safety
/// The caller must satisfy the pointer and range requirements of
/// [`copy_from_user`].
#[unsafe(export_name = "copy_from_user")]
pub unsafe extern "C" fn copy_from_user_c(to: *mut c_void, from: *const c_void, n: usize) -> usize {
    // SAFETY: caller guarantees valid arguments per `copy_from_user` preconditions.
    unsafe { copy_from_user(to, from, n) }
}

/// Linux `copy_to_user()` ABI entry; see [`copy_to_user`].
///
/// # Safety
/// The caller must satisfy the pointer and range requirements of
/// [`copy_to_user`].
#[unsafe(export_name = "copy_to_user")]
pub unsafe extern "C" fn copy_to_user_c(to: *mut c_void, from: *const c_void, n: usize) -> usize {
    // SAFETY: caller guarantees valid arguments per `copy_to_user` preconditions.
    unsafe { copy_to_user(to, from, n) }
}

/// Linux `__copy_to_user()`: the caller has already performed `access_ok()`;
/// the native backend still performs the checked address-space write.
///
/// # Safety
/// For nonzero `n`, `from` must point to `n` readable kernel bytes that do not
/// overlap the user destination. `to` must identify the current task's user destination; when
/// no user MM is bound it must instead be directly writable for `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __copy_to_user(to: *mut c_void, from: *const c_void, n: usize) -> usize {
    // SAFETY: caller guarantees valid arguments per `copy_to_user` preconditions.
    unsafe { copy_to_user(to, from, n) }
}

/// Linux `__copy_to_user_inatomic()`: never sleeps or faults in. If the
/// address space lock is contended the whole copy is reported as uncopied,
/// which is the value Linux's inatomic callers already retry on.
///
/// # Safety
/// For nonzero `n`, `from` must point to `n` readable kernel bytes and must not
/// overlap the user destination. The source must remain stable for the call. `to` is interpreted
/// in the current task's address space; if the MM is absent, the operation
/// reports all bytes uncopied without accessing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __copy_to_user_inatomic(
    to: *mut c_void,
    from: *const c_void,
    n: usize,
) -> usize {
    if n == 0 {
        return 0;
    }
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    if mm.is_null() || !mmap_read_trylock(mm) {
        return n;
    }
    // SAFETY: caller guarantees `from` is readable for `n` bytes; `mm` space is protected by held trylock.
    let result = unsafe {
        (*mm)
            .space
            .write(to as usize, core::slice::from_raw_parts(from.cast(), n))
    };
    // SAFETY: releases the mmap read lock acquired via mmap_read_trylock.
    unsafe { mmap_read_unlock(mm) };
    if result { 0 } else { n }
}

/// Linux `copy_from_user_inatomic_nontemporal()`. The non-temporal store hint
/// only selects the cache allocation policy of the destination writes; the
/// copied bytes and the uncopied-byte result are those of the inatomic copy.
///
/// # Safety
/// The caller must satisfy the pointer and range requirements of
/// [`__copy_from_user_inatomic`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn copy_from_user_inatomic_nontemporal(
    to: *mut c_void,
    from: *const c_void,
    n: usize,
) -> usize {
    // SAFETY: `to`, `from` and `n` are forwarded unchanged; the `# Safety` section above states the same pointer and range requirements as `__copy_from_user_inatomic`.
    unsafe { __copy_from_user_inatomic(to, from, n) }
}

/// Linux `access_ok()` on x86_64: a user range is admissible when it does not
/// wrap and ends at or below `TASK_SIZE_MAX`. This only range-checks the
/// pointer; the copy itself is still checked page by page by the core mm.
///
/// # Safety
/// `addr` is inspected only as an integer and is never dereferenced, so it need
/// not point to mapped memory. The caller must treat the result only as a range
/// check; it does not establish that the memory is mapped or accessible.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn access_ok(addr: *const c_void, size: u64) -> bool {
    const TASK_SIZE_MAX: u64 = 0x0000_7fff_ffff_f000;
    let addr = addr as u64;
    addr <= TASK_SIZE_MAX && size <= TASK_SIZE_MAX - addr
}

/// Linux usercopy ABI. The native MM implementation walks the current
/// process's checked address space directly and returns the uncopied byte
/// count, rather than dereferencing an unchecked userspace virtual address.
///
/// # Safety
/// The caller must satisfy the pointer and range requirements of
/// [`copy_from_user`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __copy_from_user(
    to: *mut c_void,
    from: *const c_void,
    size: usize,
) -> usize {
    // SAFETY: caller guarantees valid arguments per `copy_from_user` preconditions.
    unsafe { copy_from_user(to, from, size) }
}

/// In the native-MM backend page faults are explicit address-space lookups;
/// this entry therefore has the same checked-copy semantics and never invokes
/// a hardware fault handler from the caller's atomic relocation pass.
///
/// # Safety
/// `to` must point to `size` writable kernel bytes that do not overlap the user
/// source range. The source is interpreted in the current task's address
/// space; an invalid or inaccessible source is reported as uncopied bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __copy_from_user_inatomic(
    to: *mut c_void,
    from: *const c_void,
    size: usize,
) -> usize {
    if size == 0 {
        return 0;
    }
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    if !mmap_read_trylock(mm) {
        return size;
    }
    // SAFETY: caller guarantees `to` is writable for `size` bytes; `mm` space is protected by held trylock.
    let result = unsafe {
        (*mm).space.read(
            from as usize,
            core::slice::from_raw_parts_mut(to.cast(), size),
        )
    };
    // SAFETY: releases the mmap read lock acquired via mmap_read_trylock.
    unsafe { mmap_read_unlock(mm) };
    if result { 0 } else { size }
}

/// A pin record: the `Page` references handed out by one
/// `pin_user_pages_fast` call, and the core owner that keeps the frames pinned
/// until the matching `unpin_user_pages` call.
struct PinRecord {
    pages: Vec<usize>,
    _hold: Box<dyn Any + Send>,
}

static PIN_RECORDS: spin::Mutex<Vec<PinRecord>> = spin::Mutex::new(Vec::new());

/// Linux `pin_user_pages_fast(start, nr_pages, gup_flags, pages)`. The range
/// is faulted in and pinned by the core long-term pin owner of the calling
/// process (writable under `FOLL_WRITE`, which breaks COW first). Each page
/// receives one `Page` reference, and the pin lasts until
/// `unpin_user_pages` drops those references. Pinning is all-or-nothing: a
/// failure returns a negative errno and pins nothing.
///
/// # Safety
/// For positive `nr_pages`, a non-null `pages` must point to an aligned,
/// writable array of at least `nr_pages` page pointers, exclusively available
/// for output during this call; null is rejected. On success, preserve the page
/// references and array contents as the exact list passed to one matching
/// [`unpin_user_pages_c`] call; do not unpin or mutate them in the meantime.
/// `start` is interpreted in the current task's address space and must refer to
/// the range the caller intends to pin (inaccessible pages are reported as an
/// error). On failure, ignore any contents left in the output array.
#[unsafe(export_name = "pin_user_pages_fast")]
pub unsafe extern "C" fn pin_user_pages_fast_c(
    start: c_ulong,
    nr_pages: i32,
    gup_flags: u32,
    pages: *mut *mut Page,
) -> i32 {
    const FOLL_WRITE: u32 = 1 << 0;
    if nr_pages <= 0 {
        return 0;
    }
    if pages.is_null() {
        return -crate::config::EINVAL;
    }
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    if mm.is_null() {
        return -crate::config::EFAULT;
    }
    let count = nr_pages as usize;
    let base = (start as usize) & !(PAGE_SIZE - 1);
    let Some(size) = count.checked_mul(PAGE_SIZE) else {
        return -crate::config::EFAULT;
    };
    // SAFETY: `mm` is non-null and points to a valid live `MmStruct`.
    let pinned = unsafe { (*mm).space.pin(base, size, gup_flags & FOLL_WRITE != 0) };
    let PinnedFrames { frames, hold } = match pinned {
        Ok(pinned) => pinned,
        Err(err) => return -err,
    };
    let mut refs = Vec::new();
    if refs.try_reserve_exact(count).is_err() {
        return -crate::config::ENOMEM;
    }
    for (index, paddr) in frames.iter().copied().enumerate() {
        // SAFETY: `paddr` is a valid physical address returned by a successful `space.pin()` invocation.
        let page = unsafe { shmem::pin_physical_frame(paddr) };
        if page.is_null() {
            for taken in refs {
                // SAFETY: `taken` is a non-null `Page` pointer previously returned by `pin_physical_frame`.
                unsafe { shmem::put_page(taken as *mut Page) };
            }
            return -crate::config::ENOMEM;
        }
        refs.push(page as usize);
        // SAFETY: caller guarantees `pages` points to a writable array of at least `nr_pages` elements; `index < count`.
        unsafe { pages.add(index).write(page) };
    }
    PIN_RECORDS.lock().push(PinRecord {
        pages: refs,
        _hold: hold,
    });
    count as i32
}

/// Linux `unpin_user_pages(pages, npages)`: drops the references taken by
/// [`pin_user_pages_fast_c`] and releases the core pin of that call.
///
/// # Safety
/// For positive `npages`, a non-null `pages` must point to an aligned, readable
/// array of `npages` page pointers that is not mutated during this call; null
/// is ignored. The array must exactly match the output of one successful, still-live
/// [`pin_user_pages_fast_c`] call; it may be unpinned only once, and the array
/// must remain valid until this function returns.
#[unsafe(export_name = "unpin_user_pages")]
pub unsafe extern "C" fn unpin_user_pages_c(pages: *mut *mut Page, npages: c_ulong) {
    let count = npages as usize;
    if pages.is_null() || count == 0 {
        return;
    }
    // SAFETY: caller guarantees `pages` points to a readable array of `count` valid `Page` pointers.
    let slice = unsafe { core::slice::from_raw_parts(pages, count) };
    let refs: Vec<usize> = slice.iter().map(|page| *page as usize).collect();
    // Release the record first: its pin is the one that keeps these frames
    // from being reused while the references are being dropped.
    let record = {
        let mut records = PIN_RECORDS.lock();
        records
            .iter()
            .position(|record| record.pages == refs)
            .map(|index| records.swap_remove(index))
    };
    for page in refs {
        if page != 0 {
            // SAFETY: `page` was recorded from a successful `pin_user_pages_fast_c` call and represents an active `Page` reference.
            unsafe { shmem::put_page(page as *mut Page) };
        }
    }
    drop(record);
}

/// Linux `mmu_interval_notifier_insert()`: subscribe `notifier` to
/// invalidations of `[start, start + length)` in `mm`. The subscription lives in
/// the core notifier domain of the mm's address space.
///
/// # Safety
/// `notifier` must be a valid, writable, properly initialized record that is
/// not already inserted, and it must remain exclusively owned and alive until
/// [`mmu_interval_notifier_remove`] returns. `mm` must be a live `MmStruct`
/// pointer with an existing `Arc` strong reference (normally from
/// [`current_mm`]); keep that owner alive until this function has incremented
/// its reference. `ops` must point to a valid notifier-ops table that remains
/// alive and unchanged until removal completes, because invalidation callbacks
/// may read it asynchronously with respect to the caller.
#[unsafe(export_name = "mmu_interval_notifier_insert")]
pub unsafe extern "C" fn mmu_interval_notifier_insert(
    notifier: *mut MmuIntervalNotifier,
    mm: *mut MmStruct,
    start: c_ulong,
    length: c_ulong,
    ops: *const MmuIntervalNotifierOps,
) -> c_int {
    if notifier.is_null() || mm.is_null() || ops.is_null() || length == 0 {
        return -crate::config::EINVAL;
    }
    let Some(last) = (start as usize).checked_add(length as usize - 1) else {
        return -crate::config::EINVAL;
    };
    // SAFETY: caller guarantees `notifier` is writable and exclusively owned, `ops` is a valid ops table,
    // and `mm` is a live `MmStruct`.
    unsafe {
        (*notifier).interval_tree.start = start as usize;
        (*notifier).interval_tree.last = last;
        (*notifier).interval_tree.subtree_last = last;
        (*notifier).ops = ops;
        (*notifier).mm = mm;
        let subscriber = alloc::sync::Arc::new(IntervalSubscriber { notifier });
        match mmu_notifier::subscribe(
            (*mm).aspace_key,
            notifier as usize,
            start as usize,
            last,
            subscriber,
        ) {
            Ok(()) => {
                // mmgrab(): the record keeps its MM alive until remove.
                Arc::increment_strong_count(mm.cast_const());
                let owner = Arc::from_raw(mm.cast_const());
                NOTIFIER_OWNERS.lock().insert(notifier as usize, owner);
                0
            }
            Err(_) => {
                (*notifier).mm = ptr::null_mut();
                -crate::config::EINVAL
            }
        }
    }
}

/// Linux `mmu_interval_notifier_remove()`. Invalidations are synchronous in
/// this owner, so no callback can still be running when the subscription is
/// dropped.
///
/// # Safety
/// `notifier` must point to a live record successfully inserted by
/// [`mmu_interval_notifier_insert`] and not yet removed. No other thread may
/// remove, reuse, or free the record until this call returns; its MM owner and
/// ops table must remain valid while any in-flight callback is drained.
#[unsafe(export_name = "mmu_interval_notifier_remove")]
pub unsafe extern "C" fn mmu_interval_notifier_remove(notifier: *mut MmuIntervalNotifier) {
    if notifier.is_null() {
        return;
    }
    // SAFETY: caller guarantees `notifier` is a live record inserted by `mmu_interval_notifier_insert`.
    let mm = unsafe { (*notifier).mm };
    if mm.is_null() {
        return;
    }
    // SAFETY: `mm` is live and valid; unregistering `notifier` from the notifier domain and clearing its `mm` pointer.
    unsafe {
        mmu_notifier::unsubscribe((*mm).aspace_key, notifier as usize);
        (*notifier).mm = ptr::null_mut();
    }
    // mmdrop(): release the record's MM reference after its subscription ends.
    let owner = NOTIFIER_OWNERS.lock().remove(&(notifier as usize));
    drop(owner);
}

/// Linux `mmu_interval_read_begin()`: the sequence a reader samples before it
/// pins pages. A later invalidation stores a different value, which
/// `mmu_interval_read_retry` detects.
///
/// # Safety
/// `notifier` must point to a live, successfully inserted notifier whose MM
/// owner has not been released. Do not remove or free it concurrently with this
/// call; concurrent invalidation is supported by the notifier protocol.
#[unsafe(export_name = "mmu_interval_read_begin")]
pub unsafe extern "C" fn mmu_interval_read_begin(notifier: *mut MmuIntervalNotifier) -> c_ulong {
    // SAFETY: caller guarantees `notifier` is a live, valid `MmuIntervalNotifier`.
    let mm = unsafe { (*notifier).mm };
    let subscriber = IntervalSubscriber { notifier };
    if mm.is_null() {
        return subscriber.seq() as c_ulong;
    }
    // SAFETY: `mm` is non-null and points to a valid live `MmStruct`.
    mmu_notifier::read_begin(unsafe { (*mm).aspace_key }, &subscriber) as c_ulong
}

/// `get_user()` / `__get_user()` for one typed value.
///
/// # Safety
/// For non-zero-sized `T`, `from` must designate a readable `T` in the calling
/// task's user address space; if no user MM is bound, it must instead be
/// directly readable for `size_of::<T>()` bytes. The complete source
/// representation must be valid for `T`, because a checked copy can write a
/// prefix before reporting a fault.
/// The source must not alias `value` in a way that violates its exclusive
/// mutable borrow.
pub unsafe fn __get_user<T: Copy>(value: &mut T, from: *const T) -> c_int {
    // SAFETY: `value` is an exclusive mutable reference to kernel storage of `size_of::<T>()` bytes,
    // and `from` is guaranteed readable by the caller.
    if unsafe { copy_from_user((value as *mut T).cast(), from.cast(), size_of::<T>()) } == 0 {
        0
    } else {
        -crate::config::EFAULT
    }
}

/// `put_user()` / `__put_user()` for one typed value.
///
/// # Safety
/// For non-zero-sized `T`, `to` must designate writable storage for a `T` in
/// the calling task's user address space; if no user MM is bound, it must
/// instead be directly writable for `size_of::<T>()` bytes. The destination
/// must not overlap the local `value` storage in the direct-copy fallback.
pub unsafe fn __put_user<T: Copy>(value: T, to: *mut T) -> c_int {
    // SAFETY: `value` is a stack copy of `T`, and `to` is guaranteed writable by caller for `size_of::<T>()` bytes.
    if unsafe { copy_to_user(to.cast(), core::ptr::addr_of!(value).cast(), size_of::<T>()) } == 0 {
        0
    } else {
        -crate::config::EFAULT
    }
}

/// Inatomic `unsafe_put_user()` is valid only inside a successful user-access
/// session. It verifies the saved session bounds and writes through the native
/// page tables directly, without taking the mmap lock or yielding.
///
/// # Safety
/// For non-zero-sized `T`, the current task must still own a successful
/// `user_access_begin()` session covering the complete `T` at `to`;
/// `user_access_end()` must not run until
/// this call returns. The stored bytes must be a valid initialized
/// representation of `T`, and the caller must keep the task/MM and session
/// alive throughout the write.
pub unsafe fn unsafe_put_user<T: Copy>(value: T, to: *mut T) -> c_int {
    let size = size_of::<T>();
    if size == 0 {
        return 0;
    }
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    let Some(task_id) = current_task_id() else {
        return -crate::config::EFAULT;
    };
    if mm.is_null() {
        return -crate::config::EFAULT;
    }
    let address = to as usize;
    let Some(end) = address.checked_add(size) else {
        return -crate::config::EFAULT;
    };
    // SAFETY: `mm` is non-null (checked above) and live per this function's `# Safety`; `user_access_owner` is an atomic field, so the load is race-free.
    let owner = unsafe { (*mm).user_access_owner.load(Ordering::Acquire) };
    // SAFETY: `mm` is the same live manager read above; `user_access_start` is an atomic field, so this load is race-free.
    let start = unsafe { (*mm).user_access_start.load(Ordering::Relaxed) };
    // SAFETY: `mm` is the same live manager read above; `user_access_end` is an atomic field, so this load is race-free.
    let session_end = unsafe { (*mm).user_access_end.load(Ordering::Relaxed) };
    if owner != task_id || address < start || end > session_end {
        return -crate::config::EFAULT;
    }
    // SAFETY: address and size have been verified to fall within the active session bounds; `mm` space is live.
    let result = unsafe {
        (*mm).space.write(
            address,
            core::slice::from_raw_parts(ptr::addr_of!(value).cast::<u8>(), size),
        )
    };
    if result { 0 } else { -crate::config::EFAULT }
}

/// Check every leaf covered by the complete byte range, not only its first
/// address. `query()` resolves large-page leaves too, so stepping by the
/// native base page safely covers the range independent of leaf size.
fn user_write_range_mapped(mm: *mut MmStruct, address: usize, size: usize) -> bool {
    if mm.is_null() || size == 0 {
        return size == 0 && !mm.is_null();
    }
    let Some(last_byte) = address.checked_add(size - 1) else {
        return false;
    };
    // SAFETY: `mm` is checked non-null and points to a valid live `MmStruct`.
    let space = unsafe { &(*mm).space };
    if !space.contains_range(address, size) {
        return false;
    }
    let page_size = crate::config::PAGE_SIZE;
    let mut page = address & !(page_size - 1);
    let last_page = last_byte & !(page_size - 1);
    loop {
        let Some((_, flags)) = space.query(page) else {
            return false;
        };
        if !flags.contains(VmFlags::WRITE) {
            return false;
        }
        if page == last_page {
            return true;
        }
        let Some(next) = page.checked_add(page_size) else {
            return false;
        };
        page = next;
    }
}

fn current_task_id() -> Option<u64> {
    crate::kernel_services::current_task_id()
}

/// Begin a native-MM user write region without sleeping. The mmap lock remains
/// held until the matching end call, preventing unmap/protection changes while
/// the inatomic scalar writes are performed. A contended lock, absent task/MM,
/// non-writable leaf, or invalid byte range fails closed.
pub fn user_access_begin(address: *const c_void, size: usize) -> bool {
    if size == 0 {
        return true;
    }
    if address.is_null() {
        return false;
    }
    let Some(session_end) = (address as usize).checked_add(size) else {
        return false;
    };
    let Some(task_id) = current_task_id() else {
        return false;
    };
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    // SAFETY: `mm` is checked non-null and points to a valid live `MmStruct`.
    if mm.is_null() || unsafe { (*mm).user_access_owner.load(Ordering::Acquire) != u64::MAX } {
        return false;
    }
    if !mmap_read_trylock(mm) {
        return false;
    }
    if !user_write_range_mapped(mm, address as usize, size) {
        // SAFETY: releases the mmap read lock acquired via mmap_read_trylock.
        unsafe { mmap_read_unlock(mm) };
        return false;
    }
    // SAFETY: `mm` is valid and live, `mmap_read_trylock` succeeded, initializing session fields.
    unsafe {
        (*mm)
            .user_access_start
            .store(address as usize, Ordering::Relaxed);
        (*mm).user_access_end.store(session_end, Ordering::Relaxed);
        (*mm).user_access_owner.store(task_id, Ordering::Release);
    }
    true
}

/// Linux defines `user_write_access_begin()` as `user_access_begin()` on the
/// supported x86 configuration; the native-MM adapter uses the same checked,
/// nonblocking writeable-range session for both names.
pub fn user_write_access_begin(address: *const c_void, size: usize) -> bool {
    user_access_begin(address, size)
}

/// Close the current task's validated user-access session and release the
/// mmap lock. Zero-length sessions are no-ops, matching the begin helper.
pub fn user_access_end() {
    let Some(task_id) = current_task_id() else {
        return;
    };
    let mm_ref = current_mm_ref();
    let mm = mm_ptr(&mm_ref);
    if mm.is_null() {
        return;
    }
    // SAFETY: `mm` is checked non-null and points to a valid live `MmStruct`.
    let owner = unsafe { (*mm).user_access_owner.load(Ordering::Acquire) };
    if owner == u64::MAX {
        return;
    }
    assert_eq!(owner, task_id, "user access session ended by another task");
    // SAFETY: `mm` is valid and live, owner was verified to match `current_task_id()`, resetting session and unlocking.
    unsafe {
        (*mm).user_access_owner.store(u64::MAX, Ordering::Release);
        (*mm).user_access_start.store(0, Ordering::Relaxed);
        (*mm).user_access_end.store(0, Ordering::Relaxed);
        mmap_read_unlock(mm);
    }
}

/// Alias matching Linux `user_write_access_end()` on the native backend.
pub fn user_write_access_end() {
    user_access_end();
}

/// The native usercopy path does explicit page-table lookup instead of fault
/// recovery; these scope markers do not need to alter CPU fault state.
pub fn pagefault_disable() {}
pub fn pagefault_enable() {}

/// Linux u64 pointer conversion for internal, non-user ABI pointers.
pub fn u64_to_ptr<T>(address: u64) -> *mut T {
    address as usize as *mut T
}

#[cfg(test)]
mod usercopy_range_tests {
    use super::*;

    #[test]
    fn access_ok_bounds_the_user_address_space() {
        let top = 0x0000_7fff_ffff_f000usize as *const c_void;
        // SAFETY: access_ok is a pure address range check and does not dereference pointers.
        unsafe {
            assert!(access_ok(0x1000 as *const c_void, 0x1000));
            assert!(access_ok(top, 0));
            assert!(!access_ok(top, 1));
            assert!(!access_ok(0x1000 as *const c_void, u64::MAX));
        }
    }
}

#[cfg(test)]
mod process_pin_tests {
    use alloc::{collections::BTreeMap, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Process address space with a fixed set of resident 4 KiB leaves.
    struct FakeSpace {
        leaves: BTreeMap<usize, usize>,
        pins: AtomicUsize,
        released: Arc<AtomicUsize>,
    }

    /// Stands for the core long-term pin owner: dropping it releases the pin.
    struct HoldProbe(Arc<AtomicUsize>);

    impl Drop for HoldProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl UserAddressSpace for FakeSpace {
        fn id(&self) -> u64 {
            7
        }
        fn alive(&self) -> bool {
            true
        }
        fn bounds(&self) -> (usize, usize) {
            (0x1000, 0x8000_0000)
        }
        fn contains_range(&self, _addr: usize, _size: usize) -> bool {
            true
        }
        fn find_free_area(&self, hint: usize, _size: usize) -> Option<usize> {
            Some(hint)
        }
        fn query(&self, addr: usize) -> Option<(usize, VmFlags)> {
            self.leaves
                .get(&addr)
                .map(|paddr| (*paddr, VmFlags::USER | VmFlags::READ | VmFlags::WRITE))
        }
        fn read(&self, _addr: usize, _dst: &mut [u8]) -> bool {
            true
        }
        fn write(&self, _addr: usize, _src: &[u8]) -> bool {
            true
        }
        fn map_linear(&self, _addr: usize, _paddr: usize, _size: usize, _flags: VmFlags) -> bool {
            true
        }
        fn unmap(&self, _addr: usize, _size: usize) -> bool {
            true
        }
        fn pin(&self, addr: usize, size: usize, _write: bool) -> Result<PinnedFrames, c_int> {
            let mut frames = Vec::new();
            for page in (addr..addr + size).step_by(PAGE_SIZE) {
                frames.push(*self.leaves.get(&page).ok_or(-crate::config::EFAULT)?);
            }
            self.pins.fetch_add(1, Ordering::AcqRel);
            Ok(PinnedFrames {
                frames,
                hold: Box::new(HoldProbe(self.released.clone())),
            })
        }
    }

    static FAKE: spin::Once<Arc<FakeSpace>> = spin::Once::new();

    fn provider() -> Option<Arc<dyn UserAddressSpace>> {
        let space = FAKE.get()?.clone();
        Some(space)
    }

    #[test]
    fn pin_user_pages_fast_returns_the_mapped_frames_until_unpinned() {
        FAKE.call_once(|| {
            let mut leaves = BTreeMap::new();
            leaves.insert(0x10000, 0x80000);
            leaves.insert(0x11000, 0x90000);
            Arc::new(FakeSpace {
                leaves,
                pins: AtomicUsize::new(0),
                released: Arc::new(AtomicUsize::new(0)),
            })
        });
        let space = FAKE.get().unwrap().clone();
        // Installed once for this test binary; this is the only installer.
        let _ = install_current_user_address_space(provider);

        let mut pages: [*mut Page; 2] = [ptr::null_mut(); 2];
        // SAFETY: pages is a 2-element array allocated on the stack with matching length 2.
        let pinned = unsafe { pin_user_pages_fast_c(0x10000, 2, 1, pages.as_mut_ptr()) };
        assert_eq!(pinned, 2);
        let frames: Vec<usize> = pages
            .iter()
            // SAFETY: each `page` is a slot that `pin_user_pages_fast_c` filled above (asserted to return 2), so it is a valid page pointer.
            .map(|page| unsafe { shmem::page_to_phys(*page) })
            .collect();
        assert_eq!(frames, [0x80000, 0x90000]);
        assert_eq!(space.pins.load(Ordering::Acquire), 1);
        assert_eq!(space.released.load(Ordering::Acquire), 0);

        // SAFETY: pages contains the exact 2 pages pinned above; releasing them once.
        unsafe { unpin_user_pages_c(pages.as_mut_ptr(), 2) };
        assert_eq!(space.released.load(Ordering::Acquire), 1);
    }
}

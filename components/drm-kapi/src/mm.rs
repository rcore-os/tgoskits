// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Source-layout Linux MM callback records used by the DRM mmap translation.

use core::ffi::{c_char, c_int, c_ulong, c_void};

use crate::{
    gem_memory::PgProt,
    types::{File, Page},
};

/// Linux 7.2.3 `vm_area_struct` fields consumed by the DRM translations.
/// Offsets are from the x86_64 reference build (including
/// CONFIG_PER_VMA_LOCK, CONFIG_SWAP, and CONFIG_NUMA). Unused source fields
/// remain opaque bytes; this record does not model their behavior.
#[repr(C, align(64))]
pub struct VmAreaStruct {
    pub vm_start: c_ulong,
    pub vm_end: c_ulong,
    pub vm_mm: *mut c_void,
    pub vm_page_prot: PgProt,
    pub vm_flags: c_ulong,
    pub vm_lock_seq: u32,
    _vm_lock_seq_pad: u32,
    _anon_vma_chain: [u8; 16],
    _anon_vma: *mut c_void,
    pub vm_ops: *const VmOperationsStruct,
    pub vm_pgoff: c_ulong,
    pub vm_file: *mut File,
    pub vm_private_data: *mut c_void,
    _swap_readahead_info: c_ulong,
    _vm_policy: *mut c_void,
    _rest: [u8; 72],
}

impl VmAreaStruct {
    /// Zero-initialize the target's source-layout VMA record before filling
    /// the fields that a translated callback consumes.
    ///
    /// The structure contains only integers, pointers, and padding.
    pub const fn empty() -> Self {
        // `MaybeUninit::zeroed` is not const on every supported toolchain.
        // Keep initialization in the small unsafe constructor instead.
        // SAFETY: All fields of `VmAreaStruct` are primitive integers, raw pointers, or byte arrays,
        // for which an all-zero bit pattern represents a valid initial unlinked/zero state.
        unsafe { core::mem::zeroed() }
    }
}

#[repr(C)]
pub struct VmFault {
    pub vma: *mut VmAreaStruct,
    pub gfp_mask: u32,
    _gfp_pad: u32,
    pub pgoff: c_ulong,
    pub address: c_ulong,
    pub real_address: c_ulong,
    pub flags: u32,
    _flags_pad: u32,
    _pmd: *mut c_void,
    _pud: *mut c_void,
    _orig_pte_or_pmd: u64,
    _cow_page: *mut c_void,
    _page: *mut c_void,
    _pte: *mut c_void,
    _ptl: *mut c_void,
    _prealloc_pte: *mut c_void,
}

impl VmFault {
    /// Zero-initialize the target's source-layout fault record.
    pub const fn empty() -> Self {
        // SAFETY: All fields of `VmFault` are primitive integers or raw pointers,
        // for which an all-zero bit pattern represents a valid empty state.
        unsafe { core::mem::zeroed() }
    }
}

/// Read the page returned by a LinuxKPI `vm_operations_struct` fault callback.
///
/// # Safety
/// `fault` must point to a live `VmFault` for the duration of the call.
#[inline]
pub unsafe fn vm_fault_page(fault: *const VmFault) -> *mut Page {
    // SAFETY: Per function safety contract, `fault` points to a live, properly aligned `VmFault`.
    unsafe { (*fault)._page.cast() }
}

/// Set the page returned by a LinuxKPI `vm_operations_struct` fault callback.
///
/// # Safety
/// `fault` must point to a live writable `VmFault`; `page` must be a live
/// LinuxKPI page reference for the duration required by the caller.
#[inline]
pub unsafe fn vm_fault_set_page(fault: *mut VmFault, page: *mut Page) {
    // SAFETY: Per function safety contract, `fault` points to a live, writable, properly aligned `VmFault`.
    unsafe { (*fault)._page = page.cast() };
}

// `vm_fault_t` result bits from include/linux/mm_types.h.
pub const VM_FAULT_OOM: u32 = 0x000001;
pub const VM_FAULT_SIGBUS: u32 = 0x000002;
pub const VM_FAULT_NOPAGE: u32 = 0x000100;
/// `VM_FAULT_LOCKED`: the callback returned with `vmf->page` locked.
pub const VM_FAULT_LOCKED: u32 = 0x000200;
/// `VM_FAULT_FALLBACK`: retry this huge fault as base-page faults.
pub const VM_FAULT_FALLBACK: u32 = 0x000800;
/// `FAULT_FLAG_WRITE` in Linux's `vm_fault::flags`.
pub const FAULT_FLAG_WRITE: u32 = 0x000001;
/// `FAULT_FLAG_MKWRITE` in Linux's `vm_fault::flags`.
pub const FAULT_FLAG_MKWRITE: u32 = 0x000004;

// Linux `vm_flags` values from include/linux/mm.h for this CONFIG_MMU build.
pub const VM_WRITE: c_ulong = 1 << 1;
pub const VM_PFNMAP: c_ulong = 1 << 10;
pub const VM_IO: c_ulong = 1 << 14;
pub const VM_DONTEXPAND: c_ulong = 1 << 18;
pub const VM_DONTDUMP: c_ulong = 1 << 26;
pub const VM_MIXEDMAP: c_ulong = 1 << 28;
pub const VM_MAYWRITE: c_ulong = 1 << 5;

/// Linux `vma_flags_is_cow_mapping()`: private mappings that may be writable
/// are CoW mappings; MAP_SHARED mappings are not.
#[inline]
pub const fn is_cow_mapping(vm_flags: c_ulong) -> bool {
    vm_flags & VM_MAYWRITE != 0 && vm_flags & VM_SHARED == 0
}

/// Linux `VM_READ`, `VM_EXEC`, `VM_SHARED` bits from include/linux/mm.h.
pub const VM_READ: c_ulong = 1 << 0;
pub const VM_EXEC: c_ulong = 1 << 2;
pub const VM_SHARED: c_ulong = 1 << 3;

// x86 `_PAGE_PRESENT | _PAGE_USER | _PAGE_RW | _PAGE_NX` leaf bits.
const PTE_PRESENT: c_ulong = 1 << 0;
const PTE_RW: c_ulong = 1 << 1;
const PTE_USER: c_ulong = 1 << 2;
const PTE_NX: c_ulong = 1 << 63;

/// Linux `vm_get_page_prot(vm_flags)` (`protection_map[]` on x86_64). A
/// private writable mapping is copy-on-write and therefore read-only in its
/// PTEs, as in Linux; a mapping with no access bits is not present.
///
/// # Safety
/// Pure function of `vm_flags`; the result is a leaf protection value.
#[unsafe(export_name = "vm_get_page_prot")]
pub unsafe extern "C" fn vm_get_page_prot(vm_flags: c_ulong) -> PgProt {
    let access = VM_READ | VM_EXEC | VM_WRITE;
    if vm_flags & access == 0 {
        return PgProt { pgprot: 0 };
    }
    let writable = vm_flags & VM_WRITE != 0 && vm_flags & VM_SHARED != 0;
    let mut bits = PTE_PRESENT | PTE_USER;
    if writable {
        bits |= PTE_RW;
    }
    if vm_flags & VM_EXEC == 0 {
        bits |= PTE_NX;
    }
    PgProt { pgprot: bits }
}

/// Linux `compat_vma_mmap(file, vma)`: dispatches to `file->f_op->mmap`, and
/// returns `-ENODEV` when the file has none. No file type in this kernel
/// carries an mmap operation (the shmem and GEM `FileOperations` tables hold
/// only release/write hooks), so every file reports `-ENODEV`, which is the
/// value Linux returns for such a file.
///
/// # Safety
/// `file` and `vma` must be live objects.
#[unsafe(export_name = "compat_vma_mmap")]
pub unsafe extern "C" fn compat_vma_mmap(file: *mut File, vma: *mut VmAreaStruct) -> c_int {
    assert!(
        !file.is_null() && !vma.is_null(),
        "compat_vma_mmap received NULL"
    );
    -crate::config::ENODEV
}

/// Linux `vma_set_file(vma, file)`: takes a reference on the new file, swaps
/// it into the VMA, and drops the reference the VMA held on the old one.
///
/// # Safety
/// `vma` must be a live VMA and `file` a live file.
#[unsafe(export_name = "vma_set_file")]
pub unsafe extern "C" fn vma_set_file(vma: *mut VmAreaStruct, file: *mut File) {
    // SAFETY: Per function safety contract, `file` is a live `File` pointer.
    let file = unsafe { crate::shmem::get_file_file(file.cast()) };
    // SAFETY: Per function safety contract, `vma` points to a live, writable `VmAreaStruct`.
    let old = unsafe { core::mem::replace(&mut (*vma).vm_file, file.cast()) };
    if !old.is_null() {
        // SAFETY: `old` was the previous non-null file pointer held by `vma`, so dropping its reference via fput is safe.
        unsafe { crate::shmem::fput_file(old.cast()) };
    }
}

/// Linux `vma_pages()`: the page-count of the page-aligned VMA interval.
///
/// # Safety
/// `vma` must point to a live initialized VMA whose start and end describe a
/// valid page-aligned interval.
#[inline]
pub unsafe fn vma_pages(vma: *const VmAreaStruct) -> c_ulong {
    // SAFETY: Per function safety contract, `vma` points to a live, initialized `VmAreaStruct`.
    unsafe { ((*vma).vm_end - (*vma).vm_start) >> crate::config::PAGE_SHIFT }
}

// Linux 7.2.3 include/linux/mmap_lock.h uses this helper when
// CONFIG_PER_VMA_LOCK=y (the configured reference target).

/// Linux `__vma_start_write(vma, state)`: waits for per-VMA readers, then
/// write-locks the VMA. The hosting kernel has no per-VMA read locks: every VMA
/// mutation happens under the mm write lock, which already excludes every
/// reader, so there is nothing to wait for and the call succeeds at once.
///
/// # Safety
/// `vma` must be a live VMA of an mm whose write lock the caller holds.
#[unsafe(export_name = "__vma_start_write")]
pub unsafe extern "C" fn __vma_start_write(vma: *mut VmAreaStruct, _state: i32) -> i32 {
    assert!(!vma.is_null(), "__vma_start_write received a NULL VMA");
    0
}

#[inline]
/// Set flags on a live VMA while the caller holds its mm's write lock.
///
/// # Safety
/// `vma` must point to a live VMA whose mm write lock is held by the caller.
pub unsafe fn vm_flags_set(vma: *mut VmAreaStruct, flags: c_ulong) {
    // SAFETY: Caller guarantees `vma` is live and its mm write lock is held, ensuring exclusive write access.
    unsafe {
        __vma_start_write(vma, crate::wait::TASK_UNINTERRUPTIBLE as i32);
        (*vma).vm_flags |= flags;
    }
}

#[inline]
/// Clear flags on a live VMA while the caller holds its mm's write lock.
///
/// # Safety
/// `vma` must point to a live VMA whose mm write lock is held by the caller.
pub unsafe fn vm_flags_clear(vma: *mut VmAreaStruct, flags: c_ulong) {
    // SAFETY: Caller guarantees `vma` is live and its mm write lock is held, ensuring exclusive write access.
    unsafe {
        __vma_start_write(vma, crate::wait::TASK_UNINTERRUPTIBLE as i32);
        (*vma).vm_flags &= !flags;
    }
}

const _: [(); 192] = [(); core::mem::size_of::<VmAreaStruct>()];
const _: [(); 64] = [(); core::mem::align_of::<VmAreaStruct>()];
const _: [(); 40] = [(); core::mem::offset_of!(VmAreaStruct, vm_lock_seq)];
const _: [(); 80] = [(); core::mem::offset_of!(VmAreaStruct, vm_pgoff)];
const _: [(); 88] = [(); core::mem::offset_of!(VmAreaStruct, vm_file)];
const _: [(); 96] = [(); core::mem::offset_of!(VmAreaStruct, vm_private_data)];
const _: [(); 112] = [(); core::mem::offset_of!(VmAreaStruct, _vm_policy)];
const _: [(); 112] = [(); core::mem::size_of::<VmFault>()];
const _: [(); 0] = [(); core::mem::offset_of!(VmFault, vma)];
const _: [(); 24] = [(); core::mem::offset_of!(VmFault, address)];

/// Linux 7.2.3 `vm_operations_struct` callback table for the reference target.
/// The target's `auto.conf` has CONFIG_NUMA=y and CONFIG_USERFAULTFD=y, while
/// CONFIG_FIND_NORMAL_PAGE and CONFIG_NUMA_BALANCING are unset. Thus the
/// table has the core callbacks, NUMA policy callbacks, and the userfaultfd
/// tail pointer in this order.
#[repr(C)]
pub struct VmOperationsStruct {
    pub open: Option<unsafe extern "C" fn(*mut VmAreaStruct)>,
    pub close: Option<unsafe extern "C" fn(*mut VmAreaStruct)>,
    pub mapped: Option<
        unsafe extern "C" fn(usize, usize, c_ulong, *const c_void, *mut *mut c_void) -> c_int,
    >,
    pub may_split: Option<unsafe extern "C" fn(*mut VmAreaStruct, c_ulong) -> c_int>,
    pub mremap: Option<unsafe extern "C" fn(*mut VmAreaStruct) -> c_int>,
    pub mprotect:
        Option<unsafe extern "C" fn(*mut VmAreaStruct, c_ulong, c_ulong, c_ulong) -> c_int>,
    pub fault: Option<unsafe extern "C" fn(*mut VmFault) -> u32>,
    pub huge_fault: Option<unsafe extern "C" fn(*mut VmFault, u32) -> u32>,
    pub map_pages: Option<unsafe extern "C" fn(*mut VmFault, c_ulong, c_ulong) -> u32>,
    pub pagesize: Option<unsafe extern "C" fn(*mut VmAreaStruct) -> c_ulong>,
    pub page_mkwrite: Option<unsafe extern "C" fn(*mut VmFault) -> u32>,
    pub pfn_mkwrite: Option<unsafe extern "C" fn(*mut VmFault) -> u32>,
    pub access: Option<
        unsafe extern "C" fn(*mut VmAreaStruct, c_ulong, *mut c_void, c_int, c_int) -> c_int,
    >,
    pub name: Option<unsafe extern "C" fn(*mut VmAreaStruct) -> *const c_char>,
    pub set_policy: Option<unsafe extern "C" fn(*mut VmAreaStruct, *mut c_void) -> c_int>,
    pub get_policy:
        Option<unsafe extern "C" fn(*mut VmAreaStruct, c_ulong, *mut c_ulong) -> *mut c_void>,
    pub uffd_ops: *const c_void,
}

// SAFETY: `VmOperationsStruct` consists solely of immutable function pointers and an opaque
// static tail pointer. Instances are published as read-only kernel metadata and never mutated after initialization.
unsafe impl Sync for VmOperationsStruct {}

impl VmOperationsStruct {
    pub const EMPTY: Self = Self {
        open: None,
        close: None,
        mapped: None,
        may_split: None,
        mremap: None,
        mprotect: None,
        fault: None,
        huge_fault: None,
        map_pages: None,
        pagesize: None,
        page_mkwrite: None,
        pfn_mkwrite: None,
        access: None,
        name: None,
        set_policy: None,
        get_policy: None,
        uffd_ops: core::ptr::null(),
    };
}

const _: [(); 17 * core::mem::size_of::<usize>()] =
    [(); core::mem::size_of::<VmOperationsStruct>()];

/// The hosting kernel has no swap daemon; no native task has Linux's PF_KSWAPD role.
#[inline]
pub fn current_is_kswapd() -> bool {
    false
}

#[cfg(test)]
mod prot_tests {
    use super::*;

    #[test]
    fn protections_follow_protection_map_bits() {
        // SAFETY: Pure bitwise test exercising `vm_get_page_prot` with numeric test flags.
        unsafe {
            assert_eq!(vm_get_page_prot(0).pgprot, 0, "no access is not present");
            let read_only_private = vm_get_page_prot(VM_READ).pgprot;
            assert_eq!(read_only_private, PTE_PRESENT | PTE_USER | PTE_NX);
            let writable_private = vm_get_page_prot(VM_READ | VM_WRITE).pgprot;
            assert_eq!(
                writable_private & PTE_RW,
                0,
                "private writable mappings are COW"
            );
            let writable_shared = vm_get_page_prot(VM_READ | VM_WRITE | VM_SHARED).pgprot;
            assert_ne!(writable_shared & PTE_RW, 0);
            let executable = vm_get_page_prot(VM_READ | VM_EXEC).pgprot;
            assert_eq!(executable & PTE_NX, 0);
        }
    }
}

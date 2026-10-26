// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! GEM shared-memory memory management helpers.
//!
//! Provides memory-backed GEM buffer objects with lifecycle management,
//! page allocation from kernel shmem files, CPU and I/O memory mapping,
//! scatter-gather table construction, and PRIME buffer sharing.

use core::{
    ffi::{c_int, c_uint, c_void},
    ptr,
    sync::atomic::{AtomicI32, Ordering},
};

use drm_kapi::{
    assert::warn_on,
    config::{
        EINVAL, EIO, EOPNOTSUPP, EOVERFLOW, GFP_KERNEL, PAGE_SHIFT, PAGE_SIZE, is_err, ptr_err,
    },
    list::init_list_head,
    memory::{
        kfree, kmalloc, kzalloc_obj, refcount_dec_and_test, refcount_inc_not_zero, refcount_read,
        refcount_set,
    },
    mm::{
        VM_DONTDUMP, VM_DONTEXPAND, VM_FAULT_FALLBACK, VM_FAULT_NOPAGE, VM_FAULT_SIGBUS, VM_PFNMAP,
        VmAreaStruct, VmFault, VmOperationsStruct,
    },
    scatterlist::{sg_alloc_table_from_pages_segment, sg_free_table},
    shmem::{
        folio_mark_accessed, folio_mark_dirty, fput_file, page_folio, page_to_phys,
        shmem_file_setup, shmem_read_mapping_page_gfp, shmem_truncate_range,
    },
    types::{Page, SgTable},
    vm::{PAGE_KERNEL, PAGE_KERNEL_WC, vmap, vunmap},
};

use crate::{
    dma_resv_types::DmaResv,
    drm_device_types::DrmDevice,
    drm_drv_types::DrmDriver,
    drm_file_types::DrmFile,
    drm_gem_shmem_helper_types_upstream::{
        DRM_GEM_SHMEM_MAP_WC, DRM_GEM_SHMEM_PAGES_MARK_ACCESSED_ON_PUT,
        DRM_GEM_SHMEM_PAGES_MARK_DIRTY_ON_PUT, DRM_GEM_SHMEM_PURGED, DrmGemShmemObject,
    },
    drm_gem_types::{DrmGemObject, DrmGemObjectFuncs},
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_prime_upstream::{DmaBuf, DmaBufAttachment},
    drm_print_types::DrmPrinter,
    iosys_map_types::{IosysMap, IosysMapAnon0},
    uapi_drm_mode_types::DrmModeCreateDumb,
};

const DMA_BIDIRECTIONAL: i32 = 0;
const SHMEM_ALLOC_FLAGS: u32 =
    GFP_KERNEL | drm_kapi::memory::__GFP_RETRY_MAYFAIL | drm_kapi::config::__GFP_NOWARN;

unsafe extern "C" {
    fn dma_map_sgtable(dev: *mut c_void, sgt: *mut SgTable, direction: i32, attrs: u64) -> i32;
    fn dma_buf_attach(dmabuf: *mut DmaBuf, dev: *mut c_void) -> *mut DmaBufAttachment;
    fn dma_buf_detach(dmabuf: *mut DmaBuf, attach: *mut DmaBufAttachment);
    fn dma_buf_unmap_attachment_unlocked(
        attach: *mut DmaBufAttachment,
        sgt: *mut SgTable,
        direction: c_int,
    );
    fn dma_buf_put(dmabuf: *mut DmaBuf);
    fn drm_mode_size_dumb(
        dev: *mut DrmDevice,
        args: *mut DrmModeCreateDumb,
        hw_pitch_align: usize,
        hw_size_align: usize,
    ) -> i32;
    fn vmf_insert_pfn(vma: *mut VmAreaStruct, address: usize, pfn: usize) -> u32;
}

/// # Safety
/// `gem` must be non-null, properly aligned, and point to an initialized `DrmGemObject`.
#[inline]
unsafe fn object_is_imported(gem: *const DrmGemObject) -> bool {
    // SAFETY: `gem` is a valid, aligned pointer to an initialized `DrmGemObject`.
    // Reading `import_attach` inspects whether an external DMA-BUF attachment was recorded.
    unsafe { !(*gem).import_attach.is_null() }
}

#[inline]
fn gem_to_shmem(gem: *mut DrmGemObject) -> *mut DrmGemShmemObject {
    gem.cast()
}

/// # Safety
/// `shmem` must point to an initialized `DrmGemShmemObject` with an immutable `base.size`.
#[inline]
unsafe fn total_pages(shmem: *const DrmGemShmemObject) -> usize {
    // SAFETY: `shmem` is non-null and aligned; `base.size` is established during object
    // creation and remains constant throughout its lifecycle.
    unsafe { (*shmem).base.size >> PAGE_SHIFT }
}

/// # Safety
/// `shmem` must point to an initialized `DrmGemShmemObject` readable for its flags byte.
#[inline]
unsafe fn get_flags(shmem: *const DrmGemShmemObject) -> u8 {
    // SAFETY: `shmem` points to initialized object memory; reading the `flags` field
    // does not mutate any state.
    unsafe { (*shmem).flags }
}

/// # Safety
/// `shmem` must point to an initialized `DrmGemShmemObject`. The caller must have exclusive
/// access or hold the reservation lock to ensure no concurrent readers or writers of `flags`.
#[inline]
unsafe fn update_flags(shmem: *mut DrmGemShmemObject, flags: u8) {
    // SAFETY: The caller holds exclusive access or the reservation lock for `shmem`,
    // ensuring this write to `flags` does not race with concurrent access.
    unsafe { (*shmem).flags = flags }
}

/// # Safety
/// `shmem` must point to an initialized `DrmGemShmemObject`.
#[inline]
unsafe fn uses_write_combine(shmem: *const DrmGemShmemObject) -> bool {
    // SAFETY: Calls `get_flags`, which safely reads the initialized `flags` byte from `shmem`.
    unsafe { (get_flags(shmem) & DRM_GEM_SHMEM_MAP_WC) != 0 }
}

/// # Safety
/// `shmem` must point to an initialized `DrmGemShmemObject`. If `base.filp` is non-null,
/// it must point to a live `File` struct with a readable `f_mapping` pointer.
#[inline]
unsafe fn file_mapping(shmem: *mut DrmGemShmemObject) -> *mut drm_kapi::shmem::AddressSpace {
    // SAFETY: `shmem` is valid; if `filp` is present, it points to the kernel shmem file
    // established during initialization, whose `f_mapping` field remains valid until `fput_file`.
    unsafe {
        let filp = (*shmem).base.filp;
        if filp.is_null() {
            ptr::null_mut()
        } else {
            (*filp).f_mapping.cast()
        }
    }
}

/// # Safety
/// `resv` must point to an initialized `DmaResv` structure. If `ctx` is non-null, it must point
/// to an active, properly initialized `DrmModesetAcquireCtx`.
#[inline]
unsafe fn acquire_reservation_lock(resv: *mut DmaResv, ctx: *mut DrmModesetAcquireCtx) -> i32 {
    #[cfg(test)]
    {
        let _ = (resv, ctx);
        0
    }
    #[cfg(not(test))]
    // SAFETY: `resv` is a non-null, aligned pointer to an initialized `DmaResv`;
    // `ctx` is either null or a valid modeset acquire context.
    unsafe {
        drm_kapi::ww_mutex::ww_mutex_lock(ptr::addr_of_mut!((*resv).lock), ctx.cast())
    }
}

/// # Safety
/// `resv` must point to an initialized `DmaResv` structure. If `ctx` is non-null, it must point
/// to an active, properly initialized `DrmModesetAcquireCtx`.
#[inline]
unsafe fn acquire_reservation_lock_interruptible(
    resv: *mut DmaResv,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    #[cfg(test)]
    {
        let _ = (resv, ctx);
        0
    }
    #[cfg(not(test))]
    // SAFETY: `resv` is a valid pointer to an initialized `DmaResv` with an embedded lock;
    // `ctx` is either null or an active acquire context.
    unsafe {
        drm_kapi::ww_mutex::ww_mutex_lock_interruptible(
            ptr::addr_of_mut!((*resv).lock).cast(),
            ctx.cast(),
        )
    }
}

/// # Safety
/// `resv` must point to an initialized `DmaResv` whose lock is currently held by the calling context.
#[inline]
unsafe fn release_reservation_lock(resv: *mut DmaResv) {
    #[cfg(not(test))]
    // SAFETY: `resv` points to an initialized `DmaResv` whose embedded lock was acquired
    // by this execution thread.
    unsafe {
        drm_kapi::ww_mutex::ww_mutex_unlock(ptr::addr_of_mut!((*resv).lock));
    }
    #[cfg(test)]
    let _ = resv;
}

/// # Safety
/// `value` must be a valid, aligned pointer to an initialized `RefcountT` whose memory remains
/// allocated for the duration of this call.
#[inline]
unsafe fn decrement_refcount_if_above_one(value: *mut drm_kapi::types::RefcountT) -> bool {
    // SAFETY: `value` is aligned to `RefcountT`; its `refs.counter` has identical layout
    // and alignment to `AtomicI32`, and the compare_exchange loop atomically decrements
    // only if the counter exceeds 1, preventing drops to zero.
    unsafe {
        let atomic = &*ptr::addr_of!((*value).refs.counter).cast::<AtomicI32>();
        let mut current = atomic.load(Ordering::Acquire);
        loop {
            if current <= 1 {
                return false;
            }
            match atomic.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }
}

#[repr(transparent)]
struct StaticGemFuncs(DrmGemObjectFuncs);
// SAFETY: `DrmGemObjectFuncs` contains only stateless function pointers and an immutable
// pointer to the static `SHMEM_VM_OPERATIONS` table, making it safe to share across threads.
unsafe impl Sync for StaticGemFuncs {}

static SHMEM_GEM_FUNCS: StaticGemFuncs = StaticGemFuncs(DrmGemObjectFuncs {
    free: Some(drm_gem_shmem_object_free),
    open: None,
    close: None,
    print_info: Some(drm_gem_shmem_object_print_info),
    export: None,
    pin: Some(drm_gem_shmem_object_pin),
    unpin: Some(drm_gem_shmem_object_unpin),
    get_sg_table: Some(drm_gem_shmem_object_get_sg_table),
    vmap: Some(drm_gem_shmem_object_vmap),
    vunmap: Some(drm_gem_shmem_object_vunmap),
    mmap: Some(drm_gem_shmem_object_mmap),
    evict: None,
    status: None,
    rss: None,
    vm_ops: &SHMEM_VM_OPERATIONS.0 as *const VmOperationsStruct as *mut c_void,
});

#[repr(transparent)]
struct StaticVmOps(VmOperationsStruct);
// SAFETY: `VmOperationsStruct` contains only function pointers to reentrant VM callbacks
// invoked by the memory management subsystem, which are safe for concurrent invocation.
unsafe impl Sync for StaticVmOps {}

static SHMEM_VM_OPERATIONS: StaticVmOps = StaticVmOps(VmOperationsStruct {
    open: Some(handle_vm_open),
    close: Some(handle_vm_close),
    mapped: None,
    may_split: None,
    mremap: None,
    mprotect: None,
    fault: Some(handle_vm_fault),
    huge_fault: Some(handle_vm_huge_fault),
    map_pages: None,
    pagesize: None,
    page_mkwrite: None,
    pfn_mkwrite: Some(handle_vm_pfn_mkwrite),
    access: None,
    name: None,
    set_policy: None,
    get_policy: None,
    uffd_ops: ptr::null(),
});

/// # Safety
/// `vmf` must point to an active `VmFault` provided by the VM subsystem, with `vma` pointing
/// to an initialized `VmAreaStruct` whose `vm_private_data` holds a valid `DrmGemObject`.
unsafe fn mark_page_dirty_from_fault(vmf: *mut VmFault) -> u32 {
    // SAFETY: `vmf` and `vma` are provided by the VM fault handler. We verify that `gem` and
    // `pages` are non-null and that `index` is within the object's page count before accessing
    // the page array and marking the page folio dirty.
    unsafe {
        let area = (*vmf).vma;
        let gem = (*area).vm_private_data.cast::<DrmGemObject>();
        let shmem = gem_to_shmem(gem);
        let index = (*vmf).pgoff.wrapping_sub((*area).vm_pgoff) as usize;
        if gem.is_null() || (*shmem).pages.is_null() || index >= total_pages(shmem) {
            warn_on(true);
            return VM_FAULT_SIGBUS;
        }
        let target_page = *(*shmem).pages.add(index);
        if target_page.is_null() {
            warn_on(true);
            return VM_FAULT_SIGBUS;
        }
        folio_mark_dirty(page_folio(target_page));
        0
    }
}

/// # Safety
/// `vmf` must point to a live `VmFault` provided by the VM subsystem during page fault dispatch.
unsafe fn dispatch_fault_handler(vmf: *mut VmFault, order: u32) -> u32 {
    if order != 0 {
        return VM_FAULT_FALLBACK;
    }
    // SAFETY: Verifies `area`, `gem`, `pages`, and bounds on `index`. Acquires `gem.resv`
    // to protect access to `pages` array, reads `target_page`, inserts the PFN via `vmf_insert_pfn`,
    // marks the page accessed on success, and releases the reservation lock.
    unsafe {
        let area = (*vmf).vma;
        if area.is_null() {
            return VM_FAULT_SIGBUS;
        }
        let gem = (*area).vm_private_data.cast::<DrmGemObject>();
        if gem.is_null() {
            return VM_FAULT_SIGBUS;
        }
        let shmem = gem_to_shmem(gem);
        let index = (*vmf).pgoff.wrapping_sub((*area).vm_pgoff) as usize;
        if index >= total_pages(shmem) || (*shmem).pages.is_null() || (*shmem).madv < 0 {
            return VM_FAULT_SIGBUS;
        }
        if acquire_reservation_lock((*gem).resv, ptr::null_mut()) != 0 {
            return VM_FAULT_SIGBUS;
        }
        let target_page = *(*shmem).pages.add(index);
        if target_page.is_null() {
            release_reservation_lock((*gem).resv);
            return VM_FAULT_SIGBUS;
        }
        let pfn = page_to_phys(target_page) >> PAGE_SHIFT;
        let outcome = vmf_insert_pfn(area, (*vmf).address as usize, pfn);
        if outcome == VM_FAULT_NOPAGE {
            folio_mark_accessed(page_folio(target_page));
        }
        release_reservation_lock((*gem).resv);
        outcome
    }
}

/// # Safety
/// Invoked by the kernel VM subsystem on page fault. `vmf` must point to a valid `VmFault`.
unsafe extern "C" fn handle_vm_fault(vmf: *mut VmFault) -> u32 {
    // SAFETY: Passes `vmf` with order 0 to `dispatch_fault_handler`, which handles lock
    // acquisition, bounds validation, and PFN insertion.
    unsafe { dispatch_fault_handler(vmf, 0) }
}

/// # Safety
/// Invoked by the kernel VM subsystem on huge-page fault. `vmf` must point to a valid `VmFault`.
unsafe extern "C" fn handle_vm_huge_fault(vmf: *mut VmFault, order: u32) -> u32 {
    // SAFETY: Passes `vmf` and `order` to `dispatch_fault_handler`. Non-zero orders return
    // `VM_FAULT_FALLBACK` safely without dereferencing invalid state.
    unsafe { dispatch_fault_handler(vmf, order) }
}

/// # Safety
/// Invoked by the VM subsystem when a VMA is cloned or split. `area` must point to a valid
/// `VmAreaStruct` whose `vm_private_data` holds the mapped `DrmGemObject`.
unsafe extern "C" fn handle_vm_open(area: *mut VmAreaStruct) {
    // SAFETY: `area` is a valid `VmAreaStruct` supplied by the VM subsystem. Acquires the
    // reservation lock to increment `pages_use_count`, then increments the GEM base refcount.
    unsafe {
        let gem = (*area).vm_private_data.cast::<DrmGemObject>();
        if gem.is_null() {
            warn_on(true);
            return;
        }
        let shmem = gem_to_shmem(gem);
        warn_on(object_is_imported(gem));
        if acquire_reservation_lock((*gem).resv, ptr::null_mut()) != 0 {
            warn_on(true);
            return;
        }
        let bumped = refcount_inc_not_zero(&(*shmem).pages_use_count);
        warn_on(!bumped);
        release_reservation_lock((*gem).resv);
        crate::drm_gem_upstream::drm_gem_object_get(gem);
    }
}

/// # Safety
/// Invoked by the VM subsystem when a VMA is unmapped or destroyed. `area` must point to a
/// valid `VmAreaStruct` whose `vm_private_data` holds a reference to the `DrmGemObject`.
unsafe extern "C" fn handle_vm_close(area: *mut VmAreaStruct) {
    // SAFETY: `area` is a valid `VmAreaStruct` undergoing destruction. Acquires the reservation
    // lock to drop a page reference via `drm_gem_shmem_put_pages_locked`, then releases the GEM ref.
    unsafe {
        let gem = (*area).vm_private_data.cast::<DrmGemObject>();
        if gem.is_null() {
            warn_on(true);
            return;
        }
        let shmem = gem_to_shmem(gem);
        if acquire_reservation_lock((*gem).resv, ptr::null_mut()) == 0 {
            drm_gem_shmem_put_pages_locked(shmem);
            release_reservation_lock((*gem).resv);
        } else {
            warn_on(true);
        }
        crate::drm_gem_upstream::drm_gem_object_put(gem);
    }
}

/// # Safety
/// Invoked by the VM subsystem when a shared page mapping is about to be dirtied.
/// `vmf` must point to a valid `VmFault`.
unsafe extern "C" fn handle_vm_pfn_mkwrite(vmf: *mut VmFault) -> u32 {
    // SAFETY: Delegates to `mark_page_dirty_from_fault`, which validates pointers and marks
    // the page folio dirty in the page cache.
    unsafe { mark_page_dirty_from_fault(vmf) }
}

/// # Safety
/// - `dev` must point to an active, initialized `DrmDevice`.
/// - `shmem` must point to zero-initialized or uninitialized memory with at least
///   `size_of::<DrmGemShmemObject>()` bytes and aligned to 8 bytes, not shared with other threads.
unsafe fn setup_shmem_object(
    dev: *mut DrmDevice,
    shmem: *mut DrmGemShmemObject,
    byte_size: usize,
    is_private: bool,
) -> i32 {
    // SAFETY: `shmem` points to allocated memory with valid size and alignment. Initializes
    // `base.funcs` to `SHMEM_GEM_FUNCS`, configures the GEM base object and backing shmem file,
    // establishes the mmap offset, and initializes `madv_list`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        if (*gem).funcs.is_null() {
            (*gem).funcs = &SHMEM_GEM_FUNCS.0 as *const DrmGemObjectFuncs as *mut DrmGemObjectFuncs;
        }

        if is_private {
            crate::drm_gem_upstream::drm_gem_private_object_init(dev, gem, byte_size);
            update_flags(shmem, get_flags(shmem) & !DRM_GEM_SHMEM_MAP_WC);
        } else {
            let status = crate::drm_gem_upstream::drm_gem_object_init(dev, gem, byte_size);
            if status != 0 {
                crate::drm_gem_upstream::drm_gem_private_object_fini(gem);
                return status;
            }

            let backing = shmem_file_setup(c"drm-gem-shmem".as_ptr(), byte_size as u64, 0);
            if is_err(backing) {
                let err = ptr_err(backing);
                crate::drm_gem_upstream::drm_gem_private_object_fini(gem);
                return err;
            }
            (*gem).filp = backing.cast();
        }

        let offset_res = crate::drm_gem_upstream::drm_gem_create_mmap_offset(gem);
        if offset_res != 0 {
            if !(*gem).filp.is_null() {
                fput_file((*gem).filp.cast());
                (*gem).filp = ptr::null_mut();
            }
            crate::drm_gem_upstream::drm_gem_object_release(gem);
            return offset_res;
        }

        init_list_head(ptr::addr_of_mut!((*shmem).madv_list));
        if !is_private {
            let map = file_mapping(shmem);
            if !map.is_null() {
                drm_kapi::shmem::mapping_set_gfp_mask(map, SHMEM_ALLOC_FLAGS);
            }
        }
        0
    }
}

/// # Safety
/// - `dev` must point to an active, initialized `DrmDevice`.
/// - `shmem` must point to memory with at least `size_of::<DrmGemShmemObject>()` bytes and aligned
///   to 8 bytes, unshared and not in concurrent use.
/// - On success, returns 0 and caller owns the initialized object; on failure, returns negative errno.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_init(
    dev: *mut DrmDevice,
    shmem: *mut DrmGemShmemObject,
    size: usize,
) -> i32 {
    // SAFETY: Caller guarantees `dev` is valid and `shmem` is adequately sized, aligned,
    // and unaliased. Delegates to `setup_shmem_object` for full initialization.
    unsafe { setup_shmem_object(dev, shmem, size, false) }
}

/// # Safety
/// If `dev` is non-null, `(*dev).driver` must point to an active `DrmDriver` if present.
unsafe fn allocate_and_init_shmem(
    dev: *mut DrmDevice,
    size: usize,
    is_private: bool,
) -> *mut DrmGemShmemObject {
    let Some(aligned_size) = size
        .checked_add(PAGE_SIZE - 1)
        .map(|s| s & !(PAGE_SIZE - 1))
    else {
        return drm_kapi::config::err_ptr(-EOVERFLOW);
    };

    // SAFETY: Checks `dev` and `(*dev).driver` for null. If the driver provides a custom
    // `gem_create_object` callback, invokes it; otherwise allocates zeroed memory via `kzalloc_obj`.
    let shmem_ptr = unsafe {
        if dev.is_null() || (*dev).driver.is_null() {
            return drm_kapi::config::err_ptr(-EINVAL);
        }
        let driver = (*dev).driver.cast::<DrmDriver>();
        if let Some(create_fn) = (*driver).gem_create_object {
            let created = create_fn(dev, aligned_size);
            if is_err(created) {
                return created.cast();
            }
            if created.is_null() {
                return drm_kapi::config::err_ptr(-EINVAL);
            }
            gem_to_shmem(created)
        } else {
            kzalloc_obj::<DrmGemShmemObject>()
        }
    };

    if shmem_ptr.is_null() {
        return drm_kapi::config::err_ptr(-drm_kapi::config::ENOMEM);
    }

    // SAFETY: `shmem_ptr` was successfully allocated with suitable alignment and size,
    // and is not shared with any other thread during initialization.
    let status = unsafe { setup_shmem_object(dev, shmem_ptr, aligned_size, is_private) };
    if status != 0 {
        // SAFETY: Deallocates `shmem_ptr` upon initialization failure to prevent leak.
        unsafe { kfree(shmem_ptr.cast::<c_void>()) };
        return drm_kapi::config::err_ptr(status);
    }
    shmem_ptr
}

/// # Safety
/// - `dev` must point to an active `DrmDevice` whose `driver` field is initialized.
/// - `size` is the requested buffer size in bytes (rounded up to PAGE_SIZE internally).
/// - On success, returns a pointer to a newly allocated `DrmGemShmemObject` with refcount 1.
///   On failure, returns an err_ptr encoded error code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_create(
    dev: *mut DrmDevice,
    size: usize,
) -> *mut DrmGemShmemObject {
    // SAFETY: Passes `dev` to `allocate_and_init_shmem`, which checks validity and allocates
    // a standard (non-private) shmem GEM object.
    unsafe { allocate_and_init_shmem(dev, size, false) }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must be an initialized `DrmGemShmemObject` that is not imported from DMA-BUF.
unsafe fn acquire_pages_locked(shmem: *mut DrmGemShmemObject) -> i32 {
    // SAFETY: Called under `shmem.base.resv`. Allocates the `Page` pointer array via `kmalloc`,
    // reads page-cache folios from `shmem_file_setup` mapping, configures write-combine direct
    // map attributes if requested, and stores the array in `shmem.pages` with `pages_use_count = 1`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        if refcount_inc_not_zero(&(*shmem).pages_use_count) {
            return 0;
        }
        if object_is_imported(gem) {
            return -EINVAL;
        }

        let num_pages = total_pages(shmem);
        if num_pages == 0 || (*gem).filp.is_null() {
            return -EINVAL;
        }
        let Some(table_size) = num_pages.checked_mul(core::mem::size_of::<*mut Page>()) else {
            return -EOVERFLOW;
        };

        let page_table = kmalloc(table_size, GFP_KERNEL).cast::<*mut Page>();
        if page_table.is_null() {
            return -drm_kapi::config::ENOMEM;
        }
        ptr::write_bytes(page_table, 0, num_pages);

        let space = file_mapping(shmem);
        if space.is_null() {
            kfree(page_table.cast::<c_void>());
            return -EINVAL;
        }

        for idx in 0..num_pages {
            let retrieved = shmem_read_mapping_page_gfp(space, idx as u64, SHMEM_ALLOC_FLAGS);
            if retrieved.is_null() || is_err(retrieved) {
                let err_code = if is_err(retrieved) {
                    ptr_err(retrieved)
                } else {
                    -drm_kapi::config::ENOMEM
                };
                for done_idx in 0..idx {
                    drm_kapi::shmem::put_page(*page_table.add(done_idx));
                }
                kfree(page_table.cast::<c_void>());
                return err_code;
            }
            *page_table.add(idx) = retrieved;
        }

        if uses_write_combine(shmem) {
            let Ok(signed_count) = c_int::try_from(num_pages) else {
                for idx in 0..num_pages {
                    drm_kapi::shmem::put_page(*page_table.add(idx));
                }
                kfree(page_table.cast::<c_void>());
                return -EOVERFLOW;
            };
            let wc_err = drm_kapi::page_cache::set_pages_array_wc_kernel(page_table, signed_count);
            if wc_err != 0 {
                for idx in 0..num_pages {
                    drm_kapi::shmem::put_page(*page_table.add(idx));
                }
                kfree(page_table.cast::<c_void>());
                return wc_err;
            }
        }

        (*shmem).pages = page_table;
        refcount_set(&(*shmem).pages_use_count, 1);
        0
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - Must balance a prior successful call to `drm_gem_shmem_pin_locked` or `acquire_pages_locked`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_put_pages_locked(shmem: *mut DrmGemShmemObject) {
    // SAFETY: Decrements `pages_use_count` under reservation lock. If count hits 0, reverts
    // write-combine caching attributes, flushes dirty/accessed flags to folios, releases all
    // page references via `put_page`, frees the `pages` array, and clears `shmem.pages`.
    unsafe {
        if !refcount_dec_and_test(&(*shmem).pages_use_count) {
            return;
        }
        let count = total_pages(shmem);
        let table = (*shmem).pages;
        if table.is_null() {
            warn_on(true);
            return;
        }

        if uses_write_combine(shmem) {
            let status = c_int::try_from(count).map_or(-EOVERFLOW, |c| {
                drm_kapi::page_cache::set_pages_array_wb_kernel(table, c)
            });
            if status != 0 {
                refcount_set(&(*shmem).pages_use_count, 1);
                warn_on(true);
                return;
            }
        }

        let flags = get_flags(shmem);
        for idx in 0..count {
            let p = *table.add(idx);
            if !p.is_null() {
                if flags & DRM_GEM_SHMEM_PAGES_MARK_DIRTY_ON_PUT != 0 {
                    folio_mark_dirty(page_folio(p));
                }
                if flags & DRM_GEM_SHMEM_PAGES_MARK_ACCESSED_ON_PUT != 0 {
                    folio_mark_accessed(page_folio(p));
                }
                drm_kapi::shmem::put_page(p);
            }
        }

        kfree(table.cast::<c_void>());
        (*shmem).pages = ptr::null_mut();
        update_flags(
            shmem,
            flags
                & !(DRM_GEM_SHMEM_PAGES_MARK_DIRTY_ON_PUT
                    | DRM_GEM_SHMEM_PAGES_MARK_ACCESSED_ON_PUT),
        );
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized, non-imported `DrmGemShmemObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_pin_locked(shmem: *mut DrmGemShmemObject) -> i32 {
    // SAFETY: Called under `shmem.base.resv`. If already pinned, increments `pages_pin_count`.
    // Otherwise, calls `acquire_pages_locked` to allocate backing pages and sets `pages_pin_count = 1`.
    unsafe {
        warn_on(object_is_imported(ptr::addr_of!((*shmem).base)));
        if refcount_inc_not_zero(&(*shmem).pages_pin_count) {
            return 0;
        }
        let status = acquire_pages_locked(shmem);
        if status == 0 {
            refcount_set(&(*shmem).pages_pin_count, 1);
        }
        status
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject` with `pages_pin_count > 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_unpin_locked(shmem: *mut DrmGemShmemObject) {
    // SAFETY: Called under `shmem.base.resv`. Decrements `pages_pin_count`; when it reaches 0,
    // releases backing pages via `drm_gem_shmem_put_pages_locked`.
    unsafe {
        if refcount_dec_and_test(&(*shmem).pages_pin_count) {
            drm_gem_shmem_put_pages_locked(shmem);
        }
    }
}

/// # Safety
/// - `shmem` must point to an initialized, non-imported `DrmGemShmemObject`.
/// - Must not be called while already holding `(*shmem).base.resv` lock (acquires it internally).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_pin(shmem: *mut DrmGemShmemObject) -> i32 {
    // SAFETY: Fast path atomically increments `pages_pin_count` if non-zero. Slow path
    // acquires `gem.resv` interruptibly, delegates to `drm_gem_shmem_pin_locked`, and unlocks.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        warn_on(object_is_imported(gem));
        if refcount_inc_not_zero(&(*shmem).pages_pin_count) {
            return 0;
        }
        let lock_res = acquire_reservation_lock_interruptible((*gem).resv, ptr::null_mut());
        if lock_res != 0 {
            return lock_res;
        }
        let pin_res = drm_gem_shmem_pin_locked(shmem);
        release_reservation_lock((*gem).resv);
        pin_res
    }
}

/// # Safety
/// - `shmem` must point to an initialized, non-imported `DrmGemShmemObject` with `pages_pin_count > 0`.
/// - Must not be called with `(*shmem).base.resv` held if this unpin drops the pin count to zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_unpin(shmem: *mut DrmGemShmemObject) {
    // SAFETY: Fast path decrements `pages_pin_count` atomically if > 1 without locking.
    // If decrementing from 1 to 0, acquires `gem.resv` and delegates to `drm_gem_shmem_unpin_locked`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        warn_on(object_is_imported(gem));
        if decrement_refcount_if_above_one(ptr::addr_of_mut!((*shmem).pages_pin_count)) {
            return;
        }
        if acquire_reservation_lock((*gem).resv, ptr::null_mut()) == 0 {
            drm_gem_shmem_unpin_locked(shmem);
            release_reservation_lock((*gem).resv);
        }
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - `map` must point to an uninitialized or valid, writable `IosysMap`.
/// - On success, returns 0 and populates `map`; on failure, returns negative errno.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_vmap_locked(
    shmem: *mut DrmGemShmemObject,
    map: *mut IosysMap,
) -> i32 {
    // SAFETY: Called under `shmem.base.resv`. For imported buffers, delegates to `dmabuf.ops.vmap`.
    // For native objects, increments `vmap_use_count` if already mapped, or pins pages and
    // calls `vmap` to map the page array into kernel virtual memory, recording `vaddr`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        if map.is_null() || (*gem).resv.is_null() {
            return -EINVAL;
        }
        if object_is_imported(gem) {
            let attach = (*gem).import_attach.cast::<DmaBufAttachment>();
            let buf = (*attach).dmabuf;
            let ops = (*buf).ops;
            if ops.is_null() || (*ops).vmap.is_none() {
                return -EOPNOTSUPP;
            }
            return (*ops).vmap.unwrap()(buf, map.cast());
        }

        if refcount_inc_not_zero(&(*shmem).vmap_use_count) {
            (*map).is_iomem = false;
            (*map)._anon_0 = IosysMapAnon0 {
                vaddr: (*shmem).vaddr,
            };
            return 0;
        }

        let pin_res = drm_gem_shmem_pin_locked(shmem);
        if pin_res != 0 {
            return pin_res;
        }

        let prot = if uses_write_combine(shmem) {
            PAGE_KERNEL_WC
        } else {
            PAGE_KERNEL
        };
        let Ok(page_count_u32) = u32::try_from(total_pages(shmem)) else {
            drm_gem_shmem_unpin_locked(shmem);
            return -EOVERFLOW;
        };

        let mapped_addr = vmap((*shmem).pages, page_count_u32, 0, prot);
        if mapped_addr.is_null() {
            drm_gem_shmem_unpin_locked(shmem);
            return -drm_kapi::config::ENOMEM;
        }

        (*shmem).vaddr = mapped_addr;
        (*map).is_iomem = false;
        (*map)._anon_0 = IosysMapAnon0 { vaddr: mapped_addr };
        refcount_set(&(*shmem).vmap_use_count, 1);
        update_flags(
            shmem,
            get_flags(shmem)
                | DRM_GEM_SHMEM_PAGES_MARK_ACCESSED_ON_PUT
                | DRM_GEM_SHMEM_PAGES_MARK_DIRTY_ON_PUT,
        );
        0
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - `map` must point to the `IosysMap` populated by a prior successful `drm_gem_shmem_vmap_locked`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_vunmap_locked(
    shmem: *mut DrmGemShmemObject,
    map: *mut IosysMap,
) {
    // SAFETY: Called under `shmem.base.resv`. For imported buffers, delegates to `dmabuf.ops.vunmap`.
    // For native buffers, decrements `vmap_use_count`; when 0, unmaps `vaddr` via `vunmap`,
    // clears `shmem.vaddr`, and unpins backing pages via `drm_gem_shmem_unpin_locked`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        if object_is_imported(gem) {
            let attach = (*gem).import_attach.cast::<DmaBufAttachment>();
            let buf = (*attach).dmabuf;
            if let Some(unmap_op) = (*(*buf).ops).vunmap {
                unmap_op(buf, map.cast());
            }
            return;
        }

        if refcount_dec_and_test(&(*shmem).vmap_use_count) {
            vunmap((*shmem).vaddr);
            (*shmem).vaddr = ptr::null_mut();
            drm_gem_shmem_unpin_locked(shmem);
        }
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_madvise_locked(
    shmem: *mut DrmGemShmemObject,
    madv: c_int,
) -> c_int {
    // SAFETY: Inspects and updates `(*shmem).madv` under the held reservation lock;
    // preserves purged status (`madv < 0`) without overwriting.
    unsafe {
        if (*shmem).madv >= 0 {
            (*shmem).madv = madv;
        }
        if (*shmem).madv >= 0 { 1 } else { 0 }
    }
}

/// # Safety
/// Caller must hold the reservation lock on `(*shmem).base.resv`.
/// `shmem` must point to an initialized `DrmGemShmemObject`.
#[inline]
unsafe fn can_purge_object(shmem: *mut DrmGemShmemObject) -> bool {
    // SAFETY: Reads `madv`, `pages_pin_count`, `sgt`, `dma_buf`, and `import_attach` fields
    // protected by the reservation lock.
    unsafe {
        (*shmem).madv > 0
            && refcount_read(&(*shmem).pages_pin_count) == 0
            && !(*shmem).sgt.is_null()
            && (*shmem).base.dma_buf.is_null()
            && !object_is_imported(ptr::addr_of!((*shmem).base))
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject` with non-null `sgt`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_gem_shmem_free_sgt_locked(shmem: *mut DrmGemShmemObject) {
    // SAFETY: Called under `shmem.base.resv`. Unmaps the scatterlist DMA addresses via
    // `dma_unmap_sg_attrs`, deallocates table pages with `sg_free_table`, frees the struct,
    // and clears `shmem.sgt`.
    unsafe {
        let sgt = (*shmem).sgt;
        if sgt.is_null() {
            return;
        }
        let dev = (*(*shmem).base.dev).dev;
        drm_kapi::dma::dma_unmap_sg_attrs(
            dev,
            (*sgt).sgl,
            (*sgt).orig_nents,
            DMA_BIDIRECTIONAL as u32,
            0,
        );
        sg_free_table(sgt);
        kfree(sgt.cast::<c_void>());
        (*shmem).sgt = ptr::null_mut();
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must point to an initialized `DrmGemShmemObject` satisfying `can_purge_object`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_purge_locked(shmem: *mut DrmGemShmemObject) {
    // SAFETY: Discards cached resources under reservation lock: frees DMA scatterlist,
    // drops page references, marks `madv = DRM_GEM_SHMEM_PURGED`, frees mmap offset,
    // and truncates the backing file range via `shmem_truncate_range`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        warn_on(!can_purge_object(shmem));
        if !(*shmem).sgt.is_null() {
            __drm_gem_shmem_free_sgt_locked(shmem);
        }
        if !(*shmem).pages.is_null() {
            drm_gem_shmem_put_pages_locked(shmem);
        }
        (*shmem).madv = DRM_GEM_SHMEM_PURGED;
        crate::drm_gem_upstream::drm_gem_free_mmap_offset(gem);
        if !(*gem).filp.is_null() {
            let inode = (*(*gem).filp).f_inode.cast::<drm_kapi::shmem::Inode>();
            shmem_truncate_range(inode, 0, -1);
        }
    }
}

/// # Safety
/// - `file` must point to an open, valid `DrmFile`.
/// - `dev` must point to an active `DrmDevice`.
/// - `args` must point to a writable `DrmModeCreateDumb` struct with `width`, `height`, and `bpp` set.
/// - On success, returns 0 and populates `args.handle`, `args.pitch`, and `args.size`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_dumb_create(
    file: *mut DrmFile,
    dev: *mut DrmDevice,
    args: *mut DrmModeCreateDumb,
) -> i32 {
    // SAFETY: Calculates dumb buffer geometry via `drm_mode_size_dumb`, creates backing
    // shmem GEM object via `drm_gem_shmem_create`, adds handle to `file` via `drm_gem_handle_create`,
    // and drops the local object reference via `drm_gem_object_put`.
    unsafe {
        let size_err = drm_mode_size_dumb(dev, args, 0, 0);
        if size_err != 0 {
            return size_err;
        }
        let shmem = drm_gem_shmem_create(dev, (*args).size as usize);
        if is_err(shmem) {
            return ptr_err(shmem);
        }
        let gem_obj = ptr::addr_of_mut!((*shmem).base);
        let handle_err = crate::drm_gem_upstream::drm_gem_handle_create(
            file,
            gem_obj,
            ptr::addr_of_mut!((*args).handle),
        );
        crate::drm_gem_upstream::drm_gem_object_put(gem_obj);
        handle_err
    }
}

/// # Safety
/// - `shmem` must point to an active, valid `DrmGemShmemObject`.
/// - `vma` must point to a live `VmAreaStruct` being configured by the mmap path, not yet linked
///   into any user address space.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_mmap(
    shmem: *mut DrmGemShmemObject,
    vma: *mut drm_kapi::mm::VmAreaStruct,
) -> i32 {
    // SAFETY: For imported buffers, delegates to `dmabuf.ops.mmap`. For native buffers,
    // verifies COW flags are rejected, acquires `gem.resv` lock to allocate pages,
    // sets `VM_PFNMAP | VM_DONTEXPAND | VM_DONTDUMP`, configures write-combine page protection
    // if requested, and installs `SHMEM_VM_OPERATIONS`.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        if object_is_imported(gem) {
            (*vma).vm_private_data = ptr::null_mut();
            (*vma).vm_ops = ptr::null();
            let buf = (*gem).dma_buf.cast::<DmaBuf>();
            if buf.is_null() || (*buf).ops.is_null() {
                return -EOPNOTSUPP;
            }
            let Some(mmap_fn) = (*(*buf).ops).mmap else {
                return -EOPNOTSUPP;
            };
            let ret = mmap_fn(buf, vma.cast());
            if ret == 0 {
                crate::drm_gem_upstream::drm_gem_object_put(gem);
            }
            return ret;
        }

        let flags = (*vma).vm_flags;
        if drm_kapi::mm::is_cow_mapping(flags) {
            return -EINVAL;
        }

        let map_flags = flags | VM_PFNMAP | VM_DONTEXPAND | VM_DONTDUMP;
        let mut prot = drm_kapi::mm::vm_get_page_prot(map_flags);
        if uses_write_combine(shmem) {
            prot = match drm_kapi::vm::try_pgprot_writecombine(prot) {
                Ok(p) => p,
                Err(e) => return e,
            };
        }

        let lock_res = acquire_reservation_lock((*gem).resv, ptr::null_mut());
        if lock_res != 0 {
            return lock_res;
        }
        let pages_res = acquire_pages_locked(shmem);
        release_reservation_lock((*gem).resv);
        if pages_res != 0 {
            return pages_res;
        }

        drm_kapi::mm::vm_flags_set(vma, VM_PFNMAP | VM_DONTEXPAND | VM_DONTDUMP);
        (*vma).vm_page_prot = prot;
        (*vma).vm_private_data = gem.cast();
        (*vma).vm_ops = &SHMEM_VM_OPERATIONS.0;
        0
    }
}

/// # Safety
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - `printer` must point to an initialized `DrmPrinter` ready to accept output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_print_info(
    shmem: *const DrmGemShmemObject,
    printer: *mut DrmPrinter,
    indent: c_uint,
) {
    // SAFETY: Bails early if imported. Reads refcounts and `vaddr` atomically or immutably,
    // formats text into an allocated String, and prints via `drm_printf`.
    unsafe {
        if object_is_imported(ptr::addr_of!((*shmem).base)) {
            return;
        }
        let tab = "\t".repeat(indent as usize);
        let out = alloc::format!(
            concat!(
                "{tab}pages_pin_count={}\n",
                "{tab}pages_use_count={}\n",
                "{tab}vmap_use_count={}\n",
                "{tab}vaddr={:p}\n",
            ),
            refcount_read(&(*shmem).pages_pin_count),
            refcount_read(&(*shmem).pages_use_count),
            refcount_read(&(*shmem).vmap_use_count),
            (*shmem).vaddr,
            tab = tab,
        );
        crate::drm_print_upstream::drm_printf(printer, &out);
    }
}

/// # Safety
/// - `shmem` must point to an initialized, non-imported `DrmGemShmemObject` whose backing pages
///   are already allocated and pinned or protected by the reservation lock.
/// - Caller takes ownership of the returned `SgTable` and must free it via `sg_free_table` and `kfree`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_get_sg_table(shmem: *mut DrmGemShmemObject) -> *mut SgTable {
    // SAFETY: Validates that `shmem` is not imported and `pages` is non-null. Allocates an
    // `SgTable` structure via `kzalloc_obj`, builds scatterlist segments from `shmem.pages` via
    // `sg_alloc_table_from_pages_segment`, and cleans up on failure.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        warn_on(object_is_imported(gem));
        if object_is_imported(gem) || (*shmem).pages.is_null() {
            return drm_kapi::config::err_ptr(-EINVAL);
        }
        let count = total_pages(shmem);
        let Ok(page_count_u32) = u32::try_from(count) else {
            return drm_kapi::config::err_ptr(-EOVERFLOW);
        };
        let table = kzalloc_obj::<SgTable>();
        if table.is_null() {
            return drm_kapi::config::err_ptr(-drm_kapi::config::ENOMEM);
        }
        let alloc_res = sg_alloc_table_from_pages_segment(
            table,
            (*shmem).pages,
            page_count_u32,
            0,
            (*gem).size as u64,
            u32::MAX,
            GFP_KERNEL,
        );
        if alloc_res != 0 {
            kfree(table);
            return drm_kapi::config::err_ptr(alloc_res);
        }
        table
    }
}

/// # Safety
/// - Caller must hold the reservation lock on `(*shmem).base.resv`.
/// - `shmem` must be an initialized, non-imported `DrmGemShmemObject`.
unsafe fn create_and_map_sgt_locked(shmem: *mut DrmGemShmemObject) -> *mut SgTable {
    // SAFETY: Called under `shmem.base.resv`. Returns existing `shmem.sgt` if present.
    // Otherwise acquires backing pages, builds `SgTable` via `drm_gem_shmem_get_sg_table`,
    // maps it for DMA via `dma_map_sgtable`, and stores it in `shmem.sgt`.
    unsafe {
        if !(*shmem).sgt.is_null() {
            return (*shmem).sgt;
        }
        if object_is_imported(ptr::addr_of!((*shmem).base)) {
            return drm_kapi::config::err_ptr(-EINVAL);
        }
        let pages_res = acquire_pages_locked(shmem);
        if pages_res != 0 {
            return drm_kapi::config::err_ptr(pages_res);
        }
        let sgt = drm_gem_shmem_get_sg_table(shmem);
        if is_err(sgt) {
            let err = ptr_err(sgt);
            drm_gem_shmem_put_pages_locked(shmem);
            return drm_kapi::config::err_ptr(err);
        }
        let dma_dev = (*(*shmem).base.dev).dev;
        let map_res = dma_map_sgtable(dma_dev, sgt, DMA_BIDIRECTIONAL, 0);
        if map_res != 0 {
            sg_free_table(sgt);
            kfree(sgt.cast::<c_void>());
            drm_gem_shmem_put_pages_locked(shmem);
            return drm_kapi::config::err_ptr(map_res);
        }
        (*shmem).sgt = sgt;
        sgt
    }
}

/// # Safety
/// - `shmem` must point to an initialized, non-imported `DrmGemShmemObject`.
/// - Must not be called while already holding `(*shmem).base.resv` lock (acquires it internally).
/// - The returned `SgTable` pointer is owned by `shmem` and remains valid until purged or released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_get_pages_sgt(
    shmem: *mut DrmGemShmemObject,
) -> *mut SgTable {
    // SAFETY: Acquires `gem.resv` interruptibly, delegates to `create_and_map_sgt_locked`
    // to build and DMA-map the table, then releases the reservation lock.
    unsafe {
        let resv = (*shmem).base.resv;
        let lock_res = acquire_reservation_lock_interruptible(resv, ptr::null_mut());
        if lock_res != 0 {
            return drm_kapi::config::err_ptr(lock_res);
        }
        let table = create_and_map_sgt_locked(shmem);
        release_reservation_lock(resv);
        table
    }
}

/// # Safety
/// - `dev` must point to an active, initialized `DrmDevice`.
/// - `attach` must point to a live `DmaBufAttachment` with a non-null `dmabuf`.
/// - `sgt` must point to a valid `SgTable` already DMA-mapped for this attachment.
/// - Returns a newly allocated `DrmGemObject` with refcount 1, or an err_ptr on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_prime_import_sg_table(
    dev: *mut DrmDevice,
    attach: *mut DmaBufAttachment,
    sgt: *mut SgTable,
) -> *mut DrmGemObject {
    // SAFETY: Validates `attach` and `dmabuf`. Computes page-aligned size, creates a private
    // shmem GEM object via `allocate_and_init_shmem`, attaches `sgt`, and returns base pointer.
    unsafe {
        if attach.is_null() || (*attach).dmabuf.is_null() {
            return drm_kapi::config::err_ptr(-EINVAL);
        }
        let Some(aligned_size) = (*(*attach).dmabuf).size.checked_add(PAGE_SIZE - 1) else {
            return drm_kapi::config::err_ptr(-EOVERFLOW);
        };
        let shmem = allocate_and_init_shmem(dev, aligned_size & !(PAGE_SIZE - 1), true);
        if is_err(shmem) {
            return shmem.cast();
        }
        (*shmem).sgt = sgt;
        ptr::addr_of_mut!((*shmem).base)
    }
}

/// # Safety
/// - `dev` must point to an active, initialized `DrmDevice`.
/// - `dmabuf` must point to a live, open `DmaBuf` structure.
/// - Returns an acquired `DrmGemObject` pointer with an incremented refcount, or err_ptr on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_prime_import_no_map(
    dev: *mut DrmDevice,
    dmabuf: *mut DmaBuf,
) -> *mut DrmGemObject {
    // SAFETY: If `dmabuf` was exported by this driver, acquires another reference via
    // `drm_gem_object_get` and returns the existing GEM object. Otherwise attaches to `dmabuf` via
    // `dma_buf_attach`, acquires a buffer reference, creates a private shmem GEM object, and binds
    // `import_attach` and `resv`.
    unsafe {
        if dev.is_null() || dmabuf.is_null() {
            return drm_kapi::config::err_ptr(-EINVAL);
        }
        if crate::drm_prime_upstream::drm_gem_is_prime_exported_dma_buf(dev, dmabuf) {
            let existing = (*dmabuf).r#priv.cast::<DrmGemObject>();
            crate::drm_gem_upstream::drm_gem_object_get(existing);
            return existing;
        }

        let attach = dma_buf_attach(dmabuf, (*dev).dev);
        if is_err(attach) {
            return attach.cast();
        }
        if attach.is_null() {
            return drm_kapi::config::err_ptr(-EIO);
        }

        crate::drm_prime_upstream::get_dma_buf(dmabuf);
        let Some(aligned_size) = (*dmabuf).size.checked_add(PAGE_SIZE - 1) else {
            dma_buf_detach(dmabuf, attach);
            dma_buf_put(dmabuf);
            return drm_kapi::config::err_ptr(-EOVERFLOW);
        };

        let shmem = allocate_and_init_shmem(dev, aligned_size & !(PAGE_SIZE - 1), true);
        if is_err(shmem) {
            let err = ptr_err(shmem);
            dma_buf_detach(dmabuf, attach);
            dma_buf_put(dmabuf);
            return drm_kapi::config::err_ptr(err);
        }

        (*shmem).base.import_attach = attach.cast();
        (*shmem).base.resv = (*dmabuf).resv;
        ptr::addr_of_mut!((*shmem).base)
    }
}

/// # Safety
/// - `shmem` must point to an active `DrmGemShmemObject` whose references are being finalized.
/// - Caller must not hold `(*shmem).base.resv` lock (acquires it internally for native objects).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_release(shmem: *mut DrmGemShmemObject) {
    // SAFETY: For imported objects, unmaps DMA attachment if present and invokes
    // `drm_prime_gem_destroy`. For native objects, locks `gem.resv`, frees `sgt` and backing pages,
    // closes the backing file via `fput_file`, and releases the base GEM object.
    unsafe {
        let gem = ptr::addr_of_mut!((*shmem).base);
        if object_is_imported(gem) {
            let attach = (*gem).import_attach.cast::<DmaBufAttachment>();
            if !(*shmem).sgt.is_null() {
                dma_buf_unmap_attachment_unlocked(attach, (*shmem).sgt, DMA_BIDIRECTIONAL);
                (*shmem).sgt = ptr::null_mut();
            }
            crate::drm_prime_upstream::drm_prime_gem_destroy(gem, (*shmem).sgt);
        } else {
            let lock_res = acquire_reservation_lock((*gem).resv, ptr::null_mut());
            if lock_res != 0 {
                warn_on(true);
                return;
            }
            warn_on(refcount_read(&(*shmem).vmap_use_count) != 0);
            if !(*shmem).sgt.is_null() {
                __drm_gem_shmem_free_sgt_locked(shmem);
            }
            if !(*shmem).pages.is_null() {
                drm_gem_shmem_put_pages_locked(shmem);
            }
            warn_on(refcount_read(&(*shmem).pages_use_count) != 0);
            warn_on(refcount_read(&(*shmem).pages_pin_count) != 0);
            release_reservation_lock((*gem).resv);
        }

        if !(*gem).filp.is_null() {
            fput_file((*gem).filp.cast());
            (*gem).filp = ptr::null_mut();
        }
        crate::drm_gem_upstream::drm_gem_object_release(gem);
    }
}

/// # Safety
/// - `shmem` must point to a `DrmGemShmemObject` allocated via `kmalloc`/`kzalloc_obj` whose
///   reference count has reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_free(shmem: *mut DrmGemShmemObject) {
    // SAFETY: Calls `drm_gem_shmem_release` to clean up all internal resources, then
    // frees the outer `DrmGemShmemObject` allocation via `kfree`.
    unsafe {
        drm_gem_shmem_release(shmem);
        kfree(shmem);
    }
}

/// # Safety
/// - `obj` must point to the `base` field of an allocated `DrmGemShmemObject` whose refcount is zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_free(obj: *mut DrmGemObject) {
    // SAFETY: `obj` is the base field of `DrmGemShmemObject` at offset 0; casting to
    // `DrmGemShmemObject` and delegating to `drm_gem_shmem_free` is sound.
    unsafe { drm_gem_shmem_free(gem_to_shmem(obj)) };
}

/// # Safety
/// - `printer` must point to an initialized `DrmPrinter`.
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_print_info(
    printer: *mut DrmPrinter,
    indent: c_uint,
    obj: *mut DrmGemObject,
) {
    // SAFETY: Passes the enclosing `DrmGemShmemObject` to `drm_gem_shmem_print_info`.
    unsafe { drm_gem_shmem_print_info(gem_to_shmem(obj), printer, indent) };
}

/// # Safety
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject`.
/// - Caller must hold the reservation lock on `(*obj).resv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_pin(obj: *mut DrmGemObject) -> i32 {
    // SAFETY: Delegates to `drm_gem_shmem_pin_locked` under the caller-held reservation lock.
    unsafe { drm_gem_shmem_pin_locked(gem_to_shmem(obj)) }
}

/// # Safety
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject`.
/// - Caller must hold the reservation lock on `(*obj).resv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_unpin(obj: *mut DrmGemObject) {
    // SAFETY: Delegates to `drm_gem_shmem_unpin_locked` under the caller-held reservation lock.
    unsafe { drm_gem_shmem_unpin_locked(gem_to_shmem(obj)) };
}

/// # Safety
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject` whose pages are pinned.
/// - Caller takes ownership of the returned `SgTable`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_get_sg_table(obj: *mut DrmGemObject) -> *mut SgTable {
    // SAFETY: Casts `obj` to enclosing `DrmGemShmemObject` and calls `drm_gem_shmem_get_sg_table`.
    unsafe { drm_gem_shmem_get_sg_table(gem_to_shmem(obj)) }
}

/// # Safety
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject`.
/// - `map` must point to a writable `IosysMap`.
/// - Caller must hold the reservation lock on `(*obj).resv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_vmap(
    obj: *mut DrmGemObject,
    map: *mut IosysMap,
) -> i32 {
    // SAFETY: Delegates to `drm_gem_shmem_vmap_locked` under the caller-held reservation lock.
    unsafe { drm_gem_shmem_vmap_locked(gem_to_shmem(obj), map) }
}

/// # Safety
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject`.
/// - `map` must point to the `IosysMap` populated by a prior `drm_gem_shmem_object_vmap`.
/// - Caller must hold the reservation lock on `(*obj).resv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_vunmap(obj: *mut DrmGemObject, map: *mut IosysMap) {
    // SAFETY: Delegates to `drm_gem_shmem_vunmap_locked` under the caller-held reservation lock.
    unsafe { drm_gem_shmem_vunmap_locked(gem_to_shmem(obj), map) };
}

/// # Safety
/// - `obj` must point to the `base` field of an active `DrmGemShmemObject`.
/// - `vma` must point to a live `VmAreaStruct` being configured by the mmap path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_shmem_object_mmap(
    obj: *mut DrmGemObject,
    vma: *mut c_void,
) -> i32 {
    // SAFETY: Casts `obj` to `DrmGemShmemObject` and `vma` to `*mut VmAreaStruct`, then
    // delegates to `drm_gem_shmem_mmap`.
    unsafe { drm_gem_shmem_mmap(gem_to_shmem(obj), vma.cast()) }
}

#[cfg(test)]
/// # Safety
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - `map` must point to a writable `IosysMap`.
/// - Must not be called with `(*shmem).base.resv` held (locks internally).
pub unsafe fn drm_gem_shmem_vmap(shmem: *mut DrmGemShmemObject, map: *mut IosysMap) -> i32 {
    // SAFETY: Acquires `gem.resv` interruptibly, calls `drm_gem_shmem_vmap_locked`, and unlocks.
    unsafe {
        let resv = (*shmem).base.resv;
        let lock_res = acquire_reservation_lock_interruptible(resv, ptr::null_mut());
        if lock_res != 0 {
            return lock_res;
        }
        let ret = drm_gem_shmem_vmap_locked(shmem, map);
        release_reservation_lock(resv);
        ret
    }
}

#[cfg(test)]
/// # Safety
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - `map` must point to the `IosysMap` populated by `drm_gem_shmem_vmap`.
/// - Must not be called with `(*shmem).base.resv` held (locks internally).
pub unsafe fn drm_gem_shmem_vunmap(shmem: *mut DrmGemShmemObject, map: *mut IosysMap) {
    // SAFETY: Acquires `gem.resv` interruptibly, calls `drm_gem_shmem_vunmap_locked`, and unlocks.
    unsafe {
        let resv = (*shmem).base.resv;
        if acquire_reservation_lock_interruptible(resv, ptr::null_mut()) == 0 {
            drm_gem_shmem_vunmap_locked(shmem, map);
            release_reservation_lock(resv);
        }
    }
}

#[cfg(test)]
/// # Safety
/// - `shmem` must point to an initialized `DrmGemShmemObject`.
/// - Must not be called with `(*shmem).base.resv` held (locks internally).
pub unsafe fn drm_gem_shmem_madvise(shmem: *mut DrmGemShmemObject, madv: c_int) -> c_int {
    // SAFETY: Acquires `gem.resv` interruptibly, calls `drm_gem_shmem_madvise_locked`, and unlocks.
    unsafe {
        let resv = (*shmem).base.resv;
        let lock_res = acquire_reservation_lock_interruptible(resv, ptr::null_mut());
        if lock_res != 0 {
            return lock_res;
        }
        let ret = drm_gem_shmem_madvise_locked(shmem, madv);
        release_reservation_lock(resv);
        ret
    }
}

#[cfg(test)]
/// # Safety
/// - `shmem` must point to an initialized `DrmGemShmemObject` eligible for purging.
/// - Must not be called with `(*shmem).base.resv` held (locks internally).
pub unsafe fn drm_gem_shmem_purge(shmem: *mut DrmGemShmemObject) -> i32 {
    // SAFETY: Acquires `gem.resv` interruptibly, calls `drm_gem_shmem_purge_locked`, and unlocks.
    unsafe {
        let resv = (*shmem).base.resv;
        let lock_res = acquire_reservation_lock_interruptible(resv, ptr::null_mut());
        if lock_res != 0 {
            return lock_res;
        }
        drm_gem_shmem_purge_locked(shmem);
        release_reservation_lock(resv);
        0
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;

    use drm_kapi::config::PAGE_SIZE;

    use super::*;
    use crate::drm_vma_manager_types::DrmVmaOffsetManager;

    #[test]
    fn pin_pages_and_scatterlist_keep_independent_lifetimes() {
        // SAFETY: Test harness setup and teardown for GEM shmem lifecycle.
        // Initializes zeroed `DrmVmaOffsetManager` and manages test device lifecycle.
        unsafe {
            let mut test_dev = crate::test_support::TestDrm::new(0);
            let mut vma_mgr: Box<DrmVmaOffsetManager> = Box::new(core::mem::zeroed());
            crate::drm_vma_manager_upstream::drm_vma_offset_manager_init(
                &mut *vma_mgr,
                0x1000,
                0x10000,
            );
            test_dev.dev.vma_offset_manager = &mut *vma_mgr;

            let shmem = drm_gem_shmem_create(test_dev.ptr(), 2 * PAGE_SIZE);
            assert!(!is_err(shmem));
            assert_eq!(drm_gem_shmem_pin_locked(shmem), 0);
            assert_eq!(refcount_read(&(*shmem).pages_use_count), 1);
            assert_eq!(refcount_read(&(*shmem).pages_pin_count), 1);

            // Additional pin increments pin refcount while sharing the page array
            assert_eq!(drm_gem_shmem_pin_locked(shmem), 0);
            assert_eq!(refcount_read(&(*shmem).pages_use_count), 1);
            assert_eq!(refcount_read(&(*shmem).pages_pin_count), 2);
            let sgt = drm_gem_shmem_get_sg_table(shmem);
            assert!(!is_err(sgt));
            assert!((*sgt).orig_nents >= 1 && (*sgt).orig_nents <= 2);
            sg_free_table(sgt);
            kfree(sgt);

            drm_gem_shmem_unpin_locked(shmem);
            assert_eq!(refcount_read(&(*shmem).pages_use_count), 1);
            assert!(!(*shmem).pages.is_null());
            drm_gem_shmem_unpin_locked(shmem);
            assert_eq!(refcount_read(&(*shmem).pages_use_count), 0);
            assert_eq!(refcount_read(&(*shmem).pages_pin_count), 0);
            assert!((*shmem).pages.is_null());

            drm_gem_shmem_free(shmem);
            crate::drm_vma_manager_upstream::drm_vma_offset_manager_destroy(&mut *vma_mgr);
        }
    }

    #[test]
    fn failed_dma_mapping_releases_the_temporary_sgt_and_page_pin() {
        // SAFETY: Tests failure unwinding when DMA mapping is not available.
        // Initializes zeroed `DrmVmaOffsetManager` and manages test device lifecycle.
        unsafe {
            let mut test_dev = crate::test_support::TestDrm::new(0);
            let mut vma_mgr: Box<DrmVmaOffsetManager> = Box::new(core::mem::zeroed());
            crate::drm_vma_manager_upstream::drm_vma_offset_manager_init(
                &mut *vma_mgr,
                0x1000,
                0x10000,
            );
            test_dev.dev.vma_offset_manager = &mut *vma_mgr;

            let shmem = drm_gem_shmem_create(test_dev.ptr(), 2 * PAGE_SIZE);
            assert!(!is_err(shmem));
            let sgt = drm_gem_shmem_get_pages_sgt(shmem);
            assert!(is_err(sgt));
            assert_eq!(refcount_read(&(*shmem).pages_use_count), 0);
            assert!((*shmem).pages.is_null());
            assert!((*shmem).sgt.is_null());

            drm_gem_shmem_free(shmem);
            crate::drm_vma_manager_upstream::drm_vma_offset_manager_destroy(&mut *vma_mgr);
        }
    }
}

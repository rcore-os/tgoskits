// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_gem.c`.
//!
//! Graphics Execution Manager (GEM): object lifecycle (init/release/free),
//! handles, names/flink, mmap offset allocation through `drm_vma_manager`,
//! LRU management, and dma-resv locking helpers.

#![allow(non_snake_case)]

use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

use drm_kapi::{
    config::{EINVAL, ENOMEM, EOPNOTSUPP, GFP_KERNEL},
    dma_resv::{dma_resv_fini, dma_resv_init},
    list::{INIT_LIST_HEAD, list_add_tail, list_del_init, list_empty},
    locks::{spin_lock, spin_unlock},
    memory::{kfree, kmalloc, kref_init, kref_put},
    mutex::{mutex_init, mutex_lock, mutex_unlock},
    types::{Kref, WwAcquireCtx},
    ww_mutex::{ww_mutex_lock, ww_mutex_unlock},
};

use crate::{
    dma_resv_types::DmaResv,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_gem_types::{DrmGemLru, DrmGemObject},
    drm_mode_object_upstream::{idr_alloc, idr_destroy, idr_find, idr_remove, idr_replace},
    drm_print_types::DrmPrinter,
    drm_vma_manager_upstream::{
        drm_vma_node_allow, drm_vma_node_revoke, drm_vma_offset_add, drm_vma_offset_remove,
    },
    uapi_drm_types::{DrmGemClose, DrmGemFlink, DrmGemOpen},
};

const ENOENT: i32 = 2;
const PAGE_SIZE: usize = 4096;
const PAGE_SHIFT: usize = 12;

/// # Safety
///
/// `resv` must point to a live, properly aligned `DmaResv` object.
/// If `ctx` is non-null, it must point to an active `WwAcquireCtx`.
unsafe fn dma_resv_lock(resv: *mut DmaResv, ctx: *mut WwAcquireCtx) -> i32 {
    // SAFETY: `resv` is live per this function's `# Safety`, so `(*resv).lock` is an in-bounds field address for the WW mutex call.
    unsafe { ww_mutex_lock(core::ptr::addr_of_mut!((*resv).lock), ctx) }
}

/// # Safety
///
/// `resv` must point to a live, properly aligned `DmaResv` whose embedded lock was acquired by caller.
unsafe fn dma_resv_unlock(resv: *mut DmaResv) {
    // SAFETY: `resv` is live per this function's `# Safety`; the caller acquired the embedded WW lock, so unlocking it pairs with that acquisition.
    unsafe { ww_mutex_unlock(core::ptr::addr_of_mut!((*resv).lock)) };
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_huge_mnt_create()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to a valid, live `DrmDevice` undergoing early initialization.
pub unsafe extern "C" fn drm_gem_huge_mnt_create(
    _dev: *mut DrmDevice,
    _value: *const c_char,
) -> i32 {
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// `_dev` must point to an allocated, live `DrmDevice` initializing its GEM subsystem.
pub unsafe extern "C" fn drm_gem_init(_dev: *mut DrmDevice) -> i32 {
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_private_object_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `obj` must point to allocated, writable memory for a `DrmGemObject`.
/// - `size` must be page-aligned.
pub unsafe extern "C" fn drm_gem_private_object_init(
    dev: *mut DrmDevice,
    obj: *mut DrmGemObject,
    size: usize,
) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is checked non-null and caller guarantees exclusive ownership during object initialization; `dev` outlives this object.
    unsafe {
        (*obj).dev = dev;
        (*obj).filp = core::ptr::null_mut();
        kref_init(core::ptr::addr_of_mut!((*obj).refcount));
        (*obj).handle_count = 0;
        (*obj).size = size;
        core::ptr::write_bytes(core::ptr::addr_of_mut!((*obj).vma_node.vm_lock), 0, 1);
        (*obj).vma_node.vm_files.node = core::ptr::null_mut();
        (*obj).vma_node.driver_private = core::ptr::null_mut();
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*obj).gpuva.list));
        mutex_init(core::ptr::addr_of_mut!((*obj).gpuva.lock));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*obj).lru_node));
        (*obj).resv = core::ptr::addr_of_mut!((*obj)._resv);
        dma_resv_init((*obj).resv);
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_private_object_fini()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid `DrmGemObject` previously initialized with `drm_gem_private_object_init`
/// and no longer accessible to other threads.
pub unsafe extern "C" fn drm_gem_private_object_fini(obj: *mut DrmGemObject) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is checked non-null and points to a live `DrmGemObject` whose reservation lock is being destroyed.
    unsafe {
        dma_resv_fini(core::ptr::addr_of_mut!((*obj)._resv));
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `obj` must point to allocated, writable memory for a `DrmGemObject`.
/// - `size` must be page-aligned.
pub unsafe extern "C" fn drm_gem_object_init(
    dev: *mut DrmDevice,
    obj: *mut DrmGemObject,
    size: usize,
) -> i32 {
    // SAFETY: `dev` and `obj` are forwarded unchanged to `drm_gem_private_object_init`, which carries the same pointer requirements as this entry point.
    unsafe { drm_gem_private_object_init(dev, obj, size) };
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_handle_get()
/// # Safety
///
/// `obj` must point to a live `DrmGemObject` with caller-provided synchronization.
unsafe fn drm_gem_object_handle_get(obj: *mut DrmGemObject) {
    if !obj.is_null() {
        // SAFETY: `obj` is checked non-null and caller guarantees serialized access to `handle_count`.
        unsafe {
            (*obj).handle_count += 1;
        }
        // SAFETY: `obj` is non-null (checked above) and live per this function's `# Safety`; `refcount` is an embedded field, so the `kref_get` pointer is valid.
        unsafe { drm_kapi::memory::kref_get(core::ptr::addr_of_mut!((*obj).refcount)) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_handle_get_if_exists_unlocked()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to an initialized `DrmGemObject` whose reference count memory is valid to inspect.
pub unsafe extern "C" fn drm_gem_object_handle_get_if_exists_unlocked(
    obj: *mut DrmGemObject,
) -> bool {
    if obj.is_null() {
        return false;
    }
    // SAFETY: `obj` is checked non-null and points to a live GEM object.
    unsafe {
        if (*obj).handle_count == 0 {
            return false;
        }
        (*obj).handle_count += 1;
        drm_kapi::memory::kref_get(core::ptr::addr_of_mut!((*obj).refcount));
        true
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_handle_free()
// # Safety
//
// External C ABI symbol declarations linked from kernel or runtime modules.
unsafe extern "C" {
    fn dma_buf_put(dmabuf: *mut crate::drm_prime_upstream::DmaBuf);
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_handle_free()
/// # Safety
///
/// `obj` must point to a live `DrmGemObject`. Caller must hold `dev->object_name_lock`.
unsafe fn drm_gem_object_handle_free(obj: *mut DrmGemObject) {
    if !obj.is_null()
        // SAFETY: `obj` is checked non-null; test if global flink name has been assigned.
        && unsafe {
            (*obj).name != 0
        }
    {
        // SAFETY: `obj` is non-null (checked above) and live per this function's `# Safety`, so reading its `dev` field is valid.
        let dev = unsafe { (*obj).dev };
        if !dev.is_null() {
            // SAFETY: `dev` is non-null (checked above) and owns `object_name_idr`; the caller holds `dev->object_name_lock`, which serializes this IDR removal.
            unsafe { idr_remove(core::ptr::addr_of_mut!((*dev).object_name_idr), (*obj).name) };
        }
        // SAFETY: `obj` is valid; clear the assigned flink name.
        unsafe {
            (*obj).name = 0;
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_exported_dma_buf_free()
/// # Safety
///
/// `obj` must point to a live `DrmGemObject` whose exported DMA-BUF is being released.
unsafe fn drm_gem_object_exported_dma_buf_free(obj: *mut DrmGemObject) {
    if !obj.is_null()
        // SAFETY: `obj` is checked non-null; check if exported `dma_buf` is non-null.
        && !unsafe {
            (*obj).dma_buf.is_null()
        }
    {
        // SAFETY: `obj` is live per this function's `# Safety`; `dma_buf` was checked non-null above, and this drops the export reference it holds.
        unsafe { dma_buf_put((*obj).dma_buf.cast()) };
        // SAFETY: `obj` is live per this function's `# Safety`; clearing `dma_buf` right after `dma_buf_put` prevents a second release of that reference.
        unsafe {
            (*obj).dma_buf = core::ptr::null_mut();
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_handle_put_unlocked()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a live `DrmGemObject` with at least one active reference held by caller.
pub unsafe extern "C" fn drm_gem_object_handle_put_unlocked(obj: *mut DrmGemObject) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is checked non-null; decrement handle count under `object_name_lock` and drop object reference if last handle.
    unsafe {
        let dev = (*obj).dev;
        let mut final_put = false;
        if (*obj).handle_count == 0 {
            return;
        }
        if !dev.is_null() {
            mutex_lock(core::ptr::addr_of_mut!((*dev).object_name_lock));
        }
        (*obj).handle_count -= 1;
        if (*obj).handle_count == 0 {
            drm_gem_object_handle_free(obj);
            drm_gem_object_exported_dma_buf_free(obj);
            final_put = true;
        }
        if !dev.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock));
        }
        if final_put {
            drm_gem_object_put(obj);
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_release_handle()
/// # Safety
///
/// `ptr` must point to a live `DrmGemObject`, and `data` must point to a live `DrmFile`.
unsafe fn drm_gem_object_release_handle(id: c_int, ptr: *mut c_void, data: *mut c_void) -> i32 {
    let file_priv = data.cast::<DrmFile>();
    let obj = ptr.cast::<DrmGemObject>();

    if obj.is_null() || file_priv.is_null() {
        return 0;
    }

    // SAFETY: `obj` is the IDR entry passed in by the handle walk; its handle reference keeps it live, so reading its `funcs` table is valid.
    if let Some(funcs) = unsafe { (*obj).funcs.as_ref() }
        && let Some(close) = funcs.close
    {
        // SAFETY: `close` was read from the object's live function table, and `obj` and `file_priv` are the two pointers that callback expects.
        unsafe { close(obj, file_priv) };
    }

    // SAFETY: `file_priv` is non-null (checked above) and owns `prime`; `prime.lock` is taken before the PRIME handle table is edited.
    unsafe { mutex_lock(core::ptr::addr_of_mut!((*file_priv).prime.lock)) };
    // SAFETY: `prime.lock` on `file_priv` is held from the call above, which the PRIME table requires; `id` is the handle key from the IDR walk.
    unsafe {
        crate::drm_prime_upstream::drm_prime_remove_buf_handle(
            core::ptr::addr_of_mut!((*file_priv).prime),
            id as u32,
        )
    };
    // SAFETY: Releases the `prime.lock` taken above on the same `file_priv`, so the lock is not leaked on this path.
    unsafe { mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock)) };

    // SAFETY: `obj` is live through its handle reference, and `vma_node` is its embedded node; revoking drops this file's mmap permission for it.
    unsafe { drm_vma_node_revoke(core::ptr::addr_of_mut!((*obj).vma_node), file_priv) };

    // SAFETY: Drops the handle's reference on `obj`; this is the last use of `obj` in the release path.
    unsafe { drm_gem_object_handle_put_unlocked(obj) };
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_handle_delete()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to an open, live `DrmFile` owning the handle table.
pub unsafe extern "C" fn drm_gem_handle_delete(filp: *mut DrmFile, handle: u32) -> i32 {
    if filp.is_null() || handle == 0 {
        return -EINVAL;
    }
    // SAFETY: `filp` is checked non-null; remove handle from IDR under `table_lock`.
    unsafe {
        spin_lock(&mut (*filp).table_lock);
        let obj = idr_replace(
            core::ptr::addr_of_mut!((*filp).object_idr),
            core::ptr::null_mut(),
            handle as i32,
        )
        .cast::<DrmGemObject>();
        spin_unlock(&mut (*filp).table_lock);
        if obj.is_null() {
            return -EINVAL;
        }

        drm_gem_object_release_handle(handle as i32, obj.cast(), filp.cast());

        spin_lock(&mut (*filp).table_lock);
        idr_remove(core::ptr::addr_of_mut!((*filp).object_idr), handle as i32);
        spin_unlock(&mut (*filp).table_lock);

        0
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_dumb_map_offset()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `file` must point to an open `DrmFile`.
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `offset` must point to a writable `u64` to receive the allocated mmap offset.
pub unsafe extern "C" fn drm_gem_dumb_map_offset(
    file: *mut DrmFile,
    _dev: *mut DrmDevice,
    handle: u32,
    offset: *mut u64,
) -> i32 {
    // SAFETY: `file` is an open `DrmFile` per this function's `# Safety`; the lookup returns a referenced object that is released below.
    let obj = unsafe { drm_gem_object_lookup(file, handle) };
    if obj.is_null() {
        return -ENOENT;
    }
    // SAFETY: `obj` was returned by the lookup above with a reference held, and it is checked non-null, so it is live for offset creation.
    let ret = unsafe { drm_gem_create_mmap_offset(obj) };
    if ret == 0 && !offset.is_null() {
        // SAFETY: `offset` is non-null and writable; store the page-shifted VM start address.
        unsafe {
            *offset = (*obj).vma_node.vm_node.start << PAGE_SHIFT;
        }
    }
    // SAFETY: Releases the reference taken by `drm_gem_object_lookup` above; `obj` is non-null (checked) and is not used afterwards.
    unsafe { drm_gem_object_put(obj) };
    ret
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_handle_create_tail()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `file_priv` must point to an open `DrmFile`.
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - `handlep` must point to a writable `u32` to receive the allocated handle ID.
pub unsafe extern "C" fn drm_gem_handle_create_tail(
    file_priv: *mut DrmFile,
    obj: *mut DrmGemObject,
    handlep: *mut u32,
) -> i32 {
    if file_priv.is_null() || obj.is_null() || handlep.is_null() {
        return -EINVAL;
    }
    // SAFETY: Retain GEM object reference, allocate IDR slot under `table_lock`, grant VMA access, and invoke open callback.
    unsafe {
        // Retain the object before reserving an ID, so every error path can
        // unwind the handle reference consistently.
        drm_gem_object_handle_get(obj);
        spin_lock(&mut (*file_priv).table_lock);
        let ret = idr_alloc(
            core::ptr::addr_of_mut!((*file_priv).object_idr),
            core::ptr::null_mut(),
            1,
            0,
        );
        spin_unlock(&mut (*file_priv).table_lock);

        mutex_unlock(core::ptr::addr_of_mut!((*(*obj).dev).object_name_lock));

        if ret < 0 {
            drm_gem_object_handle_put_unlocked(obj);
            return ret;
        }

        let handle = ret as u32;

        // A GEM handle is also the authority to mmap its object's VMA offset.
        // Grant that permission before publishing the handle to the caller.
        let allow_ret = drm_vma_node_allow(core::ptr::addr_of_mut!((*obj).vma_node), file_priv);
        if allow_ret != 0 {
            spin_lock(&mut (*file_priv).table_lock);
            idr_remove(
                core::ptr::addr_of_mut!((*file_priv).object_idr),
                handle as i32,
            );
            spin_unlock(&mut (*file_priv).table_lock);
            drm_gem_object_handle_put_unlocked(obj);
            return allow_ret;
        }

        if let Some(funcs) = (*obj).funcs.as_ref()
            && let Some(open) = funcs.open
        {
            let open_ret = open(obj, file_priv);
            if open_ret != 0 {
                drm_vma_node_revoke(core::ptr::addr_of_mut!((*obj).vma_node), file_priv);
                spin_lock(&mut (*file_priv).table_lock);
                idr_remove(
                    core::ptr::addr_of_mut!((*file_priv).object_idr),
                    handle as i32,
                );
                spin_unlock(&mut (*file_priv).table_lock);
                drm_gem_object_handle_put_unlocked(obj);
                return open_ret;
            }
        }

        // Keep the reserved ID invisible to lookup until VMA permission and
        // the driver's per-file open hook are both fully established.
        spin_lock(&mut (*file_priv).table_lock);
        let replaced = idr_replace(
            core::ptr::addr_of_mut!((*file_priv).object_idr),
            obj.cast(),
            handle as i32,
        );
        spin_unlock(&mut (*file_priv).table_lock);
        debug_assert!(
            replaced.is_null(),
            "reserved GEM handle was unexpectedly occupied"
        );

        *handlep = handle;
        0
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_handle_create()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `file_priv` must point to an open `DrmFile`.
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - `handlep` must point to a writable `u32` to receive the allocated handle ID.
pub unsafe extern "C" fn drm_gem_handle_create(
    file_priv: *mut DrmFile,
    obj: *mut DrmGemObject,
    handlep: *mut u32,
) -> i32 {
    if file_priv.is_null() || obj.is_null() || handlep.is_null() {
        return -EINVAL;
    }
    // SAFETY: `obj` and `obj->dev` are valid; acquire `object_name_lock` to serialize handle creation.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*(*obj).dev).object_name_lock));
        drm_gem_handle_create_tail(file_priv, obj, handlep)
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_free_mmap_offset()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid `DrmGemObject` whose `dev` field points to a live `DrmDevice`
/// with initialized `vma_offset_manager`.
pub unsafe extern "C" fn drm_gem_free_mmap_offset(obj: *mut DrmGemObject) {
    if obj.is_null()
        // SAFETY: Inspect `dev` field of `obj`.
        || unsafe {
            (*obj).dev.is_null()
        }
    {
        return;
    }

    // SAFETY: `obj` is live per this function's `# Safety`, and `dev` was checked non-null by the guard above, so the manager field can be read.
    let vma_mgr = unsafe { (*(*obj).dev).vma_offset_manager };
    if !vma_mgr.is_null() {
        // SAFETY: `vma_mgr` is non-null (checked above), and `vma_node` is the node that was added to that manager.
        unsafe { drm_vma_offset_remove(vma_mgr, core::ptr::addr_of_mut!((*obj).vma_node)) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_create_mmap_offset_size()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid `DrmGemObject` whose `dev` points to a live `DrmDevice`
/// with initialized `vma_offset_manager`.
pub unsafe extern "C" fn drm_gem_create_mmap_offset_size(
    obj: *mut DrmGemObject,
    size: usize,
) -> i32 {
    if obj.is_null()
        // SAFETY: Inspect `dev` field of `obj`.
        || unsafe {
            (*obj).dev.is_null()
        }
    {
        return -EINVAL;
    }

    // SAFETY: `obj` is live per this function's `# Safety`, and `dev` was checked non-null by the guard above, so the manager field can be read.
    let vma_mgr = unsafe { (*(*obj).dev).vma_offset_manager };
    if vma_mgr.is_null() {
        return -EINVAL;
    }
    // SAFETY: `vma_mgr` and `obj->vma_node` are valid; add range to offset manager.
    unsafe {
        drm_vma_offset_add(
            vma_mgr,
            core::ptr::addr_of_mut!((*obj).vma_node),
            (size / PAGE_SIZE) as c_ulong,
        )
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_create_mmap_offset()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid `DrmGemObject` whose `dev` points to a live `DrmDevice`
/// with initialized `vma_offset_manager`.
pub unsafe extern "C" fn drm_gem_create_mmap_offset(obj: *mut DrmGemObject) -> i32 {
    if obj.is_null() {
        return -EINVAL;
    }
    // SAFETY: `obj` is checked non-null; read `size` and allocate mmap offset.
    unsafe { drm_gem_create_mmap_offset_size(obj, (*obj).size) }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_get_pages()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid `DrmGemObject` backed by shmem/page allocations.
pub unsafe extern "C" fn drm_gem_get_pages(obj: *mut DrmGemObject) -> *mut *mut c_void {
    if obj.is_null() {
        return (-EINVAL as isize) as *mut *mut c_void;
    }
    // SAFETY: `obj` is non-null (checked above) and live per this function's `# Safety`; `size` is read to size the page array.
    let npages = unsafe { (*obj).size } / PAGE_SIZE;
    if npages == 0 {
        return (-EINVAL as isize) as *mut *mut c_void;
    }
    let pages =
        kmalloc(npages * core::mem::size_of::<*mut c_void>(), GFP_KERNEL).cast::<*mut c_void>();
    if pages.is_null() {
        return (-ENOMEM as isize) as *mut *mut c_void;
    }
    // SAFETY: `pages` is non-null (checked above) and was allocated for `npages` pointer slots, so zeroing `npages` entries stays in bounds.
    unsafe { core::ptr::write_bytes(pages, 0, npages) };
    pages
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_put_pages()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmGemObject`.
/// - `pages` must point to the page array previously returned by `drm_gem_get_pages` for this object.
pub unsafe extern "C" fn drm_gem_put_pages(
    _obj: *mut DrmGemObject,
    pages: *mut *mut c_void,
    _dirty: bool,
    _accessed: bool,
) {
    if !pages.is_null() {
        // SAFETY: `pages` is non-null (checked above) and is the array returned by `drm_gem_get_pages`, so freeing it releases that allocation.
        unsafe { kfree(pages.cast::<c_void>()) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c objects_lookup()
/// # Safety
///
/// `filp` must point to a live `DrmFile`, `handles` must point to `count` u32s,
/// and `objs` must point to `count` writable `*mut DrmGemObject` slots.
unsafe fn objects_lookup(
    filp: *mut DrmFile,
    handles: *const u32,
    count: c_int,
    objs: *mut *mut DrmGemObject,
) -> i32 {
    // SAFETY: `filp` is live per this function's `# Safety`; `table_lock` guards `object_idr`, and it stays held until the lookup loop finishes.
    unsafe { spin_lock(&mut (*filp).table_lock) };
    for i in 0..count as usize {
        // SAFETY: `handles` holds `count` entries per this function's `# Safety`, and the loop index `i` is below `count`, so `add(i)` is in bounds.
        let handle = unsafe { *handles.add(i) };
        // SAFETY: `object_idr` is a field of the live `filp`, and the `table_lock` held above stops the IDR from changing during this lookup.
        let obj = unsafe {
            idr_find(
                core::ptr::addr_of_mut!((*filp).object_idr),
                handle as c_ulong,
            )
            .cast::<DrmGemObject>()
        };
        if obj.is_null() {
            // SAFETY: Drops the `table_lock` acquired at the top of this function before returning the error path.
            unsafe { spin_unlock(&mut (*filp).table_lock) };
            for j in 0..i {
                // SAFETY: Entries `0..i` were filled by earlier iterations, each holding a reference taken by `drm_gem_object_get`, so each one is released here.
                unsafe { drm_gem_object_put(*objs.add(j)) };
            }
            return -ENOENT;
        }
        // SAFETY: `obj` was found in the IDR under the lock held above, so it is non-null and live; the reference is taken before the lock is released.
        unsafe { drm_gem_object_get(obj) };
        // SAFETY: `objs` has room for `count` entries per this function's `# Safety`, and `i` is below `count`; the reference taken above is stored for the caller.
        unsafe {
            *objs.add(i) = obj;
        }
    }
    // SAFETY: Releases the `table_lock` taken at the top of this function, after every handle has been looked up and referenced.
    unsafe { spin_unlock(&mut (*filp).table_lock) };
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_objects_lookup()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `filp` must point to an open `DrmFile`.
/// - `bo_handles` must point to an array of `count` `u32` handle integers.
/// - `objs` must point to an array of at least `count` `*mut DrmGemObject` pointers.
///
/// On success, caller owns a reference to each returned non-null GEM object.
pub unsafe extern "C" fn drm_gem_objects_lookup(
    filp: *mut DrmFile,
    bo_handles: *const u32,
    count: c_int,
    objs_out: *mut *mut *mut DrmGemObject,
) -> i32 {
    if objs_out.is_null() {
        return -EINVAL;
    }
    // SAFETY: `objs_out` is non-null and writable; initialize output pointer to null.
    unsafe {
        *objs_out = core::ptr::null_mut();
    }
    if count <= 0 || filp.is_null() || bo_handles.is_null() {
        return 0;
    }
    let objs = kmalloc(
        count as usize * core::mem::size_of::<*mut DrmGemObject>(),
        GFP_KERNEL,
    )
    .cast::<*mut DrmGemObject>();
    if objs.is_null() {
        return -ENOMEM;
    }
    // SAFETY: `objs` is the array of `count` slots allocated above and checked non-null; `filp` and `bo_handles` are the caller's pointers from this function's `# Safety`.
    let ret = unsafe { objects_lookup(filp, bo_handles, count, objs) };
    if ret != 0 {
        // SAFETY: `objs` is non-null (checked above) and was allocated by this function; `objects_lookup` already released any references it took on failure.
        unsafe { kfree(objs.cast::<c_void>()) };
        return ret;
    }
    // SAFETY: `objs_out` is non-null and writable; store allocated object array.
    unsafe {
        *objs_out = objs;
    }
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_lookup()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to an open `DrmFile`.
/// On success, returns a referenced `DrmGemObject` pointer that caller must release via `drm_gem_object_put`.
pub unsafe extern "C" fn drm_gem_object_lookup(
    filp: *mut DrmFile,
    handle: u32,
) -> *mut DrmGemObject {
    if filp.is_null() || handle == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `filp` is checked non-null; acquire `table_lock` and look up object in IDR.
    unsafe {
        spin_lock(&mut (*filp).table_lock);
        let obj = idr_find(
            core::ptr::addr_of_mut!((*filp).object_idr),
            handle as c_ulong,
        )
        .cast::<DrmGemObject>();
        if !obj.is_null() {
            drm_gem_object_get(obj);
        }
        spin_unlock(&mut (*filp).table_lock);
        obj
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_dma_resv_wait()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `file_priv` must point to an open `DrmFile`.
/// - `dev` must point to a valid, live `DrmDevice`.
pub unsafe extern "C" fn drm_gem_dma_resv_wait(
    filep: *mut DrmFile,
    handle: u32,
    _wait_all: bool,
    _timeout: c_ulong,
) -> c_long {
    // SAFETY: `filep` is the open `DrmFile` required by this function's `# Safety`; the returned object carries a reference that is dropped below.
    let obj = unsafe { drm_gem_object_lookup(filep, handle) };
    if obj.is_null() {
        return -EINVAL as c_long;
    }
    // SAFETY: Drops the reference returned by the lookup above; `obj` is checked non-null and is not used afterwards.
    unsafe { drm_gem_object_put(obj) };
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_close_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable `DrmGemClose` ioctl payload.
/// - `file_priv` must point to an open `DrmFile`.
pub unsafe extern "C" fn drm_gem_close_ioctl(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let args = data.cast::<DrmGemClose>();
    if args.is_null() || file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: `args` and `file_priv` are checked non-null; delete the GEM handle.
    unsafe { drm_gem_handle_delete(file_priv, (*args).handle) }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_flink_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmGemFlink` ioctl payload.
/// - `file_priv` must point to an open `DrmFile`.
pub unsafe extern "C" fn drm_gem_flink_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let args = data.cast::<DrmGemFlink>();
    if dev.is_null() || data.is_null() || file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: Look up GEM object by handle, assign or read flink name under `object_name_lock`, and drop reference.
    unsafe {
        let obj = drm_gem_object_lookup(file_priv, (*args).handle);
        if obj.is_null() {
            return -ENOENT;
        }

        mutex_lock(core::ptr::addr_of_mut!((*dev).object_name_lock));
        let ret = if (*obj).name != 0 {
            (*args).name = (*obj).name as u32;
            0
        } else {
            let r = idr_alloc(
                core::ptr::addr_of_mut!((*dev).object_name_idr),
                obj.cast(),
                1,
                0,
            );
            if r >= 0 {
                let name = r as c_int;
                (*obj).name = name;
                (*args).name = name as u32;
                0
            } else {
                r
            }
        };
        mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock));

        drm_gem_object_put(obj);
        ret
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_open_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmGemOpen` ioctl payload.
/// - `file_priv` must point to an open `DrmFile`.
pub unsafe extern "C" fn drm_gem_open_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let args = data.cast::<DrmGemOpen>();
    if dev.is_null() || data.is_null() || file_priv.is_null() {
        return -EINVAL;
    }
    // SAFETY: Look up GEM object by global name under `object_name_lock`, create handle for `file_priv`, and drop reference.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).object_name_lock));
        let obj = idr_find(
            core::ptr::addr_of_mut!((*dev).object_name_idr),
            (*args).name as c_ulong,
        )
        .cast::<DrmGemObject>();
        if obj.is_null() {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock));
            return -ENOENT;
        }
        drm_gem_object_get(obj);
        mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock));

        let ret = drm_gem_handle_create(file_priv, obj, core::ptr::addr_of_mut!((*args).handle));
        (*args).size = (*obj).size as u64;
        drm_gem_object_put(obj);
        ret
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_change_handle_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable `DrmGemChangeHandle` ioctl payload.
/// - `file_priv` must point to an open `DrmFile`.
pub unsafe extern "C" fn drm_gem_change_handle_ioctl(
    _dev: *mut DrmDevice,
    _data: *mut c_void,
    _file_priv: *mut DrmFile,
) -> i32 {
    -EOPNOTSUPP
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_open()
#[unsafe(no_mangle)]
/// # Safety
///
/// `file_private` must point to a newly allocated `DrmFile` initializing its handle IDR.
pub unsafe extern "C" fn drm_gem_open(_dev: *mut DrmDevice, file_private: *mut DrmFile) {
    if !file_private.is_null() {
        // SAFETY: `file_private` is checked non-null; initialize its `object_idr`.
        unsafe {
            crate::drm_mode_object_upstream::idr_init(core::ptr::addr_of_mut!(
                (*file_private).object_idr
            ))
        };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_release()
#[unsafe(no_mangle)]
/// # Safety
///
/// `file_private` must point to an open `DrmFile` being closed; caller must guarantee
/// no concurrent handle operations are in progress on this file.
pub unsafe extern "C" fn drm_gem_release(_dev: *mut DrmDevice, file_private: *mut DrmFile) {
    if !file_private.is_null() {
        // SAFETY: `file_private` is checked non-null; release all handles in `object_idr`.
        unsafe {
            crate::drm_mode_object_upstream::idr_for_each(
                core::ptr::addr_of_mut!((*file_private).object_idr),
                |id, ptr| {
                    drm_gem_object_release_handle(id, ptr, file_private.cast());
                },
            )
        };
        // SAFETY: `file_private` is non-null (checked above), and its handle IDR was emptied by the `idr_for_each` call above; `idr_destroy` frees only the IDR storage.
        unsafe { idr_destroy(core::ptr::addr_of_mut!((*file_private).object_idr)) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_release()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid `DrmGemObject` being torn down whose reference count is zero.
pub unsafe extern "C" fn drm_gem_object_release(obj: *mut DrmGemObject) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is checked non-null; free mmap offset if dev is non-null and finalize reservation lock.
    unsafe {
        if !(*obj).dev.is_null() {
            drm_gem_free_mmap_offset(obj);
        }
        dma_resv_fini(core::ptr::addr_of_mut!((*obj)._resv));
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_free()
#[unsafe(no_mangle)]
/// # Safety
///
/// `kref` must point to the embedded `refcount` member of a `DrmGemObject` whose reference count reached zero.
pub unsafe extern "C" fn drm_gem_object_free(kref: *mut Kref) {
    let obj = kref
        .cast::<u8>()
        .wrapping_sub(core::mem::offset_of!(DrmGemObject, refcount))
        .cast::<DrmGemObject>();
    // SAFETY: `kref` container offset yields live `obj`; invoke driver `free` callback or release and kfree.
    unsafe {
        if let Some(funcs) = (*obj).funcs.as_ref()
            && let Some(free) = funcs.free
        {
            free(obj);
            return;
        }
        drm_gem_object_release(obj);
        kfree(obj.cast::<c_void>());
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_get()
#[inline]
/// # Safety
///
/// `obj` must point to a valid, live `DrmGemObject` with a non-zero reference count.
pub unsafe fn drm_gem_object_get(obj: *mut DrmGemObject) {
    if !obj.is_null() {
        // SAFETY: `obj` is checked non-null and caller holds live reference; increment kref.
        unsafe {
            drm_kapi::memory::kref_get(core::ptr::addr_of_mut!((*obj).refcount));
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_object_put()
#[inline]
/// # Safety
///
/// `obj` must be null or point to a valid, live `DrmGemObject` with at least one reference owned by caller.
pub unsafe fn drm_gem_object_put(obj: *mut DrmGemObject) {
    if !obj.is_null() {
        // SAFETY: `obj` is checked non-null; decrement kref, releasing via `drm_gem_object_free` when zero.
        unsafe {
            kref_put(
                core::ptr::addr_of_mut!((*obj).refcount),
                drm_gem_object_free,
            );
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_vm_open()
#[unsafe(no_mangle)]
/// # Safety
///
/// `vma` must point to a valid Linux `vm_area_struct` whose `vm_private_data` points to a live `DrmGemObject`.
pub unsafe extern "C" fn drm_gem_vm_open(vma: *mut c_void) {
    if !vma.is_null() {
        // vma->vm_private_data is DrmGemObject
        // SAFETY: `vma` is non-null (checked above), and its `vm_private_data` slot holds the object pointer set when the mapping was created.
        let obj = unsafe { *(vma.cast::<*mut DrmGemObject>()) };
        // SAFETY: The VMA is gaining a reference to `obj`, which `vm_open` takes for each mapping copy; `obj` is the pointer read above.
        unsafe { drm_gem_object_get(obj) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_vm_close()
#[unsafe(no_mangle)]
/// # Safety
///
/// `vma` must point to a valid Linux `vm_area_struct` whose `vm_private_data` points to a live `DrmGemObject`.
pub unsafe extern "C" fn drm_gem_vm_close(vma: *mut c_void) {
    if !vma.is_null() {
        // SAFETY: `vma` is non-null (checked above), and its `vm_private_data` slot holds the object pointer set when the mapping was created.
        let obj = unsafe { *(vma.cast::<*mut DrmGemObject>()) };
        // SAFETY: Drops the reference that `drm_gem_vm_open` took for this VMA, pairing with the reference taken there.
        unsafe { drm_gem_object_put(obj) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_mmap_obj()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - `vma` must point to a valid Linux `vm_area_struct` representing the target user address mapping.
pub unsafe extern "C" fn drm_gem_mmap_obj(
    obj: *mut DrmGemObject,
    obj_size: c_ulong,
    vma: *mut c_void,
) -> i32 {
    if obj.is_null()
        || vma.is_null()
        || (obj_size as usize)
            // SAFETY: Read `obj->size` to compare with requested mapping length.
            < unsafe {
                (*obj).size
            }
    {
        return -EINVAL;
    }
    // SAFETY: `obj` is checked non-null above and stays live for the mapping per this function's `# Safety`; the reference is dropped again if the driver mmap fails.
    unsafe { drm_gem_object_get(obj) };
    // SAFETY: `obj` is live per this function's `# Safety`; a null `funcs` pointer is reported as `None` by `as_ref`, so no driver callback runs.
    if let Some(funcs) = unsafe { (*obj).funcs.as_ref() }
        && let Some(mmap) = funcs.mmap
    {
        // SAFETY: `mmap` was read from the object's function table, and `obj` and `vma` are the pair that callback expects per this function's `# Safety`.
        let ret = unsafe { mmap(obj, vma) };
        if ret != 0 {
            // SAFETY: The driver's `mmap` failed, so the reference taken above for this mapping is released here.
            unsafe { drm_gem_object_put(obj) };
            return ret;
        }
    }
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_get_unmapped_area()
#[unsafe(no_mangle)]
/// # Safety
///
/// `filp` must point to an open Linux `struct file` whose `private_data` is a valid `DrmFile`.
pub unsafe extern "C" fn drm_gem_get_unmapped_area(
    _filp: *mut c_void,
    _uaddr: c_ulong,
    _len: c_ulong,
    _pgoff: c_ulong,
    _flags: c_ulong,
) -> c_ulong {
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_mmap()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `filp` must point to an open Linux `struct file` whose `private_data` is a valid `DrmFile`.
/// - `vma` must point to a valid Linux `vm_area_struct` requesting an mmap range.
pub unsafe extern "C" fn drm_gem_mmap(filp: *mut c_void, vma: *mut c_void) -> i32 {
    if filp.is_null() || vma.is_null() {
        return -EINVAL;
    }
    0
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_print_info()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to a valid, initialized `DrmPrinter`.
/// - `obj` must point to a valid, readable `DrmGemObject`.
pub unsafe extern "C" fn drm_gem_print_info(
    p: *mut DrmPrinter,
    indent: c_uint,
    obj: *const DrmGemObject,
) {
    if p.is_null() || obj.is_null() {
        return;
    }
    // SAFETY: `obj` is checked non-null above and readable per this function's `# Safety`; a null `funcs` pointer is reported as `None`.
    if let Some(funcs) = unsafe { (*obj).funcs.as_ref() }
        && let Some(print_info) = funcs.print_info
    {
        // SAFETY: `p` is the printer and `obj` the readable object from this function's `# Safety`; the driver callback reads both and does not keep them.
        unsafe { print_info(p, indent, obj.cast_mut()) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_vmap_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmGemObject`.
/// - Caller must hold the reservation lock on `obj`.
/// - `map` must point to a valid `struct iosys_map` to receive mapping information.
pub unsafe extern "C" fn drm_gem_vmap_locked(obj: *mut DrmGemObject, map: *mut c_void) -> i32 {
    if obj.is_null() || map.is_null() {
        return -EINVAL;
    }
    // SAFETY: `obj` is checked non-null above and live per this function's `# Safety`; its optional function table is read here.
    if let Some(funcs) = unsafe { (*obj).funcs.as_ref() }
        && let Some(vmap) = funcs.vmap
    {
        // SAFETY: `vmap` comes from the object's function table, and `map` was checked non-null above; the caller holds the reservation lock that `vmap` requires.
        return unsafe { vmap(obj, map.cast()) };
    }
    -EOPNOTSUPP
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_vunmap_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmGemObject`.
/// - Caller must hold the reservation lock on `obj`.
/// - `map` must point to a valid `struct iosys_map` previously initialized by `drm_gem_vmap_locked`.
pub unsafe extern "C" fn drm_gem_vunmap_locked(obj: *mut DrmGemObject, map: *mut c_void) {
    if obj.is_null() || map.is_null() {
        return;
    }
    let map = map.cast::<crate::iosys_map_types::IosysMap>();
    // SAFETY: `map` is non-null (checked above) and points to the caller's `IosysMap` per this function's `# Safety`; `vaddr` tells whether a mapping exists.
    if unsafe { (*map)._anon_0.vaddr.is_null() } {
        return;
    }
    // SAFETY: `obj` is checked non-null above and live per this function's `# Safety`; its optional function table is read for the unmap callback.
    if let Some(funcs) = unsafe { (*obj).funcs.as_ref() }
        && let Some(vunmap) = funcs.vunmap
    {
        // SAFETY: `vunmap` comes from the object's function table, and `map` still holds the address that `vmap` recorded (checked non-null above).
        unsafe { vunmap(obj, map.cast()) };
    }
    // Linux iosys_map_clear(): make repeated unmaps harmless and ensure a
    // later map starts from a canonical empty state, even without a callback.
    // SAFETY: `map` is non-null (checked above) and is the caller's `IosysMap`, so clearing one `IosysMap` in place stays in bounds.
    unsafe { core::ptr::write_bytes(map, 0, 1) };
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lock()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid, live `DrmGemObject` whose reservation lock is initialized.
pub unsafe extern "C" fn drm_gem_lock(obj: *mut DrmGemObject) {
    if !obj.is_null() {
        // SAFETY: `obj` is non-null (checked above) and live per this function's `# Safety`; `resv` is its embedded reservation object, locked without a WW context.
        unsafe { dma_resv_lock((*obj).resv, core::ptr::null_mut()) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_unlock()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid, live `DrmGemObject` whose reservation lock was locked by caller.
pub unsafe extern "C" fn drm_gem_unlock(obj: *mut DrmGemObject) {
    if !obj.is_null() {
        // SAFETY: `obj` is non-null (checked above) and live; `resv` is the reservation lock that the caller took through `drm_gem_lock`.
        unsafe { dma_resv_unlock((*obj).resv) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_vmap()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - `map` must point to a valid `struct iosys_map` to receive mapping information.
pub unsafe extern "C" fn drm_gem_vmap(obj: *mut DrmGemObject, map: *mut c_void) -> i32 {
    if obj.is_null() {
        return -EINVAL;
    }
    // SAFETY: `obj` was checked non-null above and is live per this function's `# Safety`; the reservation lock is held until `drm_gem_unlock` below.
    unsafe { drm_gem_lock(obj) };
    // SAFETY: The reservation lock taken above covers `obj` for this call, which `drm_gem_vmap_locked` requires; `map` is the caller's output per `# Safety`.
    let ret = unsafe { drm_gem_vmap_locked(obj, map) };
    // SAFETY: Releases the reservation lock taken by `drm_gem_lock` above on the same `obj`.
    unsafe { drm_gem_unlock(obj) };
    ret
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_vunmap()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - `map` must point to a valid `struct iosys_map` previously initialized by `drm_gem_vmap`.
pub unsafe extern "C" fn drm_gem_vunmap(obj: *mut DrmGemObject, map: *mut c_void) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is non-null (checked above) and live per this function's `# Safety`; the unmap runs under the reservation lock taken here.
    unsafe { drm_gem_lock(obj) };
    // SAFETY: The reservation lock taken above is the precondition of `drm_gem_vunmap_locked`; `map` was filled earlier by `drm_gem_vmap`.
    unsafe { drm_gem_vunmap_locked(obj, map) };
    // SAFETY: Releases the reservation lock taken above on the same `obj`.
    unsafe { drm_gem_unlock(obj) };
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lock_reservations()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `objs` must point to an array of `count` valid `DrmGemObject` pointers.
/// - `acquire_ctx` must point to an initialized WW acquire context.
pub unsafe extern "C" fn drm_gem_lock_reservations(
    objs: *mut *mut DrmGemObject,
    count: c_int,
    acquire_ctx: *mut c_void,
) -> i32 {
    if objs.is_null() || count <= 0 {
        return 0;
    }
    // SAFETY: `objs` points to `count` GEM object pointers; acquire reservation lock on each, unwinding on failure.
    unsafe {
        for i in 0..count {
            let obj = *objs.offset(i as isize);
            if !obj.is_null() {
                let ret = dma_resv_lock((*obj).resv, acquire_ctx.cast());
                if ret != 0 {
                    for j in 0..i {
                        let prev = *objs.offset(j as isize);
                        if !prev.is_null() {
                            dma_resv_unlock((*prev).resv);
                        }
                    }
                    return ret;
                }
            }
        }
        0
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_unlock_reservations()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `objs` must point to an array of `count` valid `DrmGemObject` pointers locked with `acquire_ctx`.
/// - `acquire_ctx` must point to the WW acquire context used to acquire them.
pub unsafe extern "C" fn drm_gem_unlock_reservations(
    objs: *mut *mut DrmGemObject,
    count: c_int,
    _acquire_ctx: *mut c_void,
) {
    if objs.is_null() || count <= 0 {
        return;
    }
    // SAFETY: `objs` points to `count` GEM object pointers whose reservation locks are held; unlock each.
    unsafe {
        for i in 0..count {
            let obj = *objs.offset(i as isize);
            if !obj.is_null() {
                dma_resv_unlock((*obj).resv);
            }
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lru_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// `lru` must point to allocated, writable storage of a `DrmGemLru` structure.
pub unsafe extern "C" fn drm_gem_lru_init(lru: *mut DrmGemLru) {
    if !lru.is_null() {
        // SAFETY: `lru` is checked non-null; initialize intrusive list head and zero count.
        unsafe {
            (*lru).count = 0;
            INIT_LIST_HEAD(core::ptr::addr_of_mut!((*lru).list));
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lru_remove_locked()
/// # Safety
///
/// `obj` must point to a live `DrmGemObject`, and caller must hold `dev->gem_lru_mutex`.
unsafe fn drm_gem_lru_remove_locked(obj: *mut DrmGemObject) {
    if !obj.is_null()
        // SAFETY: `obj` is checked non-null; check if object is linked to an LRU list.
        && !unsafe {
            (*obj).lru.is_null()
        }
    {
        // SAFETY: `obj` is non-null and linked (checked above); the caller holds `gem_lru_mutex` per this function's `# Safety`, so `lru` cannot change under us.
        let lru = unsafe { (*obj).lru };
        // SAFETY: `lru` came from a linked object and is non-null; subtracting the object's page count undoes the earlier add under the same mutex.
        unsafe {
            (*lru).count -= ((*obj).size >> PAGE_SHIFT) as i64;
        }
        // SAFETY: `lru_node` is linked on the list that `lru` points to, and the caller holds the LRU lock, so unlinking it here is valid.
        unsafe { list_del_init(core::ptr::addr_of_mut!((*obj).lru_node)) };
        // SAFETY: Clears the back-pointer after unlinking, so a later `drm_gem_lru_remove_locked` sees the object as no longer on any list.
        unsafe {
            (*obj).lru = core::ptr::null_mut();
        }
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lru_remove()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to a valid, live `DrmGemObject` being removed from its LRU list.
pub unsafe extern "C" fn drm_gem_lru_remove(obj: *mut DrmGemObject) {
    if !obj.is_null()
        // SAFETY: `obj` is checked non-null; check if object is linked to an LRU list.
        && !unsafe {
            (*obj).lru.is_null()
        }
    {
        // SAFETY: `obj` is non-null and linked to an LRU (checked above), so its `dev` pointer is valid under this function's `# Safety`.
        let dev = unsafe { (*obj).dev };
        // SAFETY: `dev` owns this object and was read above; `gem_lru_mutex` serializes LRU list edits, so it is taken before the unlink.
        unsafe { mutex_lock(core::ptr::addr_of_mut!((*dev).gem_lru_mutex)) };
        // SAFETY: Called with `gem_lru_mutex` held by the lock above, which is the precondition of the locked helper.
        unsafe { drm_gem_lru_remove_locked(obj) };
        // SAFETY: Releases the `gem_lru_mutex` taken above on the same `dev`.
        unsafe { mutex_unlock(core::ptr::addr_of_mut!((*dev).gem_lru_mutex)) };
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lru_move_tail_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `lru` must point to an initialized `DrmGemLru`.
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - Caller must hold the LRU's spinlock/mutex protecting `lru->list`.
pub unsafe extern "C" fn drm_gem_lru_move_tail_locked(lru: *mut DrmGemLru, obj: *mut DrmGemObject) {
    if lru.is_null() || obj.is_null() {
        return;
    }
    // SAFETY: `lru` and `obj` are checked non-null; link object intrusive node to tail of LRU under caller lock.
    unsafe {
        if !(*obj).lru.is_null() {
            drm_gem_lru_remove_locked(obj);
        }
        (*lru).count += ((*obj).size >> PAGE_SHIFT) as i64;
        list_add_tail(
            core::ptr::addr_of_mut!((*obj).lru_node),
            core::ptr::addr_of_mut!((*lru).list),
        );
        (*obj).lru = lru;
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lru_move_tail()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `lru` must point to an initialized `DrmGemLru`.
/// - `obj` must point to a valid, live `DrmGemObject`.
pub unsafe extern "C" fn drm_gem_lru_move_tail(lru: *mut DrmGemLru, obj: *mut DrmGemObject) {
    if lru.is_null() || obj.is_null() {
        return;
    }
    // SAFETY: `obj` is checked non-null; acquire `gem_lru_mutex` around `drm_gem_lru_move_tail_locked`.
    unsafe {
        let dev = (*obj).dev;
        mutex_lock(core::ptr::addr_of_mut!((*dev).gem_lru_mutex));
        drm_gem_lru_move_tail_locked(lru, obj);
        mutex_unlock(core::ptr::addr_of_mut!((*dev).gem_lru_mutex));
    }
}

// upstream: drivers/gpu/drm/drm_gem.c drm_gem_lru_scan()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice` whose `gem_lru_mutex` is initialized.
/// - `lru` must point to an initialized `DrmGemLru` that belongs to `dev`.
/// - `remaining` must be null or point to a writable `c_ulong` accumulator.
/// - `shrink`, when set, must accept a live `DrmGemObject` and `ticket` as its arguments.
pub unsafe extern "C" fn drm_gem_lru_scan(
    dev: *mut DrmDevice,
    lru: *mut DrmGemLru,
    nr_to_scan: c_uint,
    remaining: *mut c_ulong,
    shrink: Option<unsafe extern "C" fn(*mut DrmGemObject, *mut c_void) -> bool>,
    ticket: *mut c_void,
) -> c_ulong {
    if dev.is_null() || lru.is_null() {
        return 0;
    }
    let mut freed: c_ulong = 0;
    // SAFETY: `dev` is non-null (checked above); `gem_lru_mutex` is taken so the LRU list can be walked and unlinked without concurrent edits.
    unsafe { mutex_lock(core::ptr::addr_of_mut!((*dev).gem_lru_mutex)) };
    while (freed as c_uint) < nr_to_scan
        // SAFETY: Check if LRU list is empty under `gem_lru_mutex`.
        && !unsafe {
            list_empty(&(*lru).list)
        }
    {
        // SAFETY: `lru` is non-null (checked above) and initialized per this function's `# Safety`; the loop condition ensures the list is non-empty.
        let cur = unsafe { (*lru).list.next };
        let obj = cur
            .cast::<u8>()
            .wrapping_sub(core::mem::offset_of!(DrmGemObject, lru_node))
            .cast::<DrmGemObject>();
        // SAFETY: `obj` is the object embedding list node `cur`, recovered by `offset_of!`; the mutex is held, as the locked helper requires.
        unsafe { drm_gem_lru_remove_locked(obj) };
        if let Some(s) = shrink {
            // SAFETY: `s` is the caller-supplied shrink callback named in this function's `# Safety`; it is called with `obj`, which is no longer on the LRU list.
            if unsafe { s(obj, ticket) } {
                // SAFETY: `obj` is the entry unlinked above and is still live, since the LRU lock is held and the shrink callback has not freed it.
                freed += (unsafe { (*obj).size } >> PAGE_SHIFT) as c_ulong;
            } else if !remaining.is_null() {
                // SAFETY: `remaining` is non-null and writable; accumulate un-shrunk page count.
                unsafe {
                    *remaining += ((*obj).size >> PAGE_SHIFT) as c_ulong;
                }
            }
        }
    }
    // SAFETY: Releases the `gem_lru_mutex` taken before the scan loop on the same `dev`.
    unsafe { mutex_unlock(core::ptr::addr_of_mut!((*dev).gem_lru_mutex)) };
    freed
}

// ustream: drivers/gpu/drm/drm_gem.c drm_gem_evict_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid, live `DrmGemObject`.
/// - Caller must hold the reservation lock on `obj`.
pub unsafe extern "C" fn drm_gem_evict_locked(obj: *mut DrmGemObject) -> i32 {
    if obj.is_null() {
        return -EINVAL;
    }
    // SAFETY: `obj` is non-null (checked above) and live per this function's `# Safety`; its optional function table is read for the evict callback.
    if let Some(funcs) = unsafe { (*obj).funcs.as_ref() }
        && let Some(evict) = funcs.evict
    {
        // SAFETY: `evict` comes from the object's function table; the caller holds the reservation lock on `obj` as this function's `# Safety` requires.
        return unsafe { evict(obj) };
    }
    0
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_file_upstream::{drm_file_alloc, drm_file_free},
        drm_gem_types::DrmGemObjectFuncs,
        iosys_map_types::{IosysMap, IosysMapAnon0},
    };

    static TEST_VUNMAP_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// # Safety
    ///
    /// Test callback; parameters are unused mock pointers.
    unsafe extern "C" fn test_vunmap(_obj: *mut DrmGemObject, _map: *mut IosysMap) {
        TEST_VUNMAP_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn gem_vunmap_clears_iosys_map_and_is_idempotent() {
        // SAFETY: Stack-allocated mock `DrmGemObject` zero-initialized for test.
        let mut object: DrmGemObject = unsafe { core::mem::zeroed() };
        // SAFETY: Stack-allocated mock `DrmGemObjectFuncs` zero-initialized for test.
        let mut funcs: DrmGemObjectFuncs = unsafe { core::mem::zeroed() };
        funcs.vunmap = Some(test_vunmap);
        object.funcs = &mut funcs;
        let mut backing = 0u8;
        let mut map = IosysMap {
            _anon_0: IosysMapAnon0 {
                vaddr: (&mut backing as *mut u8).cast(),
            },
            is_iomem: false,
        };
        TEST_VUNMAP_CALLS.store(0, Ordering::SeqCst);

        // SAFETY: Test exercises `drm_gem_vunmap_locked` with valid stack mocks.
        unsafe {
            drm_gem_vunmap_locked(&mut object, (&mut map as *mut IosysMap).cast());
            assert!(map._anon_0.vaddr.is_null());
            assert!(!map.is_iomem);
            drm_gem_vunmap_locked(&mut object, (&mut map as *mut IosysMap).cast());
        }

        assert_eq!(TEST_VUNMAP_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_gem_handles_flink_and_mmap() {
        crate::test_support::init_runtime();
        // SAFETY: Test exercises GEM handles, flink, and mmap with valid mock structures.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);

            let mut obj_raw: DrmGemObject = core::mem::zeroed();
            let obj = &mut obj_raw as *mut DrmGemObject;
            drm_gem_private_object_init(dev, obj, 4096);

            // 1. Handle creation and lookup
            let mut handle: u32 = 0;
            assert_eq!(drm_gem_handle_create(file_priv, obj, &mut handle), 0);
            assert_ne!(handle, 0);
            assert_eq!((*obj).handle_count, 1);
            assert!(crate::drm_vma_manager_upstream::drm_vma_node_is_allowed(
                core::ptr::addr_of_mut!((*obj).vma_node),
                file_priv,
            ));

            let looked_up = drm_gem_object_lookup(file_priv, handle);
            assert_eq!(looked_up, obj);
            drm_gem_object_put(looked_up);

            // 2. Flink and open by name
            let mut flink_args = DrmGemFlink { handle, name: 0 };
            assert_eq!(
                drm_gem_flink_ioctl(dev, &mut flink_args as *mut _ as *mut _, file_priv),
                0
            );
            assert_ne!(flink_args.name, 0);

            let file_priv2 = drm_file_alloc((*dev).primary);
            let mut open_args = DrmGemOpen {
                name: flink_args.name,
                handle: 0,
                size: 0,
            };
            assert_eq!(
                drm_gem_open_ioctl(dev, &mut open_args as *mut _ as *mut _, file_priv2),
                0
            );
            assert_ne!(open_args.handle, 0);
            assert_eq!(open_args.size, 4096);
            assert_eq!((*obj).handle_count, 2);
            assert!(crate::drm_vma_manager_upstream::drm_vma_node_is_allowed(
                core::ptr::addr_of_mut!((*obj).vma_node),
                file_priv2,
            ));

            // 3. Mmap offset creation
            assert_eq!(drm_gem_create_mmap_offset(obj), 0);
            assert_ne!((*obj).vma_node.vm_node.start, 0);
            drm_gem_free_mmap_offset(obj);

            // 4. LRU management
            let mut lru: DrmGemLru = core::mem::zeroed();
            drm_gem_lru_init(&mut lru);
            drm_gem_lru_move_tail(&mut lru, obj);
            assert_eq!(lru.count, 1); // 4096 >> 12 = 1
            drm_gem_lru_remove(obj);
            assert_eq!(lru.count, 0);

            // 5. Cleanup
            assert_eq!(drm_gem_handle_delete(file_priv2, open_args.handle), 0);
            assert!(!crate::drm_vma_manager_upstream::drm_vma_node_is_allowed(
                core::ptr::addr_of_mut!((*obj).vma_node),
                file_priv2,
            ));
            assert!(crate::drm_vma_manager_upstream::drm_vma_node_is_allowed(
                core::ptr::addr_of_mut!((*obj).vma_node),
                file_priv,
            ));
            assert_eq!(drm_gem_handle_delete(file_priv, handle), 0);
            assert!(!crate::drm_vma_manager_upstream::drm_vma_node_is_allowed(
                core::ptr::addr_of_mut!((*obj).vma_node),
                file_priv,
            ));
            assert_eq!((*obj).handle_count, 0);

            drm_file_free(file_priv2);
            drm_file_free(file_priv);
            drm_gem_object_release(obj);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn test_gem_handle_delete_releases_prime_and_vma() {
        crate::test_support::init_runtime();
        let _serial = crate::drm_syncobj_upstream::test_file_ops::serial();
        crate::drm_syncobj_upstream::test_file_ops::install();
        // SAFETY: Test exercises GEM handle deletion, PRIME buffers, and VMA revoking.
        unsafe {
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);

            let obj = drm_kapi::memory::kzalloc_obj::<DrmGemObject>();
            assert!(!obj.is_null());
            drm_gem_private_object_init(dev, obj, 4096);
            assert_eq!(drm_kapi::memory::kref_read(&(*obj).refcount), 1);

            let mut handle: u32 = 0;
            assert_eq!(drm_gem_handle_create(file_priv, obj, &mut handle), 0);
            assert_ne!(handle, 0);
            assert_eq!(drm_kapi::memory::kref_read(&(*obj).refcount), 2);

            // Export the real GEM object so the PRIME handle owns a real
            // DMA-BUF file reference rather than a synthetic struct field.
            let dmabuf = crate::drm_prime_upstream::drm_gem_prime_export(obj, 0);
            assert!(!drm_kapi::config::IS_ERR(dmabuf));
            assert!(!dmabuf.is_null());
            assert_eq!(drm_kapi::memory::kref_read(&(*obj).refcount), 3);

            mutex_lock(core::ptr::addr_of_mut!((*file_priv).prime.lock));
            assert_eq!(
                crate::drm_prime_upstream::drm_prime_add_buf_handle(
                    core::ptr::addr_of_mut!((*file_priv).prime),
                    dmabuf,
                    handle
                ),
                0
            );
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock));

            // Verify lookup finds it
            mutex_lock(core::ptr::addr_of_mut!((*file_priv).prime.lock));
            let found = crate::drm_prime_upstream::drm_prime_lookup_buf_by_handle(
                core::ptr::addr_of_mut!((*file_priv).prime),
                handle,
            );
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock));
            assert_eq!(found, dmabuf,);

            // Delete handle: triggers drm_gem_object_release_handle -> drm_prime_remove_buf_handle -> dma_buf_put
            assert_eq!(drm_gem_handle_delete(file_priv, handle), 0);
            assert_eq!(drm_kapi::memory::kref_read(&(*obj).refcount), 2);

            // Prime handle is gone
            mutex_lock(core::ptr::addr_of_mut!((*file_priv).prime.lock));
            let found = crate::drm_prime_upstream::drm_prime_lookup_buf_by_handle(
                core::ptr::addr_of_mut!((*file_priv).prime),
                handle,
            );
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock));
            assert!(found.is_null());

            // The exported file still owns one reference, so handle removal
            // must not release the exporter or its retained GEM reference.
            assert_eq!(
                crate::drm_dma_buf_upstream::drm_dma_buf_from_file((*dmabuf).file.cast()),
                Some(dmabuf),
            );
            crate::drm_dma_buf_upstream::dma_buf_put(dmabuf);
            assert_eq!(drm_kapi::memory::kref_read(&(*obj).refcount), 1);

            drm_file_free(file_priv);
            drm_gem_object_put(obj);
            drm_dev_put(dev);
        }
    }
}

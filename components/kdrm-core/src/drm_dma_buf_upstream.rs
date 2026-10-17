// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! File-backed Linux DMA-BUF core operations.
//!
//! This is the small lifetime and attachment core needed by DRM PRIME.  The
//! `struct dma_buf` ABI record remains in `drm_prime_upstream`; its file is an
//! actual drm-kapi anonymous file, so fd lookup and object lifetime use the
//! kernel's file table and file reference count rather than a parallel fd map.

#![allow(non_snake_case)]

use core::{
    ffi::{c_int, c_void},
    mem::size_of,
    ptr,
};

use drm_kapi::{
    config::{EINVAL, ENOMEM, EOPNOTSUPP, ERR_PTR, GFP_KERNEL, IS_ERR, PTR_ERR},
    dma_resv::{dma_resv_fini, dma_resv_init, dma_resv_wait_timeout},
    kernel_services::{anon_inode_getfile, fget},
    list::{INIT_LIST_HEAD, list_add_tail, list_del_init, list_empty},
    locks::{spin_lock_init, spin_lock_raw, spin_unlock_raw},
    memory::{kfree, kmalloc},
    shmem::{File as ShmemFile, FileOperations, Inode, fput_file, get_file_file},
    types::{DmaResv, SgTable},
    ww_mutex::{ww_mutex_lock, ww_mutex_unlock},
};

use crate::drm_prime_upstream::{DmaBuf, DmaBufAttachment, DmaBufExportInfo};

const EOVERFLOW: i32 = 75;
const ETIME: i32 = 62;
const EBADF: i32 = 9;
const DMA_BIDIRECTIONAL: c_int = 0;
const DMA_TO_DEVICE: c_int = 1;
const DMA_RESV_USAGE_WRITE: c_int = 1;
const DMA_RESV_USAGE_READ: c_int = 2;

/// File identity used by `dma_buf_get()`; unlike an fd registry this survives
/// dup/close semantics through the actual LinuxKPI file reference count.
static DMA_BUF_FOPS: FileOperations = FileOperations {
    release: Some(dma_buf_file_release),
    ..FileOperations::empty()
};

/// Resolve a file reference to its DMA-BUF object only when it is owned by the
/// DMA-BUF anonymous-file implementation. Other anon-inode users (notably
/// sync_file) store unrelated data in `private_data` and must not be cast.
///
/// # Safety
/// `file` must be null, an error pointer, or a live `shmem::File` reference.
/// The caller must retain that reference for as long as it uses the returned
/// DMA-BUF pointer; this helper does not acquire an additional reference.
pub unsafe fn drm_dma_buf_from_file(file: *mut ShmemFile) -> Option<*mut DmaBuf> {
    if file.is_null() || IS_ERR(file) {
        return None;
    }

    // SAFETY: `file` is confirmed non-null and not an error pointer; the caller holds an open
    // reference to the ShmemFile across this call, making immutable shared dereference sound.
    let file_ref = unsafe { file.as_ref() }?;
    if file_ref.f_op != ptr::addr_of!(DMA_BUF_FOPS) {
        return None;
    }

    let dmabuf = file_ref._private_data.cast::<DmaBuf>();
    if dmabuf.is_null() || IS_ERR(dmabuf) {
        return None;
    }
    // SAFETY: `file.f_op` matched `DMA_BUF_FOPS`, so `_private_data` points to the `DmaBuf`
    // allocated during export whose lifetime is tied to `file`.
    let dmabuf_ref = unsafe { &*dmabuf };
    if dmabuf_ref.file.cast::<ShmemFile>() != file {
        return None;
    }

    Some(dmabuf)
}

/// Acquire an additional DMA-BUF reference using its backing file, matching
/// Linux's inline `get_dma_buf()` helper.
///
/// # Safety
/// `dmabuf` must be null/ERR_PTR or a live DMA-BUF pointer whose backing file reference
/// is retained by the caller.
pub unsafe fn get_dma_buf(dmabuf: *mut DmaBuf) {
    if dmabuf.is_null() || IS_ERR(dmabuf) {
        return;
    }
    // SAFETY: `dmabuf` is verified non-null and not ERR_PTR; the caller holds an active
    // reference to the DMA-BUF, so `(*dmabuf).file` is valid and safely readable.
    let file = unsafe { (*dmabuf).file.cast::<ShmemFile>() };
    if !file.is_null() && !IS_ERR(file) {
        // SAFETY: `file` was read from the live DMA-BUF; incrementing its reference count via get_file_file.
        unsafe { get_file_file(file) };
    }
}

/// Free the allocation made by `dma_buf_export()` after the backing anon file
/// reaches its final reference.
///
/// # Safety
///
/// `file` must be null or point to a live `ShmemFile` entering release whose reference count
/// has dropped to zero.
unsafe extern "C" fn dma_buf_file_release(_inode: *mut Inode, file: *mut ShmemFile) -> c_int {
    // SAFETY: `file` is checked for null before dereferencing `(*file).f_op`.
    if file.is_null() || unsafe { (*file).f_op } != ptr::addr_of!(DMA_BUF_FOPS) {
        return -EINVAL;
    }
    // SAFETY: `file` is confirmed non-null and backed by DMA_BUF_FOPS, so `_private_data`
    // contains the exported DmaBuf pointer.
    let dmabuf = unsafe { (*file)._private_data.cast::<DmaBuf>() };
    if dmabuf.is_null() {
        return -EINVAL;
    }

    // The exporter callback releases its private object before the public
    // wrapper and an optional in-place reservation object are freed.
    // SAFETY: `dmabuf` belongs to this file whose refcount reached zero; no concurrent
    // thread can access `dmabuf`. Calling the exporter release callback, finalizing any
    // embedded reservation object, and freeing via `kfree` is sound.
    unsafe {
        if let Some(release) = (*dmabuf).ops.as_ref().and_then(|ops| ops.release) {
            release(dmabuf);
        }
        if (*dmabuf).resv == dmabuf.add(1).cast::<DmaResv>() {
            dma_resv_fini((*dmabuf).resv);
        }
        kfree(dmabuf);
    }
    0
}

/// Linux `dma_buf_export()`: create an anonymous file whose final put owns the
/// exporter release callback and DMA-BUF allocation.
#[unsafe(no_mangle)]
/// # Safety
/// - `exp_info` must be null or point to a readable, properly aligned `DmaBufExportInfo` struct.
/// - If non-null, `(*exp_info).ops` must point to a valid `DmaBufOps` table with required callbacks
///   (`map_dma_buf`, `unmap_dma_buf`, `release`, and matched `pin`/`unpin`).
/// - `(*exp_info).r#priv` must point to exporter-managed private state that outlives this buffer.
/// - `(*exp_info).resv`, if non-null, must point to an initialized `DmaResv` object outliving the buffer.
/// - `(*exp_info).exp_name`, if non-null, must point to a valid null-terminated C string.
pub unsafe extern "C" fn dma_buf_export(exp_info: *const DmaBufExportInfo) -> *mut DmaBuf {
    if exp_info.is_null() {
        return ERR_PTR(-EINVAL);
    }
    // SAFETY: `exp_info` was checked non-null and properly aligned; it points to an initialized
    // `DmaBufExportInfo` value whose memory remains valid and unmodified for the duration of export.
    let info = unsafe { &*exp_info };
    if info.ops.is_null() || IS_ERR(info.ops) {
        return ERR_PTR(-EINVAL);
    }
    // SAFETY: `info.ops` was checked non-null and non-error; it points to a static or exporter-owned
    // `DmaBufOps` operations table that remains immutable and valid across buffer lifetime.
    let ops = unsafe { info.ops.as_ref() };
    let Some(ops) = ops else {
        return ERR_PTR(-EINVAL);
    };

    // Linux requires these exporter operations and requires pin/unpin to be
    // supplied as a pair. Optional CPU access callbacks are intentionally
    // optional: coherent exporters use the Linux-defined successful no-op.
    if info.r#priv.is_null()
        || ops.map_dma_buf.is_none()
        || ops.unmap_dma_buf.is_none()
        || ops.release.is_none()
        || ops.pin.is_some() != ops.unpin.is_some()
    {
        return ERR_PTR(-EINVAL);
    }

    let Some(alloc_size) = size_of::<DmaBuf>().checked_add(size_of::<DmaResv>()) else {
        return ERR_PTR(-EOVERFLOW);
    };
    let dmabuf = kmalloc(alloc_size, GFP_KERNEL).cast::<DmaBuf>();
    if dmabuf.is_null() {
        return ERR_PTR(-ENOMEM);
    }
    // SAFETY: `dmabuf` was just allocated with `alloc_size` bytes; initializing fields and optional in-place DmaResv.
    unsafe {
        ptr::write_bytes(dmabuf.cast::<u8>(), 0, alloc_size);
        (*dmabuf).size = info.size;
        (*dmabuf).ops = info.ops;
        (*dmabuf).exp_name = info.exp_name;
        (*dmabuf).owner = info.owner;
        (*dmabuf).r#priv = info.r#priv;
        (*dmabuf).resv = if info.resv.is_null() {
            let resv = dmabuf.add(1).cast::<DmaResv>();
            dma_resv_init(resv);
            resv
        } else {
            info.resv
        };
        INIT_LIST_HEAD(ptr::addr_of_mut!((*dmabuf).attachments));
        INIT_LIST_HEAD(ptr::addr_of_mut!((*dmabuf).list_node));
        spin_lock_init(&mut (*dmabuf).name_lock);
    }

    let name = if info.exp_name.is_null() {
        c"dma_buf".as_ptr()
    } else {
        info.exp_name
    };
    // SAFETY: `dmabuf` is initialized and `DMA_BUF_FOPS` is a static operations table.
    let file = unsafe {
        anon_inode_getfile(
            name,
            ptr::addr_of!(DMA_BUF_FOPS).cast(),
            dmabuf.cast(),
            info.flags,
        )
    };
    if IS_ERR(file) {
        let error = PTR_ERR(file);
        // SAFETY: `file` creation failed, clean up the allocated `dmabuf`.
        unsafe { kfree(dmabuf) };
        return ERR_PTR(error);
    }
    // SAFETY: `dmabuf` points to the newly allocated DmaBuf and `file` is the valid ShmemFile
    // returned by anon_inode_getfile; no other thread has access to `dmabuf` yet.
    unsafe { (*dmabuf).file = file.cast() };
    dmabuf
}

/// Linux `dma_buf_get()`: returns an owned file reference or an `ERR_PTR`.
#[unsafe(no_mangle)]
/// # Safety
/// - `fd` must be an integer file descriptor within the current process table.
/// - On success, returns the `DmaBuf` pointer stored in the file's private data. The caller
///   acquires an owned file reference and must eventually call [`dma_buf_put`] to release it.
pub unsafe extern "C" fn dma_buf_get(fd: c_int) -> *mut DmaBuf {
    // SAFETY: `fget` safely performs table bounds-checking and reference acquisition on `fd`.
    let file = unsafe { fget(fd) };
    if file.is_null() {
        return ERR_PTR(-EBADF);
    }
    // SAFETY: `file` was acquired via `fget`; reading `(*file).f_op` is safe while holding that reference.
    if unsafe { (*file).f_op } != ptr::addr_of!(DMA_BUF_FOPS) {
        // SAFETY: `file` is a non-null ShmemFile reference acquired above; releasing it upon type mismatch.
        unsafe { fput_file(file) };
        return ERR_PTR(-EINVAL);
    }
    // SAFETY: `file.f_op` matched `DMA_BUF_FOPS`, so `_private_data` contains the DmaBuf pointer set during export.
    let dmabuf = unsafe { (*file)._private_data.cast::<DmaBuf>() };
    if dmabuf.is_null() {
        // SAFETY: `file` is an owned reference acquired above; releasing it before returning error.
        unsafe { fput_file(file) };
        return ERR_PTR(-EINVAL);
    }
    dmabuf
}

/// Linux `dma_buf_put()`: drop the file reference returned by export/get.
#[unsafe(no_mangle)]
/// # Safety
/// - `dmabuf` can be null or an `ERR_PTR` (in which case this function is a no-op).
/// - If non-null and non-error, `dmabuf` must point to a live `DmaBuf` previously obtained from
///   [`dma_buf_export`] or [`dma_buf_get`].
/// - The caller relinquishes one file reference count corresponding to `dmabuf`. If this drops
///   the refcount to zero, the buffer is freed and `dmabuf` must not be dereferenced again.
pub unsafe extern "C" fn dma_buf_put(dmabuf: *mut DmaBuf) {
    if dmabuf.is_null() || IS_ERR(dmabuf) {
        return;
    }
    // SAFETY: `dmabuf` is verified non-null and not ERR_PTR; the caller holds an active
    // reference to the buffer, ensuring `(*dmabuf).file` can be safely read.
    let file = unsafe { (*dmabuf).file.cast::<ShmemFile>() };
    if !file.is_null() && !IS_ERR(file) {
        // SAFETY: `file` is the backing anon file of `dmabuf`; dropping one acquired reference.
        unsafe { fput_file(file) };
    }
}

/// Linux `dma_buf_attach()`: allocate and link a static device attachment.
#[unsafe(no_mangle)]
/// # Safety
/// - `dmabuf` must be null/ERR_PTR or point to a live, initialized `DmaBuf` whose reference is held by the caller.
/// - `dev` must be a non-null pointer to a device struct (e.g. `struct device`) that remains valid until
///   [`dma_buf_detach`] is called.
/// - On success, the caller acquires ownership of the returned `DmaBufAttachment` and must free it via [`dma_buf_detach`].
pub unsafe extern "C" fn dma_buf_attach(
    dmabuf: *mut DmaBuf,
    dev: *mut c_void,
) -> *mut DmaBufAttachment {
    if dmabuf.is_null() || IS_ERR(dmabuf) || dev.is_null() {
        return ERR_PTR(-EINVAL);
    }
    // SAFETY: `dmabuf` is verified non-null and not ERR_PTR; the caller-held reference guarantees
    // `(*dmabuf).ops` is valid and immutable.
    let Some(ops) = (unsafe { (*dmabuf).ops.as_ref() }) else {
        return ERR_PTR(-EINVAL);
    };
    let attach = drm_kapi::memory::kzalloc_obj::<DmaBufAttachment>();
    if attach.is_null() {
        return ERR_PTR(-ENOMEM);
    }
    // SAFETY: `attach` was freshly allocated via kzalloc_obj and is uniquely owned.
    // Initializing fields, calling exporter attach callback, and linking into `(*dmabuf).attachments`
    // under `(*dmabuf).name_lock`.
    unsafe {
        (*attach).dmabuf = dmabuf;
        (*attach).dev = dev;
        INIT_LIST_HEAD(ptr::addr_of_mut!((*attach).node));
        if let Some(attach_op) = ops.attach {
            let ret = attach_op(dmabuf, attach);
            if ret != 0 {
                kfree(attach);
                return ERR_PTR(ret);
            }
        }
        spin_lock_raw(ptr::addr_of_mut!((*dmabuf).name_lock));
        list_add_tail(
            ptr::addr_of_mut!((*attach).node),
            ptr::addr_of_mut!((*dmabuf).attachments),
        );
        spin_unlock_raw(ptr::addr_of_mut!((*dmabuf).name_lock));
    }
    attach
}

/// Linux `dma_buf_detach()`: unlink the attachment, notify the exporter, free
/// the attachment record.
#[unsafe(no_mangle)]
/// # Safety
/// - `dmabuf` can be null or ERR_PTR (no-op).
/// - `attach` can be null or ERR_PTR (no-op).
/// - If non-null, `attach` must point to a live `DmaBufAttachment` created by [`dma_buf_attach`] on `dmabuf`.
/// - All scatter-gather mappings created via [`dma_buf_map_attachment_unlocked`] for `attach`
///   must have been unmapped via [`dma_buf_unmap_attachment_unlocked`] prior to detachment.
/// - `attach` is deallocated by this function and must not be accessed after this call.
pub unsafe extern "C" fn dma_buf_detach(dmabuf: *mut DmaBuf, attach: *mut DmaBufAttachment) {
    if dmabuf.is_null() || IS_ERR(dmabuf) || attach.is_null() || IS_ERR(attach) {
        return;
    }
    // SAFETY: `attach` is verified non-null and not ERR_PTR; reading `(*attach).dmabuf` is safe
    // while the caller retains ownership of the attachment.
    if unsafe { (*attach).dmabuf != dmabuf } {
        return;
    }
    // SAFETY: `attach` is confirmed attached to `dmabuf`. Unlinking `(*attach).node` from
    // `(*dmabuf).attachments` under `name_lock`, calling exporter `detach` callback, and freeing `attach`.
    unsafe {
        spin_lock_raw(ptr::addr_of_mut!((*dmabuf).name_lock));
        if list_empty(&*ptr::addr_of!((*attach).node)) {
            spin_unlock_raw(ptr::addr_of_mut!((*dmabuf).name_lock));
            return;
        }
        list_del_init(ptr::addr_of_mut!((*attach).node));
        spin_unlock_raw(ptr::addr_of_mut!((*dmabuf).name_lock));

        if let Some(detach) = (*dmabuf).ops.as_ref().and_then(|ops| ops.detach) {
            detach(dmabuf, attach);
        }
        kfree(attach);
    }
}

/// Internal reservation-lock wrapper used by the unlocked map/unmap exports.
///
/// # Safety
///
/// `dmabuf` must be null, ERR_PTR, or a valid `DmaBuf` pointer whose `resv` points to an initialized `DmaResv`.
unsafe fn lock_reservation(dmabuf: *mut DmaBuf) -> Result<(), c_int> {
    // SAFETY: `dmabuf` is checked for null/ERR_PTR before reading its `resv` field.
    if dmabuf.is_null() || IS_ERR(dmabuf) || unsafe { (*dmabuf).resv.is_null() } {
        return Err(-EINVAL);
    }
    // SAFETY: `dmabuf` has a non-null `resv` containing an initialized ww_mutex lock.
    let ret = unsafe { ww_mutex_lock(ptr::addr_of_mut!((*(*dmabuf).resv).lock), ptr::null_mut()) };
    if ret != 0 { Err(ret) } else { Ok(()) }
}

/// # Safety
///
/// `dmabuf` must point to a valid `DmaBuf` with a non-null reservation object whose ww_mutex lock
/// was locked by the calling thread.
unsafe fn unlock_reservation(dmabuf: *mut DmaBuf) {
    // SAFETY: `dmabuf` and its `resv` are valid, and the reservation ww_mutex was acquired by
    // the calling context and is now unlocked.
    unsafe { ww_mutex_unlock(ptr::addr_of_mut!((*(*dmabuf).resv).lock)) };
}

/// Linux `dma_buf_map_attachment_unlocked()`: hold the reservation while the
/// exporter pins/maps and return its real scatterlist or an explicit error.
#[unsafe(no_mangle)]
/// # Safety
/// - `attach` must be null/ERR_PTR or point to a live `DmaBufAttachment` whose `dmabuf` field
///   points to a valid `DmaBuf`.
/// - The backing buffer's `ops` must implement `map_dma_buf` and `unmap_dma_buf`.
/// - `direction` must be a valid DMA data direction constant (`DMA_BIDIRECTIONAL`, `DMA_TO_DEVICE`, etc.).
/// - The returned `SgTable` must be unmapped via [`dma_buf_unmap_attachment_unlocked`] using the
///   same `attach` and `direction` before `attach` or `dmabuf` is released.
pub unsafe extern "C" fn dma_buf_map_attachment_unlocked(
    attach: *mut DmaBufAttachment,
    direction: c_int,
) -> *mut SgTable {
    // SAFETY: `attach` is checked for null/ERR_PTR before reading its `dmabuf` field.
    if attach.is_null()
        || IS_ERR(attach)
        || unsafe { (*attach).dmabuf.is_null() || IS_ERR((*attach).dmabuf) }
    {
        return ERR_PTR(-EINVAL);
    }
    // SAFETY: `attach` is confirmed non-null/non-ERR and `(*attach).dmabuf` is verified non-null.
    let dmabuf = unsafe { (*attach).dmabuf };
    // SAFETY: `dmabuf` is a valid pointer whose ops table is guaranteed valid by the exporter.
    let Some(ops) = (unsafe { (*dmabuf).ops.as_ref() }) else {
        return ERR_PTR(-EINVAL);
    };
    let Some(map) = ops.map_dma_buf else {
        return ERR_PTR(-EOPNOTSUPP);
    };
    if ops.unmap_dma_buf.is_none() {
        return ERR_PTR(-EOPNOTSUPP);
    }
    // SAFETY: `dmabuf` is valid; locking its reservation object around pin/map calls.
    if let Err(error) = unsafe { lock_reservation(dmabuf) } {
        return ERR_PTR(error);
    }

    let mut pinned = false;
    let mut result = 0;
    if let Some(pin) = ops.pin {
        // SAFETY: `attach` is valid and the buffer reservation lock is held as required by exporter pin.
        result = unsafe { pin(attach) };
        pinned = result == 0;
    }
    let sgt = if result == 0 {
        // SAFETY: `attach` is valid and the buffer reservation lock is held as required by exporter map.
        unsafe { map(attach, direction) }
    } else {
        ERR_PTR(result)
    };
    if IS_ERR(sgt) || sgt.is_null() {
        if pinned && let Some(unpin) = ops.unpin {
            // SAFETY: `attach` is valid and the buffer reservation lock is held as required by exporter unpin.
            unsafe { unpin(attach) };
        }
        // SAFETY: unlocking reservation lock previously acquired via lock_reservation on error path.
        unsafe { unlock_reservation(dmabuf) };
        return if sgt.is_null() { ERR_PTR(-EINVAL) } else { sgt };
    }
    // SAFETY: unlocking reservation lock previously acquired via lock_reservation on success path.
    unsafe { unlock_reservation(dmabuf) };
    sgt
}

/// Linux `dma_buf_unmap_attachment_unlocked()`: unmap and release any pin
/// acquired by the matching static attachment map.
#[unsafe(no_mangle)]
/// # Safety
/// - `attach` must be null/ERR_PTR or point to a live `DmaBufAttachment` previously mapped.
/// - `sgt` must be null/ERR_PTR or point to the `SgTable` returned by a prior matching call to
///   [`dma_buf_map_attachment_unlocked`].
/// - `direction` must match the direction passed to [`dma_buf_map_attachment_unlocked`].
/// - After this call, `sgt` is consumed/freed by the exporter and must not be dereferenced.
pub unsafe extern "C" fn dma_buf_unmap_attachment_unlocked(
    attach: *mut DmaBufAttachment,
    sgt: *mut SgTable,
    direction: c_int,
) {
    if attach.is_null() || IS_ERR(attach) || sgt.is_null() || IS_ERR(sgt) {
        return;
    }
    // SAFETY: `attach` is verified non-null and not an error pointer; reading its `dmabuf` field.
    let dmabuf = unsafe { (*attach).dmabuf };
    if dmabuf.is_null() || IS_ERR(dmabuf) {
        return;
    }
    // SAFETY: `dmabuf` is verified non-null and not an error pointer; reading `(*dmabuf).ops`.
    let Some(ops) = (unsafe { (*dmabuf).ops.as_ref() }) else {
        return;
    };
    let Some(unmap) = ops.unmap_dma_buf else {
        return;
    };
    // SAFETY: `dmabuf` is valid; acquiring reservation lock before exporter unmap/unpin.
    if unsafe { lock_reservation(dmabuf) }.is_err() {
        return;
    }
    // SAFETY: reservation lock was acquired above; invoking exporter unmap, optional unpin,
    // and releasing the reservation lock.
    unsafe {
        unmap(attach, sgt, direction);
        if let Some(unpin) = ops.unpin {
            unpin(attach);
        }
        unlock_reservation(dmabuf);
    }
}

/// Linux `dma_buf_begin_cpu_access()`: run exporter cache preparation and wait
/// for all implicit fences relevant to this access direction. An absent
/// callback is permitted for exporters that need no cache maintenance.
#[unsafe(no_mangle)]
/// # Safety
/// - `dmabuf` must be null/ERR_PTR or point to a live `DmaBuf` whose reference is retained by the caller.
/// - `direction` must be a valid DMA direction constant (`DMA_BIDIRECTIONAL`, `DMA_TO_DEVICE`, or `DMA_FROM_DEVICE`).
/// - Every successful call to `dma_buf_begin_cpu_access` must eventually be balanced by a call to
///   [`dma_buf_end_cpu_access`] with the same direction before device access resumes.
pub unsafe extern "C" fn dma_buf_begin_cpu_access(dmabuf: *mut DmaBuf, direction: c_int) -> c_int {
    if dmabuf.is_null() || IS_ERR(dmabuf) {
        return -EINVAL;
    }
    // SAFETY: `dmabuf` is verified non-null and not an error pointer; reading `(*dmabuf).ops`.
    let Some(ops) = (unsafe { (*dmabuf).ops.as_ref() }) else {
        return -EINVAL;
    };
    let ret = match ops.begin_cpu_access {
        // SAFETY: `dmabuf` is valid and the optional exporter callback is invoked with caller's direction.
        Some(begin) => unsafe { begin(dmabuf, direction) },
        None => 0,
    };
    if ret != 0 {
        return ret;
    }
    let usage = if direction == DMA_BIDIRECTIONAL || direction == DMA_TO_DEVICE {
        DMA_RESV_USAGE_READ
    } else {
        DMA_RESV_USAGE_WRITE
    };
    // SAFETY: `dmabuf` is verified non-null; reading its `resv` field.
    let resv = unsafe { (*dmabuf).resv };
    if resv.is_null() {
        return -EINVAL;
    }
    // SAFETY: `resv` is a non-null, initialized reservation object outliving the caller-retained buffer.
    let waited = unsafe {
        dma_resv_wait_timeout(
            resv.cast(),
            usage,
            true,
            drm_kapi::config::MAX_SCHEDULE_TIMEOUT as i64,
        )
    };
    if waited < 0 {
        waited as c_int
    } else if waited == 0 {
        -ETIME
    } else {
        0
    }
}

/// Linux `dma_buf_end_cpu_access()`. An absent callback is permitted by the
/// DMA-BUF API for exporters that need no cache maintenance.
#[unsafe(no_mangle)]
/// # Safety
/// - `dmabuf` must be null/ERR_PTR or point to a live `DmaBuf` whose reference is retained by the caller.
/// - `direction` must match the direction passed to a preceding successful call to [`dma_buf_begin_cpu_access`].
pub unsafe extern "C" fn dma_buf_end_cpu_access(dmabuf: *mut DmaBuf, direction: c_int) -> c_int {
    if dmabuf.is_null() || IS_ERR(dmabuf) {
        return -EINVAL;
    }
    // SAFETY: `dmabuf` is verified non-null and not an error pointer; reading `(*dmabuf).ops`.
    let Some(ops) = (unsafe { (*dmabuf).ops.as_ref() }) else {
        return -EINVAL;
    };
    match ops.end_cpu_access {
        // SAFETY: `dmabuf` is valid and the optional exporter callback is invoked with caller's direction.
        Some(end) => unsafe { end(dmabuf, direction) },
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use core::{
        ffi::{c_int, c_void},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use drm_kapi::{
        config::{EINVAL, ERR_PTR, IS_ERR},
        memory::{kfree, kmalloc},
        types::{Scatterlist, SgTable},
    };

    use super::*;
    use crate::drm_prime_upstream::DmaBufOps;

    static RELEASES: AtomicUsize = AtomicUsize::new(0);
    static ATTACHES: AtomicUsize = AtomicUsize::new(0);
    static DETACHES: AtomicUsize = AtomicUsize::new(0);
    static UNMAPS: AtomicUsize = AtomicUsize::new(0);
    static BEGINS: AtomicUsize = AtomicUsize::new(0);
    static ENDS: AtomicUsize = AtomicUsize::new(0);

    /// # Safety
    ///
    /// Mock test callback; `_dmabuf` and `_attach` are passed by the test caller.
    unsafe extern "C" fn attach(_dmabuf: *mut DmaBuf, _attach: *mut DmaBufAttachment) -> c_int {
        ATTACHES.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// # Safety
    ///
    /// Mock test callback; `_dmabuf` and `_attach` are passed by the test caller.
    unsafe extern "C" fn detach(_dmabuf: *mut DmaBuf, _attach: *mut DmaBufAttachment) {
        DETACHES.fetch_add(1, Ordering::SeqCst);
    }

    /// # Safety
    ///
    /// Mock test callback; `_attach` is passed by the test caller.
    unsafe extern "C" fn map(_attach: *mut DmaBufAttachment, _direction: c_int) -> *mut SgTable {
        let bytes = size_of::<SgTable>() + size_of::<Scatterlist>();
        let sgt = kmalloc(bytes, GFP_KERNEL).cast::<SgTable>();
        if sgt.is_null() {
            return ERR_PTR(-ENOMEM);
        }
        // SAFETY: pointer arithmetic within the single allocated block for SgTable + Scatterlist.
        let sgl = unsafe {
            sgt.cast::<u8>()
                .add(size_of::<SgTable>())
                .cast::<Scatterlist>()
        };
        // SAFETY: `sgt` and `sgl` point to valid allocated storage.
        unsafe {
            sgt.write(SgTable {
                sgl,
                nents: 1,
                orig_nents: 1,
            });
            sgl.write(Scatterlist {
                page_link: 0,
                offset: 0,
                length: 4096,
                dma_address: 0x1000,
                dma_length: 4096,
                _dma_flags_or_padding: 0,
            });
        }
        sgt
    }

    /// # Safety
    ///
    /// `sgt` must point to an `SgTable` previously allocated in `map`.
    unsafe extern "C" fn unmap(
        _attach: *mut DmaBufAttachment,
        sgt: *mut SgTable,
        _direction: c_int,
    ) {
        // SAFETY: `sgt` was initialized in `map` with 1 nent.
        assert_eq!(unsafe { (*sgt).nents }, 1);
        // SAFETY: `sgt.sgl` was initialized in `map`.
        assert_eq!(unsafe { (*(*sgt).sgl).dma_address }, 0x1000);
        UNMAPS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `sgt` was allocated with kmalloc in `map`.
        unsafe { kfree(sgt) };
    }

    /// # Safety
    ///
    /// Mock test callback; `_dmabuf` is passed by the test caller.
    unsafe extern "C" fn release(_dmabuf: *mut DmaBuf) {
        RELEASES.fetch_add(1, Ordering::SeqCst);
    }

    /// # Safety
    ///
    /// Mock test callback; `_dmabuf` is passed by the test caller.
    unsafe extern "C" fn begin(_dmabuf: *mut DmaBuf, _direction: c_int) -> c_int {
        BEGINS.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// # Safety
    ///
    /// Mock test callback; `_dmabuf` is passed by the test caller.
    unsafe extern "C" fn end(_dmabuf: *mut DmaBuf, _direction: c_int) -> c_int {
        ENDS.fetch_add(1, Ordering::SeqCst);
        0
    }

    static TEST_OPS: DmaBufOps = DmaBufOps {
        attach: Some(attach),
        detach: Some(detach),
        pin: None,
        unpin: None,
        map_dma_buf: Some(map),
        unmap_dma_buf: Some(unmap),
        release: Some(release),
        begin_cpu_access: Some(begin),
        end_cpu_access: Some(end),
        mmap: None,
        vmap: None,
        vunmap: None,
    };

    #[test]
    fn file_refs_attachments_mapping_and_cpu_access_follow_exporter_ops() {
        let _serial = crate::drm_syncobj_upstream::test_file_ops::serial();
        crate::drm_syncobj_upstream::test_file_ops::install();
        RELEASES.store(0, Ordering::SeqCst);
        ATTACHES.store(0, Ordering::SeqCst);
        DETACHES.store(0, Ordering::SeqCst);
        UNMAPS.store(0, Ordering::SeqCst);
        BEGINS.store(0, Ordering::SeqCst);
        ENDS.store(0, Ordering::SeqCst);

        let info = DmaBufExportInfo {
            exp_name: c"kdrm-core-test".as_ptr(),
            owner: ptr::null_mut(),
            ops: &TEST_OPS,
            size: 4096,
            flags: 0,
            resv: ptr::null_mut(),
            r#priv: 1usize as *mut c_void,
        };
        // SAFETY: `info` is a valid test export descriptor.
        let dmabuf = unsafe { dma_buf_export(&info) };
        assert!(!IS_ERR(dmabuf));
        // SAFETY: `dmabuf` is non-null and holds a valid backing file.
        let file = unsafe { (*dmabuf).file.cast::<ShmemFile>() };
        // SAFETY: `file` is valid ShmemFile for `dmabuf`.
        assert_eq!(unsafe { drm_dma_buf_from_file(file) }, Some(dmabuf));

        // SAFETY: obtaining unused file descriptor in test harness.
        let fd = unsafe { drm_kapi::kernel_services::get_unused_fd_flags(0) };
        assert!(fd >= 0);
        // SAFETY: installing the DMA-BUF file into the test fd table.
        unsafe { drm_kapi::kernel_services::fd_install(fd, (*dmabuf).file.cast()) };

        // SAFETY: retrieving DMA-BUF from installed fd.
        let got = unsafe { dma_buf_get(fd) };
        assert_eq!(got, dmabuf);
        // SAFETY: dropping acquired reference.
        unsafe { dma_buf_put(got) };
        assert_eq!(RELEASES.load(Ordering::SeqCst), 0);

        // SAFETY: attaching mock device to `dmabuf`.
        let attach = unsafe { dma_buf_attach(dmabuf, 0x4000usize as *mut c_void) };
        assert!(!IS_ERR(attach));
        assert_eq!(ATTACHES.load(Ordering::SeqCst), 1);
        // SAFETY: testing cpu access begin.
        assert_eq!(unsafe { dma_buf_begin_cpu_access(dmabuf, 0) }, 0);
        // SAFETY: testing cpu access end.
        assert_eq!(unsafe { dma_buf_end_cpu_access(dmabuf, 0) }, 0);
        assert_eq!(BEGINS.load(Ordering::SeqCst), 1);
        assert_eq!(ENDS.load(Ordering::SeqCst), 1);
        // SAFETY: testing mapping attachment.
        let sgt = unsafe { dma_buf_map_attachment_unlocked(attach, 0) };
        assert!(!IS_ERR(sgt));
        // SAFETY: testing unmapping attachment.
        unsafe { dma_buf_unmap_attachment_unlocked(attach, sgt, 0) };
        assert_eq!(UNMAPS.load(Ordering::SeqCst), 1);
        // SAFETY: testing detaching.
        unsafe { dma_buf_detach(dmabuf, attach) };
        assert_eq!(DETACHES.load(Ordering::SeqCst), 1);

        // SAFETY: getting second reference to test fd cleanup.
        let outstanding = unsafe { dma_buf_get(fd) };
        crate::drm_syncobj_upstream::test_file_ops::close_fd(fd);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 0);
        // SAFETY: final put triggers release.
        unsafe { dma_buf_put(outstanding) };
        assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn export_rejects_incomplete_ops_and_get_rejects_other_file_types() {
        let _serial = crate::drm_syncobj_upstream::test_file_ops::serial();
        crate::drm_syncobj_upstream::test_file_ops::install();
        let bad_ops = DmaBufOps {
            attach: Some(attach),
            detach: Some(detach),
            pin: None,
            unpin: None,
            map_dma_buf: None,
            unmap_dma_buf: Some(unmap),
            release: Some(release),
            begin_cpu_access: Some(begin),
            end_cpu_access: Some(end),
            mmap: None,
            vmap: None,
            vunmap: None,
        };
        let info = DmaBufExportInfo {
            exp_name: c"invalid".as_ptr(),
            owner: ptr::null_mut(),
            ops: &bad_ops,
            size: 4096,
            flags: 0,
            resv: ptr::null_mut(),
            r#priv: 1usize as *mut c_void,
        };
        // SAFETY: testing export rejection of incomplete ops.
        let err = unsafe { dma_buf_export(&info) };
        assert!(IS_ERR(err));
        assert_eq!(drm_kapi::config::PTR_ERR(err), -EINVAL);

        let fd = crate::drm_syncobj_upstream::test_file_ops::install_plain_file();
        // SAFETY: testing get on non-DMA-BUF file.
        let err = unsafe { dma_buf_get(fd) };
        assert!(IS_ERR(err));
        assert_eq!(drm_kapi::config::PTR_ERR(err), -EINVAL);

        // SAFETY: testing fget on plain file.
        let file = unsafe { drm_kapi::kernel_services::fget(fd) };
        assert!(!file.is_null());
        // SAFETY: testing drm_dma_buf_from_file returns None for plain file.
        assert_eq!(unsafe { drm_dma_buf_from_file(file) }, None);
        // SAFETY: dropping plain file reference.
        unsafe { fput_file(file) };
        crate::drm_syncobj_upstream::test_file_ops::close_fd(fd);
    }
}

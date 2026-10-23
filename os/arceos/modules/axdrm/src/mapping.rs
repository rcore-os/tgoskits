// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! Starry VMA lookup for GEM offsets owned by kdrm-core.

use alloc::sync::Arc;
use core::{mem::offset_of, ptr, sync::atomic::AtomicU32};

use ax_memory_addr::{MemoryAddr, PhysAddrRange};
use drm_kapi::{
    config::PAGE_SIZE,
    locks::{read_lock, read_unlock},
};
use kdrm_core::{
    drm_file_types::DrmFile,
    drm_gem_types::DrmGemObject,
    drm_gem_upstream::{drm_gem_object_get, drm_gem_object_put},
    drm_vma_manager_upstream::{drm_vma_node_is_allowed, drm_vma_offset_lookup_locked},
};

use crate::{backend::gem, device};

const NEG_EINVAL: i32 = -drm_kapi::config::EINVAL;
const NEG_ENODEV: i32 = -drm_kapi::config::ENODEV;
const NEG_EACCES: i32 = -drm_kapi::config::EACCES;

/// A VMA pin for the exact GEM object and RDIF allocation backing a mapping.
///
/// Keep this object (normally inside `DeviceMmap`'s `Arc` retainer) until all
/// forked and cloned VMAs have closed. The retained GEM reference prevents
/// object teardown, including its DMA backing or host BAR lease.
pub struct MmapLease {
    physical: PhysAddrRange,
    cached: bool,
    gem: usize,
    _device: Arc<device::DeviceOwner>,
}

// SAFETY: The lease contains immutable physical metadata and an owned GEM
// reference that pins its backing or BAR lease. The GEM pointer
// has no other dereferences here; its only
// use after construction is the matching final put in Drop.
unsafe impl Send for MmapLease {}
// SAFETY: Shared access only reads immutable range/allocation state. The GEM
// reference count and backing's interior synchronization protect teardown.
unsafe impl Sync for MmapLease {}

impl MmapLease {
    /// Physical range resolved for this already-offset-adjusted request.
    pub fn physical(&self) -> PhysAddrRange {
        self.physical
    }

    /// Whether the device requires a normal cacheable shared-memory mapping.
    pub fn cached(&self) -> bool {
        self.cached
    }
}

impl Drop for MmapLease {
    fn drop(&mut self) {
        // SAFETY: construction takes exactly one GEM reference and stores its
        // address as usize; the reference remains live until this final put.
        unsafe { drm_gem_object_put(self.gem as *mut DrmGemObject) };
    }
}

/// Resolve a DRM-node mmap offset through kdrm-core's VMA offset manager.
///
/// The returned range is offset-adjusted. The caller must retain the lease
/// with the resulting VMA (including clones/fork) instead of applying the
/// synthetic DRM offset a second time.
///
/// # Safety
///
/// `file` must be null or point to a live, properly aligned `DrmFile` that remains open
/// and valid for the duration of this call.
pub unsafe fn mmap(file: *mut DrmFile, offset: u64, length: u64) -> Result<MmapLease, i32> {
    if file.is_null()
        || length == 0
        || !offset.is_multiple_of(PAGE_SIZE as u64)
        || !length.is_multiple_of(PAGE_SIZE as u64)
    {
        return Err(NEG_EINVAL);
    }
    // SAFETY: `file` is verified non-null above and points to a live `DrmFile` per caller contract.
    // `minor` and `dev` are checked for null before dereferencing, ensuring safe pointer traversal to `vma_offset_manager`.
    let vma_manager = unsafe {
        let minor = (*file).minor;
        if minor.is_null() {
            return Err(NEG_EINVAL);
        }
        let dev = (*minor).dev;
        if dev.is_null() {
            return Err(NEG_ENODEV);
        }
        (*dev).vma_offset_manager
    };
    if vma_manager.is_null() {
        return Err(NEG_ENODEV);
    }
    let pages = usize::try_from(length / PAGE_SIZE as u64).map_err(|_| NEG_EINVAL)?;
    let start_page = offset / PAGE_SIZE as u64;
    start_page.checked_add(pages as u64).ok_or(NEG_EINVAL)?;

    // SAFETY:
    // - `vma_manager` is verified non-null and points to the live `DrmVmaOffsetManager` owned by `dev`.
    // - `vm_lock` is acquired as a reader and released on all exit paths.
    // - `drm_vma_offset_lookup_locked` searches nodes within `vma_manager` under `vm_lock`.
    // - If found, `node` is embedded within `DrmGemObject` at `offset_of!(DrmGemObject, vma_node)`.
    // - `kref_get_unless_zero` safely attempts to increment the refcount only if the object is still alive.
    let node = unsafe {
        read_lock(ptr::addr_of_mut!((*vma_manager).vm_lock).cast::<AtomicU32>());
        let node = drm_vma_offset_lookup_locked(vma_manager, start_page as _, pages as _);
        if node.is_null() {
            read_unlock(ptr::addr_of_mut!((*vma_manager).vm_lock).cast::<AtomicU32>());
            return Err(NEG_EINVAL);
        }
        let obj = node
            .cast::<u8>()
            .wrapping_sub(offset_of!(DrmGemObject, vma_node))
            .cast::<DrmGemObject>();
        // The destructor may already have decremented the refcount to zero
        // and be waiting to remove this node under the VMA write lock. Do not
        // resurrect that object while holding the read lock.
        if !drm_kapi::memory::kref_get_unless_zero(&(*obj).refcount) {
            read_unlock(ptr::addr_of_mut!((*vma_manager).vm_lock).cast::<AtomicU32>());
            return Err(NEG_EINVAL);
        }
        read_unlock(ptr::addr_of_mut!((*vma_manager).vm_lock).cast::<AtomicU32>());
        (node, obj)
    };
    let (node, obj) = node;
    // Permission is per DrmFile, not per GEM handle. The node remains alive
    // because the object reference is held; the helper takes its own node lock.
    // SAFETY: `node` and `file` are valid pointers; `obj` has a live GEM reference acquired above, keeping `node` allocated.
    if !unsafe { drm_vma_node_is_allowed(node, file) } {
        // SAFETY: `obj` holds the GEM reference acquired during lookup; release it on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_EACCES);
    }
    // Imported DMA-BUFs are not map-offset objects in this driver; userspace
    // maps the exporting DMA-BUF fd, whose owner can provide the right lease.
    if gem::is_imported(obj) {
        // SAFETY: `obj` holds the GEM reference acquired during lookup; release it on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_EINVAL);
    }
    // SAFETY: `obj` is non-null and holds a live GEM reference acquired during lookup; `(*obj).size` is valid to read.
    let (object_size, mapping) = unsafe { ((*obj).size, gem::cpu_mapping(obj)) };
    let Some(mapping) = mapping else {
        // SAFETY: `obj` holds the GEM reference acquired during lookup; release it on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_ENODEV);
    };
    // SAFETY: `node` is embedded within `obj`, which is kept alive by the held GEM reference.
    let node_start = unsafe { (*node).vm_node.start };
    let relative_pages = start_page.checked_sub(node_start).ok_or_else(|| {
        // SAFETY: Releases the GEM reference acquired during lookup on this error path.
        unsafe { drm_gem_object_put(obj) };
        NEG_EINVAL
    })?;
    let object_offset = usize::try_from(relative_pages)
        .ok()
        .and_then(|pages| pages.checked_mul(PAGE_SIZE))
        .ok_or_else(|| {
            // SAFETY: Releases the GEM reference acquired during lookup on this error path.
            unsafe { drm_gem_object_put(obj) };
            NEG_EINVAL
        })?;
    let request_len = usize::try_from(length).map_err(|_| {
        // SAFETY: Releases the GEM reference acquired during lookup on this error path.
        unsafe { drm_gem_object_put(obj) };
        NEG_EINVAL
    })?;
    if object_offset
        .checked_add(request_len)
        .is_none_or(|end| end > object_size)
        || object_offset
            .checked_add(request_len)
            .is_none_or(|end| end > mapping.physical.size())
    {
        // SAFETY: Releases the GEM reference acquired during lookup on this error path.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_EINVAL);
    }
    lease_for_object(obj, mapping, object_offset, request_len)
}

/// Resolve an exported DRM DMA-BUF mapping without a second synthetic GEM
/// offset table. The fd itself authorizes the object; its provider retains
/// the resulting lease through all VMA clones.
///
/// # Safety
///
/// `obj` must be null or point to a live GEM object reference held by the
/// caller for the duration of this call.
pub unsafe fn mmap_gem_object(
    obj: *mut DrmGemObject,
    offset: u64,
    length: u64,
) -> Result<MmapLease, i32> {
    if obj.is_null()
        || length == 0
        || !offset.is_multiple_of(PAGE_SIZE as u64)
        || !length.is_multiple_of(PAGE_SIZE as u64)
    {
        return Err(NEG_EINVAL);
    }
    let relative = usize::try_from(offset).map_err(|_| NEG_EINVAL)?;
    let length = usize::try_from(length).map_err(|_| NEG_EINVAL)?;
    // SAFETY: The DMA-BUF file owns a GEM reference for the lifetime of this
    // callback. Take our own pin before returning an independent VMA lease.
    unsafe { drm_gem_object_get(obj) };
    if gem::is_imported(obj) {
        // SAFETY: Releases the newly acquired GEM reference on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_EINVAL);
    }
    // SAFETY: `obj` has a live GEM reference acquired above.
    let mapping = gem::cpu_mapping(obj);
    let Some(mapping) = mapping else {
        // SAFETY: Releases the newly acquired GEM reference on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_ENODEV);
    };
    // SAFETY: `obj` remains pinned by the held GEM reference.
    let size = unsafe { (*obj).size };
    if relative
        .checked_add(length)
        .is_none_or(|end| end > size || end > mapping.physical.size())
    {
        // SAFETY: Releases the newly acquired GEM reference on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_EINVAL);
    }
    lease_for_object(obj, mapping, relative, length)
}

fn lease_for_object(
    obj: *mut DrmGemObject,
    mapping: gem::CpuMapping,
    offset: usize,
    length: usize,
) -> Result<MmapLease, i32> {
    let physical = mapping.physical;
    let start = physical.start.checked_add(offset).ok_or_else(|| {
        // SAFETY: Releases the held GEM reference on error.
        unsafe { drm_gem_object_put(obj) };
        NEG_EINVAL
    })?;
    let range = PhysAddrRange::from_start_size(start, length);
    let Some(owner) = device::device() else {
        // SAFETY: Releases the held GEM reference on error.
        unsafe { drm_gem_object_put(obj) };
        return Err(NEG_ENODEV);
    };
    Ok(MmapLease {
        physical: range,
        cached: mapping.cached,
        gem: obj as usize,
        _device: owner,
    })
}

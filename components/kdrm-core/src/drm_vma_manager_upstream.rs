// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_vma_manager.c`.
//!
//! VMA offset manager for GPU memory mappings into user-space linear VMs.
//! Uses `drm_mm` to allocate page-aligned offsets and an rb-tree of
//! `drm_vma_offset_file` entries keyed by the file pointer to track mapping permissions.

use alloc::boxed::Box;
use core::ffi::{c_int, c_ulong};

use drm_kapi::{
    drm_mm::{drm_mm_init, drm_mm_insert_node_in_range, drm_mm_remove_node, drm_mm_takedown},
    gem_memory::drm_mm_node_allocated,
    locks::{read_lock, read_unlock, write_lock, write_unlock},
    rbtree::{rb_erase, rb_insert_color, rb_link_node},
    types::RbNode,
};

use crate::{
    drm_file_types::DrmFile,
    drm_vma_manager_types::{DrmVmaOffsetFile, DrmVmaOffsetManager, DrmVmaOffsetNode},
};

/// `DRM_FILE_PAGE_OFFSET_START` (include/drm/drm_vma_manager.h, 64-bit): the
/// first fake page offset GEM mmap offsets are handed out from.
pub const DRM_FILE_PAGE_OFFSET_START: c_ulong = (0xFFFF_FFFFu64 >> 12) as c_ulong + 1;
/// `DRM_FILE_PAGE_OFFSET_SIZE` (include/drm/drm_vma_manager.h, 64-bit).
pub const DRM_FILE_PAGE_OFFSET_SIZE: c_ulong = (0xFFFF_FFFFu64 >> 12) as c_ulong * 256;

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_offset_manager_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `mgr` is non-null, it must point to properly aligned, writable, unaliased memory of at
/// least `size_of::<DrmVmaOffsetManager>()` bytes. If `mgr` is null, this function does nothing.
pub unsafe extern "C" fn drm_vma_offset_manager_init(
    mgr: *mut DrmVmaOffsetManager,
    page_offset: c_ulong,
    size: c_ulong,
) {
    if mgr.is_null() {
        return;
    }
    // SAFETY: `mgr` is verified non-null above and points to properly aligned, writable storage
    // for `DrmVmaOffsetManager`.
    // - `write_bytes` zeroes the `vm_lock` rwlock structure to an unlocked state.
    // - `drm_mm_init` initializes `vm_addr_space_mm` over `[page_offset, page_offset + size)`.
    unsafe {
        core::ptr::write_bytes(core::ptr::addr_of_mut!((*mgr).vm_lock), 0, 1);
        drm_mm_init(
            core::ptr::addr_of_mut!((*mgr).vm_addr_space_mm),
            page_offset,
            size,
        );
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_offset_manager_destroy()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `mgr` is non-null, it must point to an initialized, live `DrmVmaOffsetManager` that is not
/// concurrently accessed by any other thread. If `mgr` is null, this function does nothing.
pub unsafe extern "C" fn drm_vma_offset_manager_destroy(mgr: *mut DrmVmaOffsetManager) {
    if mgr.is_null() {
        return;
    }
    // SAFETY: `mgr` is checked non-null above and points to an initialized `DrmVmaOffsetManager`
    // with exclusive access. `drm_mm_takedown` cleans up nodes allocated within `vm_addr_space_mm`.
    unsafe {
        drm_mm_takedown(core::ptr::addr_of_mut!((*mgr).vm_addr_space_mm));
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_offset_lookup_locked()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `mgr` is null, returns null.
/// - If non-null, `mgr` must point to an initialized `DrmVmaOffsetManager`.
/// - Caller must hold at least a read lock on `(*mgr).vm_lock` to serialize access to the interval
///   tree in `vm_addr_space_mm`.
pub unsafe extern "C" fn drm_vma_offset_lookup_locked(
    mgr: *mut DrmVmaOffsetManager,
    start: c_ulong,
    pages: c_ulong,
) -> *mut DrmVmaOffsetNode {
    if mgr.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `mgr` is checked non-null above and caller holds read access via `(*mgr).vm_lock`.
    // - Interval tree links in `vm_addr_space_mm` are traversed while holding the lock.
    // - `offset_of!(DrmMmNode, rb)` correctly recovers the enclosing `DrmMmNode` from the `RbNode`.
    // - `offset_of!(DrmVmaOffsetNode, vm_node)` correctly recovers the enclosing `DrmVmaOffsetNode`
    //   from `best` (`DrmMmNode`), which was allocated as an embedded member of `DrmVmaOffsetNode`.
    unsafe {
        let mut iter = (*mgr).vm_addr_space_mm.interval_tree.root.node;
        let mut best: *mut drm_kapi::types::DrmMmNode = core::ptr::null_mut();

        while !iter.is_null() {
            let node = iter
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(drm_kapi::types::DrmMmNode, rb))
                .cast::<drm_kapi::types::DrmMmNode>();
            let offset = (*node).start;
            if start >= offset {
                iter = (*iter).right;
                best = node;
                if start == offset {
                    break;
                }
            } else {
                iter = (*iter).left;
            }
        }

        if !best.is_null() {
            let offset = (*best).start + (*best).size;
            if offset < start + pages {
                best = core::ptr::null_mut();
            }
        }

        if best.is_null() {
            core::ptr::null_mut()
        } else {
            best.cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmVmaOffsetNode, vm_node))
                .cast()
        }
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_offset_add()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `mgr` or `node` is null, returns `-EINVAL`.
/// - If non-null, `mgr` must point to an initialized, live `DrmVmaOffsetManager`.
/// - If non-null, `node` must point to an initialized, live `DrmVmaOffsetNode` whose lifetime
///   outlasts its membership in `mgr`, and which is not concurrently inserted into another manager.
pub unsafe extern "C" fn drm_vma_offset_add(
    mgr: *mut DrmVmaOffsetManager,
    node: *mut DrmVmaOffsetNode,
    pages: c_ulong,
) -> c_int {
    if mgr.is_null() || node.is_null() {
        return -drm_kapi::config::EINVAL;
    }
    // SAFETY: `mgr` and `node` are checked non-null above.
    // - `write_lock` on `(*mgr).vm_lock` provides mutual exclusion for inserting into `(*mgr).vm_addr_space_mm`.
    // - `drm_mm_node_allocated` checks whether `(*node).vm_node` is already in an mm allocator.
    // - `drm_mm_insert_node_in_range` links `(*node).vm_node` into `mgr->vm_addr_space_mm`.
    // - `write_unlock` releases `(*mgr).vm_lock`.
    unsafe {
        let mut ret = 0;
        write_lock(core::ptr::addr_of_mut!((*mgr).vm_lock).cast());
        if !drm_mm_node_allocated(core::ptr::addr_of_mut!((*node).vm_node)) {
            ret = drm_mm_insert_node_in_range(
                core::ptr::addr_of_mut!((*mgr).vm_addr_space_mm),
                core::ptr::addr_of_mut!((*node).vm_node),
                pages,
                0,
                0,
                0,
                u64::MAX,
                0,
            );
        }
        write_unlock(core::ptr::addr_of_mut!((*mgr).vm_lock).cast());
        ret
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_offset_remove()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `mgr` or `node` is null, this function does nothing.
/// - If non-null, `mgr` must point to an initialized, live `DrmVmaOffsetManager`.
/// - If non-null, `node` must point to a live `DrmVmaOffsetNode` that, if allocated, belongs
///   to `mgr`.
pub unsafe extern "C" fn drm_vma_offset_remove(
    mgr: *mut DrmVmaOffsetManager,
    node: *mut DrmVmaOffsetNode,
) {
    if mgr.is_null() || node.is_null() {
        return;
    }
    // SAFETY: `mgr` and `node` are checked non-null above.
    // - `write_lock` on `(*mgr).vm_lock` synchronizes removal from `(*mgr).vm_addr_space_mm`.
    // - `drm_mm_node_allocated` checks whether `(*node).vm_node` is currently inserted.
    // - `drm_mm_remove_node` unlinks the node from `vm_addr_space_mm`, and `write_bytes` zeroes
    //   `(*node).vm_node` to mark it unallocated.
    // - `write_unlock` releases `(*mgr).vm_lock`.
    unsafe {
        write_lock(core::ptr::addr_of_mut!((*mgr).vm_lock).cast());
        if drm_mm_node_allocated(core::ptr::addr_of_mut!((*node).vm_node)) {
            drm_mm_remove_node(core::ptr::addr_of_mut!((*node).vm_node));
            core::ptr::write_bytes(core::ptr::addr_of_mut!((*node).vm_node), 0, 1);
        }
        write_unlock(core::ptr::addr_of_mut!((*mgr).vm_lock).cast());
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c vma_node_allow()
/// # Safety
///
/// - `node` must be non-null, properly aligned, and point to an initialized `DrmVmaOffsetNode`.
/// - `tag` is used as an opaque lookup key; it is not dereferenced, but must remain consistent
///   across matching `allow` and `revoke` calls.
unsafe fn vma_node_allow(
    node: *mut DrmVmaOffsetNode,
    tag: *mut DrmFile,
    ref_counted: bool,
) -> c_int {
    // SAFETY: `node` is guaranteed non-null, aligned, and live by the caller.
    // - `write_lock` on `(*node).vm_lock` protects the rb-tree `(*node).vm_files` during search and insertion.
    // - `offset_of!(DrmVmaOffsetFile, vm_rb)` recovers the enclosing `DrmVmaOffsetFile` from each `RbNode`.
    // - If no matching tag entry exists, `Box::new` allocates a heap-backed `DrmVmaOffsetFile` and
    //   `Box::into_raw` hands ownership of the node to the rb-tree.
    // - `rb_link_node` and `rb_insert_color` link the new entry into `(*node).vm_files`.
    // - `write_unlock` releases `(*node).vm_lock`.
    unsafe {
        let ret = 0;
        let mut parent: *mut RbNode = core::ptr::null_mut();
        let mut found = false;

        write_lock(core::ptr::addr_of_mut!((*node).vm_lock).cast());
        let mut link = core::ptr::addr_of_mut!((*node).vm_files.node);

        while !(*link).is_null() {
            parent = *link;
            let entry = parent
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmVmaOffsetFile, vm_rb))
                .cast::<DrmVmaOffsetFile>();
            if tag == (*entry).vm_tag {
                if ref_counted {
                    (*entry).vm_count += 1;
                }
                found = true;
                break;
            } else if (tag as usize) > ((*entry).vm_tag as usize) {
                link = core::ptr::addr_of_mut!((*parent).right);
            } else {
                link = core::ptr::addr_of_mut!((*parent).left);
            }
        }

        if !found {
            let new_entry = Box::into_raw(Box::new(DrmVmaOffsetFile {
                vm_rb: RbNode {
                    parent_color: 0,
                    right: core::ptr::null_mut(),
                    left: core::ptr::null_mut(),
                },
                vm_tag: tag,
                vm_count: 1,
            }));
            rb_link_node(core::ptr::addr_of_mut!((*new_entry).vm_rb), parent, link);
            rb_insert_color(
                core::ptr::addr_of_mut!((*new_entry).vm_rb),
                core::ptr::addr_of_mut!((*node).vm_files),
            );
        }

        write_unlock(core::ptr::addr_of_mut!((*node).vm_lock).cast());
        ret
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_node_allow()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `node` is null, returns `-EINVAL`.
/// - If non-null, `node` must point to an initialized, live `DrmVmaOffsetNode`.
/// - `tag` represents the file identity key; it is used only as an address tag and is not dereferenced.
pub unsafe extern "C" fn drm_vma_node_allow(
    node: *mut DrmVmaOffsetNode,
    tag: *mut DrmFile,
) -> c_int {
    if node.is_null() {
        return -drm_kapi::config::EINVAL;
    }
    // SAFETY: `node` was checked non-null above and caller contract ensures it points to an
    // initialized `DrmVmaOffsetNode`. `vma_node_allow` safely acquires `(*node).vm_lock`.
    unsafe { vma_node_allow(node, tag, true) }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_node_allow_once()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `node` is null, returns `-EINVAL`.
/// - If non-null, `node` must point to an initialized, live `DrmVmaOffsetNode`.
/// - `tag` represents the file identity key; it is used only as an address tag and is not dereferenced.
pub unsafe extern "C" fn drm_vma_node_allow_once(
    node: *mut DrmVmaOffsetNode,
    tag: *mut DrmFile,
) -> c_int {
    if node.is_null() {
        return -drm_kapi::config::EINVAL;
    }
    // SAFETY: `node` was checked non-null above and caller contract ensures it points to an
    // initialized `DrmVmaOffsetNode`. `vma_node_allow` safely acquires `(*node).vm_lock`.
    unsafe { vma_node_allow(node, tag, false) }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_node_revoke()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `node` is null, this function does nothing.
/// - If non-null, `node` must point to an initialized, live `DrmVmaOffsetNode`.
/// - `tag` is matched by address equality against tags previously added by `drm_vma_node_allow`.
pub unsafe extern "C" fn drm_vma_node_revoke(node: *mut DrmVmaOffsetNode, tag: *mut DrmFile) {
    if node.is_null() {
        return;
    }
    // SAFETY: `node` is verified non-null above and points to an initialized `DrmVmaOffsetNode`.
    // - `write_lock` on `(*node).vm_lock` protects rb-tree traversal and deletion.
    // - `offset_of!(DrmVmaOffsetFile, vm_rb)` safely recovers the enclosing `DrmVmaOffsetFile` pointer.
    // - When `vm_count` drops to 0, `rb_erase` unlinks the node, and `Box::from_raw` reclaims the heap memory
    //   originally allocated by `Box::into_raw` in `vma_node_allow`.
    // - `write_unlock` releases `(*node).vm_lock`.
    unsafe {
        write_lock(core::ptr::addr_of_mut!((*node).vm_lock).cast());
        let mut iter = (*node).vm_files.node;
        while !iter.is_null() {
            let entry = iter
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmVmaOffsetFile, vm_rb))
                .cast::<DrmVmaOffsetFile>();
            if tag == (*entry).vm_tag {
                (*entry).vm_count -= 1;
                if (*entry).vm_count == 0 {
                    rb_erase(
                        core::ptr::addr_of_mut!((*entry).vm_rb),
                        core::ptr::addr_of_mut!((*node).vm_files),
                    );
                    drop(Box::from_raw(entry));
                }
                break;
            } else if (tag as usize) > ((*entry).vm_tag as usize) {
                iter = (*iter).right;
            } else {
                iter = (*iter).left;
            }
        }
        write_unlock(core::ptr::addr_of_mut!((*node).vm_lock).cast());
    }
}

// upstream: drivers/gpu/drm/drm_vma_manager.c drm_vma_node_is_allowed()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `node` is null, returns false.
/// - If non-null, `node` must point to an initialized, live `DrmVmaOffsetNode`.
/// - `tag` is matched by address equality against tags previously added to `node`.
pub unsafe extern "C" fn drm_vma_node_is_allowed(
    node: *mut DrmVmaOffsetNode,
    tag: *mut DrmFile,
) -> bool {
    if node.is_null() {
        return false;
    }
    // SAFETY: `node` is checked non-null above and points to an initialized `DrmVmaOffsetNode`.
    // - `read_lock` on `(*node).vm_lock` permits concurrent rb-tree reads.
    // - `offset_of!(DrmVmaOffsetFile, vm_rb)` safely recovers `DrmVmaOffsetFile` pointers from tree nodes.
    // - `read_unlock` releases `(*node).vm_lock`.
    unsafe {
        read_lock(core::ptr::addr_of_mut!((*node).vm_lock).cast());
        let mut iter = (*node).vm_files.node;
        while !iter.is_null() {
            let entry = iter
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmVmaOffsetFile, vm_rb))
                .cast::<DrmVmaOffsetFile>();
            if tag == (*entry).vm_tag {
                break;
            } else if (tag as usize) > ((*entry).vm_tag as usize) {
                iter = (*iter).right;
            } else {
                iter = (*iter).left;
            }
        }
        read_unlock(core::ptr::addr_of_mut!((*node).vm_lock).cast());
        !iter.is_null()
    }
}

// upstream: include/drm/drm_vma_manager.h drm_vma_node_has_offset()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `node` is null, returns false.
/// - If non-null, `node` must point to an initialized, readable `DrmVmaOffsetNode`.
pub unsafe extern "C" fn drm_vma_node_has_offset(node: *const DrmVmaOffsetNode) -> bool {
    if node.is_null() {
        return false;
    }
    // SAFETY: `node` is checked non-null above; `addr_of!((*node).vm_node)` generates a pointer
    // to the embedded `DrmMmNode` without creating an intermediate reference.
    unsafe { drm_mm_node_allocated(core::ptr::addr_of!((*node).vm_node)) }
}

// upstream: include/drm/drm_vma_manager.h drm_vma_node_start()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `node` is null, returns 0.
/// - If non-null, `node` must point to an initialized, readable `DrmVmaOffsetNode`.
pub unsafe extern "C" fn drm_vma_node_start(node: *const DrmVmaOffsetNode) -> c_ulong {
    if node.is_null() {
        return 0;
    }
    // SAFETY: `node` is verified non-null above, aligned, and readable per caller contract,
    // permitting dereferencing field `(*node).vm_node.start`.
    unsafe { (*node).vm_node.start as c_ulong }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vma_manager_allocation_and_permissions() {
        // SAFETY: Stack-allocated test variables `mgr` and `node` are properly initialized and discarded at test end.
        unsafe {
            let mut mgr: DrmVmaOffsetManager = core::mem::zeroed();
            drm_vma_offset_manager_init(&mut mgr, 0x1000, 0x10000);

            let mut node: DrmVmaOffsetNode = core::mem::zeroed();
            assert!(!drm_vma_node_has_offset(&node));

            assert_eq!(drm_vma_offset_add(&mut mgr, &mut node, 4), 0);
            assert!(drm_vma_node_has_offset(&node));
            let start = drm_vma_node_start(&node);
            assert!(start >= 0x1000);

            // Lookup
            let found = drm_vma_offset_lookup_locked(&mut mgr, start, 4);
            assert_eq!(found, &mut node as *mut _);

            // Permissions
            let dummy_file = 0x5555usize as *mut DrmFile;
            assert!(!drm_vma_node_is_allowed(&mut node, dummy_file));
            assert_eq!(drm_vma_node_allow(&mut node, dummy_file), 0);
            assert!(drm_vma_node_is_allowed(&mut node, dummy_file));
            drm_vma_node_revoke(&mut node, dummy_file);
            assert!(!drm_vma_node_is_allowed(&mut node, dummy_file));

            // Remove
            drm_vma_offset_remove(&mut mgr, &mut node);
            assert!(!drm_vma_node_has_offset(&node));

            drm_vma_offset_manager_destroy(&mut mgr);
        }
    }
}

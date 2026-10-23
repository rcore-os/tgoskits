// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_drv.c`.
//!
//! DRM core device allocation, initialization, registration, device-managed
//! lifetime, unplug semantics, minors, and drm_dev_enter/exit.

use core::{
    ffi::{c_char, c_int, c_uint, c_ulong, c_void},
    sync::atomic::Ordering,
};

pub use drm_kapi::config::ENODEV;
use drm_kapi::{
    config::{EBUSY, EINVAL, ENOMEM, GFP_KERNEL},
    idr::idr_init_base,
    list::init_list_head,
    locks::spin_lock_init,
    memory::{kfree, kmalloc, kref_init, kref_put},
    mutex::{mutex_init, mutex_lock, mutex_unlock},
    types::{Kref, XArray},
    wait::wake_up_interruptible_poll,
    xarray::XA_FLAGS_ALLOC,
};

use crate::{
    drm_device_types::DrmDevice,
    drm_drv_types::DrmDriver,
    drm_file_types::{DrmFile, DrmMinor},
    drm_vma_manager_types::DrmVmaOffsetManager,
    drm_vma_manager_upstream::{
        DRM_FILE_PAGE_OFFSET_SIZE, DRM_FILE_PAGE_OFFSET_START, drm_vma_offset_manager_destroy,
        drm_vma_offset_manager_init,
    },
};

pub const DRM_MINOR_PRIMARY: c_int = 0;
pub const DRM_MINOR_CONTROL: c_int = 1;
pub const DRM_MINOR_RENDER: c_int = 2;
pub const DRM_MINOR_ACCEL: c_int = 3;

#[repr(C)]
pub struct DrmWedgeTaskInfo {
    pub pid: i32,
    pub comm: [u8; 16],
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minors_xa
#[unsafe(no_mangle)]
pub static mut drm_minors_xa: XArray = XArray::new(XA_FLAGS_ALLOC);

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_get_xa()
/// # Safety
///
/// Caller must ensure concurrent access to the returned global `drm_minors_xa` pointer is properly synchronized.
unsafe fn drm_minor_get_xa(r#type: c_int) -> *mut XArray {
    if r#type == DRM_MINOR_PRIMARY || r#type == DRM_MINOR_RENDER {
        core::ptr::addr_of_mut!(drm_minors_xa)
    } else {
        core::ptr::null_mut()
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_get_slot()
/// # Safety
///
/// If `dev` is non-null, it must point to an initialized, dereferenceable `DrmDevice`.
unsafe fn drm_minor_get_slot(dev: *mut DrmDevice, r#type: c_int) -> *mut *mut DrmMinor {
    if dev.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: callers provide a live `DrmDevice`; the null check handles the
    // only optional-pointer case, and each returned slot is embedded in it.
    unsafe {
        match r#type {
            DRM_MINOR_PRIMARY => core::ptr::addr_of_mut!((*dev).primary),
            DRM_MINOR_RENDER => core::ptr::addr_of_mut!((*dev).render),
            DRM_MINOR_ACCEL => core::ptr::addr_of_mut!((*dev).accel),
            _ => core::ptr::null_mut(),
        }
    }
}

/// `ACCEL_MAX_MINORS` (include/drm/drm_accel.h).
const ACCEL_MAX_MINORS: u32 = 256;

// upstream: drivers/gpu/drm/drm_drv.c DRM_MINOR_LIMIT()
fn drm_minor_limit(r#type: c_int) -> (u32, u32) {
    if r#type == DRM_MINOR_ACCEL {
        (0, ACCEL_MAX_MINORS - 1)
    } else {
        let base = 64 * r#type as u32;
        (base, base + 63)
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_alloc() (xa_alloc over
// DRM_MINOR_LIMIT). The index is chosen here and published by drm_minor_register().
/// # Safety
///
/// Access to the global `drm_minors_xa` registry must be synchronized by caller.
unsafe fn drm_minor_alloc_index(r#type: c_int) -> Option<u32> {
    // SAFETY: caller synchronizes access to the global minor registry.
    let xa = unsafe { drm_minor_get_xa(r#type) };
    let (first, last) = drm_minor_limit(r#type);
    if xa.is_null() {
        return Some(first);
    }
    // SAFETY: `xa` is non-null in this branch and names the global DRM
    // minor XArray; lookup borrows it only for this predicate.
    (first..=last).find(|&index| unsafe {
        drm_kapi::xarray::xa_load::<XArray, DrmMinor>(&mut *xa, index).is_null()
    })
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_alloc()
/// # Safety
///
/// `dev` must point to an initialized, writable `DrmDevice`.
unsafe fn drm_minor_alloc(dev: *mut DrmDevice, r#type: c_int) -> i32 {
    // SAFETY: caller synchronizes access to minor registry for index allocation.
    let Some(index) = (unsafe { drm_minor_alloc_index(r#type) }) else {
        return -EBUSY;
    };
    let minor = kmalloc(core::mem::size_of::<DrmMinor>(), GFP_KERNEL).cast::<DrmMinor>();
    if minor.is_null() {
        return -ENOMEM;
    }
    // SAFETY: `kmalloc` returned a live allocation large enough for one minor.
    unsafe {
        core::ptr::write_bytes(minor, 0, 1);
        (*minor).r#type = r#type;
        (*minor).dev = dev;
        (*minor).index = index as _;
    }

    // SAFETY: dev is a valid initialized DrmDevice pointer.
    let slot = unsafe { drm_minor_get_slot(dev, r#type) };
    if !slot.is_null() {
        // SAFETY: `slot` points to the selected pointer field in the live dev.
        unsafe { *slot = minor };
    }
    0
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_register()
/// # Safety
///
/// `dev` must point to an initialized `DrmDevice` with allocated minor slots.
unsafe fn drm_minor_register(dev: *mut DrmDevice, r#type: c_int) -> i32 {
    // SAFETY: dev points to an initialized DrmDevice per caller contract.
    let slot = unsafe { drm_minor_get_slot(dev, r#type) };
    // SAFETY: slot is checked non-null before dereferencing *slot.
    if slot.is_null() || unsafe { (*slot).is_null() } {
        return 0;
    }
    // SAFETY: the non-null slot contains the live minor allocated for `dev`.
    let minor = unsafe { *slot };
    // SAFETY: caller synchronizes access to the minor registry.
    let xa = unsafe { drm_minor_get_xa(r#type) };
    if !xa.is_null() {
        // SAFETY: xa is the global minor registry and the minor remains live
        // for the duration of the publication.
        unsafe {
            drm_kapi::xarray::xa_store(xa, (*minor).index as c_ulong, minor.cast(), GFP_KERNEL);
        }
    }
    0
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_unregister()
/// # Safety
///
/// `dev` must point to an initialized `DrmDevice`.
unsafe fn drm_minor_unregister(dev: *mut DrmDevice, r#type: c_int) {
    // SAFETY: dev points to an initialized DrmDevice per caller contract.
    let slot = unsafe { drm_minor_get_slot(dev, r#type) };
    // SAFETY: slot is checked non-null before dereferencing *slot.
    if slot.is_null() || unsafe { (*slot).is_null() } {
        return;
    }
    // SAFETY: the non-null slot contains the live minor allocated for `dev`.
    let minor = unsafe { *slot };
    // SAFETY: caller synchronizes access to the minor registry.
    let xa = unsafe { drm_minor_get_xa(r#type) };
    if !xa.is_null() {
        // SAFETY: xa is the global minor registry and the minor remains live
        // until its dev-managed allocation cleanup runs.
        unsafe { drm_kapi::xarray::xa_erase(xa, (*minor).index as c_ulong) };
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_acquire()
#[unsafe(no_mangle)]
/// # Safety
///
/// `minor_xa` must be null or point to an active `XArray` tracking DRM minors.
pub unsafe extern "C" fn drm_minor_acquire(
    minor_xa: *mut XArray,
    minor_id: c_uint,
) -> *mut DrmMinor {
    if minor_xa.is_null() {
        return (-ENODEV as isize) as *mut DrmMinor;
    }
    // SAFETY: the caller supplies the live minor XArray selected by the DRM
    // minor-acquire interface; this lookup borrows it only for the call.
    let minor = unsafe { drm_kapi::xarray::xa_load::<XArray, DrmMinor>(&mut *minor_xa, minor_id) };
    if minor.is_null() {
        return (-ENODEV as isize) as *mut DrmMinor;
    }
    // SAFETY: a published minor owns a reference to its live device.
    if unsafe { (*(*minor).dev).unplugged.load(Ordering::Acquire) } {
        return (-ENODEV as isize) as *mut DrmMinor;
    }
    // SAFETY: the minor's device remains live while its registry entry is held.
    unsafe { drm_dev_get((*minor).dev) };
    minor
}

// upstream: drivers/gpu/drm/drm_drv.c drm_minor_release()
#[unsafe(no_mangle)]
/// # Safety
///
/// `minor` must be null or point to a valid `DrmMinor` acquired via `drm_minor_acquire`.
pub unsafe extern "C" fn drm_minor_release(minor: *mut DrmMinor) {
    if !minor.is_null() {
        // SAFETY: the caller passes a minor reference previously returned by
        // drm_minor_acquire, so its embedded device pointer remains valid.
        let dev = unsafe { (*minor).dev };
        if !dev.is_null() {
            // SAFETY: this drops the device reference acquired with the minor.
            unsafe { drm_dev_put(dev) };
        }
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_enter()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must be null or point to an active `DrmDevice`.
/// - `idx` must be null or point to a valid, writable `c_int`.
pub unsafe extern "C" fn drm_dev_enter(dev: *mut DrmDevice, idx: *mut c_int) -> bool {
    if dev.is_null() {
        return false;
    }
    // SAFETY: `dev` was checked non-null above; `idx` is checked non-null before writing.
    unsafe {
        if !idx.is_null() {
            *idx = 0;
        }
        !(*dev).unplugged.load(Ordering::Acquire)
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_exit()
#[unsafe(no_mangle)]
/// # Safety
///
/// Caller should pass the cookie `_idx` returned by matching `drm_dev_enter`.
pub unsafe extern "C" fn drm_dev_exit(_idx: c_int) {}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_unplug()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be null or point to an active, initialized `DrmDevice`.
pub unsafe extern "C" fn drm_dev_unplug(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` was checked non-null; `filelist_mutex` serializes filelist traversal and event wakeup.
    unsafe {
        (*dev).unplugged.store(true, Ordering::Release);
        const EPOLLERR: u32 = 0x0008;
        const EPOLLHUP: u32 = 0x0010;
        mutex_lock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        let head = core::ptr::addr_of_mut!((*dev).filelist);
        let mut cur = (*head).next;
        while cur != head {
            let file = cur
                .cast::<u8>()
                .sub(core::mem::offset_of!(DrmFile, lhead))
                .cast::<DrmFile>();
            wake_up_interruptible_poll(
                core::ptr::addr_of_mut!((*file).event_wait),
                EPOLLERR | EPOLLHUP,
            );
            cur = (*cur).next;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).filelist_mutex));
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_set_dma_dev()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be null or point to an active `DrmDevice`.
pub unsafe extern "C" fn drm_dev_set_dma_dev(dev: *mut DrmDevice, dma_dev: *mut c_void) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` was checked non-null and is a valid, writable pointer.
    unsafe {
        (*dev).dma_dev = dma_dev;
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_wedged_event()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `_dev` is non-null, it must point to an active `DrmDevice`.
pub unsafe extern "C" fn drm_dev_wedged_event(
    _dev: *mut DrmDevice,
    _method: c_ulong,
    _info: *mut DrmWedgeTaskInfo,
) -> i32 {
    0
}

/// The anonymous `struct inode` of `drm_fs_inode_new()`. Only the members DRM
/// consumers read are named: `i_mapping` at byte 48 (x86_64, Linux 7.2.3).
/// `mapping_file` is the
/// LinuxKPI shmem file that owns the address_space `i_mapping` points at, the
/// stand-in for `inode->i_data`; it is released by `drm_fs_inode_free()`.
#[repr(C, align(8))]
pub struct DrmFsInode {
    _before_i_mapping: [u8; 48],
    pub i_mapping: *mut c_void,
    mapping_file: *mut drm_kapi::shmem::File,
    _tail: [u8; 480],
}

const _: () = assert!(core::mem::size_of::<DrmFsInode>() == 544);
const _: () = assert!(core::mem::offset_of!(DrmFsInode, i_mapping) == 48);

// upstream: drivers/gpu/drm/drm_drv.c drm_fs_inode_new()
// Returns the inode, or an err_ptr when the page-cache owner cannot be created.
/// # Safety
///
/// Caller assumes ownership of the returned inode pointer, which must eventually be released with `drm_fs_inode_free`.
unsafe fn drm_fs_inode_new() -> *mut c_void {
    let inode = kmalloc(core::mem::size_of::<DrmFsInode>(), GFP_KERNEL).cast::<DrmFsInode>();
    if inode.is_null() {
        return (-ENOMEM as isize) as *mut c_void;
    }
    // SAFETY: `kmalloc` returned an allocation large enough for one inode.
    unsafe { core::ptr::write_bytes(inode, 0, 1) };
    // alloc_anon_inode(): the inode's mapping is a real address_space, which
    // the shmem file owns; the empty file grows on later page faults.
    // SAFETY: `c"drm"` is NUL-terminated and the shmem API consumes the name
    // during setup, retaining its own file state on success.
    let file = unsafe { drm_kapi::shmem::shmem_file_setup(c"drm".as_ptr(), 0, 0) };
    if drm_kapi::config::is_err(file) {
        // SAFETY: inode is the still-owned allocation from the earlier kmalloc.
        unsafe { kfree(inode) };
        return file.cast();
    }
    // SAFETY: both objects are live, and inode's mapping_file retains the
    // shmem file until drm_fs_inode_free releases it.
    unsafe {
        (*inode).i_mapping = (*file).f_mapping.cast();
        (*inode).mapping_file = file;
    }
    inode.cast()
}

// upstream: drivers/gpu/drm/drm_drv.c drm_fs_inode_free()
/// # Safety
///
/// `inode` must be null or a valid pointer previously returned by `drm_fs_inode_new`.
unsafe fn drm_fs_inode_free(inode: *mut c_void) {
    if inode.is_null() {
        return;
    }
    let inode = inode.cast::<DrmFsInode>();
    // SAFETY: this is the matching release for a live inode created above;
    // its owned shmem file and inode allocation are each released once.
    unsafe {
        drm_kapi::shmem::fput_file((*inode).mapping_file);
        kfree(inode);
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_init()
/// # Safety
///
/// - `dev` must point to an uninitialized or zeroed, writable `DrmDevice` allocation.
/// - `driver` must point to a valid `DrmDriver` that outlives `dev`.
/// - `parent` must be null or a valid parent device pointer.
unsafe fn drm_dev_init(dev: *mut DrmDevice, driver: *const DrmDriver, parent: *mut c_void) -> i32 {
    // SAFETY: Caller guarantees `dev` points to a writable `DrmDevice` allocation, and `driver` outlives `dev`.
    unsafe {
        kref_init(core::ptr::addr_of_mut!((*dev).r#ref));
        (*dev).dev = parent;
        (*dev).dma_dev = parent;
        (*dev).driver = driver.cast_mut();
        (*dev).unplugged.store(false, Ordering::Relaxed);
        (*dev).driver_features = if !driver.is_null() {
            (*driver).driver_features
        } else {
            0
        };

        init_list_head(core::ptr::addr_of_mut!((*dev).managed.resources));
        spin_lock_init(&mut (*dev).managed.lock);

        mutex_init(core::ptr::addr_of_mut!((*dev).master_mutex));
        mutex_init(core::ptr::addr_of_mut!((*dev).filelist_mutex));
        init_list_head(core::ptr::addr_of_mut!((*dev).filelist));
        init_list_head(core::ptr::addr_of_mut!((*dev).filelist_internal));
        mutex_init(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        init_list_head(core::ptr::addr_of_mut!((*dev).clientlist));
        init_list_head(core::ptr::addr_of_mut!((*dev).client_sysrq_list));

        spin_lock_init(&mut (*dev).event_lock);
        spin_lock_init(&mut (*dev).vblank_time_lock);
        spin_lock_init(&mut (*dev).vbl_lock);
        init_list_head(core::ptr::addr_of_mut!((*dev).vblank_event_list));

        mutex_init(core::ptr::addr_of_mut!((*dev).object_name_lock));
        idr_init_base(core::ptr::addr_of_mut!((*dev).object_name_idr), 1);
        mutex_init(core::ptr::addr_of_mut!((*dev).gem_lru_mutex));

        let inode = drm_fs_inode_new();
        if drm_kapi::config::is_err(inode) {
            return drm_kapi::config::ptr_err(inode);
        }
        (*dev).anon_inode = inode;

        // allocate vma_offset_manager
        let vma_mgr = kmalloc(core::mem::size_of::<DrmVmaOffsetManager>(), GFP_KERNEL)
            .cast::<DrmVmaOffsetManager>();
        core::ptr::write_bytes(vma_mgr, 0, 1);
        drm_vma_offset_manager_init(
            vma_mgr,
            DRM_FILE_PAGE_OFFSET_START,
            DRM_FILE_PAGE_OFFSET_SIZE,
        );
        (*dev).vma_offset_manager = vma_mgr;

        let ret = drm_minor_alloc(dev, DRM_MINOR_PRIMARY);
        if ret != 0 {
            return ret;
        }
        let ret = drm_minor_alloc(dev, DRM_MINOR_RENDER);
        if ret != 0 {
            return ret;
        }

        if ((*dev).driver_features & (1 << 0)) != 0 {
            crate::drm_gem_upstream::drm_gem_init(dev);
        }

        0
    }
}

// upstream: drivers/gpu/drm/drm_drv.c __devm_drm_dev_alloc()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `driver` must point to a valid `DrmDriver` that outlives the returned device container.
/// - `size >= offset + size_of::<DrmDevice>()`.
/// - `offset` must be suitably aligned for `DrmDevice`.
pub unsafe extern "C" fn __devm_drm_dev_alloc(
    parent: *mut c_void,
    driver: *const DrmDriver,
    size: usize,
    offset: usize,
) -> *mut c_void {
    let container = kmalloc(size, GFP_KERNEL);
    if container.is_null() {
        return (-ENOMEM as isize) as *mut c_void;
    }
    // SAFETY: the allocation has `size` bytes; the caller contract requires
    // `offset` to select an in-bounds, properly aligned DrmDevice region.
    unsafe { core::ptr::write_bytes(container, 0, size) };
    // SAFETY: container has `size` bytes and caller contract guarantees `offset` is within bounds and properly aligned.
    let dev = unsafe { container.cast::<u8>().add(offset).cast::<DrmDevice>() };
    // SAFETY: `dev` is the caller-selected DrmDevice subobject in the live
    // allocation, and driver/parent satisfy drm_dev_init's lifetime contract.
    let ret = unsafe { drm_dev_init(dev, driver, parent) };
    if ret != 0 {
        // SAFETY: initialization failed before the container was published.
        unsafe { kfree(container) };
        return (ret as isize) as *mut c_void;
    }
    // SAFETY: successful initialization leaves this live embedded devres field.
    unsafe { (*dev).managed.final_kfree = container };
    container
}

// upstream: drivers/gpu/drm/drm_drv.c __drm_dev_alloc()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `driver` must point to a valid `DrmDriver` that outlives the returned device container.
/// - `size >= offset + size_of::<DrmDevice>()`.
/// - `offset` must be suitably aligned for `DrmDevice`.
pub unsafe extern "C" fn __drm_dev_alloc(
    parent: *mut c_void,
    driver: *const DrmDriver,
    size: usize,
    offset: usize,
) -> *mut c_void {
    let container = kmalloc(size, GFP_KERNEL);
    if container.is_null() {
        return (-ENOMEM as isize) as *mut c_void;
    }
    // SAFETY: the allocation has `size` bytes; the caller contract requires
    // `offset` to select an in-bounds, properly aligned DrmDevice region.
    unsafe { core::ptr::write_bytes(container, 0, size) };
    // SAFETY: container has `size` bytes and caller contract guarantees `offset` is within bounds and properly aligned.
    let dev = unsafe { container.cast::<u8>().add(offset).cast::<DrmDevice>() };
    // SAFETY: `dev` is the caller-selected DrmDevice subobject in the live
    // allocation, and driver/parent satisfy drm_dev_init's lifetime contract.
    let ret = unsafe { drm_dev_init(dev, driver, parent) };
    if ret != 0 {
        // SAFETY: initialization failed before the container was published.
        unsafe { kfree(container) };
        return (ret as isize) as *mut c_void;
    }
    // SAFETY: successful initialization leaves this live embedded devres field.
    unsafe { (*dev).managed.final_kfree = container };
    container
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_alloc()
#[unsafe(no_mangle)]
/// # Safety
///
/// `driver` must point to a valid `DrmDriver` that outlives the allocated `DrmDevice`.
pub unsafe extern "C" fn drm_dev_alloc(
    driver: *const DrmDriver,
    parent: *mut c_void,
) -> *mut DrmDevice {
    // SAFETY: this allocates exactly one DrmDevice with a zero offset, so the
    // device region is in bounds and properly aligned within its allocation.
    let ptr = unsafe { __drm_dev_alloc(parent, driver, core::mem::size_of::<DrmDevice>(), 0) };
    if (ptr as isize) < 0 && (ptr as isize) > -4096 {
        return core::ptr::null_mut();
    }
    ptr.cast()
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_release()
/// # Safety
///
/// `refcount` must point to the embedded `r#ref` field of an active `DrmDevice`.
unsafe extern "C" fn drm_dev_release(refcount: *mut Kref) {
    let dev = refcount
        .cast::<u8>()
        .wrapping_sub(core::mem::offset_of!(DrmDevice, r#ref))
        .cast::<DrmDevice>();
    // SAFETY: `dev` was derived via `container_of` from its embedded `r#ref` field;
    // this callback is invoked only when the last reference is dropped.
    unsafe {
        if let Some(driver) = (*dev).driver.as_ref()
            && let Some(release) = driver.release
        {
            release(dev);
        }
        if !(*dev).vma_offset_manager.is_null() {
            drm_vma_offset_manager_destroy((*dev).vma_offset_manager);
            kfree((*dev).vma_offset_manager.cast::<c_void>());
            (*dev).vma_offset_manager = core::ptr::null_mut();
        }
        if !(*dev).primary.is_null() {
            kfree((*dev).primary.cast::<c_void>());
            (*dev).primary = core::ptr::null_mut();
        }
        if !(*dev).render.is_null() {
            kfree((*dev).render.cast::<c_void>());
            (*dev).render = core::ptr::null_mut();
        }
        if !(*dev).unique.is_null() {
            kfree((*dev).unique.cast::<c_void>());
        }
        if !(*dev).anon_inode.is_null() {
            drm_fs_inode_free((*dev).anon_inode.cast());
        }
        let final_kfree = (*dev).managed.final_kfree;
        if !final_kfree.is_null() {
            kfree(final_kfree);
        } else {
            kfree(dev.cast::<c_void>());
        }
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_get()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `dev` is non-null, it must point to an active `DrmDevice` with a positive reference count.
pub unsafe extern "C" fn drm_dev_get(dev: *mut DrmDevice) -> *mut DrmDevice {
    if !dev.is_null() {
        // SAFETY: `dev` is non-null and valid per caller contract; increments `r#ref`.
        unsafe {
            drm_kapi::memory::kref_get(core::ptr::addr_of_mut!((*dev).r#ref));
        }
    }
    dev
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_put()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `dev` is non-null, it must point to an active `DrmDevice` with a positive reference count.
pub unsafe extern "C" fn drm_dev_put(dev: *mut DrmDevice) {
    if !dev.is_null() {
        // SAFETY: `dev` is non-null and valid; decrements `r#ref` and calls `drm_dev_release` on final release.
        unsafe {
            kref_put(core::ptr::addr_of_mut!((*dev).r#ref), drm_dev_release);
        }
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_put_dev()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `dev` is non-null, it must point to an active `DrmDevice` with a positive reference count.
pub unsafe extern "C" fn drm_put_dev(dev: *mut DrmDevice) {
    // SAFETY: Delegated to `drm_dev_put`.
    unsafe {
        drm_dev_put(dev);
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drmm_cgroup_register_region()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `_dev` is non-null, it must point to an active `DrmDevice`.
pub unsafe extern "C" fn drmm_cgroup_register_region(
    _dev: *mut DrmDevice,
    _region_name: *const c_char,
    _size: u64,
) -> *mut c_void {
    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_drv.c create_compat_control_link()
/// # Safety
///
/// `_dev` must point to an initialized `DrmDevice`.
unsafe fn create_compat_control_link(_dev: *mut DrmDevice) -> i32 {
    0
}

// upstream: drivers/gpu/drm/drm_drv.c remove_compat_control_link()
/// # Safety
///
/// `_dev` must point to an initialized `DrmDevice`.
unsafe fn remove_compat_control_link(_dev: *mut DrmDevice) {}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_register()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an initialized `DrmDevice` with a non-null `driver`.
pub unsafe extern "C" fn drm_dev_register(dev: *mut DrmDevice, flags: c_ulong) -> i32 {
    if dev.is_null() {
        return -EINVAL;
    }
    // SAFETY: `dev` is non-null; `(*dev).driver` is checked; registers minors and initializes KMS modeset objects.
    unsafe {
        let driver = (*dev).driver;
        if driver.is_null() {
            return -EINVAL;
        }
        if (*driver).load.is_none() {
            crate::drm_mode_config_upstream::drm_mode_config_validate(dev);
        }

        let need_global = crate::drm_file_upstream::drm_dev_needs_global_mutex(dev);
        if need_global {
            mutex_lock(core::ptr::addr_of_mut!(
                crate::drm_file_upstream::drm_global_mutex
            ));
        }

        let mut ret = drm_minor_register(dev, DRM_MINOR_RENDER);
        if ret != 0 {
            if need_global {
                mutex_unlock(core::ptr::addr_of_mut!(
                    crate::drm_file_upstream::drm_global_mutex
                ));
            }
            return ret;
        }
        ret = drm_minor_register(dev, DRM_MINOR_PRIMARY);
        if ret != 0 {
            drm_minor_unregister(dev, DRM_MINOR_RENDER);
            if need_global {
                mutex_unlock(core::ptr::addr_of_mut!(
                    crate::drm_file_upstream::drm_global_mutex
                ));
            }
            return ret;
        }

        create_compat_control_link(dev);
        (*dev).registered = true;

        if let Some(load) = (*driver).load {
            ret = load(dev, flags);
            if ret != 0 {
                remove_compat_control_link(dev);
                drm_minor_unregister(dev, DRM_MINOR_PRIMARY);
                drm_minor_unregister(dev, DRM_MINOR_RENDER);
                if need_global {
                    mutex_unlock(core::ptr::addr_of_mut!(
                        crate::drm_file_upstream::drm_global_mutex
                    ));
                }
                return ret;
            }
        }

        if ((*dev).driver_features & (1 << 1)) != 0 {
            ret = crate::drm_mode_config_upstream::drm_modeset_register_all(dev);
            if ret != 0 {
                if let Some(unload) = (*driver).unload {
                    unload(dev);
                }
                remove_compat_control_link(dev);
                drm_minor_unregister(dev, DRM_MINOR_PRIMARY);
                drm_minor_unregister(dev, DRM_MINOR_RENDER);
                if need_global {
                    mutex_unlock(core::ptr::addr_of_mut!(
                        crate::drm_file_upstream::drm_global_mutex
                    ));
                }
                return ret;
            }
        }

        if need_global {
            mutex_unlock(core::ptr::addr_of_mut!(
                crate::drm_file_upstream::drm_global_mutex
            ));
        }
        0
    }
}

// upstream: drivers/gpu/drm/drm_drv.c drm_dev_unregister()
#[unsafe(no_mangle)]
/// # Safety
///
/// If `dev` is non-null, it must point to an initialized `DrmDevice`.
pub unsafe extern "C" fn drm_dev_unregister(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is non-null; unregisters minors and unloads driver safely.
    unsafe {
        (*dev).registered = false;
        crate::drm_client_event_upstream::drm_client_dev_unregister(dev);

        if ((*dev).driver_features & (1 << 1)) != 0 {
            crate::drm_mode_config_upstream::drm_modeset_unregister_all(dev);
        }

        if let Some(driver) = (*dev).driver.as_ref()
            && let Some(unload) = driver.unload
        {
            unload(dev);
        }

        remove_compat_control_link(dev);
        drm_minor_unregister(dev, DRM_MINOR_ACCEL);
        drm_minor_unregister(dev, DRM_MINOR_PRIMARY);
        drm_minor_unregister(dev, DRM_MINOR_RENDER);
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    static RELEASE_CALLED: AtomicBool = AtomicBool::new(false);

    /// # Safety
    ///
    /// Test driver release callback; `_dev` is ignored.
    unsafe extern "C" fn test_driver_release(_dev: *mut DrmDevice) {
        RELEASE_CALLED.store(true, Ordering::SeqCst);
    }

    #[test]
    fn test_device_lifecycle_and_enter_exit() {
        crate::test_support::init_runtime();
        // SAFETY: `DrmDriver` is a POD-like C struct with optional function pointer fields, valid to zero-initialize.
        let mut driver: DrmDriver = unsafe { core::mem::zeroed() };
        driver.driver_features = 0;
        driver.release = Some(test_driver_release);
        RELEASE_CALLED.store(false, Ordering::SeqCst);

        // SAFETY: Test exercises driver and device lifecycle using stack driver and dynamically allocated DrmDevice.
        unsafe {
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            assert!(!dev.is_null());
            assert!(!(*dev).primary.is_null());
            assert!(!(*dev).render.is_null());
            assert!(!(*dev).vma_offset_manager.is_null());

            // Test register
            assert_eq!(drm_dev_register(dev, 0), 0);
            assert!((*dev).registered);

            // Test enter/exit before unplug
            let mut idx: c_int = -1;
            assert!(drm_dev_enter(dev, &mut idx));
            assert_eq!(idx, 0);
            drm_dev_exit(idx);

            // Test minor acquire/release
            let minor = drm_minor_acquire(core::ptr::addr_of_mut!(drm_minors_xa), 0);
            assert!(!minor.is_null());
            assert_eq!((*minor).dev, dev);
            drm_minor_release(minor);

            // Test unplug
            drm_dev_unplug(dev);
            assert!((*dev).unplugged.load(Ordering::Acquire));
            assert!(!drm_dev_enter(dev, &mut idx));

            // Test unregister
            drm_dev_unregister(dev);
            assert!(!(*dev).registered);

            // Test refcounting & release
            drm_dev_get(dev);
            drm_dev_put(dev);
            assert!(!RELEASE_CALLED.load(Ordering::SeqCst));

            drm_dev_put(dev);
            assert!(RELEASE_CALLED.load(Ordering::SeqCst));
        }
    }
}

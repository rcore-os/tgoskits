// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_client_event.c`.

#![allow(non_snake_case, non_upper_case_globals)]

use core::mem::offset_of;

use drm_kapi::list::list_del;
#[cfg(not(test))]
use drm_kapi::mutex::{mutex_lock, mutex_unlock};
#[cfg(test)]
/// # Safety
/// No-op in test builds; caller must uphold any lock semantics.
unsafe fn mutex_lock<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
/// No-op in test builds; caller must uphold any lock semantics.
unsafe fn mutex_unlock<M>(_mutex: M) {}

use crate::{
    drm_client_types::{DrmClientDev, DrmClientFuncs},
    drm_device_types::DrmDevice,
    drm_mode_object_upstream::{DRIVER_MODESET, drm_core_check_feature},
};

#[inline]
/// # Safety
/// `node` must point to the `list` field within a valid, properly aligned, live `DrmClientDev` instance.
unsafe fn client_from_node(node: *mut drm_kapi::types::ListHead) -> *mut DrmClientDev {
    // SAFETY: 'node' points to the 'list' field of a live, valid DrmClientDev at offset_of!(DrmClientDev, list).
    // Pointer arithmetic subtracts this offset to compute the outer DrmClientDev pointer without aliasing violations.
    unsafe { node.cast::<u8>().sub(offset_of!(DrmClientDev, list)).cast() }
}

#[inline]
/// # Safety
/// `node` and `head` must belong to the same circular list head of `DrmClientDev` elements,
/// and `node` must point to a valid `list` field embedded in `DrmClientDev` if not null and not equal to `head`.
unsafe fn next_client(
    node: *mut drm_kapi::types::ListHead,
    head: *mut drm_kapi::types::ListHead,
) -> *mut DrmClientDev {
    if node.is_null() || core::ptr::eq(node, head) {
        return core::ptr::null_mut();
    }
    // SAFETY: 'node' is non-null and not equal to 'head', meaning it is an entry node in the clientlist
    // where the 'list' field is embedded in a valid DrmClientDev instance.
    unsafe { client_from_node(node) }
}

/// # Safety
/// `client`, if non-null, must point to a valid, live, aligned `DrmClientDev` whose `funcs` pointer
/// (if non-null) points to a valid `DrmClientFuncs` vtable. Access must be serialized under `clientlist_mutex`.
unsafe fn drm_client_hotplug(client: *mut DrmClientDev) {
    if client.is_null() {
        return;
    }
    // SAFETY: 'client' is non-null and valid as checked above. The client and its funcs table are valid and
    // preserved during callback invocation. Mutating 'hotplug_pending' and 'hotplug_failed' is safe
    // because access to client list operations is serialized by the caller holding clientlist_mutex.
    unsafe {
        let funcs: *mut DrmClientFuncs = (*client).funcs;
        if funcs.is_null() || (*funcs).hotplug.is_none() || (*client).hotplug_failed {
            return;
        }
        if (*client).suspended {
            (*client).hotplug_pending = true;
            return;
        }
        (*client).hotplug_pending = false;
        let ret = (*funcs).hotplug.unwrap()(client);
        drm_kapi::print::drm_log_at(
            drm_kapi::print::DrmLogLevel::Debug,
            "drm",
            file!(),
            line!(),
            &alloc::format!("client hotplug ret={ret}"),
        );
        if ret != 0 {
            (*client).hotplug_failed = true;
        }
    }
}

// upstream: drm_client_event.c drm_client_dev_unregister()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must either be null or point to an initialized, live, properly aligned `DrmDevice`.
/// The caller must ensure that `dev` is not concurrently freed.
pub unsafe extern "C" fn drm_client_dev_unregister(dev: *mut DrmDevice) {
    // SAFETY: 'dev' is non-null and points to an initialized DrmDevice.
    if dev.is_null() || !unsafe { drm_core_check_feature(dev, DRIVER_MODESET) } {
        return;
    }
    // SAFETY: 'dev' is non-null, properly aligned, and points to an initialized DrmDevice.
    // Access to 'clientlist' is serialized by acquiring 'clientlist_mutex'. 'list_del' removes each node
    // before unregister/release, avoiding use-after-free by saving 'next' beforehand.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).clientlist);
        mutex_lock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let next = (*node).next;
            let client = next_client(node, head);
            list_del(node);
            // The callback consumes and may free the client; use `next`, not
            // client state, after the call.
            if let Some(unregister) = (*client).funcs.as_ref().and_then(|f| f.unregister) {
                unregister(client);
            } else {
                crate::drm_client_upstream::drm_client_release(client);
            }
            node = next;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
    }
}

// upstream: drm_client_event.c drm_client_dev_hotplug()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must either be null or point to an initialized, live, properly aligned `DrmDevice`.
/// The caller must ensure that `dev` is not concurrently freed.
pub unsafe extern "C" fn drm_client_dev_hotplug(dev: *mut DrmDevice) {
    // SAFETY: 'dev' is non-null and points to an initialized DrmDevice.
    if dev.is_null() || !unsafe { drm_core_check_feature(dev, DRIVER_MODESET) } {
        return;
    }
    // SAFETY: 'dev' is non-null, properly aligned, and points to an initialized DrmDevice.
    // Access to 'clientlist' is serialized by acquiring 'clientlist_mutex'. All clients in the list
    // are valid DrmClientDev instances.
    unsafe {
        if (*dev).mode_config.num_connector == 0 {
            return;
        }
        let head = core::ptr::addr_of_mut!((*dev).clientlist);
        mutex_lock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let client = next_client(node, head);
            drm_client_hotplug(client);
            node = (*node).next;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
    }
}

// upstream: drm_client_event.c drm_client_dev_restore()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must either be null or point to an initialized, live, properly aligned `DrmDevice`.
/// The caller must ensure that `dev` is not concurrently freed.
pub unsafe extern "C" fn drm_client_dev_restore(dev: *mut DrmDevice, force: bool) {
    // SAFETY: 'dev' is non-null and points to an initialized DrmDevice.
    if dev.is_null() || !unsafe { drm_core_check_feature(dev, DRIVER_MODESET) } {
        return;
    }
    // SAFETY: 'dev' is non-null, properly aligned, and points to an initialized DrmDevice.
    // Access to 'clientlist' is serialized under 'clientlist_mutex'. Each client node traversed
    // is a valid DrmClientDev instance.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).clientlist);
        mutex_lock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let client = next_client(node, head);
            if let Some(restore) = (*client).funcs.as_ref().and_then(|f| f.restore)
                && restore(client, force) == 0
            {
                break;
            }
            node = (*node).next;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
    }
}

/// # Safety
/// `client` must point to a valid, live, aligned `DrmClientDev` whose `funcs` pointer
/// (if non-null) points to a valid `DrmClientFuncs` vtable. Access must be serialized under `clientlist_mutex`.
unsafe fn drm_client_suspend(client: *mut DrmClientDev) -> i32 {
    // SAFETY: 'client' is guaranteed valid, aligned, and non-null by the caller holding 'clientlist_mutex'.
    // The 'funcs' vtable reference is read safely and optional callback invoked if present.
    unsafe {
        if (*client).suspended {
            return 0;
        }
        let ret = (*client)
            .funcs
            .as_ref()
            .and_then(|f| f.suspend)
            .map_or(0, |suspend| suspend(client));
        (*client).suspended = true;
        ret
    }
}

// upstream: drm_client_event.c drm_client_dev_suspend()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must either be null or point to an initialized, live, properly aligned `DrmDevice`.
/// The caller must ensure that `dev` is not concurrently freed.
pub unsafe extern "C" fn drm_client_dev_suspend(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: 'dev' is non-null, properly aligned, and points to an initialized DrmDevice.
    // 'clientlist' iteration and mutations are protected by 'clientlist_mutex'. Each node in the list
    // is embedded in a valid DrmClientDev instance.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).clientlist);
        mutex_lock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let client = next_client(node, head);
            if !(*client).suspended {
                let _ = drm_client_suspend(client);
            }
            node = (*node).next;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
    }
}

/// # Safety
/// `client` must point to a valid, live, aligned `DrmClientDev` whose `funcs` pointer
/// (if non-null) points to a valid `DrmClientFuncs` vtable. Access must be serialized under `clientlist_mutex`.
unsafe fn drm_client_resume(client: *mut DrmClientDev) -> i32 {
    // SAFETY: 'client' is guaranteed valid, aligned, and non-null by the caller holding 'clientlist_mutex'.
    // The 'funcs' vtable reference is read safely and optional callback invoked if present.
    unsafe {
        if !(*client).suspended {
            return 0;
        }
        let ret = (*client)
            .funcs
            .as_ref()
            .and_then(|f| f.resume)
            .map_or(0, |resume| resume(client));
        (*client).suspended = false;
        if (*client).hotplug_pending {
            drm_client_hotplug(client);
        }
        ret
    }
}

// upstream: drm_client_event.c drm_client_dev_resume()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must either be null or point to an initialized, live, properly aligned `DrmDevice`.
/// The caller must ensure that `dev` is not concurrently freed.
pub unsafe extern "C" fn drm_client_dev_resume(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: 'dev' is non-null, properly aligned, and points to an initialized DrmDevice.
    // Iterating and updating clients on 'clientlist' is synchronized via 'clientlist_mutex'.
    // Each traversed node is embedded in a valid DrmClientDev instance.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).clientlist);
        mutex_lock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let client = next_client(node, head);
            if (*client).suspended {
                let _ = drm_client_resume(client);
            }
            node = (*node).next;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).clientlist_mutex));
    }
}

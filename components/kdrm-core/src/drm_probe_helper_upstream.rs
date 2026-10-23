// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_probe_helper.c`.

use core::{
    mem::offset_of,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(not(test))]
use drm_kapi::mutex::{mutex_lock, mutex_trylock, mutex_unlock};
use drm_kapi::{
    config::EDEADLK,
    types::{DelayedWork, WorkStruct},
    workqueue::{SYSTEM_DFL_WQ, cancel_delayed_work_sync, init_delayed_work, mod_delayed_work},
};
#[cfg(test)]
/// # Safety
///
/// Test stub; performs no memory operations.
unsafe fn mutex_lock<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
///
/// Test stub; performs no memory operations.
unsafe fn mutex_unlock<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
///
/// Test stub; performs no memory operations.
unsafe fn mutex_trylock<M>(_mutex: M) -> bool {
    true
}

use crate::{
    drm_connector_types::{
        CONNECTOR_STATUS_CONNECTED, CONNECTOR_STATUS_DISCONNECTED, CONNECTOR_STATUS_UNKNOWN,
        DRM_FORCE_ON, DRM_FORCE_ON_DIGITAL, DRM_FORCE_UNSPECIFIED, DRM_MODE_TV_MODE_MONOCHROME,
        DRM_MODE_TV_MODE_NTSC, DRM_MODE_TV_MODE_NTSC_443, DRM_MODE_TV_MODE_NTSC_J,
        DRM_MODE_TV_MODE_PAL, DRM_MODE_TV_MODE_PAL_M, DRM_MODE_TV_MODE_PAL_N,
        DRM_MODE_TV_MODE_SECAM, DrmConnector, DrmConnectorListIter,
    },
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_modes_types::{
        DrmDisplayMode, DrmModeStatus, MODE_BAD, MODE_ERROR, MODE_NO_DBLESCAN, MODE_NO_INTERLACE,
        MODE_NO_STEREO, MODE_OK, MODE_STALE,
    },
    drm_modeset_helper_vtables_types::{
        DrmConnectorHelperFuncs, DrmCrtcHelperFuncs, DrmEncoderHelperFuncs,
    },
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    uapi_drm_consts::{
        DRM_MODE_CONNECTOR_DISPLAYPORT, DRM_MODE_FLAG_3D_MASK, DRM_MODE_FLAG_DBLSCAN,
        DRM_MODE_FLAG_INTERLACE, DRM_MODE_TYPE_PREFERRED, DRM_MODE_TYPE_USERDEF,
    },
};

const DRM_CONNECTOR_POLL_CONNECT: u8 = 1 << 0;
const DRM_CONNECTOR_POLL_DISCONNECT: u8 = 1 << 1;
const DRM_CONNECTOR_POLL_HPD: u8 = 1 << 2;
const DRM_OUTPUT_POLL_PERIOD: u64 = 10 * drm_kapi::config::HZ as u64;
const DRM_KMS_HELPER_POLL_ENABLED: bool = true;
static POLL_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct PollWorkerGuard;

impl Drop for PollWorkerGuard {
    fn drop(&mut self) {
        POLL_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

/// # Safety
///
/// `node` must point to the `head` field of a valid, live `DrmDisplayMode` allocation.
#[inline]
unsafe fn mode_at(node: *mut drm_kapi::types::ListHead) -> *mut DrmDisplayMode {
    // SAFETY: `node` points to the `head` list node of an enclosing `DrmDisplayMode`
    // within an active allocation per caller preconditions. Offset arithmetic matches
    // the layout of `DrmDisplayMode`.
    unsafe {
        node.cast::<u8>()
            .sub(offset_of!(DrmDisplayMode, head))
            .cast()
    }
}

// upstream: drm_probe_helper.c drm_mode_validate_flag()
/// # Safety
///
/// `mode` must be a valid, readable pointer to an initialized `DrmDisplayMode`.
unsafe fn drm_mode_validate_flag(mode: *const DrmDisplayMode, flags: u32) -> DrmModeStatus {
    // SAFETY: `mode` is a valid, readable pointer to an initialized `DrmDisplayMode`
    // per caller preconditions; reading `flags` does not alias mutably.
    unsafe {
        if (*mode).flags & DRM_MODE_FLAG_INTERLACE != 0 && flags & DRM_MODE_FLAG_INTERLACE == 0 {
            return MODE_NO_INTERLACE;
        }
        if (*mode).flags & DRM_MODE_FLAG_DBLSCAN != 0 && flags & DRM_MODE_FLAG_DBLSCAN == 0 {
            return MODE_NO_DBLESCAN;
        }
        if (*mode).flags & DRM_MODE_FLAG_3D_MASK != 0 && flags & DRM_MODE_FLAG_3D_MASK == 0 {
            return MODE_NO_STEREO;
        }
    }
    MODE_OK
}

// upstream: drm_probe_helper.c drm_connector_mode_valid()
/// # Safety
///
/// - `connector` must be a valid, live pointer to `DrmConnector`.
/// - `mode` must be a valid, live pointer to `DrmDisplayMode`.
/// - `status` must be a valid, writable pointer to `DrmModeStatus`.
/// - If `ctx` is non-null, it must point to a valid `DrmModesetAcquireCtx`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_mode_valid(
    connector: *mut DrmConnector,
    mode: *mut DrmDisplayMode,
    ctx: *mut DrmModesetAcquireCtx,
    status: *mut DrmModeStatus,
) -> i32 {
    // SAFETY: `connector` and `status` are valid and live per caller preconditions.
    // If `helper_private` is non-null, it points to a valid `DrmConnectorHelperFuncs`
    // table that remains live for the duration of the connector. Driver callbacks
    // are invoked with valid pointers as required by their contracts.
    unsafe {
        let funcs: *mut DrmConnectorHelperFuncs = (*connector).helper_private;
        if funcs.is_null() {
            *status = MODE_OK;
        } else if let Some(mode_valid_ctx) = (*funcs).mode_valid_ctx {
            return mode_valid_ctx(connector, mode, ctx, status);
        } else if let Some(mode_valid) = (*funcs).mode_valid {
            *status = mode_valid(connector, mode);
        } else {
            *status = MODE_OK;
        }
    }
    0
}

/// # Safety
///
/// `encoder` and `crtc` must be valid, live pointers to initialized `DrmEncoder`
/// and `DrmCrtc` structs respectively.
unsafe fn drm_encoder_crtc_ok(encoder: *mut DrmEncoder, crtc: *mut DrmCrtc) -> bool {
    // SAFETY: `encoder` and `crtc` are valid, live pointers per caller preconditions.
    // Reading `possible_crtcs` is sound, and `drm_crtc_mask` safely accesses `crtc->index`.
    unsafe { ((*encoder).possible_crtcs & crate::drm_crtc_upstream::drm_crtc_mask(crtc)) != 0 }
}

/// # Safety
///
/// - `mode` must be a valid, live pointer to `DrmDisplayMode`.
/// - `connector` must be a valid, live pointer to `DrmConnector` attached to a valid `dev`.
/// - `status` must be a valid, writable pointer to `DrmModeStatus`.
/// - `ctx` must be a valid pointer to `DrmModesetAcquireCtx` or null.
/// - Modeset locks protecting `encoder_list` and `crtc_list` must be held by the caller.
unsafe fn drm_mode_validate_pipeline(
    mode: *mut DrmDisplayMode,
    connector: *mut DrmConnector,
    ctx: *mut DrmModesetAcquireCtx,
    status: *mut DrmModeStatus,
) -> i32 {
    // SAFETY: `connector`, `mode`, and `status` are valid per caller preconditions.
    // `(*connector).dev` points to a live `DrmDevice` whose `mode_config` lists are
    // protected by the caller's modeset synchronization. List entries in `encoder_list`
    // and `crtc_list` are embedded in valid `DrmEncoder` and `DrmCrtc` structs; offset
    // subtraction via `offset_of!` correctly recovers container pointers within their allocations.
    unsafe {
        let dev = (*connector).dev;
        let ret = drm_connector_mode_valid(connector, mode, ctx, status);
        if ret != 0 || *status != MODE_OK {
            return ret;
        }
        let enc_head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut enc_link = (*enc_head).next;
        while !core::ptr::eq(enc_link, enc_head) {
            let encoder = enc_link
                .cast::<u8>()
                .sub(offset_of!(DrmEncoder, head))
                .cast::<DrmEncoder>();
            enc_link = (*enc_link).next;
            if ((*connector).possible_encoders & (1u32 << (*encoder).index)) == 0 {
                continue;
            }
            *status = drm_encoder_mode_valid(encoder, mode);
            if *status != MODE_OK {
                continue;
            }

            if !drm_kapi::list::list_empty(&(*encoder).bridge_chain) {
                *status = MODE_ERROR;
                continue;
            }

            let crtc_head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
            let mut crtc_link = (*crtc_head).next;
            while !core::ptr::eq(crtc_link, crtc_head) {
                let crtc = crtc_link
                    .cast::<u8>()
                    .sub(offset_of!(DrmCrtc, head))
                    .cast::<DrmCrtc>();
                crtc_link = (*crtc_link).next;
                if !drm_encoder_crtc_ok(encoder, crtc) {
                    continue;
                }
                *status = drm_crtc_mode_valid(crtc, mode);
                if *status == MODE_OK {
                    return 0;
                }
            }
        }
        ret
    }
}

// upstream: drm_probe_helper.c drm_crtc_mode_valid()
/// # Safety
///
/// - If non-null, `crtc` must point to a valid, live `DrmCrtc`.
/// - `mode` must point to a valid, readable `DrmDisplayMode`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_crtc_mode_valid(
    crtc: *mut DrmCrtc,
    mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    if crtc.is_null() {
        return MODE_OK;
    }
    // SAFETY: `crtc` is verified non-null and points to a live `DrmCrtc` per caller
    // preconditions. If `helper_private` is non-null, it points to a valid
    // `DrmCrtcHelperFuncs` vtable. `mode` is cast to mut per C callback signature.
    unsafe {
        let funcs: *mut DrmCrtcHelperFuncs = (*crtc).helper_private;
        if funcs.is_null() {
            return MODE_OK;
        }
        match (*funcs).mode_valid {
            Some(mode_valid) => mode_valid(crtc, mode.cast_mut()),
            None => MODE_OK,
        }
    }
}

// upstream: drm_probe_helper.c drm_encoder_mode_valid()
/// # Safety
///
/// - If non-null, `encoder` must point to a valid, live `DrmEncoder`.
/// - `mode` must point to a valid, readable `DrmDisplayMode`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_encoder_mode_valid(
    encoder: *mut DrmEncoder,
    mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    if encoder.is_null() {
        return MODE_OK;
    }
    // SAFETY: `encoder` is verified non-null and points to a live `DrmEncoder` per
    // caller preconditions. If `helper_private` is non-null, it points to a valid
    // `DrmEncoderHelperFuncs` vtable. `mode` is cast to mut per C callback signature.
    unsafe {
        let funcs: *mut DrmEncoderHelperFuncs = (*encoder).helper_private;
        if funcs.is_null() {
            return MODE_OK;
        }
        match (*funcs).mode_valid {
            Some(mode_valid) => mode_valid(encoder, mode.cast_mut()),
            None => MODE_OK,
        }
    }
}

// upstream: drm_crtc_helper.c drm_connector_get_single_encoder()
/// # Safety
///
/// - If non-null, `connector` must point to a valid, live `DrmConnector`.
/// - `(*connector).dev` must point to a valid `DrmDevice` whose `encoder_list` is stable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_get_single_encoder(
    connector: *mut DrmConnector,
) -> *mut DrmEncoder {
    // SAFETY: `connector` is verified non-null by the first check, so reading its `dev` field is valid.
    if connector.is_null() || unsafe { (*connector).dev.is_null() } {
        return core::ptr::null_mut();
    }
    // SAFETY: `connector` and `(*connector).dev` are non-null and valid. Traversal of
    // `dev->mode_config.encoder_list` accesses nodes embedded in live `DrmEncoder` allocations
    // where offset subtraction via `offset_of!(DrmEncoder, head)` yields valid container pointers.
    unsafe {
        let dev = (*connector).dev;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let encoder = node
                .cast::<u8>()
                .sub(offset_of!(DrmEncoder, head))
                .cast::<DrmEncoder>();
            if ((*connector).possible_encoders & (1u32 << (*encoder).index)) != 0 {
                return encoder;
            }
            node = (*node).next;
        }
    }
    core::ptr::null_mut()
}

/// # Safety
///
/// - `connector` must be a valid, live pointer to `DrmConnector`.
/// - If `ctx` is non-null, it must be an initialized `DrmModesetAcquireCtx`.
/// - Modeset locks required by the driver's detect callbacks must be held by the caller.
unsafe fn detect_connector_status(
    connector: *mut DrmConnector,
    ctx: *mut DrmModesetAcquireCtx,
    force: bool,
) -> i32 {
    // SAFETY: `connector` is valid and live per caller preconditions. If `helper_private`
    // or `funcs` is non-null, they point to initialized function tables whose lifetimes
    // outlive `connector`. Driver detect callbacks are invoked with caller-provided arguments.
    unsafe {
        let funcs: *mut DrmConnectorHelperFuncs = (*connector).helper_private;
        if !funcs.is_null()
            && let Some(detect_ctx) = (*funcs).detect_ctx
        {
            return detect_ctx(connector, ctx, force);
        }
        if !(*connector).funcs.is_null()
            && let Some(detect) = (*(*connector).funcs).detect
        {
            return detect(connector, force) as i32;
        }
    }
    CONNECTOR_STATUS_CONNECTED as i32
}

/// # Safety
///
/// `connector` must be a valid, live pointer to `DrmConnector` whose `dev` points to
/// a valid `DrmDevice`.
unsafe fn drm_helper_probe_detect_locked(connector: *mut DrmConnector, force: bool) -> i32 {
    // SAFETY: `connector` is valid and live per caller preconditions, so reading `dev` is sound.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `DrmModesetAcquireCtx` is a C-compatible struct zero-initialized before acquisition.
    let mut ctx: DrmModesetAcquireCtx = unsafe { core::mem::zeroed() };
    // SAFETY: `ctx` points to the local stack-allocated acquire context; flags are 0.
    unsafe { crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0) };
    let ret = loop {
        // SAFETY: `dev` is valid, so `connection_mutex` is a live modeset lock.
        // `ctx` is the local acquire context initialized above.
        let ret = unsafe {
            crate::drm_modeset_lock_upstream::drm_modeset_lock(
                core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
                &mut ctx,
            )
        };
        if ret == -EDEADLK {
            // SAFETY: `ctx` is an initialized acquire context backed by the stack that encountered deadlock.
            unsafe { crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx) };
            continue;
        }
        if ret != 0 {
            break CONNECTOR_STATUS_UNKNOWN as i32;
        }
        // SAFETY: `connector` is valid, and `ctx` holds `connection_mutex` acquired above.
        let detect = unsafe { detect_connector_status(connector, &mut ctx, force) };
        if detect == -EDEADLK {
            // SAFETY: `ctx` encountered deadlock during status detection and must back off.
            unsafe { crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx) };
            continue;
        }
        break if detect < 0 {
            CONNECTOR_STATUS_UNKNOWN as i32
        } else {
            detect
        };
    };
    // SAFETY: `connector` is valid per preconditions, allowing `status` and `epoch_counter`
    // access. `ctx` was initialized above and must drop held locks and finalize before exiting.
    unsafe {
        if ret as u32 != (*connector).status {
            (*connector).epoch_counter = (*connector).epoch_counter.wrapping_add(1);
        }
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
    }
    ret
}

// upstream: drm_probe_helper.c drm_helper_probe_detect()
/// # Safety
///
/// - If non-null, `connector` must point to a valid, live `DrmConnector`.
/// - If `ctx` is non-null, it must be an initialized `DrmModesetAcquireCtx`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_helper_probe_detect(
    connector: *mut DrmConnector,
    ctx: *mut DrmModesetAcquireCtx,
    force: bool,
) -> i32 {
    // SAFETY: `connector` is verified non-null by the first check, making reading `dev` valid.
    if connector.is_null() || unsafe { (*connector).dev.is_null() } {
        return CONNECTOR_STATUS_UNKNOWN as i32;
    }
    // SAFETY: `connector` and `(*connector).dev` are verified non-null. When `ctx` is non-null,
    // `connection_mutex` is locked with the caller-provided context and `detect_connector_status`
    // is called with valid pointers. `epoch_counter` is updated on the live connector.
    unsafe {
        let dev = (*connector).dev;
        if ctx.is_null() {
            return drm_helper_probe_detect_locked(connector, force);
        }
        let ret = crate::drm_modeset_lock_upstream::drm_modeset_lock(
            core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
            ctx,
        );
        if ret != 0 {
            return ret;
        }
        let status = detect_connector_status(connector, ctx, force);
        if status >= 0 && status as u32 != (*connector).status {
            (*connector).epoch_counter = (*connector).epoch_counter.wrapping_add(1);
        }
        status
    }
}

/// # Safety
///
/// `connector` must point to a valid, live `DrmConnector` with initialized
/// `cmdline_mode`, `probed_modes` list, and valid `dev` pointer.
unsafe fn drm_helper_probe_add_cmdline_mode(connector: *mut DrmConnector) -> i32 {
    // SAFETY: `connector` and `(*connector).dev` are valid per caller preconditions.
    // `probed_modes` is an intrusive list of `DrmDisplayMode` nodes; `mode_at` safely recovers
    // enclosing structs. `drm_mode_create_from_cmdline_mode` and `drm_mode_probed_add` operate
    // on valid device, mode, and connector pointers.
    unsafe {
        let cmdline = core::ptr::addr_of_mut!((*connector).cmdline_mode);
        if !(*cmdline).specified {
            return 0;
        }
        let head = core::ptr::addr_of_mut!((*connector).probed_modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            node = (*node).next;
            if (*mode).hdisplay as i32 != (*cmdline).xres
                || (*mode).vdisplay as i32 != (*cmdline).yres
            {
                continue;
            }
            if (*cmdline).refresh_specified
                && crate::drm_modes_upstream::drm_mode_vrefresh(mode) != (*cmdline).refresh
            {
                continue;
            }
            (*mode).r#type |= DRM_MODE_TYPE_USERDEF as u8;
            return 0;
        }
        let mode =
            crate::drm_modes_upstream::drm_mode_create_from_cmdline_mode((*connector).dev, cmdline);
        if mode.is_null() {
            return 0;
        }
        crate::drm_modes_upstream::drm_mode_probed_add(connector, mode);
        1
    }
}

/// # Safety
///
/// `connector` must be a valid, live pointer to `DrmConnector`.
unsafe fn drm_helper_probe_get_modes(connector: *mut DrmConnector) -> i32 {
    // SAFETY: `connector` is valid per caller preconditions. If `helper_private` is non-null,
    // it points to a valid `DrmConnectorHelperFuncs` with an optional `get_modes` hook.
    // `drm_edid_override_connector_update` operates on the live connector.
    unsafe {
        let funcs: *mut DrmConnectorHelperFuncs = (*connector).helper_private;
        if funcs.is_null() || (*funcs).get_modes.is_none() {
            return 0;
        }
        let mut count = (*funcs).get_modes.unwrap()(connector);
        if count < 0 {
            count = 0;
        }
        if count == 0 && (*connector).status == CONNECTOR_STATUS_CONNECTED {
            count = crate::drm_edid_upstream::drm_edid_override_connector_update(connector);
        }
        count
    }
}

/// # Safety
///
/// - `connector` must be a valid, live pointer to `DrmConnector` associated with a valid `dev`.
/// - If `ctx` is non-null, it must be an initialized `DrmModesetAcquireCtx`.
/// - `connector->modes` list must be protected against concurrent modification.
unsafe fn __drm_helper_update_and_validate(
    connector: *mut DrmConnector,
    max_x: u32,
    max_y: u32,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `connector` and `(*connector).dev` are valid and live per caller preconditions.
    // Iterating `connector->modes` traverses intrusive nodes in valid `DrmDisplayMode` structs.
    // Driver and size validation helpers receive valid pointers to mode, device, and connector.
    unsafe {
        let dev = (*connector).dev;
        crate::drm_modes_upstream::drm_connector_list_update(connector);
        let mut allowed_flags = 0;
        if (*connector).interlace_allowed {
            allowed_flags |= DRM_MODE_FLAG_INTERLACE;
        }
        if (*connector).doublescan_allowed {
            allowed_flags |= DRM_MODE_FLAG_DBLSCAN;
        }
        if (*connector).stereo_allowed {
            allowed_flags |= DRM_MODE_FLAG_3D_MASK;
        }
        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            node = (*node).next;
            if (*mode).status != MODE_OK {
                continue;
            }
            (*mode).status = crate::drm_modes_upstream::drm_mode_validate_driver(dev, mode);
            if (*mode).status != MODE_OK {
                continue;
            }
            (*mode).status =
                crate::drm_modes_upstream::drm_mode_validate_size(mode, max_x as i32, max_y as i32);
            if (*mode).status != MODE_OK {
                continue;
            }
            (*mode).status = drm_mode_validate_flag(mode, allowed_flags);
            if (*mode).status != MODE_OK {
                continue;
            }
            (*mode).status = crate::drm_modes_upstream::drm_mode_validate_ycbcr420(mode, connector);
            if (*mode).status != MODE_OK {
                continue;
            }
            let ret = drm_mode_validate_pipeline(
                mode,
                connector,
                ctx,
                core::ptr::addr_of_mut!((*mode).status),
            );
            if ret != 0 {
                if ret == -EDEADLK {
                    return ret;
                }
                (*mode).status = MODE_ERROR;
            }
        }
    }
    0
}

// upstream: drm_probe_helper.c drm_helper_probe_single_connector_modes()
/// # Safety
///
/// - If non-null, `connector` must point to a valid, live `DrmConnector` associated with a valid `DrmDevice`.
/// - Modeset locks protecting connector modes and configuration must be acquired or coordinated as expected.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_helper_probe_single_connector_modes(
    connector: *mut DrmConnector,
    max_x: u32,
    max_y: u32,
) -> i32 {
    // SAFETY: `connector` is verified non-null by the first check, so reading `dev` is sound.
    if connector.is_null() || unsafe { (*connector).dev.is_null() } {
        return 0;
    }
    // SAFETY: `connector` was verified non-null above, so reading its `dev` pointer is sound.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `DrmModesetAcquireCtx` is a plain C-compatible struct zero-initialized before acquisition.
    let mut ctx: DrmModesetAcquireCtx = unsafe { core::mem::zeroed() };
    // SAFETY: `ctx` points to the local stack-allocated acquire context; flags are 0.
    unsafe { crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0) };

    let result = loop {
        // SAFETY: `dev` is valid, so `connection_mutex` is a live modeset lock. `ctx` is our initialized context.
        let lock_ret = unsafe {
            crate::drm_modeset_lock_upstream::drm_modeset_lock(
                core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
                &mut ctx,
            )
        };
        if lock_ret == -EDEADLK {
            // SAFETY: `ctx` encountered deadlock while locking `connection_mutex` and must back off.
            unsafe { crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx) };
            continue;
        }
        if lock_ret != 0 {
            break 0;
        }

        // SAFETY: `connector` is valid and live, so reading `status` is sound.
        let old_status = unsafe { (*connector).status };
        // SAFETY: `connector->modes` is an intrusive list; nodes are embedded in live `DrmDisplayMode` structs.
        unsafe {
            let head = core::ptr::addr_of_mut!((*connector).modes);
            let mut node = (*head).next;
            while !core::ptr::eq(node, head) {
                (*mode_at(node)).status = MODE_STALE;
                node = (*node).next;
            }
        }
        // SAFETY: `connector` is valid and live, so reading `force` is sound.
        let status = if unsafe { (*connector).force != DRM_FORCE_UNSPECIFIED } {
            // SAFETY: `connector` is valid and live, so reading `force` is sound.
            let forced = unsafe { (*connector).force };
            let new_status = if forced == DRM_FORCE_ON || forced == DRM_FORCE_ON_DIGITAL {
                CONNECTOR_STATUS_CONNECTED
            } else {
                CONNECTOR_STATUS_DISCONNECTED
            };
            // SAFETY: `connector` is valid and live; `funcs->force` if non-null is a valid callback.
            unsafe {
                (*connector).status = new_status;
                let funcs = (*connector).funcs;
                if !funcs.is_null()
                    && let Some(force) = (*funcs).force
                {
                    force(connector);
                }
            }
            new_status as i32
        } else {
            // SAFETY: `connector` is valid and live; `ctx` is our active modeset acquire context.
            unsafe { drm_helper_probe_detect(connector, &mut ctx, true) }
        };
        if status == -EDEADLK {
            // SAFETY: `ctx` encountered deadlock during probe detect and must back off.
            unsafe { crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx) };
            continue;
        }
        let status = if status < 0 {
            CONNECTOR_STATUS_UNKNOWN as i32
        } else {
            status
        };
        // SAFETY: `connector` is valid and live, so writing `status` is sound.
        unsafe { (*connector).status = status as u32 };

        if old_status as i32 != status {
            // SAFETY: `dev` is valid and live; `output_poll_work` is an initialized delayed work struct.
            unsafe {
                (*dev).mode_config.delayed_event = true;
                if (*dev).mode_config.poll_enabled {
                    mod_delayed_work(
                        SYSTEM_DFL_WQ,
                        core::ptr::addr_of_mut!((*dev).mode_config.output_poll_work),
                        0,
                    );
                }
            }
        }
        // SAFETY: `dev` is valid and live, so reading `poll_enabled` is sound.
        if unsafe { (*dev).mode_config.poll_enabled } {
            // SAFETY: `dev` is valid and live.
            unsafe { drm_kms_helper_poll_enable(dev) };
        }

        if status as u32 == CONNECTOR_STATUS_DISCONNECTED {
            // SAFETY: `connector` and `dev` are valid and live; pruning modes operates on `connector->modes`.
            unsafe {
                crate::drm_edid_upstream::drm_connector_update_edid_property(
                    connector,
                    core::ptr::null(),
                );
                crate::drm_modes_upstream::drm_mode_prune_invalid(
                    dev,
                    core::ptr::addr_of_mut!((*connector).modes),
                    false,
                );
            }
            break 0;
        }

        // SAFETY: `connector` is valid and live per caller preconditions.
        let mut count = unsafe { drm_helper_probe_get_modes(connector) };
        if count == 0
            && (status as u32 == CONNECTOR_STATUS_CONNECTED
                || status as u32 == CONNECTOR_STATUS_UNKNOWN)
        {
            // SAFETY: `connector` is valid and live per caller preconditions.
            count = unsafe { crate::drm_edid_upstream::drm_add_modes_noedid(connector, 1024, 768) };
            // SAFETY: `connector` is valid and live, so reading `connector_type` is sound.
            if unsafe { (*connector).connector_type as u32 == DRM_MODE_CONNECTOR_DISPLAYPORT } {
                // SAFETY: `connector` is valid and live.
                unsafe { crate::drm_modes_upstream::drm_set_preferred_mode(connector, 640, 480) };
            }
        }
        // SAFETY: `connector` is valid and live.
        count += unsafe { drm_helper_probe_add_cmdline_mode(connector) };
        if count != 0 {
            // SAFETY: `connector` is valid and live; `ctx` is our active modeset acquire context.
            let ret =
                unsafe { __drm_helper_update_and_validate(connector, max_x, max_y, &mut ctx) };
            if ret == -EDEADLK {
                // SAFETY: `ctx` encountered deadlock during validation and must back off.
                unsafe { crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx) };
                continue;
            }
        }
        // SAFETY: `dev` and `connector` are valid and live; `connector->modes` is a valid list.
        unsafe {
            crate::drm_modes_upstream::drm_mode_prune_invalid(
                dev,
                core::ptr::addr_of_mut!((*connector).modes),
                true,
            );
        }
        // SAFETY: `connector` is valid and live, so accessing `modes` is sound.
        if unsafe { drm_kapi::list::list_empty(&(*connector).modes) }
            // SAFETY: `connector` is valid and live, so reading `connector_type` is sound.
            && unsafe { (*connector).connector_type as u32 == DRM_MODE_CONNECTOR_DISPLAYPORT }
        {
            // SAFETY: `connector` is valid and live.
            count = unsafe { crate::drm_edid_upstream::drm_add_modes_noedid(connector, 640, 480) };
            // SAFETY: `connector` is valid and live; `ctx` is our active acquire context.
            let ret =
                unsafe { __drm_helper_update_and_validate(connector, max_x, max_y, &mut ctx) };
            if ret == -EDEADLK {
                // SAFETY: `ctx` encountered deadlock during validation and must back off.
                unsafe { crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx) };
                continue;
            }
            // SAFETY: `dev` and `connector` are valid and live.
            unsafe {
                crate::drm_modes_upstream::drm_mode_prune_invalid(
                    dev,
                    core::ptr::addr_of_mut!((*connector).modes),
                    true,
                );
            }
        }
        break count;
    };

    // SAFETY: `ctx` was initialized above and must drop all held locks and finalize before exit.
    // `connector->modes` list is sorted and its modes updated with valid CRTC info.
    unsafe {
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
        if drm_kapi::list::list_empty(&(*connector).modes) {
            return 0;
        }
        crate::drm_modes_upstream::drm_mode_sort(core::ptr::addr_of_mut!((*connector).modes));
        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            crate::drm_modes_upstream::drm_mode_set_crtcinfo(
                mode,
                crate::drm_modes_upstream::CRTC_INTERLACE_HALVE_V as i32,
            );
            node = (*node).next;
        }
    }
    result
}

/// # Safety
///
/// `dev` must be a valid, live pointer to `DrmDevice`.
unsafe fn drm_kms_helper_disable_hpd(dev: *mut DrmDevice) {
    // SAFETY: `DrmConnectorListIter` is a C-compatible iterator struct zero-initialized before start.
    let mut iter: DrmConnectorListIter = unsafe { core::mem::zeroed() };
    // SAFETY: `dev` is valid and live per caller preconditions; `iter` is local storage.
    unsafe { crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut iter) };
    loop {
        // SAFETY: `iter` is an active connector list iterator initialized above.
        let connector =
            unsafe { crate::drm_connector_upstream::drm_connector_list_iter_next(&mut iter) };
        if connector.is_null() {
            break;
        }
        // SAFETY: `connector` is a live pointer returned by the iterator. `helper_private` if non-null
        // points to a valid `DrmConnectorHelperFuncs` vtable containing an optional `disable_hpd` hook.
        unsafe {
            let funcs: *mut DrmConnectorHelperFuncs = (*connector).helper_private;
            if !funcs.is_null()
                && let Some(disable_hpd) = (*funcs).disable_hpd
            {
                disable_hpd(connector);
            }
        }
    }
    // SAFETY: `iter` was begun above and must be ended to drop any iterator references.
    unsafe { crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter) };
}

/// # Safety
///
/// `dev` must be a valid, live pointer to `DrmDevice`.
unsafe fn drm_kms_helper_enable_hpd(dev: *mut DrmDevice) -> bool {
    let mut poll = false;
    // SAFETY: `DrmConnectorListIter` is a C-compatible iterator struct zero-initialized before start.
    let mut iter: DrmConnectorListIter = unsafe { core::mem::zeroed() };
    // SAFETY: `dev` is valid and live per caller preconditions; `iter` is local storage.
    unsafe { crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut iter) };
    loop {
        // SAFETY: `iter` is an active connector list iterator initialized above.
        let connector =
            unsafe { crate::drm_connector_upstream::drm_connector_list_iter_next(&mut iter) };
        if connector.is_null() {
            break;
        }
        // SAFETY: `connector` is a live pointer returned by the iterator. `helper_private` if non-null
        // points to a valid `DrmConnectorHelperFuncs` vtable. Reading `polled` is sound on live connector.
        unsafe {
            let funcs: *mut DrmConnectorHelperFuncs = (*connector).helper_private;
            if !funcs.is_null()
                && let Some(enable_hpd) = (*funcs).enable_hpd
            {
                enable_hpd(connector);
            }
            poll |= (*connector).polled
                & (DRM_CONNECTOR_POLL_CONNECT | DRM_CONNECTOR_POLL_DISCONNECT)
                != 0;
        }
    }
    // SAFETY: `iter` was begun above and must be ended to release iterator resources.
    unsafe { crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter) };
    poll
}

/// # Safety
///
/// `dev` must be a valid, live pointer to `DrmDevice` with initialized `output_poll_work`.
unsafe fn reschedule_output_poll_work(dev: *mut DrmDevice) {
    // SAFETY: `dev` is valid and live per caller preconditions. `output_poll_work` is an
    // initialized delayed work item in `dev->mode_config`, and `SYSTEM_DFL_WQ` is a live workqueue.
    unsafe {
        let delay = if (*dev).mode_config.delayed_event {
            drm_kapi::config::HZ as u64
        } else {
            DRM_OUTPUT_POLL_PERIOD
        };
        mod_delayed_work(
            SYSTEM_DFL_WQ,
            core::ptr::addr_of_mut!((*dev).mode_config.output_poll_work),
            delay,
        );
    }
}

/// # Safety
///
/// If non-null, `work` must be the `work` member embedded in `dev->mode_config.output_poll_work`.
unsafe fn output_poll_device_from_work(work: *mut WorkStruct) -> *mut DrmDevice {
    if work.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `work` is verified non-null above. By construction in `drm_kms_helper_poll_init`,
    // `work` is embedded within `DelayedWork`, which is embedded in `DrmModeConfig::output_poll_work`,
    // which is embedded in `DrmDevice::mode_config`. Offset calculations via `offset_of!` match
    // the layout of these enclosing structures within the same allocation.
    unsafe {
        let delayed = work
            .cast::<u8>()
            .sub(offset_of!(DelayedWork, work))
            .cast::<DelayedWork>();
        let config = delayed
            .cast::<u8>()
            .sub(offset_of!(
                crate::drm_mode_config_types::DrmModeConfig,
                output_poll_work
            ))
            .cast::<crate::drm_mode_config_types::DrmModeConfig>();
        config
            .cast::<u8>()
            .sub(offset_of!(DrmDevice, mode_config))
            .cast::<DrmDevice>()
    }
}

/// # Safety
///
/// If non-null, `work` must be the `work` member of `dev->mode_config.output_poll_work`.
unsafe fn output_poll_execute(work: *mut WorkStruct) {
    POLL_WORKER_ACTIVE.store(true, Ordering::Release);
    let _poll_worker_guard = PollWorkerGuard;
    // SAFETY: `work` points to the work struct passed by the workqueue runtime.
    let dev = unsafe { output_poll_device_from_work(work) };
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is non-null and points to a live `DrmDevice` recovered from `work`.
    let mut changed = unsafe { (*dev).mode_config.delayed_event };
    let mut repoll = false;
    // SAFETY: `dev` is valid and live.
    unsafe { (*dev).mode_config.delayed_event = false };
    // SAFETY: `dev` is valid and live, so reading `poll_enabled` is sound.
    if unsafe { !(*dev).mode_config.poll_enabled } {
        return;
    }
    // SAFETY: `dev->mode_config.mutex` is an initialized mutex in the live device.
    if !unsafe { mutex_trylock(core::ptr::addr_of_mut!((*dev).mode_config.mutex)) } {
        repoll = true;
    } else {
        // SAFETY: `DrmConnectorListIter` is zero-initialized before iterator begin.
        let mut iter: DrmConnectorListIter = unsafe { core::mem::zeroed() };
        // SAFETY: `dev` is valid and live, and `mode_config.mutex` is locked.
        unsafe { crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut iter) };
        loop {
            // SAFETY: `iter` is an active iterator over the live device connectors.
            let connector =
                unsafe { crate::drm_connector_upstream::drm_connector_list_iter_next(&mut iter) };
            if connector.is_null() {
                break;
            }
            // SAFETY: `connector` is a live connector returned by the iterator. We check its polled
            // flags and detect changes while `mode_config.mutex` is held.
            unsafe {
                // Forced connectors are controlled by the command line, and
                // HPD-only connectors are handled by their interrupt path.
                if (*connector).force != DRM_FORCE_UNSPECIFIED
                    || (*connector).polled == 0
                    || (*connector).polled == DRM_CONNECTOR_POLL_HPD
                {
                    continue;
                }
                let old_status = (*connector).status;
                if old_status == CONNECTOR_STATUS_CONNECTED
                    && (*connector).polled & DRM_CONNECTOR_POLL_DISCONNECT == 0
                {
                    continue;
                }
                repoll = true;
                let old_epoch = (*connector).epoch_counter;
                let new_status = drm_helper_probe_detect(connector, core::ptr::null_mut(), false);
                if old_epoch == (*connector).epoch_counter {
                    continue;
                }
                if new_status as u32 == CONNECTOR_STATUS_UNKNOWN {
                    (*connector).status = old_status;
                    continue;
                }
                (*connector).status = new_status as u32;
                changed = true;
            }
        }
        // SAFETY: `iter` is ended to release iterator state, and `mode_config.mutex` is unlocked.
        unsafe {
            crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter);
            mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        }
    }
    // SAFETY: `dev` is valid and live; hotplug event emission and rescheduling operate on the live device.
    unsafe {
        if changed {
            drm_kms_helper_hotplug_event(dev);
        }
        if repoll && (*dev).mode_config.poll_running {
            reschedule_output_poll_work(dev);
        }
    }
}

fn output_poll_work_callback(work: &mut WorkStruct) {
    // SAFETY: `work` is a live mutable reference provided by the workqueue runtime,
    // corresponding to `dev->mode_config.output_poll_work.work`.
    unsafe { output_poll_execute(work as *mut WorkStruct) };
}

// upstream: drm_probe_helper.c drm_kms_helper_poll_enable()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_poll_enable(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null and points to a live `DrmDevice` per caller preconditions.
    // Rescheduling delayed work operates on `dev->mode_config.output_poll_work`.
    unsafe {
        if !(*dev).mode_config.poll_enabled
            || !DRM_KMS_HELPER_POLL_ENABLED
            || (*dev).mode_config.poll_running
        {
            return;
        }
        if drm_kms_helper_enable_hpd(dev) || (*dev).mode_config.delayed_event {
            reschedule_output_poll_work(dev);
        }
        (*dev).mode_config.poll_running = true;
    }
}

// upstream: drm_probe_helper.c drm_kms_helper_poll_reschedule()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_poll_reschedule(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null, so reading `poll_running` is sound.
    if unsafe { (*dev).mode_config.poll_running } {
        // SAFETY: `dev` is valid and live.
        unsafe { reschedule_output_poll_work(dev) };
    }
}

// upstream: drm_probe_helper.c drm_kms_helper_hotplug_event()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_hotplug_event(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null and points to a live `DrmDevice` per caller preconditions.
    unsafe { crate::drm_client_event_upstream::drm_client_dev_hotplug(dev) };
}

// upstream: drm_probe_helper.c drm_kms_helper_connector_hotplug_event()
/// # Safety
///
/// If non-null, `connector` must point to a valid, live `DrmConnector` whose `dev` points to a live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_connector_hotplug_event(connector: *mut DrmConnector) {
    if connector.is_null() {
        return;
    }
    // SAFETY: `connector` is verified non-null and its `dev` points to a live `DrmDevice` per caller preconditions.
    unsafe { crate::drm_client_event_upstream::drm_client_dev_hotplug((*connector).dev) };
}

// upstream: drm_probe_helper.c drm_kms_helper_poll_disable()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_poll_disable(dev: *mut DrmDevice) {
    // SAFETY: `dev` is non-null by first check, so reading `poll_enabled` is sound.
    if dev.is_null() || unsafe { !(*dev).mode_config.poll_enabled } {
        return;
    }
    // SAFETY: `dev` is verified non-null and points to a live `DrmDevice` per caller preconditions.
    // Cancelling `output_poll_work` synchronizes with any executing delayed work instance.
    unsafe {
        if (*dev).mode_config.poll_running {
            drm_kms_helper_disable_hpd(dev);
        }
        cancel_delayed_work_sync(core::ptr::addr_of_mut!((*dev).mode_config.output_poll_work));
        (*dev).mode_config.poll_running = false;
    }
}

// upstream: drm_probe_helper.c drm_kms_helper_poll_init()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice` whose `output_poll_work` is allocated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_poll_init(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null and points to a live `DrmDevice` per caller preconditions.
    // `init_delayed_work` initializes the embedded `output_poll_work` in `dev->mode_config`.
    unsafe {
        init_delayed_work(
            &mut (*dev).mode_config.output_poll_work,
            output_poll_work_callback,
        );
        (*dev).mode_config.poll_enabled = true;
        drm_kms_helper_poll_enable(dev);
    }
}

// upstream: drm_probe_helper.c drm_kms_helper_poll_fini()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_poll_fini(dev: *mut DrmDevice) {
    // SAFETY: `dev` is non-null by first check, so reading `poll_enabled` is sound.
    if dev.is_null() || unsafe { !(*dev).mode_config.poll_enabled } {
        return;
    }
    // SAFETY: `dev` is valid and live, so disabling polling and clearing `poll_enabled` is sound.
    unsafe {
        drm_kms_helper_poll_disable(dev);
        (*dev).mode_config.poll_enabled = false;
    }
}

/// # Safety
///
/// `dev` must point to a valid, live `DrmDevice` registered with drmm.
unsafe extern "C" fn drm_kms_helper_poll_init_release(
    dev: *mut DrmDevice,
    _data: *mut core::ffi::c_void,
) {
    // SAFETY: `dev` is a valid `DrmDevice` pointer passed by the drmm managed action cleanup.
    unsafe { drm_kms_helper_poll_fini(dev) };
}

// upstream: drm_probe_helper.c drmm_kms_helper_poll_init()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice` managed by drmm.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_kms_helper_poll_init(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: `dev` is verified non-null and points to a live `DrmDevice` per caller preconditions.
    // `__drmm_add_action_or_reset` registers `drm_kms_helper_poll_init_release` to clean up on device release.
    unsafe {
        drm_kms_helper_poll_init(dev);
        let ret = crate::drm_managed_upstream::__drmm_add_action_or_reset(
            dev,
            Some(drm_kms_helper_poll_init_release),
            dev.cast(),
            c"drm_kms_helper_poll_fini".as_ptr(),
        );
        if ret != 0 {
            drm_kapi::print::drm_log_at(
                drm_kapi::print::DrmLogLevel::Warn,
                "drm",
                file!(),
                line!(),
                &alloc::format!("failed to register poll cleanup action: {ret}"),
            );
        }
    }
}

// upstream: drm_probe_helper.c drm_kms_helper_is_poll_worker()
/// # Safety
///
/// Safe to call from any context; marked unsafe for C-ABI export compatibility.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_kms_helper_is_poll_worker() -> bool {
    POLL_WORKER_ACTIVE.load(Ordering::Acquire) && !drm_kapi::workqueue::current_work().is_null()
}

// upstream: drm_probe_helper.c check_connector_changed()
/// # Safety
///
/// `connector` must point to a valid, live `DrmConnector` with a valid `dev`.
unsafe fn check_connector_changed(connector: *mut DrmConnector) -> bool {
    // SAFETY: `connector` is valid and live per caller preconditions. `drm_helper_probe_detect`
    // is called with null ctx under caller's mutex synchronization.
    unsafe {
        let dev = (*connector).dev;
        let old_status = (*connector).status;
        let old_epoch = (*connector).epoch_counter;
        let new_status = drm_helper_probe_detect(connector, core::ptr::null_mut(), false);
        (*connector).status = new_status as u32;
        let _ = dev;
        old_epoch != (*connector).epoch_counter || old_status != (*connector).status
    }
}

// upstream: drm_probe_helper.c drm_connector_helper_hpd_irq_event()
/// # Safety
///
/// - If non-null, `connector` must point to a valid, live `DrmConnector`.
/// - `(*connector).dev` must point to a valid `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_helper_hpd_irq_event(connector: *mut DrmConnector) -> bool {
    // SAFETY: `connector` is verified non-null by the first check, making reading `dev` sound.
    if connector.is_null() || unsafe { (*connector).dev.is_null() } {
        return false;
    }
    // SAFETY: `connector` was verified non-null above, so reading its `dev` pointer is sound.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `dev` is valid and live; `mode_config.mutex` is an initialized mutex protecting probe state.
    let changed = unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        let changed = check_connector_changed(connector);
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        changed
    };
    if changed {
        // SAFETY: `connector` is valid and live.
        unsafe { drm_kms_helper_connector_hotplug_event(connector) };
    }
    changed
}

// upstream: drm_probe_helper.c drm_helper_hpd_irq_event()
/// # Safety
///
/// If non-null, `dev` must point to a valid, live `DrmDevice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_helper_hpd_irq_event(dev: *mut DrmDevice) -> bool {
    // SAFETY: `dev` is non-null by the first check, so reading `poll_enabled` is sound.
    if dev.is_null() || unsafe { !(*dev).mode_config.poll_enabled } {
        return false;
    }
    let mut changed_count = 0usize;
    let mut first_changed: *mut DrmConnector = core::ptr::null_mut();
    // SAFETY: `dev` is valid and live. `mode_config.mutex` is an initialized mutex protecting
    // connector iteration and detection. Reference counting via `drm_connector_get` / `drm_connector_put`
    // ensures `first_changed` remains valid across event dispatch and is properly released before exit.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        let mut iter: DrmConnectorListIter = core::mem::zeroed();
        crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut iter);
        loop {
            let connector = crate::drm_connector_upstream::drm_connector_list_iter_next(&mut iter);
            if connector.is_null() {
                break;
            }
            if (*connector).polled & DRM_CONNECTOR_POLL_HPD == 0 {
                continue;
            }
            if check_connector_changed(connector) {
                if first_changed.is_null() {
                    crate::drm_connector_upstream::drm_connector_get(connector);
                    first_changed = connector;
                }
                changed_count += 1;
            }
        }
        crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter);
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        if changed_count == 1 {
            drm_kms_helper_connector_hotplug_event(first_changed);
        } else if changed_count > 1 {
            drm_kms_helper_hotplug_event(dev);
        }
        if !first_changed.is_null() {
            crate::drm_connector_upstream::drm_connector_put(first_changed);
        }
    }
    changed_count != 0
}

// upstream: drm_crtc_helper.c drm_crtc_helper_mode_valid_fixed()
/// # Safety
///
/// If non-null, `mode` and `fixed_mode` must point to valid, readable `DrmDisplayMode` structs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_crtc_helper_mode_valid_fixed(
    _crtc: *mut DrmCrtc,
    mode: *const DrmDisplayMode,
    fixed_mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    if mode.is_null() || fixed_mode.is_null() {
        return MODE_BAD;
    }
    // SAFETY: `mode` and `fixed_mode` are verified non-null and point to valid `DrmDisplayMode`
    // structs per caller preconditions. Reading display dimensions is sound.
    unsafe {
        if (*mode).hdisplay != (*fixed_mode).hdisplay && (*mode).vdisplay != (*fixed_mode).vdisplay
        {
            crate::drm_modes_types::MODE_ONE_SIZE
        } else if (*mode).hdisplay != (*fixed_mode).hdisplay {
            crate::drm_modes_types::MODE_ONE_WIDTH
        } else if (*mode).vdisplay != (*fixed_mode).vdisplay {
            crate::drm_modes_types::MODE_ONE_HEIGHT
        } else {
            MODE_OK
        }
    }
}

// upstream: drm_probe_helper.c drm_connector_helper_get_modes_fixed()
/// # Safety
///
/// - If non-null, `connector` must point to a valid, live `DrmConnector` with a valid `dev`.
/// - `fixed_mode` must point to a valid, readable `DrmDisplayMode`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_helper_get_modes_fixed(
    connector: *mut DrmConnector,
    fixed_mode: *const DrmDisplayMode,
) -> i32 {
    if connector.is_null() || fixed_mode.is_null() {
        return 0;
    }
    // SAFETY: `connector` and `fixed_mode` are verified non-null. `drm_mode_duplicate` allocates
    // a new mode linked to `dev`, which is then named and added to `connector->probed_modes`.
    // Mutating `display_info` on the live connector is sound.
    unsafe {
        let mode = crate::drm_modes_upstream::drm_mode_duplicate((*connector).dev, fixed_mode);
        if mode.is_null() {
            return 0;
        }
        if (*mode).name[0] == 0 {
            crate::drm_modes_upstream::drm_mode_set_name(mode);
        }
        (*mode).r#type |= DRM_MODE_TYPE_PREFERRED as u8;
        crate::drm_modes_upstream::drm_mode_probed_add(connector, mode);
        if (*mode).width_mm != 0 {
            (*connector).display_info.width_mm = (*mode).width_mm as u32;
        }
        if (*mode).height_mm != 0 {
            (*connector).display_info.height_mm = (*mode).height_mm as u32;
        }
    }
    1
}

// upstream: drm_probe_helper.c drm_connector_helper_get_modes()
/// # Safety
///
/// If non-null, `connector` must point to a valid, live `DrmConnector`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_helper_get_modes(connector: *mut DrmConnector) -> i32 {
    if connector.is_null() {
        return 0;
    }
    // SAFETY: `connector` is verified non-null and points to a live `DrmConnector` per caller preconditions.
    // `drm_edid_read` obtains an EDID allocation, updated on connector and parsed into modes,
    // and freed via `drm_edid_free` before return.
    unsafe {
        let edid = crate::drm_edid_upstream::drm_edid_read(connector);
        crate::drm_edid_upstream::drm_edid_connector_update(connector, edid);
        let count = crate::drm_edid_upstream::drm_edid_connector_add_modes(connector);
        crate::drm_edid_upstream::drm_edid_free(edid);
        count
    }
}

// upstream: drm_probe_helper.c drm_connector_helper_tv_get_modes()
/// # Safety
///
/// If non-null, `connector` must point to a valid, live `DrmConnector` with a valid `dev`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_helper_tv_get_modes(connector: *mut DrmConnector) -> i32 {
    // SAFETY: `connector` is verified non-null by the first check, making reading `dev` sound.
    if connector.is_null() || unsafe { (*connector).dev.is_null() } {
        return 0;
    }
    // SAFETY: `connector` and `(*connector).dev` are verified non-null. Property values pointer
    // is traversed up to `property->num_values` entries within the allocated property values array.
    // Generated modes are created via `drm_analog_tv_mode` and added to `connector->probed_modes`.
    unsafe {
        let dev = (*connector).dev;
        let property = (*dev).mode_config.tv_mode_property;
        let cmdline = core::ptr::addr_of!((*connector).cmdline_mode);
        if property.is_null() {
            return 0;
        }
        let ntsc_modes = (1u32 << DRM_MODE_TV_MODE_NTSC)
            | (1u32 << DRM_MODE_TV_MODE_NTSC_443)
            | (1u32 << DRM_MODE_TV_MODE_NTSC_J)
            | (1u32 << DRM_MODE_TV_MODE_PAL_M);
        let pal_modes = (1u32 << DRM_MODE_TV_MODE_PAL)
            | (1u32 << DRM_MODE_TV_MODE_PAL_N)
            | (1u32 << DRM_MODE_TV_MODE_SECAM);
        let mut supported = 0u32;
        for i in 0..(*property).num_values as usize {
            let value = *(*property).values.add(i) as u32;
            if value < 32 {
                supported |= 1u32 << value;
            }
        }
        let mut modes = [u32::MAX, u32::MAX];
        if ((supported & ntsc_modes) != 0 && (supported & pal_modes) != 0)
            || supported & (1u32 << DRM_MODE_TV_MODE_MONOCHROME) != 0
        {
            let mut default_mode = 0u64;
            if crate::drm_mode_object_upstream::drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                property,
                &mut default_mode,
            ) != 0
            {
                return 0;
            }
            if (*cmdline).tv_mode_specified {
                default_mode = (*cmdline).tv_mode as u64;
            }
            if default_mode < 32 && 1u32 << default_mode as u32 & ntsc_modes != 0 {
                modes = [DRM_MODE_TV_MODE_NTSC, DRM_MODE_TV_MODE_PAL];
            } else {
                modes = [DRM_MODE_TV_MODE_PAL, DRM_MODE_TV_MODE_NTSC];
            }
        } else if supported & ntsc_modes != 0 {
            modes[0] = DRM_MODE_TV_MODE_NTSC;
        } else if supported & pal_modes != 0 {
            modes[0] = DRM_MODE_TV_MODE_PAL;
        } else {
            return 0;
        }

        let mut count = 0;
        for mode_type in modes {
            if mode_type == u32::MAX {
                break;
            }
            let (tv_mode, height) = if mode_type == DRM_MODE_TV_MODE_NTSC {
                (DRM_MODE_TV_MODE_NTSC, 480)
            } else {
                (DRM_MODE_TV_MODE_PAL, 576)
            };
            let mode = crate::drm_modes_upstream::drm_analog_tv_mode(
                dev, tv_mode, 13_500_000, 720, height, true,
            );
            if mode.is_null() {
                return count;
            }
            if count == 0 {
                (*mode).r#type |= DRM_MODE_TYPE_PREFERRED as u8;
            }
            crate::drm_modes_upstream::drm_mode_probed_add(connector, mode);
            count += 1;
        }
        count
    }
}

// upstream: drm_probe_helper.c drm_connector_helper_detect_from_ddc()
/// # Safety
///
/// If non-null, `connector` must point to a valid, live `DrmConnector`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_connector_helper_detect_from_ddc(
    connector: *mut DrmConnector,
    _ctx: *mut DrmModesetAcquireCtx,
    _force: bool,
) -> i32 {
    if connector.is_null() {
        return CONNECTOR_STATUS_UNKNOWN as i32;
    }
    // SAFETY: `connector` is verified non-null. `(*connector).ddc` if non-null points to
    // an initialized I2C adapter passed to `drm_probe_ddc`.
    unsafe {
        let ddc = (*connector).ddc;
        if ddc.is_null() {
            return CONNECTOR_STATUS_UNKNOWN as i32;
        }
        if crate::drm_edid_upstream::drm_probe_ddc(ddc) {
            CONNECTOR_STATUS_CONNECTED as i32
        } else {
            CONNECTOR_STATUS_DISCONNECTED as i32
        }
    }
}

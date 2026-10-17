// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_client_modeset.c`.

#![allow(non_snake_case, non_upper_case_globals)]

use alloc::{vec, vec::Vec};
use core::mem::offset_of;

#[cfg(not(test))]
use drm_kapi::mutex::{mutex_destroy, mutex_init, mutex_lock, mutex_unlock};
use drm_kapi::{
    config::{EINVAL, ENOMEM, IS_ERR, PTR_ERR},
    memory::{kfree, kzalloc_objs},
};
#[cfg(test)]
/// # Safety
/// Test mock; no preconditions.
unsafe fn mutex_destroy<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
/// Test mock; no preconditions.
unsafe fn mutex_init<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
/// Test mock; no preconditions.
unsafe fn mutex_lock<M>(_mutex: M) {}
#[cfg(test)]
/// # Safety
/// Test mock; no preconditions.
unsafe fn mutex_unlock<M>(_mutex: M) {}

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_client_types::DrmClientDev,
    drm_connector_types::{
        CONNECTOR_STATUS_CONNECTED, CONNECTOR_STATUS_DISCONNECTED,
        DRM_MODE_PANEL_ORIENTATION_BOTTOM_UP, DRM_MODE_PANEL_ORIENTATION_LEFT_UP,
        DRM_MODE_PANEL_ORIENTATION_RIGHT_UP, DrmCmdlineMode, DrmConnector, DrmConnectorListIter,
    },
    drm_crtc_types::{DrmCrtc, DrmModeSet},
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_modes_types::DrmDisplayMode,
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_plane_types::{DRM_PLANE_TYPE_PRIMARY, DrmPlane},
    drm_property_types::DrmProperty,
    uapi_drm_consts::{
        DRM_MODE_DPMS_ON, DRM_MODE_FLAG_INTERLACE, DRM_MODE_ROTATE_0, DRM_MODE_ROTATE_90,
        DRM_MODE_ROTATE_180, DRM_MODE_ROTATE_270, DRM_MODE_ROTATE_MASK, DRM_MODE_TYPE_PREFERRED,
        DRM_MODE_TYPE_USERDEF,
    },
};

const DRM_CLIENT_MAX_CLONED_CONNECTORS: usize = 8;

#[derive(Clone, Copy, Default)]
struct ClientOffset {
    x: i32,
    y: i32,
}

/// Returns a pointer to the mode-set configuration at `index` for `client`.
///
/// # Safety
/// `client` must point to a live, valid `DrmClientDev` whose `modesets` array
/// is allocated and contains at least `index + 1` entries.
#[inline]
unsafe fn mode_set_at(client: *mut DrmClientDev, index: usize) -> *mut DrmModeSet {
    // SAFETY: caller guarantees client and client.modesets are valid for index + 1 entries.
    unsafe { (*client).modesets.add(index) }
}

/// Iterates over every active mode set in `client` until a null CRTC is reached.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` whose `modesets` array is null-CRTC terminated.
unsafe fn drm_client_for_each_modeset(
    client: *mut DrmClientDev,
    mut callback: impl FnMut(*mut DrmModeSet),
) {
    // SAFETY: client is checked non-null before dereferencing modesets.
    if client.is_null() || unsafe { (*client).modesets.is_null() } {
        return;
    }
    let mut i = 0;
    loop {
        // SAFETY: client is valid and non-null, indexing within modesets array.
        let modeset = unsafe { mode_set_at(client, i) };
        // SAFETY: modeset points to a valid DrmModeSet entry within client.modesets.
        if unsafe { (*modeset).crtc.is_null() } {
            break;
        }
        callback(modeset);
        i += 1;
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_create()
/// Initializes the modeset configuration for `client`.
///
/// # Safety
/// `client` must point to a live, valid `DrmClientDev` whose `dev` pointer points
/// to an initialized `DrmDevice`. Must not be called concurrently on the same client.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_create(client: *mut DrmClientDev) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client and dev are confirmed non-null and valid.
    unsafe {
        let dev = (*client).dev;
        let num_crtc = (*dev).mode_config.num_crtc.max(0) as usize;
        let mut max_connector_count = 1usize;
        let modesets = kzalloc_objs::<DrmModeSet, _>(num_crtc.saturating_add(1));
        if modesets.is_null() {
            return -ENOMEM;
        }
        (*client).modesets = modesets;
        mutex_init(core::ptr::addr_of_mut!((*client).modeset_mutex));

        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut node = (*head).next;
        let mut i = 0usize;
        while !core::ptr::eq(node, head) && i < num_crtc {
            let crtc = node
                .cast::<u8>()
                .sub(offset_of!(DrmCrtc, head))
                .cast::<DrmCrtc>();
            (*modesets.add(i)).crtc = crtc;
            node = (*node).next;
            i += 1;
        }

        // Cloning is supported only with a single CRTC, and Linux reserves
        // eight connector slots for that pipeline.
        if num_crtc == 1 {
            max_connector_count = DRM_CLIENT_MAX_CLONED_CONNECTORS;
        }
        for index in 0..i {
            let modeset = modesets.add(index);
            let connectors = kzalloc_objs::<*mut DrmConnector, _>(max_connector_count);
            if connectors.is_null() {
                drm_client_modeset_free(client);
                return -ENOMEM;
            }
            (*modeset).connectors = connectors;
        }
    }
    0
}

/// Release allocated modes and connector references within `client`'s mode sets.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid mode sets and references,
/// and `client.modeset_mutex` must be held by the caller.
unsafe fn drm_client_modeset_release(client: *mut DrmClientDev) {
    // SAFETY: client is valid and modeset_mutex is held by caller.
    unsafe {
        drm_client_for_each_modeset(client, |modeset| {
            if !(*modeset).mode.is_null() {
                crate::drm_modes_upstream::drm_mode_destroy((*client).dev, (*modeset).mode);
                (*modeset).mode = core::ptr::null_mut();
            }
            (*modeset).fb = core::ptr::null_mut();
            for j in 0..(*modeset).num_connectors {
                let connector = *(*modeset).connectors.add(j);
                if !connector.is_null() {
                    crate::drm_connector_upstream::drm_connector_put(connector);
                    *(*modeset).connectors.add(j) = core::ptr::null_mut();
                }
            }
            (*modeset).num_connectors = 0;
        });
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_free()
/// Frees all mode sets and associated resources of `client`.
///
/// # Safety
/// `client` must point to a valid, live `DrmClientDev` or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_free(client: *mut DrmClientDev) {
    // SAFETY: client is checked non-null before dereferencing modesets.
    if client.is_null() || unsafe { (*client).modesets.is_null() } {
        return;
    }
    // SAFETY: client and client.modesets are non-null and valid.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        drm_client_modeset_release(client);
        drm_client_for_each_modeset(client, |modeset| {
            if !(*modeset).connectors.is_null() {
                kfree((*modeset).connectors);
                (*modeset).connectors = core::ptr::null_mut();
            }
        });
        mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        mutex_destroy(&mut (*client).modeset_mutex);
        kfree((*client).modesets);
        (*client).modesets = core::ptr::null_mut();
    }
}

/// Finds the mode set corresponding to `crtc` in `client`.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with an initialized modesets array.
unsafe fn drm_client_find_modeset(
    client: *mut DrmClientDev,
    crtc: *mut DrmCrtc,
) -> *mut DrmModeSet {
    let mut found = core::ptr::null_mut();
    // SAFETY: client is valid with initialized modesets.
    unsafe {
        drm_client_for_each_modeset(client, |modeset| {
            if (*modeset).crtc == crtc {
                found = modeset;
            }
        });
    }
    found
}

/// Converts a list head node pointer to its enclosing `DrmDisplayMode`.
///
/// # Safety
/// `list_node` must be an embedded `head` member inside a valid `DrmDisplayMode`.
unsafe fn mode_at(list_node: *mut drm_kapi::types::ListHead) -> *mut DrmDisplayMode {
    // SAFETY: caller guarantees list_node is embedded at offset_of!(DrmDisplayMode, head).
    unsafe {
        list_node
            .cast::<u8>()
            .sub(offset_of!(DrmDisplayMode, head))
            .cast()
    }
}

/// Returns the first mode in the connector's mode list, or null if empty.
///
/// # Safety
/// `connector` must point to a live, valid `DrmConnector`.
unsafe fn connector_first_mode(connector: *mut DrmConnector) -> *mut DrmDisplayMode {
    // SAFETY: caller guarantees connector is valid and its modes list is initialized.
    unsafe {
        let head = core::ptr::addr_of_mut!((*connector).modes);
        if core::ptr::eq((*head).next, head) {
            core::ptr::null_mut()
        } else {
            mode_at((*head).next)
        }
    }
}

/// Returns the preferred mode matching the given maximum dimensions, or null.
///
/// # Safety
/// `connector` must point to a live, valid `DrmConnector`.
unsafe fn connector_preferred_mode(
    connector: *mut DrmConnector,
    width: u32,
    height: u32,
) -> *mut DrmDisplayMode {
    // SAFETY: caller guarantees connector is valid and its modes list is initialized.
    unsafe {
        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            if (*mode).hdisplay as u32 <= width
                && (*mode).vdisplay as u32 <= height
                && ((*mode).r#type as u32 & DRM_MODE_TYPE_PREFERRED) != 0
            {
                return mode;
            }
            node = (*node).next;
        }
    }
    core::ptr::null_mut()
}

/// Returns a mode matching the connector's command-line configuration, or null.
///
/// # Safety
/// `connector` must point to a live, valid `DrmConnector`.
unsafe fn connector_cmdline_mode(connector: *mut DrmConnector) -> *mut DrmDisplayMode {
    // SAFETY: caller guarantees connector is valid and its modes list is initialized.
    unsafe {
        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            if ((*mode).r#type as u32 & DRM_MODE_TYPE_USERDEF) != 0 {
                return mode;
            }
            node = (*node).next;
        }

        let cmdline: *mut DrmCmdlineMode = core::ptr::addr_of_mut!((*connector).cmdline_mode);
        if !(*cmdline).specified {
            return core::ptr::null_mut();
        }
        let mut pass_interlace = !(*cmdline).interlace;
        loop {
            node = (*head).next;
            while !core::ptr::eq(node, head) {
                let mode = mode_at(node);
                if (*mode).hdisplay as i32 == (*cmdline).xres
                    && (*mode).vdisplay as i32 == (*cmdline).yres
                    && (!(*cmdline).refresh_specified
                        || crate::drm_modes_upstream::drm_mode_vrefresh(mode) == (*cmdline).refresh)
                    && ((!(*cmdline).interlace && !pass_interlace)
                        || ((*mode).flags & DRM_MODE_FLAG_INTERLACE != 0) == (*cmdline).interlace)
                {
                    return mode;
                }
                node = (*node).next;
            }
            if !pass_interlace {
                break;
            }
            pass_interlace = false;
        }
    }
    core::ptr::null_mut()
}

/// Returns a mode matching the connector's tile dimensions, or null.
///
/// # Safety
/// `connector` must point to a live, valid `DrmConnector`.
unsafe fn connector_tiled_mode(connector: *mut DrmConnector) -> *mut DrmDisplayMode {
    // SAFETY: caller guarantees connector is valid and its modes list is initialized.
    unsafe {
        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            if (*mode).hdisplay == (*connector).tile_h_size
                && (*mode).vdisplay == (*connector).tile_v_size
            {
                return mode;
            }
            node = (*node).next;
        }
    }
    core::ptr::null_mut()
}

/// Returns a fallback mode that does not match tile dimensions, or null.
///
/// # Safety
/// `connector` must point to a live, valid `DrmConnector`.
unsafe fn connector_fallback_non_tiled_mode(connector: *mut DrmConnector) -> *mut DrmDisplayMode {
    // SAFETY: caller guarantees connector is valid and its modes list is initialized.
    unsafe {
        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let mode = mode_at(node);
            if (*mode).hdisplay != (*connector).tile_h_size
                || (*mode).vdisplay != (*connector).tile_v_size
            {
                return mode;
            }
            node = (*node).next;
        }
    }
    core::ptr::null_mut()
}

/// Destroys `*dst` if non-null and duplicates `src` into `*dst`.
///
/// # Safety
/// `dev` must point to a valid `DrmDevice`. If `*dst` is non-null, it must be a valid
/// allocated `DrmDisplayMode`. If `src` is non-null, it must point to a valid `DrmDisplayMode`.
unsafe fn mode_replace(
    dev: *mut DrmDevice,
    dst: &mut *mut DrmDisplayMode,
    src: *mut DrmDisplayMode,
) {
    // SAFETY: caller guarantees dev is valid, and *dst and src point to valid modes if non-null.
    unsafe {
        if !(*dst).is_null() {
            crate::drm_modes_upstream::drm_mode_destroy(dev, *dst);
        }
        *dst = if src.is_null() {
            core::ptr::null_mut()
        } else {
            crate::drm_modes_upstream::drm_mode_duplicate(dev, src)
        };
    }
}

/// Destroys all modes in the slice and sets each pointer to null.
///
/// # Safety
/// `dev` must point to a valid `DrmDevice`, and each non-null mode in `modes` must be valid.
unsafe fn modes_destroy(dev: *mut DrmDevice, modes: &mut [*mut DrmDisplayMode]) {
    for mode in modes {
        // SAFETY: caller guarantees dev and mode pointers are valid.
        unsafe { mode_replace(dev, mode, core::ptr::null_mut()) };
    }
}

/// Determines which connectors are enabled based on connection status.
///
/// # Safety
/// Each non-null pointer in `connectors` must point to a valid, live `DrmConnector`.
unsafe fn drm_client_connectors_enabled(connectors: &[*mut DrmConnector], enabled: &mut [bool]) {
    let mut any_connected = false;
    for (i, connector) in connectors.iter().copied().enumerate() {
        // SAFETY: caller guarantees connector is valid and live.
        enabled[i] = unsafe {
            (*connector).status == CONNECTOR_STATUS_CONNECTED
                && !(*connector).display_info.non_desktop
        };
        any_connected |= enabled[i];
    }
    if !any_connected {
        for (i, connector) in connectors.iter().copied().enumerate() {
            // SAFETY: caller guarantees connector is valid and live.
            enabled[i] = unsafe {
                (*connector).status != CONNECTOR_STATUS_DISCONNECTED
                    && !(*connector).display_info.non_desktop
            };
        }
    }
}

/// Configures cloned mode display across connectors if supported.
///
/// # Safety
/// `dev` must point to a live `DrmDevice`. Each pointer in `connectors` must be a valid, live
/// `DrmConnector`. `modes` must have matching length to `connectors`.
unsafe fn target_cloned(
    dev: *mut DrmDevice,
    connectors: &[*mut DrmConnector],
    modes: &mut [*mut DrmDisplayMode],
    enabled: &[bool],
) -> bool {
    // SAFETY: caller guarantees dev, connectors, and modes are valid and live.
    unsafe {
        if (*dev).mode_config.num_crtc > 1 || enabled.iter().filter(|e| **e).count() <= 1 {
            return false;
        }
        let mut ok = true;
        for i in 0..connectors.len() {
            if !enabled[i] {
                continue;
            }
            mode_replace(dev, &mut modes[i], connector_cmdline_mode(connectors[i]));
            if modes[i].is_null() {
                ok = false;
                break;
            }
            for j in 0..i {
                if enabled[j] && !crate::drm_modes_upstream::drm_mode_equal(modes[j], modes[i]) {
                    ok = false;
                }
            }
        }
        if ok {
            return true;
        }
        modes_destroy(dev, modes);

        let dmt = crate::drm_edid_upstream::drm_mode_find_dmt(dev, 1024, 768, 60, false);
        if dmt.is_null() {
            return false;
        }
        ok = true;
        for i in 0..connectors.len() {
            if !enabled[i] {
                continue;
            }
            let mut node = (*connectors[i]).modes.next;
            let head = core::ptr::addr_of_mut!((*connectors[i]).modes);
            while !core::ptr::eq(node, head) {
                let mode = mode_at(node);
                if crate::drm_modes_upstream::drm_mode_equal(dmt, mode) {
                    mode_replace(dev, &mut modes[i], mode);
                    break;
                }
                node = (*node).next;
            }
            if modes[i].is_null() {
                ok = false;
            }
        }
        crate::drm_modes_upstream::drm_mode_destroy(dev, dmt);
        ok
    }
}

/// Computes preferred modes and screen offsets across connectors.
///
/// # Safety
/// `dev` must point to a live `DrmDevice`. Each connector in `connectors` must be valid and live.
/// `modes` and `offsets` must have matching length to `connectors`.
unsafe fn target_preferred(
    dev: *mut DrmDevice,
    connectors: &[*mut DrmConnector],
    modes: &mut [*mut DrmDisplayMode],
    offsets: &mut [ClientOffset],
    enabled: &[bool],
    width: u32,
    height: u32,
) -> bool {
    let mut tiled_count = 0usize;
    for c in connectors.iter().copied() {
        // SAFETY: caller guarantees connector pointer in connectors is valid and live.
        unsafe {
            if (*c).has_tile && (*c).status == CONNECTOR_STATUS_CONNECTED {
                tiled_count += 1;
            }
        }
    }
    let mut configured = vec![false; connectors.len()];
    for pass in 0..=connectors.len() {
        let mut progress = false;
        for (i, connector) in connectors.iter().copied().enumerate() {
            if configured[i] || !enabled[i] {
                configured[i] = true;
                continue;
            }
            // SAFETY: connector is valid and live; accessing tiling information and replacing modes.
            unsafe {
                if (*connector).has_tile {
                    let h = (*connector).tile_h_loc;
                    let v = (*connector).tile_v_loc;
                    if pass == 0 || (pass == 1 && (h != 0 || v != 0)) {
                        continue;
                    }
                    if pass > 1 && h != (pass - 1) as u8 && v != (pass - 1) as u8 {
                        continue;
                    }
                    for prior in connectors.iter().copied() {
                        if (*prior).has_tile
                            && ((*prior).tile_h_loc < h || (*prior).tile_v_loc < v)
                            && let Some(prior_mode) = modes.get(
                                connectors
                                    .iter()
                                    .position(|c| *c == prior)
                                    .unwrap_or(usize::MAX),
                            )
                            && !prior_mode.is_null()
                        {
                            if (*prior).tile_h_loc < h {
                                offsets[i].x += (**prior_mode).hdisplay as i32;
                            }
                            if (*prior).tile_v_loc < v {
                                offsets[i].y += (**prior_mode).vdisplay as i32;
                            }
                        }
                    }
                } else if pass > 0 {
                    configured[i] = true;
                    continue;
                }
                mode_replace(dev, &mut modes[i], connector_cmdline_mode(connector));
                if modes[i].is_null() {
                    mode_replace(
                        dev,
                        &mut modes[i],
                        connector_preferred_mode(connector, width, height),
                    );
                }
                if modes[i].is_null() {
                    mode_replace(dev, &mut modes[i], connector_first_mode(connector));
                }
                if (*connector).has_tile {
                    let complete = tiled_count
                        >= ((*connector).num_h_tile as usize * (*connector).num_v_tile as usize);
                    let tile_mode = connector_tiled_mode(connector);
                    if !complete
                        || (tile_mode.is_null()
                            && (*connector).tile_h_loc == 0
                            && (*connector).tile_v_loc == 0)
                    {
                        mode_replace(
                            dev,
                            &mut modes[i],
                            connector_fallback_non_tiled_mode(connector),
                        );
                    } else {
                        mode_replace(dev, &mut modes[i], tile_mode);
                    }
                }
            }
            configured[i] = true;
            progress = true;
        }
        if configured.iter().all(|v| *v) || !progress {
            break;
        }
    }
    modes
        .iter()
        .zip(enabled)
        .all(|(mode, enabled)| !*enabled || !mode.is_null())
}

/// Checks if `connector` has an encoder that can route to `crtc`.
///
/// # Safety
/// `connector` and `crtc` must point to live, valid DRM objects with initialized parent `dev`.
unsafe fn connector_has_possible_crtc(connector: *mut DrmConnector, crtc: *mut DrmCrtc) -> bool {
    // SAFETY: caller guarantees connector and crtc are valid and live.
    unsafe {
        let dev = (*connector).dev;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let encoder = node
                .cast::<u8>()
                .sub(offset_of!(DrmEncoder, head))
                .cast::<DrmEncoder>();
            if ((*connector).possible_encoders & (1u32 << (*encoder).index)) != 0
                && ((*encoder).possible_crtcs & crate::drm_crtc_upstream::drm_crtc_mask(crtc)) != 0
            {
                return true;
            }
            node = (*node).next;
        }
    }
    false
}

fn pick_crtcs_recursive(
    client: *mut DrmClientDev,
    connectors: &[*mut DrmConnector],
    modes: &[*mut DrmDisplayMode],
    index: usize,
    assignment: &mut [*mut DrmCrtc],
    width: u32,
    height: u32,
) -> i32 {
    if index == connectors.len() {
        return 0;
    }
    assignment[index] = core::ptr::null_mut();
    let mut best_score = pick_crtcs_recursive(
        client,
        connectors,
        modes,
        index + 1,
        assignment,
        width,
        height,
    );
    if modes[index].is_null() {
        return best_score;
    }
    let connector = connectors[index];
    let mut score = 1;
    // SAFETY: connector and client.dev are valid and live; modeset list is traversed safely.
    unsafe {
        if (*connector).status == CONNECTOR_STATUS_CONNECTED {
            score += 1;
        }
        if (*connector).cmdline_mode.specified {
            score += 1;
        }
        if !connector_preferred_mode(connector, width, height).is_null() {
            score += 1;
        }
        let mut best_assignment = assignment.to_vec();
        drm_client_for_each_modeset(client, |modeset| {
            let crtc = (*modeset).crtc;
            if !connector_has_possible_crtc(connector, crtc) {
                return;
            }
            for prev in 0..index {
                if assignment[prev] == crtc
                    && ((*(*client).dev).mode_config.num_crtc > 1
                        || !crate::drm_modes_upstream::drm_mode_equal(modes[prev], modes[index]))
                {
                    return;
                }
            }
            let mut candidate = assignment.to_vec();
            candidate[index] = crtc;
            let child_score = pick_crtcs_recursive(
                client,
                connectors,
                modes,
                index + 1,
                &mut candidate,
                width,
                height,
            );
            let total = score + child_score;
            if total > best_score {
                best_score = total;
                best_assignment = candidate;
            }
        });
        assignment.copy_from_slice(&best_assignment);
    }
    best_score
}

/// Upstream: drm_client_modeset.c drm_client_firmware_config()
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev`. `connectors`, `modes`, and
/// `crtcs` must be valid arrays of matching length.
unsafe fn drm_client_firmware_config(
    client: *mut DrmClientDev,
    connectors: &[*mut DrmConnector],
    modes: &mut [*mut DrmDisplayMode],
    crtcs: &mut [*mut DrmCrtc],
    enabled: &[bool],
    width: u32,
    height: u32,
) -> bool {
    // SAFETY: caller guarantees client, connectors, modes, and crtcs are valid and live.
    unsafe {
        let dev = (*client).dev;
        if !crate::drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset(dev) {
            return false;
        }
        let mut ctx: DrmModesetAcquireCtx = core::mem::zeroed();
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0);
        while crate::drm_modeset_lock_upstream::drm_modeset_lock_all_ctx(dev, &mut ctx) != 0 {
            crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx);
        }
        let mut count = 0usize;
        let mut fallback = false;
        for i in 0..connectors.len() {
            if !enabled[i] {
                continue;
            }
            let connector = connectors[i];
            let state = (*connector).state;
            if state.is_null() || (*state).crtc.is_null() || (*state).best_encoder.is_null() {
                fallback = true;
                continue;
            }
            let crtc = (*state).crtc;
            if (0..i).any(|j| crtcs[j] == crtc) {
                fallback = true;
                continue;
            }
            let crtc_state = (*crtc).state;
            let current = if !crtc_state.is_null() && (*crtc_state).active {
                &mut (*crtc_state).mode as *mut DrmDisplayMode
            } else {
                core::ptr::null_mut()
            };
            let mode = if !current.is_null() {
                current
            } else {
                connector_cmdline_mode(connector)
            };
            mode_replace(dev, &mut modes[i], mode);
            if modes[i].is_null() {
                mode_replace(
                    dev,
                    &mut modes[i],
                    connector_preferred_mode(connector, width, height),
                );
            }
            if modes[i].is_null() {
                mode_replace(dev, &mut modes[i], connector_first_mode(connector));
            }
            if modes[i].is_null() {
                fallback = true;
                continue;
            }
            crtcs[i] = crtc;
            count += 1;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
        count > 0 && !fallback
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_probe()
/// Probes and configures display modes across all connected outputs of `client`.
///
/// # Safety
/// `client` must point to a live, valid `DrmClientDev` with an initialized `dev`.
/// Must not be called concurrently with other modeset configuration on the same client.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_probe(
    client: *mut DrmClientDev,
    mut width: u32,
    mut height: u32,
) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client and client.dev are non-null and point to live objects.
    let dev = unsafe { (*client).dev };
    if width == 0 {
        // SAFETY: dev points to a valid live DrmDevice.
        width = unsafe { (*dev).mode_config.max_width };
    }
    if height == 0 {
        // SAFETY: dev points to a valid live DrmDevice.
        height = unsafe { (*dev).mode_config.max_height };
    }

    let mut connectors = Vec::<*mut DrmConnector>::new();
    // SAFETY: zero-initializes C iterator struct.
    let mut iter: DrmConnectorListIter = unsafe { core::mem::zeroed() };
    // SAFETY: dev points to a valid live DrmDevice and iter is stack-allocated.
    unsafe { crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut iter) };
    loop {
        // SAFETY: iter was initialized by drm_connector_list_iter_begin.
        let connector =
            unsafe { crate::drm_connector_upstream::drm_connector_list_iter_next(&mut iter) };
        if connector.is_null() {
            break;
        }
        if connectors.try_reserve(1).is_err() {
            // SAFETY: ends iterator and drops acquired connector references on allocation failure.
            unsafe { crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter) };
            for connector in connectors.iter().copied() {
                // SAFETY: connector reference was incremented below.
                unsafe { crate::drm_connector_upstream::drm_connector_put(connector) };
            }
            return -ENOMEM;
        }
        // SAFETY: connector is non-null and valid.
        unsafe { crate::drm_connector_upstream::drm_connector_get(connector) };
        connectors.push(connector);
    }
    // SAFETY: ends iteration over device connector list.
    unsafe { crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter) };
    if connectors.is_empty() {
        return 0;
    }

    let count = connectors.len();
    let mut modes = vec![core::ptr::null_mut(); count];
    let mut crtcs = vec![core::ptr::null_mut(); count];
    let mut offsets = vec![ClientOffset::default(); count];
    let mut enabled = vec![false; count];

    // SAFETY: dev, client, connectors, modes, and crtcs are valid and protected by modeset_mutex and mode_config.mutex.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        for connector in connectors.iter().copied() {
            if !(*connector).funcs.is_null()
                && let Some(fill_modes) = (*(*connector).funcs).fill_modes
            {
                let _ = fill_modes(connector, width, height);
            }
        }
        drm_client_connectors_enabled(&connectors, &mut enabled);

        if !drm_client_firmware_config(
            client,
            &connectors,
            &mut modes,
            &mut crtcs,
            &enabled,
            width,
            height,
        ) {
            modes_destroy(dev, &mut modes);
            crtcs.fill(core::ptr::null_mut());
            offsets.fill(ClientOffset::default());
            if !target_cloned(dev, &connectors, &mut modes, &enabled)
                && !target_preferred(
                    dev,
                    &connectors,
                    &mut modes,
                    &mut offsets,
                    &enabled,
                    width,
                    height,
                )
            {
                modes_destroy(dev, &mut modes);
            }
            let mut assignment = vec![core::ptr::null_mut(); count];
            pick_crtcs_recursive(
                client,
                &connectors,
                &modes,
                0,
                &mut assignment,
                width,
                height,
            );
            crtcs.copy_from_slice(&assignment);
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));

        drm_client_modeset_release(client);
        for i in 0..count {
            if modes[i].is_null() || crtcs[i].is_null() {
                continue;
            }
            let modeset = drm_client_find_modeset(client, crtcs[i]);
            if modeset.is_null() || (*modeset).connectors.is_null() {
                continue;
            }
            let max_connectors = if (*dev).mode_config.num_crtc == 1 {
                DRM_CLIENT_MAX_CLONED_CONNECTORS
            } else {
                1
            };
            if (*modeset).num_connectors >= max_connectors
                || ((*dev).mode_config.num_crtc > 1 && (*modeset).num_connectors >= 1)
            {
                mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
                modes_destroy(dev, &mut modes);
                for connector in connectors.iter().copied() {
                    crate::drm_connector_upstream::drm_connector_put(connector);
                }
                return -EINVAL;
            }
            if !(*modeset).mode.is_null() {
                crate::drm_modes_upstream::drm_mode_destroy(dev, (*modeset).mode);
            }
            (*modeset).mode = crate::drm_modes_upstream::drm_mode_duplicate(dev, modes[i]);
            if (*modeset).mode.is_null() {
                mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
                modes_destroy(dev, &mut modes);
                for connector in connectors.iter().copied() {
                    crate::drm_connector_upstream::drm_connector_put(connector);
                }
                return -ENOMEM;
            }
            crate::drm_connector_upstream::drm_connector_get(connectors[i]);
            *(*modeset).connectors.add((*modeset).num_connectors) = connectors[i];
            (*modeset).num_connectors += 1;
            (*modeset).x = offsets[i].x.max(0) as u32;
            (*modeset).y = offsets[i].y.max(0) as u32;
        }
        mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
    }

    // SAFETY: cleans up local mode copies and connector references.
    unsafe {
        modes_destroy(dev, &mut modes);
        for connector in connectors {
            crate::drm_connector_upstream::drm_connector_put(connector);
        }
    }
    0
}

// upstream: drm_client_modeset.c drm_client_rotation()
/// Determines the hardware plane rotation for `modeset` and writes it to `rotation`.
///
/// # Safety
/// If non-null, `modeset` must point to an initialized `DrmModeSet` with valid `connectors`
/// and `crtc`. `rotation` must point to a writable `u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_rotation(modeset: *mut DrmModeSet, rotation: *mut u32) -> bool {
    // SAFETY: modeset is checked non-null before dereferencing num_connectors.
    if modeset.is_null() || rotation.is_null() || unsafe { (*modeset).num_connectors == 0 } {
        return false;
    }
    // SAFETY: modeset and rotation are non-null and valid; modeset.num_connectors > 0.
    unsafe {
        let connector = *(*modeset).connectors;
        let plane = (*(*modeset).crtc).primary;
        if connector.is_null() || plane.is_null() {
            return false;
        }
        *rotation = match (*connector).display_info.panel_orientation {
            DRM_MODE_PANEL_ORIENTATION_BOTTOM_UP => DRM_MODE_ROTATE_180,
            DRM_MODE_PANEL_ORIENTATION_LEFT_UP => DRM_MODE_ROTATE_90,
            DRM_MODE_PANEL_ORIENTATION_RIGHT_UP => DRM_MODE_ROTATE_270,
            _ => DRM_MODE_ROTATE_0,
        };
        let cmdline = core::ptr::addr_of!((*connector).cmdline_mode);
        if (*cmdline).specified && (*cmdline).rotation_reflection != 0 {
            let panel_rot = (*rotation & DRM_MODE_ROTATE_MASK).trailing_zeros();
            let cmdline_rot =
                ((*cmdline).rotation_reflection & DRM_MODE_ROTATE_MASK).trailing_zeros();
            let sum_rot = (panel_rot + cmdline_rot) % 4;
            let panel_rest = *rotation & !DRM_MODE_ROTATE_MASK;
            let cmdline_rest = (*cmdline).rotation_reflection & !DRM_MODE_ROTATE_MASK;
            *rotation = (1 << sum_rot) | (panel_rest ^ cmdline_rest);
        }
        if ((*rotation & DRM_MODE_ROTATE_MASK) != DRM_MODE_ROTATE_0
            && (*rotation & DRM_MODE_ROTATE_MASK) != DRM_MODE_ROTATE_180)
            || (*plane).rotation_property.is_null()
        {
            return false;
        }
        let property: *mut DrmProperty = (*plane).rotation_property;
        let mut valid_mask = 0u64;
        for i in 0..(*property).num_values as usize {
            valid_mask |= 1u64 << *(*property).values.add(i);
        }
        (*rotation as u64 & valid_mask) != 0
    }
}

/// Upstream: drm_client_modeset.c drm_client_modeset_commit_atomic()
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with initialized `dev` and modesets.
/// Caller must hold client.modeset_mutex.
unsafe fn drm_client_modeset_commit_atomic(
    client: *mut DrmClientDev,
    active: bool,
    check: bool,
) -> i32 {
    // SAFETY: client points to a live DrmClientDev.
    let dev = unsafe { (*client).dev };
    // SAFETY: zero-initializes DrmModesetAcquireCtx.
    let mut ctx: DrmModesetAcquireCtx = unsafe { core::mem::zeroed() };
    // SAFETY: ctx is stack-allocated.
    unsafe { crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0) };
    // SAFETY: dev points to a valid live DrmDevice.
    let state: *mut DrmAtomicCommit =
        unsafe { crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev) };
    if state.is_null() {
        // SAFETY: cleans up acquire context on allocation failure.
        unsafe {
            crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
        }
        return -ENOMEM;
    }
    // SAFETY: state was allocated above and is non-null.
    unsafe { (*state).acquire_ctx = &mut ctx };

    let ret = loop {
        // SAFETY: dev is a valid live DrmDevice.
        let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.plane_list) };
        // SAFETY: head points to an initialized list head.
        let mut node = unsafe { (*head).next };
        let mut ret = 0;
        while !core::ptr::eq(node, head) {
            // SAFETY: node is an embedded list head in DrmPlane.
            let plane = unsafe {
                node.cast::<u8>()
                    .sub(offset_of!(DrmPlane, head))
                    .cast::<DrmPlane>()
            };
            // SAFETY: state and plane are valid DRM objects.
            let plane_state =
                unsafe { crate::drm_atomic_upstream::drm_atomic_get_plane_state(state, plane) };
            if IS_ERR(plane_state) {
                ret = PTR_ERR(plane_state);
                break;
            }
            // SAFETY: plane_state is a valid non-error pointer.
            unsafe { (*plane_state).rotation = DRM_MODE_ROTATE_0 };
            // SAFETY: plane is a valid pointer.
            if unsafe { (*plane).r#type != DRM_PLANE_TYPE_PRIMARY } {
                // SAFETY: plane and plane_state are valid atomic state objects.
                ret = unsafe {
                    crate::drm_atomic_upstream::__drm_atomic_helper_disable_plane(
                        plane,
                        plane_state,
                    )
                };
                if ret != 0 {
                    break;
                }
            }
            // SAFETY: node is a valid list node.
            node = unsafe { (*node).next };
        }
        if ret == 0 {
            let mut mode_index = 0usize;
            loop {
                // SAFETY: client is valid, indexing modesets array.
                let modeset = unsafe { mode_set_at(client, mode_index) };
                // SAFETY: modeset points to a valid DrmModeSet entry.
                if unsafe { (*modeset).crtc.is_null() } {
                    break;
                }
                // SAFETY: modeset and modeset.crtc are non-null.
                let primary = unsafe { (*(*modeset).crtc).primary };
                let mut rotation = DRM_MODE_ROTATE_0;
                // SAFETY: modeset is valid; rotation receives the calculated orientation.
                if unsafe { drm_client_rotation(modeset, &mut rotation) } {
                    // SAFETY: state and primary plane are valid.
                    let plane_state = unsafe {
                        crate::drm_atomic_upstream::drm_atomic_get_new_plane_state(state, primary)
                    };
                    if plane_state.is_null() {
                        ret = -EINVAL;
                        break;
                    }
                    // SAFETY: plane_state is non-null.
                    unsafe { (*plane_state).rotation = rotation };
                }
                // SAFETY: modeset and state are valid DRM objects.
                ret = unsafe {
                    crate::drm_atomic_upstream::__drm_atomic_helper_set_config(modeset, state)
                };
                if ret != 0 {
                    break;
                }
                if !active {
                    // SAFETY: state and modeset.crtc are valid.
                    let crtc_state = unsafe {
                        crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                            state,
                            (*modeset).crtc,
                        )
                    };
                    if crtc_state.is_null() {
                        ret = -EINVAL;
                        break;
                    }
                    // SAFETY: crtc_state is non-null.
                    unsafe { (*crtc_state).active = false };
                }
                mode_index += 1;
            }
        }
        if ret == 0 {
            // SAFETY: state is a fully populated atomic commit object.
            ret = unsafe {
                if check {
                    crate::drm_atomic_upstream::drm_atomic_check_only(state)
                } else {
                    crate::drm_atomic_upstream::drm_atomic_commit(state)
                }
            };
        }
        if ret == -drm_kapi::config::EDEADLK {
            // SAFETY: clears state and backs off locks on deadlock retry.
            unsafe {
                crate::drm_atomic_upstream::drm_atomic_commit_clear(state);
                crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx);
            }
            continue;
        }
        break ret;
    };

    // SAFETY: drops reference to atomic commit state and finishes acquire context.
    unsafe {
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
    }
    ret
}

/// Upstream: drm_client_modeset.c drm_client_modeset_commit_legacy()
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and modesets.
/// Caller must hold client.modeset_mutex.
unsafe fn drm_client_modeset_commit_legacy(client: *mut DrmClientDev) -> i32 {
    // SAFETY: client is valid and live.
    let dev = unsafe { (*client).dev };
    // SAFETY: dev points to a valid live DrmDevice.
    unsafe { crate::drm_modeset_lock_upstream::drm_modeset_lock_all(dev) };
    let mut ret = 0;
    // SAFETY: dev and client are locked and valid; traversing plane and modeset lists.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let plane = node
                .cast::<u8>()
                .sub(offset_of!(DrmPlane, head))
                .cast::<DrmPlane>();
            if (*plane).r#type != DRM_PLANE_TYPE_PRIMARY {
                crate::drm_plane_upstream::drm_plane_force_disable(plane);
            }
            if !(*plane).rotation_property.is_null() {
                crate::drm_plane_upstream::drm_mode_plane_set_obj_prop(
                    plane,
                    (*plane).rotation_property,
                    DRM_MODE_ROTATE_0 as u64,
                );
            }
            node = (*node).next;
        }
        let mut mode_index = 0usize;
        loop {
            let modeset = mode_set_at(client, mode_index);
            if (*modeset).crtc.is_null() {
                break;
            }
            let crtc = (*modeset).crtc;
            let funcs = (*crtc).funcs;
            if !funcs.is_null() {
                if let Some(cursor_set2) = (*funcs).cursor_set2 {
                    ret = cursor_set2(crtc, core::ptr::null_mut(), 0, 0, 0, 0, 0);
                } else if let Some(cursor_set) = (*funcs).cursor_set {
                    ret = cursor_set(crtc, core::ptr::null_mut(), 0, 0, 0);
                }
            }
            if ret != 0 {
                break;
            }
            ret = crate::drm_crtc_upstream::drm_mode_set_config_internal(modeset);
            if ret != 0 {
                break;
            }
            mode_index += 1;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_unlock_all(dev);
    }
    ret
}

// upstream: drm_client_modeset.c drm_client_modeset_check()
/// Checks whether the mode-set configuration of `client` is valid without applying it.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and modesets, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_check(client: *mut DrmClientDev) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client is non-null.
    let dev = unsafe { (*client).dev };
    // SAFETY: dev is non-null.
    if !unsafe { crate::drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset(dev) } {
        return 0;
    }
    // SAFETY: client and dev are valid; committing atomic check under modeset_mutex.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        let ret = drm_client_modeset_commit_atomic(client, true, true);
        mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        ret
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_commit_locked()
/// Commits the modeset configuration for `client` with locks held.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and modesets, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_commit_locked(client: *mut DrmClientDev) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client and client.dev are valid; performs commit under modeset_mutex.
    unsafe {
        let dev = (*client).dev;
        mutex_lock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        let ret = if crate::drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset(dev) {
            drm_client_modeset_commit_atomic(client, true, false)
        } else {
            drm_client_modeset_commit_legacy(client)
        };
        mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        ret
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_commit()
/// Commits the modeset configuration for `client`, acquiring device master first.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and modesets, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_commit(client: *mut DrmClientDev) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client and client.dev are valid; acquires master and commits.
    unsafe {
        let dev = (*client).dev;
        if !crate::drm_auth_upstream::drm_master_internal_acquire(dev) {
            return -drm_kapi::config::EBUSY;
        }
        let ret = drm_client_modeset_commit_locked(client);
        crate::drm_auth_upstream::drm_master_internal_release(dev);
        ret
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_dpms()
/// Sets DPMS power state for all connectors configured on `client`.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and modesets, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_dpms(client: *mut DrmClientDev, mode: i32) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev.
    if client.is_null() || unsafe { (*client).dev.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client and client.dev are valid.
    unsafe {
        let dev = (*client).dev;
        if !crate::drm_auth_upstream::drm_master_internal_acquire(dev) {
            return -drm_kapi::config::EBUSY;
        }
        mutex_lock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        let ret = if crate::drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset(dev) {
            drm_client_modeset_commit_atomic(client, mode == DRM_MODE_DPMS_ON as i32, false)
        } else {
            drm_client_modeset_dpms_legacy(client, mode);
            0
        };
        mutex_unlock(core::ptr::addr_of_mut!((*client).modeset_mutex));
        crate::drm_auth_upstream::drm_master_internal_release(dev);
        ret
    }
}

/// Upstream: drm_client_modeset.c drm_client_modeset_dpms_legacy()
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and modesets.
unsafe fn drm_client_modeset_dpms_legacy(client: *mut DrmClientDev, dpms_mode: i32) {
    // SAFETY: client is valid and live.
    let dev = unsafe { (*client).dev };
    // SAFETY: zero-initializes DrmModesetAcquireCtx.
    let mut ctx: DrmModesetAcquireCtx = unsafe { core::mem::zeroed() };
    // SAFETY: dev and modeset list are valid; locks are held.
    unsafe {
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0);
        while crate::drm_modeset_lock_upstream::drm_modeset_lock_all_ctx(dev, &mut ctx) != 0 {
            crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx);
        }
        let mut index = 0usize;
        loop {
            let modeset = mode_set_at(client, index);
            if (*modeset).crtc.is_null() {
                break;
            }
            if (*modeset).crtc.is_null() || !(*(*modeset).crtc).enabled {
                index += 1;
                continue;
            }
            for i in 0..(*modeset).num_connectors {
                let connector = *(*modeset).connectors.add(i);
                if connector.is_null() {
                    continue;
                }
                if let Some(dpms) = (*(*connector).funcs).dpms {
                    dpms(connector, dpms_mode);
                }
                if !(*dev).mode_config.dpms_property.is_null() {
                    crate::drm_mode_object_upstream::drm_object_property_set_value(
                        core::ptr::addr_of_mut!((*connector).base),
                        (*dev).mode_config.dpms_property,
                        dpms_mode as u64,
                    );
                }
            }
            index += 1;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
    }
}

// upstream: drm_client_modeset.c drm_client_modeset_wait_for_vblank()
/// Waits for a vblank interrupt on the CRTC identified by `crtc_index`.
///
/// # Safety
/// `client` must point to a live `DrmClientDev` with valid `dev` and `modesets`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_client_modeset_wait_for_vblank(
    client: *mut DrmClientDev,
    crtc_index: u32,
) -> i32 {
    // SAFETY: client is checked non-null before dereferencing dev and modesets.
    if client.is_null() || unsafe { (*client).dev.is_null() || (*client).modesets.is_null() } {
        return -EINVAL;
    }
    // SAFETY: client, dev, and modeset at crtc_index are valid and live.
    unsafe {
        let dev = (*client).dev;
        if !crate::drm_auth_upstream::drm_master_internal_acquire(dev) {
            return -drm_kapi::config::EBUSY;
        }
        if crtc_index >= (*dev).mode_config.num_crtc.max(0) as u32 {
            crate::drm_auth_upstream::drm_master_internal_release(dev);
            return -EINVAL;
        }
        let crtc = (*(*client).modesets.add(crtc_index as usize)).crtc;
        let ret = crate::drm_vblank_upstream::drm_crtc_vblank_get(crtc);
        if ret == 0 {
            crate::drm_vblank_upstream::drm_crtc_wait_one_vblank(crtc);
            crate::drm_vblank_upstream::drm_crtc_vblank_put(crtc);
        }
        crate::drm_auth_upstream::drm_master_internal_release(dev);
        0
    }
}

// upstream: drm_crtc_helper.c drm_helper_encoder_in_use()
/// Checks if `encoder` is currently attached to any active connector.
///
/// # Safety
/// `encoder` must point to a live, valid `DrmEncoder` with an initialized `dev`.
unsafe fn drm_helper_encoder_in_use(encoder: *mut DrmEncoder) -> bool {
    // SAFETY: encoder and encoder.dev are valid and live.
    unsafe {
        let dev = (*encoder).dev;
        let mut iter: DrmConnectorListIter = core::mem::zeroed();
        crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut iter);
        loop {
            let connector = crate::drm_connector_upstream::drm_connector_list_iter_next(&mut iter);
            if connector.is_null() {
                break;
            }
            if (*connector).encoder == encoder {
                crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter);
                return true;
            }
        }
        crate::drm_connector_upstream::drm_connector_list_iter_end(&mut iter);
    }
    false
}

// upstream: drm_crtc_helper.c drm_helper_crtc_in_use()
/// Checks if `crtc` is currently driven by any active encoder.
///
/// # Safety
/// `crtc` must point to a live, valid `DrmCrtc` with an initialized `dev`.
unsafe fn drm_helper_crtc_in_use(crtc: *mut DrmCrtc) -> bool {
    // SAFETY: crtc and crtc.dev are valid and live.
    unsafe {
        let dev = (*crtc).dev;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let encoder = node
                .cast::<u8>()
                .sub(offset_of!(DrmEncoder, head))
                .cast::<DrmEncoder>();
            if (*encoder).crtc == crtc && drm_helper_encoder_in_use(encoder) {
                return true;
            }
            node = (*node).next;
        }
    }
    false
}

// upstream: drm_crtc_helper.c drm_helper_disable_unused_functions()
/// Disables unused encoders and CRTCs on `dev`.
///
/// # Safety
/// `dev` must point to a live, valid `DrmDevice`, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_helper_disable_unused_functions(dev: *mut DrmDevice) {
    if dev.is_null() {
        return;
    }
    // SAFETY: dev points to a valid live DrmDevice; acquires modeset locks.
    unsafe {
        crate::drm_modeset_lock_upstream::drm_modeset_lock_all(dev);
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let encoder = node
                .cast::<u8>()
                .sub(offset_of!(DrmEncoder, head))
                .cast::<DrmEncoder>();
            if !drm_helper_encoder_in_use(encoder) {
                if let Some(funcs) = (*encoder).helper_private.as_ref() {
                    if let Some(disable) = funcs.disable {
                        disable(encoder);
                    } else if let Some(dpms) = funcs.dpms {
                        dpms(encoder, 3);
                    }
                }
                (*encoder).crtc = core::ptr::null_mut();
            }
            node = (*node).next;
        }
        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut node = (*head).next;
        while !core::ptr::eq(node, head) {
            let crtc = node
                .cast::<u8>()
                .sub(offset_of!(DrmCrtc, head))
                .cast::<DrmCrtc>();
            (*crtc).enabled = drm_helper_crtc_in_use(crtc);
            if !(*crtc).enabled {
                if let Some(funcs) = (*crtc).helper_private.as_ref() {
                    if let Some(disable) = funcs.disable {
                        disable(crtc);
                    } else if let Some(dpms) = funcs.dpms {
                        dpms(crtc, 3);
                    }
                }
                if !(*crtc).primary.is_null() {
                    (*(*crtc).primary).fb = core::ptr::null_mut();
                }
            }
            node = (*node).next;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_unlock_all(dev);
    }
}

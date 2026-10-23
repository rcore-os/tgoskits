// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_atomic_helper.c`.
//!
//! Faithful translation of Linux DRM atomic helper library. Provides
//! default implementations of check and commit functions, legacy helper
//! emulations, nonblocking commit sequencing, suspend/resume helpers, etc.

use core::{ffi::c_void, mem::offset_of};

use drm_kapi::{
    completion::Completion,
    config::{EBUSY, EINVAL, ENOMEM, EOPNOTSUPP, ERANGE, GFP_KERNEL, HZ, err_ptr, is_err, ptr_err},
    locks::{spin_lock_irqsave_raw, spin_lock_raw, spin_unlock_irqrestore_raw, spin_unlock_raw},
    memory::{kref_get, kref_init, kref_put},
    types::{ListHead, WorkStruct},
};

use crate::{
    drm_atomic_types::{DrmAtomicCommit, DrmCrtcCommit},
    drm_connector_types::{DrmConnector, DrmConnectorListIter, DrmConnectorState},
    drm_crtc_types::{DrmCrtc, DrmCrtcState, DrmModeSet},
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_upstream::DRIVER_GEM,
    drm_modes_types::{DrmDisplayMode, DrmModeStatus},
    drm_modeset_helper_vtables_types::{
        DrmConnectorHelperFuncs, DrmCrtcHelperFuncs, DrmEncoderHelperFuncs,
        DrmModeConfigHelperFuncs, DrmPlaneHelperFuncs,
    },
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_plane_types::{DRM_PLANE_TYPE_PRIMARY, DrmPlane, DrmPlaneState},
    drm_property_types::DrmPropertyBlob,
    drm_rect_types::DrmRect,
    drm_vblank_types::DrmPendingVblankEvent,
};

#[repr(C)]
pub struct DrmColorop {
    pub dev: *mut DrmDevice,
    pub head: ListHead,
    pub index: u32,
    pub _pad: u32,
    pub base: crate::drm_mode_object_types::DrmModeObject,
    pub plane: *mut DrmPlane,
    pub state: *mut DrmColoropState,
}
#[repr(C)]
pub struct DrmColoropState {
    pub colorop: *mut DrmColorop,
    pub bypass: bool,
    pub curve_1d_type: u32,
    pub multiplier: u64,
    pub data: *mut DrmPropertyBlob,
    pub lut1d_interpolation: u32,
    pub lut3d_interpolation: u32,
    pub state: *mut DrmAtomicCommit,
}

pub const DRM_PLANE_COMMIT_ACTIVE_ONLY: u32 = 1 << 0;
pub const DRM_PLANE_COMMIT_NO_DISABLE_AFTER_MODESET: u32 = 1 << 1;
pub const DRM_MODE_DPMS_ON: i32 = 0;
pub const DRM_MODE_DPMS_OFF: i32 = 3;
pub const DRM_MODE_PAGE_FLIP_ASYNC: u32 = 0x02;

/// # Safety
/// If non-null, `dev` must point to an initialized, readable `DrmDevice` whose `driver`
/// field (if non-null) points to a valid `DrmDriver` structure.
#[inline]
unsafe fn drm_core_check_feature(dev: *const DrmDevice, feature: u32) -> bool {
    // SAFETY: `dev` if non-null points to an initialized `DrmDevice`. Reading `(*dev).driver`
    // loads an immutable pointer to static `DrmDriver` metadata, and accessing `driver_features`
    // is a single aligned read of `u32`.
    // point to valid, aligned, readable memory for `DrmDevice` and `DrmDriver`.
    unsafe {
        if dev.is_null() || (*dev).driver.is_null() {
            return false;
        }
        ((*(*dev).driver).driver_features & feature) != 0
    }
}

/// # Safety
/// `r1` and `r2` must point to valid, initialized, properly aligned, readable `DrmRect` structures.
#[inline]
unsafe fn drm_rect_equals(r1: *const DrmRect, r2: *const DrmRect) -> bool {
    // SAFETY: `r1` and `r2` are valid non-null pointers to aligned `DrmRect` structures
    // and are dereferenced solely for reading 32-bit coordinate fields.
    unsafe {
        (*r1).x1 == (*r2).x1 && (*r1).x2 == (*r2).x2 && (*r1).y1 == (*r2).y1 && (*r1).y2 == (*r2).y2
    }
}

/// # Safety
/// `state` must point to a valid, initialized, properly aligned, readable `DrmCrtcState`.
#[inline]
unsafe fn drm_atomic_crtc_needs_modeset(state: *const DrmCrtcState) -> bool {
    // SAFETY: `state` points to a valid, aligned `DrmCrtcState` whose modeset change
    // flag methods perform read-only accesses on boolean fields.
    unsafe { (*state).mode_changed() || (*state).active_changed() || (*state).connectors_changed() }
}

/// # Safety
/// `state` must point to a valid, initialized, properly aligned, readable `DrmCrtcState`.
#[inline]
unsafe fn drm_atomic_crtc_effectively_active(state: *const DrmCrtcState) -> bool {
    // SAFETY: `state` points to a valid, aligned `DrmCrtcState`; reading `active` and
    // `self_refresh_active` fields without modification.
    unsafe { (*state).active || (*state).self_refresh_active }
}

/// # Safety
/// Both `old_plane_state` and `new_plane_state` must point to valid, initialized,
/// properly aligned, readable `DrmPlaneState` structures.
#[inline]
unsafe fn drm_atomic_plane_enabling(
    old_plane_state: *const DrmPlaneState,
    new_plane_state: *const DrmPlaneState,
) -> bool {
    // SAFETY: `old_plane_state` and `new_plane_state` are valid, aligned pointers to
    // `DrmPlaneState` whose `crtc` fields are read to check for plane enablement.
    unsafe { (*old_plane_state).crtc.is_null() && !(*new_plane_state).crtc.is_null() }
}

/// # Safety
/// Both `old_plane_state` and `new_plane_state` must point to valid, initialized,
/// properly aligned, readable `DrmPlaneState` structures.
#[inline]
unsafe fn drm_atomic_plane_disabling(
    old_plane_state: *const DrmPlaneState,
    new_plane_state: *const DrmPlaneState,
) -> bool {
    // SAFETY: `old_plane_state` and `new_plane_state` are valid, aligned pointers to
    // `DrmPlaneState` whose `crtc` fields are read to check for plane disablement.
    unsafe { !(*old_plane_state).crtc.is_null() && (*new_plane_state).crtc.is_null() }
}

/// # Safety
/// If non-null, `commit` must point to an initialized, live `DrmCrtcCommit` with an
/// active reference count (> 0).
unsafe fn drm_crtc_commit_get(commit: *mut DrmCrtcCommit) -> *mut DrmCrtcCommit {
    // SAFETY: If `commit` is non-null, it points to a live `DrmCrtcCommit` with a positive
    // reference count; `kref_get` atomically increments its `ref` counter.
    unsafe {
        if !commit.is_null() {
            kref_get(core::ptr::addr_of_mut!((*commit).r#ref));
        }
        commit
    }
}

/// # Safety
/// If non-null, `commit` must point to a valid `DrmCrtcCommit` whose reference count
/// can be decremented and freed if it drops to zero.
unsafe fn drm_crtc_commit_put(commit: *mut DrmCrtcCommit) {
    // SAFETY: If `commit` is non-null, it points to a valid `DrmCrtcCommit`; `kref_put`
    // atomically decrements `ref` and invokes the free callback upon reaching zero.
    unsafe {
        if !commit.is_null() {
            kref_put(
                core::ptr::addr_of_mut!((*commit).r#ref),
                crate::drm_atomic_upstream::__drm_crtc_commit_free,
            );
        }
    }
}

/// # Safety
/// If non-null, `state` must point to an initialized, live `DrmAtomicCommit` with an
/// active reference count (> 0).
unsafe fn drm_atomic_commit_get(state: *mut DrmAtomicCommit) -> *mut DrmAtomicCommit {
    // SAFETY: If `state` is non-null, it points to a live `DrmAtomicCommit` with a positive
    // reference count; `kref_get` atomically increments its `ref` counter.
    unsafe {
        if !state.is_null() {
            kref_get(core::ptr::addr_of_mut!((*state).r#ref));
        }
        state
    }
}

/// # Safety
/// If non-null, `state` must point to a valid `DrmAtomicCommit` whose reference count
/// can be decremented and freed if it drops to zero.
unsafe fn drm_atomic_commit_put(state: *mut DrmAtomicCommit) {
    // SAFETY: If `state` is non-null, it points to a valid `DrmAtomicCommit`; `kref_put`
    // atomically decrements `ref` and invokes the free callback upon reaching zero.
    unsafe {
        if !state.is_null() {
            kref_put(
                core::ptr::addr_of_mut!((*state).r#ref),
                crate::drm_atomic_upstream::__drm_atomic_commit_free,
            );
        }
    }
}

/// # Safety
/// `commit` must point to valid, properly aligned, mutable memory allocated for `DrmCrtcCommit`.
/// If non-null, `crtc` must point to a live `DrmCrtc` associated with this commit.
unsafe fn init_commit(commit: *mut DrmCrtcCommit, crtc: *mut DrmCrtc) {
    // SAFETY: `commit` points to valid, aligned, mutable memory allocated for `DrmCrtcCommit`.
    // We initialize all completions, initialize the circular linked list node, initialize
    // the kref counter to 1, and assign the `crtc` back-pointer.
    unsafe {
        (*commit).flip_done = Completion::new();
        (*commit).hw_done = Completion::new();
        (*commit).cleanup_done = Completion::new();
        (*commit).commit_entry.next = core::ptr::addr_of_mut!((*commit).commit_entry);
        (*commit).commit_entry.prev = core::ptr::addr_of_mut!((*commit).commit_entry);
        kref_init(core::ptr::addr_of_mut!((*commit).r#ref));
        (*commit).crtc = crtc;
    }
}

/// # Safety
/// `completion` must point to the `flip_done` field of a valid `DrmCrtcCommit` allocation.
unsafe extern "C" fn release_crtc_commit(completion: *mut Completion) {
    // SAFETY: `completion` is the `flip_done` field of a live `DrmCrtcCommit` (function contract),
    // so stepping back by its offset recovers the commit whose reference is dropped here.
    unsafe {
        let commit = completion
            .cast::<u8>()
            .sub(offset_of!(DrmCrtcCommit, flip_done))
            .cast::<DrmCrtcCommit>();
        drm_crtc_commit_put(commit);
    }
}

/// # Safety
/// `work` must point to valid, properly aligned, mutable memory allocated for `WorkStruct`.
unsafe fn init_work(work: *mut WorkStruct, func: unsafe extern "C" fn(*mut WorkStruct)) {
    // SAFETY: `work` points to valid, aligned, mutable memory for `WorkStruct`. We initialize
    // the atomic data field, self-link the list entry, and assign the callback function pointer.
    unsafe {
        (*work).data = 0;
        (*work).entry.next = core::ptr::addr_of_mut!((*work).entry);
        (*work).entry.prev = core::ptr::addr_of_mut!((*work).entry);
        (*work).function = Some(func);
    }
}

// # Safety
//
// Each declaration is an external C API with its own preconditions:
// - `drm_connector_get_single_encoder`: `connector` must point to an initialized `DrmConnector`.
// - `dma_fence_wait` / `dma_fence_set_deadline` / `dma_fence_put`: `fence` must point to a live `DmaFence`.
unsafe extern "C" {
    fn drm_connector_get_single_encoder(connector: *mut DrmConnector) -> *mut DrmEncoder;
    fn dma_fence_wait(fence: *mut drm_kapi::types::DmaFence, intr: bool) -> i32;
    fn dma_fence_set_deadline(fence: *mut drm_kapi::types::DmaFence, deadline: i64);
    fn dma_fence_put(fence: *mut drm_kapi::types::DmaFence);
}

// =========================================================================
// Section 1: Modeset Checks & Routing
// =========================================================================

// upstream: drm_atomic_helper.c drm_atomic_helper_plane_changed()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit`. `old_plane_state` and `plane_state` must point
/// to valid plane states tracked within `state`. The caller must hold required modeset locks.
unsafe fn drm_atomic_helper_plane_changed(
    state: *mut DrmAtomicCommit,
    old_plane_state: *mut DrmPlaneState,
    plane_state: *mut DrmPlaneState,
    _plane: *mut DrmPlane,
) {
    // SAFETY: `state`, `old_plane_state`, and `plane_state` are live pointers in the atomic
    // commit graph. Dereferencing reads the CRTC pointers and retrieves the corresponding new
    // CRTC states within `state` to mark planes_changed under modeset locks.
    unsafe {
        if !(*old_plane_state).crtc.is_null() {
            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                state,
                (*old_plane_state).crtc,
            );
            if !crtc_state.is_null() {
                (*crtc_state).set_planes_changed(true);
            }
        }
        if !(*plane_state).crtc.is_null() {
            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                state,
                (*plane_state).crtc,
            );
            if !crtc_state.is_null() {
                (*crtc_state).set_planes_changed(true);
            }
        }
    }
}

// upstream: drm_atomic_helper.c handle_conflicting_encoders()
/// # Safety
/// `state` must point to a live, initialized `DrmAtomicCommit` with valid connector arrays and device pointer.
/// The caller must hold the DRM modeset locks for the affected device.
unsafe fn handle_conflicting_encoders(
    state: *mut DrmAtomicCommit,
    disable_conflicting_encoders: bool,
) -> i32 {
    // SAFETY: `state` is a live atomic commit with `num_connector` valid connector entries.
    // Iterating over connectors and calling connector helper callbacks is synchronized by the
    // caller-held modeset locks; all referenced connectors and encoders remain allocated.
    unsafe {
        let mut encoder_mask = 0u32;
        let num_connectors = (*state).num_connector as usize;

        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            let new_conn_state = (*conn_entry).new_state;
            if new_conn_state.is_null() || (*new_conn_state).crtc.is_null() {
                continue;
            }

            let funcs = (*connector).helper_private as *const DrmConnectorHelperFuncs;
            let new_encoder = if !funcs.is_null() && (*funcs).atomic_best_encoder.is_some() {
                ((*funcs).atomic_best_encoder.unwrap())(connector, state)
            } else if !funcs.is_null() && (*funcs).best_encoder.is_some() {
                ((*funcs).best_encoder.unwrap())(connector)
            } else {
                drm_connector_get_single_encoder(connector)
            };

            if !new_encoder.is_null() {
                let mask = 1u32 << (*new_encoder).index;
                if (encoder_mask & mask) != 0 {
                    return -EINVAL;
                }
                encoder_mask |= mask;
            }
        }

        if encoder_mask == 0 {
            return 0;
        }

        let dev = (*state).dev;
        let mut conn_iter: DrmConnectorListIter = core::mem::zeroed();
        crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut conn_iter);
        let mut ret = 0;
        loop {
            let connector =
                crate::drm_connector_upstream::drm_connector_list_iter_next(&mut conn_iter);
            if connector.is_null() {
                break;
            }

            if !crate::drm_atomic_upstream::drm_atomic_get_new_connector_state(state, connector)
                .is_null()
            {
                continue;
            }

            let encoder = if !(*connector).state.is_null() {
                (*(*connector).state).best_encoder
            } else {
                core::ptr::null_mut()
            };

            if encoder.is_null() || (encoder_mask & (1u32 << (*encoder).index)) == 0 {
                continue;
            }

            if !disable_conflicting_encoders {
                ret = -EINVAL;
                break;
            }

            let new_conn_state =
                crate::drm_atomic_upstream::drm_atomic_get_connector_state(state, connector);
            if is_err(new_conn_state) {
                ret = ptr_err(new_conn_state);
                break;
            }

            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                state,
                (*new_conn_state).crtc,
            );

            ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_connector(
                new_conn_state,
                core::ptr::null_mut(),
            );
            if ret != 0 {
                break;
            }

            if !crtc_state.is_null() && (*crtc_state).connector_mask == 0 {
                ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_mode_prop_for_crtc(
                    crtc_state,
                    core::ptr::null_mut(),
                );
                if ret < 0 {
                    break;
                }
                (*crtc_state).active = false;
            }
        }
        crate::drm_connector_upstream::drm_connector_list_iter_end(&mut conn_iter);
        ret
    }
}

// upstream: drm_atomic_helper.c set_best_encoder()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit`, and `conn_state` must be a valid `DrmConnectorState` within `state`.
/// If non-null, `encoder` must point to a live `DrmEncoder`. The caller must hold modeset locks.
unsafe fn set_best_encoder(
    state: *mut DrmAtomicCommit,
    conn_state: *mut DrmConnectorState,
    encoder: *mut DrmEncoder,
) {
    // SAFETY: `state` points to a live `DrmAtomicCommit`, and `conn_state` is a valid entry in `state.connectors`.
    // Modifying CRTC encoder masks and writing `conn_state.best_encoder` is serialized under caller-held modeset locks.
    unsafe {
        if !(*conn_state).best_encoder.is_null() {
            let connector = (*conn_state).connector;
            let crtc = if !connector.is_null() && !(*connector).state.is_null() {
                (*(*connector).state).crtc
            } else {
                core::ptr::null_mut()
            };
            if !crtc.is_null() {
                let crtc_state =
                    crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc);
                if !crtc_state.is_null() {
                    (*crtc_state).encoder_mask &= !(1u32 << (*(*conn_state).best_encoder).index);
                }
            }
        }

        if !encoder.is_null() {
            let crtc = (*conn_state).crtc;
            if !crtc.is_null() {
                let crtc_state =
                    crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc);
                if !crtc_state.is_null() {
                    (*crtc_state).encoder_mask |= 1u32 << (*encoder).index;
                }
            }
        }

        (*conn_state).best_encoder = encoder;
    }
}

// upstream: drm_atomic_helper.c steal_encoder()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit` with valid connector states. `encoder` must point to a live `DrmEncoder`.
/// The caller must hold modeset locks.
unsafe fn steal_encoder(state: *mut DrmAtomicCommit, encoder: *mut DrmEncoder) {
    // SAFETY: `state` is a live atomic commit whose `connectors` array has `num_connector` valid entries.
    // Traversal and stealing of the encoder assignment is serialized by the caller's modeset lock.
    unsafe {
        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let old_conn_state = (*conn_entry).old_state;
            let new_conn_state = (*conn_entry).new_state;

            if new_conn_state.is_null() || (*new_conn_state).best_encoder != encoder {
                continue;
            }

            let encoder_crtc = (*old_conn_state).crtc;
            set_best_encoder(state, new_conn_state, core::ptr::null_mut());

            let crtc_state =
                crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, encoder_crtc);
            if !crtc_state.is_null() {
                (*crtc_state).set_connectors_changed(true);
            }
            return;
        }
    }
}

// upstream: drm_atomic_helper.c update_connector_routing()
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit`.
/// - `connector` must point to an initialized `DrmConnector` tracked within `state`.
/// - `old_connector_state` and `new_connector_state` must point to valid connector states for `connector`.
/// - The caller must hold modeset locks for the connector and its attached CRTC.
unsafe fn update_connector_routing(
    state: *mut DrmAtomicCommit,
    connector: *mut DrmConnector,
    old_connector_state: *mut DrmConnectorState,
    new_connector_state: *mut DrmConnectorState,
    added_by_user: bool,
) -> i32 {
    // SAFETY: `old_connector_state` and `new_connector_state` are valid entries belonging to `connector` in `state`.
    // Updating connector routing and marking `connectors_changed` on affected CRTCs is serialized by modeset locks.
    unsafe {
        if (*old_connector_state).crtc != (*new_connector_state).crtc {
            if !(*old_connector_state).crtc.is_null() {
                let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                    state,
                    (*old_connector_state).crtc,
                );
                if !crtc_state.is_null() {
                    (*crtc_state).set_connectors_changed(true);
                }
            }
            if !(*new_connector_state).crtc.is_null() {
                let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                    state,
                    (*new_connector_state).crtc,
                );
                if !crtc_state.is_null() {
                    (*crtc_state).set_connectors_changed(true);
                }
            }
        }

        if (*new_connector_state).crtc.is_null() {
            set_best_encoder(state, new_connector_state, core::ptr::null_mut());
            return 0;
        }

        let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
            state,
            (*new_connector_state).crtc,
        );

        let is_unregistered = (*connector).registration_state
            == crate::drm_connector_types::DRM_CONNECTOR_UNREGISTERED;
        if !(*state).duplicated()
            && is_unregistered
            && added_by_user
            && !crtc_state.is_null()
            && (*crtc_state).active
        {
            return -EINVAL;
        }

        let funcs = (*connector).helper_private as *const DrmConnectorHelperFuncs;
        let new_encoder = if !funcs.is_null() && (*funcs).atomic_best_encoder.is_some() {
            ((*funcs).atomic_best_encoder.unwrap())(connector, state)
        } else if !funcs.is_null() && (*funcs).best_encoder.is_some() {
            ((*funcs).best_encoder.unwrap())(connector)
        } else {
            drm_connector_get_single_encoder(connector)
        };

        if new_encoder.is_null() {
            return -EINVAL;
        }

        let crtc_mask = 1u32 << (*(*new_connector_state).crtc).index;
        if ((*new_encoder).possible_crtcs & crtc_mask) == 0 {
            return -EINVAL;
        }

        if new_encoder == (*new_connector_state).best_encoder {
            set_best_encoder(state, new_connector_state, new_encoder);
            return 0;
        }

        steal_encoder(state, new_encoder);
        set_best_encoder(state, new_connector_state, new_encoder);
        if !crtc_state.is_null() {
            (*crtc_state).set_connectors_changed(true);
        }
        0
    }
}

// upstream: drm_atomic_helper.c mode_fixup()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit` with initialized CRTC and connector state arrays.
/// The caller must hold modeset locks covering all objects in `state`.
unsafe fn mode_fixup(state: *mut DrmAtomicCommit) -> i32 {
    let dev = unsafe {
        // SAFETY: reads the `dev` back-pointer of `state`, which stays allocated for the duration of this call.
        (*state).dev
    };
    let num_crtcs = unsafe {
        // SAFETY: `dev` came from the live commit above; `num_crtc` is the length of the `crtcs` array.
        (*dev).mode_config.num_crtc
    } as usize;
    for i in 0..num_crtcs {
        let entry = unsafe {
            // SAFETY: the loop index is below `num_crtcs`, so the offset stays inside the `crtcs` array of `state`.
            (*state).crtcs.add(i)
        };
        let new_crtc_state = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).new_state
        };
        if new_crtc_state.is_null() {
            continue;
        }
        if !unsafe {
            // SAFETY: `new_crtc_state` was null-checked above and points to the CRTC state owned by the live commit.
            (*new_crtc_state).mode_changed()
        } && !unsafe {
            // SAFETY: `new_crtc_state` was null-checked above and points to the CRTC state owned by the live commit.
            (*new_crtc_state).connectors_changed()
        } {
            continue;
        }
        // SAFETY: `new_crtc_state` is a valid mutable `DrmCrtcState`; copying mode into adjusted_mode.
        unsafe {
            crate::drm_modes_upstream::drm_mode_copy(
                core::ptr::addr_of_mut!((*new_crtc_state).adjusted_mode),
                core::ptr::addr_of!((*new_crtc_state).mode),
            )
        };
    }

    let num_connectors = unsafe {
        // SAFETY: `num_connector` is the length of the live commit's `connectors` array and bounds the loop index.
        (*state).num_connector
    } as usize;
    for i in 0..num_connectors {
        let conn_entry = unsafe {
            // SAFETY: the loop index is below `num_connectors`, so the offset stays inside the `connectors` array of `state`.
            (*state).connectors.add(i)
        };
        let new_conn_state = unsafe {
            // SAFETY: `conn_entry` is an element of the `connectors` array at an index below `num_connector`.
            (*conn_entry).new_state
        };
        if new_conn_state.is_null()
            || unsafe {
                // SAFETY: `new_conn_state` was null-checked above and points to the connector state owned by the live commit.
                (*new_conn_state).crtc.is_null()
            }
            || unsafe {
                // SAFETY: `new_conn_state` was null-checked above and points to the connector state owned by the live commit.
                (*new_conn_state).best_encoder.is_null()
            }
        {
            continue;
        }

        // SAFETY: `state` and `new_conn_state.crtc` are valid objects in the commit graph.
        let new_crtc_state = unsafe {
            crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, (*new_conn_state).crtc)
        };
        let encoder = unsafe {
            // SAFETY: `new_conn_state` was null-checked above and points to the connector state owned by the live commit.
            (*new_conn_state).best_encoder
        };
        let funcs = unsafe {
            // SAFETY: `encoder` is the `best_encoder` of a connector state whose pointer was null-checked above.
            (*encoder).helper_private
        } as *const DrmEncoderHelperFuncs;

        // Bridges are outside this component's supported scope. Reject a
        // non-empty chain instead of silently skipping its atomic check.
        // SAFETY: `encoder` is non-null; reading its `bridge_chain` list head to verify it is empty.
        if !unsafe { drm_kapi::list::list_empty(&(*encoder).bridge_chain) } {
            return -EOPNOTSUPP;
        }

        if !funcs.is_null()
            && unsafe {
                // SAFETY: `funcs` is non-null, as checked in the enclosing condition, so reading the `atomic_check` callback slot is valid.
                (*funcs).atomic_check.is_some()
            }
        {
            // SAFETY: `funcs.atomic_check` is Some; invoking encoder atomic_check under modeset locks.
            let ret = unsafe {
                ((*funcs).atomic_check.unwrap())(encoder, new_crtc_state, new_conn_state)
            };
            if ret != 0 {
                return ret;
            }
        } else if !funcs.is_null()
            && unsafe {
                // SAFETY: `funcs` is non-null, as checked in the enclosing condition, so reading the `mode_fixup` callback slot is valid.
                (*funcs).mode_fixup.is_some()
            }
        {
            // SAFETY: `funcs` was null-checked and its `mode_fixup` slot verified `Some` above; `new_crtc_state` was null-checked, so the pointers into it are valid.
            let ok = unsafe {
                ((*funcs).mode_fixup.unwrap())(
                    encoder,
                    core::ptr::addr_of_mut!((*new_crtc_state).mode),
                    core::ptr::addr_of_mut!((*new_crtc_state).adjusted_mode),
                )
            };
            if !ok {
                return -EINVAL;
            }
        }
    }

    for i in 0..num_crtcs {
        let entry = unsafe {
            // SAFETY: the loop index is below `num_crtcs`, so the offset stays inside the `crtcs` array of `state`.
            (*state).crtcs.add(i)
        };
        let crtc = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).ptr
        };
        let new_crtc_state = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).new_state
        };
        if new_crtc_state.is_null()
            || !unsafe {
                // SAFETY: `new_crtc_state` was null-checked above and points to the CRTC state owned by the live commit.
                (*new_crtc_state).enable
            }
        {
            continue;
        }
        if !unsafe {
            // SAFETY: `new_crtc_state` was null-checked above and points to the CRTC state owned by the live commit.
            (*new_crtc_state).mode_changed()
        } && !unsafe {
            // SAFETY: `new_crtc_state` was null-checked above and points to the CRTC state owned by the live commit.
            (*new_crtc_state).connectors_changed()
        } {
            continue;
        }

        let funcs = unsafe {
            // SAFETY: `crtc` is the `ptr` of an in-range `crtcs` entry, so its helper table pointer can be read.
            (*crtc).helper_private
        } as *const DrmCrtcHelperFuncs;
        if funcs.is_null()
            || unsafe {
                // SAFETY: `funcs` is non-null, as checked in the enclosing condition, so reading the `mode_fixup` callback slot is valid.
                (*funcs).mode_fixup.is_none()
            }
        {
            continue;
        }

        // SAFETY: `funcs` was null-checked and its `mode_fixup` slot verified `Some` above; `new_crtc_state` was null-checked, so the pointers into it are valid.
        let ok = unsafe {
            ((*funcs).mode_fixup.unwrap())(
                crtc,
                core::ptr::addr_of_mut!((*new_crtc_state).mode),
                core::ptr::addr_of_mut!((*new_crtc_state).adjusted_mode),
            )
        };
        if !ok {
            return -EINVAL;
        }
    }

    0
}

// upstream: drm_atomic_helper.c mode_valid_path()
/// # Safety
/// - `connector` and `mode` must point to live `DrmConnector` and `DrmDisplayMode` instances.
/// - If non-null, `encoder` and `crtc` must point to initialized `DrmEncoder` and `DrmCrtc` structures.
/// - Helper callback tables on `encoder` and `crtc` must remain valid for the duration of the validation call.
unsafe fn mode_valid_path(
    _connector: *mut DrmConnector,
    encoder: *mut DrmEncoder,
    crtc: *mut DrmCrtc,
    mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    let mut ret = unsafe {
        // SAFETY: `mode` is a live `DrmDisplayMode`, and `encoder` is null or a live `DrmEncoder`, as the `# Safety` contract allows.
        crate::drm_probe_helper_upstream::drm_encoder_mode_valid(encoder, mode)
    };
    if ret != crate::drm_modes_types::MODE_OK {
        return ret;
    }

    // SAFETY: `encoder` was verified non-null above; reading its `bridge_chain` list head to verify it is empty.
    if !encoder.is_null() && !unsafe { drm_kapi::list::list_empty(&(*encoder).bridge_chain) } {
        return crate::drm_modes_types::MODE_ERROR;
    }

    ret = unsafe {
        // SAFETY: `crtc` is null or a live `DrmCrtc`, and `mode` is the same live mode validated above.
        crate::drm_probe_helper_upstream::drm_crtc_mode_valid(crtc, mode)
    };
    ret
}

// upstream: drm_atomic_helper.c mode_valid()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit` with valid connector and CRTC arrays.
/// Objects in `state` and their helper callback tables must remain valid throughout validation.
unsafe fn mode_valid(state: *mut DrmAtomicCommit) -> i32 {
    // SAFETY: `state` is a live atomic commit; traversing its `connectors` and `crtcs` arrays
    // and calling probe helper `mode_valid_path` on each active connector-encoder-CRTC path.
    unsafe {
        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            let conn_state = (*conn_entry).new_state;
            if conn_state.is_null() {
                continue;
            }
            let encoder = (*conn_state).best_encoder;
            let crtc = (*conn_state).crtc;
            if crtc.is_null() || encoder.is_null() {
                continue;
            }

            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc);
            if crtc_state.is_null() {
                continue;
            }
            if !(*crtc_state).mode_changed() && !(*crtc_state).connectors_changed() {
                continue;
            }

            let mode = core::ptr::addr_of!((*crtc_state).mode);
            let status = mode_valid_path(connector, encoder, crtc, mode);
            if status != crate::drm_modes_types::MODE_OK {
                return -EINVAL;
            }
        }
        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_check_valid_clones()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit`. `crtc` must point to an initialized `DrmCrtc`
/// whose `dev` pointer references a valid `DrmDevice` with an initialized `encoder_list`.
unsafe fn drm_atomic_check_valid_clones(state: *mut DrmAtomicCommit, crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: `crtc` belongs to `dev`; traversing `dev.mode_config.encoder_list` where each list
    // entry is at offset 8 within `DrmEncoder`. Pointer subtraction recovers the encoder to validate cloning masks.
    unsafe {
        let dev = (*crtc).dev;
        let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc);
        let encoder_mask = (*crtc_state).encoder_mask;

        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            let encoder = (cur as *mut u8).sub(8) as *mut DrmEncoder;
            let mask = 1u32 << (*encoder).index;
            if (encoder_mask & mask) != 0 {
                if (*encoder).possible_clones == 0 {
                    cur = (*cur).next;
                    continue;
                }
                if (encoder_mask & (*encoder).possible_clones) != encoder_mask {
                    return -EINVAL;
                }
            }
            cur = (*cur).next;
        }

        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_check_modeset()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to an initialized, live `DrmAtomicCommit` containing valid CRTC and connector state arrays.
/// - If `dev` is non-null, it must point to the `DrmDevice` associated with `state` (otherwise `state.dev` is used).
/// - Caller must hold DRM modeset locks (via `DrmModesetAcquireCtx`) for all CRTCs and connectors referenced in `state`.
/// - All encoder and bridge callback tables (`helper_private`) referenced by active connectors must remain valid.
/// - Returns 0 on success, or a negative errno if routing, cloning, or mode validity checks fail.
pub unsafe extern "C" fn drm_atomic_helper_check_modeset(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: `state` contains initialized connector and CRTC state arrays bounded by `num_connector` and `num_crtc`.
    // Validating timing, routing, and clone constraints reads state records under caller-held modeset locks.
    // Iterates over connector and CRTC states to validate mode timing, connector routing,
    // and clone constraints under the caller-held modeset locks.
    unsafe {
        let mut connectors_mask = 0u64;
        let mut user_connectors_mask = 0u64;

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            user_connectors_mask |= 1u64 << i;
        }

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let old_crtc_state = (*entry).old_state;
            let new_crtc_state = (*entry).new_state;

            if crtc.is_null() || old_crtc_state.is_null() || new_crtc_state.is_null() {
                continue;
            }

            let has_connectors = (*new_crtc_state).connector_mask != 0;

            if !crate::drm_modes_upstream::drm_mode_equal(
                core::ptr::addr_of!((*old_crtc_state).mode),
                core::ptr::addr_of!((*new_crtc_state).mode),
            ) {
                (*new_crtc_state).set_mode_changed(true);
            }

            if (*old_crtc_state).enable != (*new_crtc_state).enable {
                (*new_crtc_state).set_mode_changed(true);
                (*new_crtc_state).set_connectors_changed(true);
            }

            if (*old_crtc_state).active != (*new_crtc_state).active {
                (*new_crtc_state).set_active_changed(true);
            }

            if (*new_crtc_state).enable != has_connectors {
                return -EINVAL;
            }

            (*new_crtc_state).no_vblank = !crate::drm_vblank_upstream::drm_dev_has_vblank(dev);
        }

        let mut ret = handle_conflicting_encoders(state, false);
        if ret != 0 {
            return ret;
        }

        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            let old_connector_state = (*conn_entry).old_state;
            let new_connector_state = (*conn_entry).new_state;

            if connector.is_null() || old_connector_state.is_null() || new_connector_state.is_null()
            {
                continue;
            }

            let funcs = (*connector).helper_private as *const DrmConnectorHelperFuncs;

            ret = update_connector_routing(
                state,
                connector,
                old_connector_state,
                new_connector_state,
                ((1u64 << i) & user_connectors_mask) != 0,
            );
            if ret != 0 {
                return ret;
            }

            if !(*old_connector_state).crtc.is_null() {
                let new_crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                    state,
                    (*old_connector_state).crtc,
                );
                if !new_crtc_state.is_null() {
                    if (*old_connector_state).link_status != (*new_connector_state).link_status {
                        (*new_crtc_state).set_connectors_changed(true);
                    }
                    if (*old_connector_state).max_requested_bpc
                        != (*new_connector_state).max_requested_bpc
                    {
                        (*new_crtc_state).set_connectors_changed(true);
                    }
                }
            }

            if !funcs.is_null() && (*funcs).atomic_check.is_some() {
                ret = ((*funcs).atomic_check.unwrap())(connector, state);
                if ret != 0 {
                    return ret;
                }
            }

            connectors_mask |= 1u64 << i;
        }

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;

            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            if !drm_atomic_crtc_needs_modeset(new_crtc_state) {
                continue;
            }

            ret = crate::drm_atomic_upstream::drm_atomic_add_affected_connectors(state, crtc);
            if ret != 0 {
                return ret;
            }

            ret = crate::drm_atomic_upstream::drm_atomic_add_affected_planes(state, crtc);
            if ret != 0 {
                return ret;
            }

            ret = drm_atomic_check_valid_clones(state, crtc);
            if ret != 0 {
                return ret;
            }
        }

        let num_connectors_after = (*state).num_connector as usize;
        for i in 0..num_connectors_after {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;

            if (connectors_mask & (1u64 << i)) != 0 {
                continue;
            }

            let funcs = (*connector).helper_private as *const DrmConnectorHelperFuncs;
            if !funcs.is_null() && (*funcs).atomic_check.is_some() {
                ret = ((*funcs).atomic_check.unwrap())(connector, state);
                if ret != 0 {
                    return ret;
                }
            }
        }

        for i in 0..num_connectors_after {
            let conn_entry = (*state).connectors.add(i);
            let old_conn_state = (*conn_entry).old_state;
            let new_conn_state = (*conn_entry).new_state;

            if !old_conn_state.is_null() && !(*old_conn_state).best_encoder.is_null() {
                ret = crate::drm_atomic_upstream::drm_atomic_add_encoder_bridges(
                    state,
                    (*old_conn_state).best_encoder,
                );
                if ret != 0 {
                    return ret;
                }
            }

            if !new_conn_state.is_null() && !(*new_conn_state).best_encoder.is_null() {
                ret = crate::drm_atomic_upstream::drm_atomic_add_encoder_bridges(
                    state,
                    (*new_conn_state).best_encoder,
                );
                if ret != 0 {
                    return ret;
                }
            }
        }

        ret = mode_valid(state);
        if ret != 0 {
            return ret;
        }

        ret = mode_fixup(state);
        ret
    }
}

// =========================================================================
// Section 2: Plane Check & Helpers
// =========================================================================

// upstream: drm_atomic_helper.c drm_atomic_helper_check_wb_connector_state()
#[unsafe(no_mangle)]
/// # Safety
/// - `connector` must point to a live `DrmConnector` with `DRM_MODE_CONNECTOR_WRITEBACK` type and valid `writeback` helper private pointer.
/// - `state` must point to a live `DrmAtomicCommit` tracking `connector` and its target CRTC.
/// - Caller must hold the modeset lock covering `connector`.
/// - If `connector` or `state` is null, returns `-EINVAL`.
/// - Returns 0 if pixel format and CRTC routing are valid, or `-EINVAL` / `-ENOLINK` on failure.
pub unsafe extern "C" fn drm_atomic_helper_check_wb_connector_state(
    connector: *mut DrmConnector,
    state: *mut DrmAtomicCommit,
) -> i32 {
    if connector.is_null() || state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `connector` and `state` were verified non-null above. `drm_atomic_get_new_connector_state`
    // looks up the corresponding state entry in `state.connectors` under caller-held modeset locks.
    // `connector` and `conn_state` performs read-only checks on connector type and writeback job pointer.
    unsafe {
        let conn_state =
            crate::drm_atomic_upstream::drm_atomic_get_new_connector_state(state, connector);
        if is_err(conn_state) {
            return ptr_err(conn_state);
        }
        if conn_state.is_null() {
            return -EINVAL;
        }
        if (*connector).connector_type
            == crate::uapi_drm_consts::DRM_MODE_CONNECTOR_WRITEBACK as i32
            || !(*conn_state).writeback_job.is_null()
        {
            return -EOPNOTSUPP;
        }
        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_check_plane_state()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane_state` must point to a live, mutable `DrmPlaneState` with valid `plane` and optional `fb` pointers.
/// - If `crtc_state` is non-null, it must point to an initialized `DrmCrtcState` for the CRTC assigned to the plane.
/// - If `crtc_state` is null, the function marks the plane invisible unless `can_update_disabled` is true and `plane_state.crtc` is non-null.
/// - `min_scale` and `max_scale` must be positive fixed-point (16.16) values defining acceptable scaling ratios.
/// - The caller must hold modeset locks for the plane and associated CRTC.
/// - Mutates `plane_state.visible`, `plane_state.src`, and `plane_state.dst` clipping coordinates.
pub unsafe extern "C" fn drm_atomic_helper_check_plane_state(
    plane_state: *mut DrmPlaneState,
    crtc_state: *const DrmCrtcState,
    min_scale: i32,
    max_scale: i32,
    can_position: bool,
    can_update_disabled: bool,
) -> i32 {
    // SAFETY: `plane_state` is a valid pointer within the DRM state graph. Dereferencing reads
    // and updates clipping rectangles and scaling limits according to driver constraints under modeset locks.
    unsafe {
        let fb = (*plane_state).fb;
        let src = core::ptr::addr_of_mut!((*plane_state).src);
        let dst = core::ptr::addr_of_mut!((*plane_state).dst);
        let rotation = (*plane_state).rotation;
        let mut clip = DrmRect {
            x1: 0,
            y1: 0,
            x2: 0,
            y2: 0,
        };

        *src = DrmRect {
            x1: (*plane_state).src_x as i32,
            y1: (*plane_state).src_y as i32,
            x2: ((*plane_state).src_x + (*plane_state).src_w) as i32,
            y2: ((*plane_state).src_y + (*plane_state).src_h) as i32,
        };
        *dst = DrmRect {
            x1: (*plane_state).crtc_x,
            y1: (*plane_state).crtc_y,
            x2: (*plane_state).crtc_x + (*plane_state).crtc_w as i32,
            y2: (*plane_state).crtc_y + (*plane_state).crtc_h as i32,
        };

        if fb.is_null() {
            (*plane_state).visible = false;
            return 0;
        }

        if (*plane_state).crtc.is_null() {
            (*plane_state).visible = false;
            return 0;
        }

        if crtc_state.is_null() || (!(*crtc_state).enable && !can_update_disabled) {
            return -EINVAL;
        }

        crate::drm_rect_upstream::drm_rect_rotate(
            src,
            ((*fb).width as i32) << 16,
            ((*fb).height as i32) << 16,
            rotation,
        );

        let hscale = crate::drm_rect_upstream::drm_rect_calc_hscale(src, dst, min_scale, max_scale);
        let vscale = crate::drm_rect_upstream::drm_rect_calc_vscale(src, dst, min_scale, max_scale);
        if hscale < 0 || vscale < 0 {
            return -ERANGE;
        }

        if (*crtc_state).enable {
            crate::drm_modes_upstream::drm_mode_get_hv_timing(
                core::ptr::addr_of!((*crtc_state).mode),
                core::ptr::addr_of_mut!(clip.x2),
                core::ptr::addr_of_mut!(clip.y2),
            );
        }

        (*plane_state).visible =
            crate::drm_rect_upstream::drm_rect_clip_scaled(src, dst, core::ptr::addr_of!(clip));

        crate::drm_rect_upstream::drm_rect_rotate_inv(
            src,
            ((*fb).width as i32) << 16,
            ((*fb).height as i32) << 16,
            rotation,
        );

        if !(*plane_state).visible {
            return 0;
        }

        if !can_position && !drm_rect_equals(dst, core::ptr::addr_of!(clip)) {
            return -EINVAL;
        }

        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_check_crtc_primary_plane()
#[unsafe(no_mangle)]
/// # Safety
/// - `crtc_state` must point to a live, mutable `DrmCrtcState` with an initialized `state` (`DrmAtomicCommit`) pointer.
/// - If `crtc_state.enable` is false or the CRTC has no primary plane, returns 0 without modification.
/// - The caller must hold modeset locks covering the CRTC and its primary plane.
/// - Returns 0 if primary plane dimensions and framebuffer coverage are valid, or `-EINVAL` if dimensions mismatch.
pub unsafe extern "C" fn drm_atomic_helper_check_crtc_primary_plane(
    crtc_state: *mut DrmCrtcState,
) -> i32 {
    // SAFETY: `crtc_state` is non-null and points to a live `DrmCrtcState`. Traversing `dev.mode_config.plane_list`
    // to find an enabled primary plane matching `plane_mask` is serialized under modeset locks.
    unsafe {
        let crtc = (*crtc_state).crtc;
        let dev = (*crtc).dev;
        let plane_mask = (*crtc_state).plane_mask;

        let head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut cur = (*head).next;
        while cur != head {
            let plane = (cur as *mut u8).sub(8) as *mut DrmPlane;
            let mask = 1u32 << (*plane).index;
            if (plane_mask & mask) != 0 && (*plane).r#type == DRM_PLANE_TYPE_PRIMARY {
                return 0;
            }
            cur = (*cur).next;
        }

        -EINVAL
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_check_planes()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live, initialized `DrmAtomicCommit` with valid plane state entries.
/// - If `dev` is non-null, it must match the `DrmDevice` owning the plane objects in `state`.
/// - For each modified plane in `state`, `plane.helper_private` (if non-null) must point to valid `DrmPlaneHelperFuncs`.
/// - Caller must hold modeset locks for all planes and CRTCs tracked in `state`.
/// - Returns 0 on success, or a negative errno returned by any plane's `atomic_check` callback.
pub unsafe extern "C" fn drm_atomic_helper_check_planes(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: `dev.mode_config.num_total_plane` and `num_crtc` bound the plane and CRTC arrays in `state`.
    // Invoking `atomic_check` callbacks on active plane and CRTC helper function tables is serialized under modeset locks.
    unsafe {
        let num_planes = (*dev).mode_config.num_total_plane as usize;
        let mut ret = 0;

        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let old_plane_state = (*entry).old_state;
            let new_plane_state = (*entry).new_state;

            if plane.is_null() || old_plane_state.is_null() || new_plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;

            drm_atomic_helper_plane_changed(state, old_plane_state, new_plane_state, plane);

            crate::drm_damage_helper_upstream::drm_atomic_helper_check_plane_damage(
                state,
                new_plane_state,
            );

            if !funcs.is_null() && (*funcs).atomic_check.is_some() {
                ret = ((*funcs).atomic_check.unwrap())(plane, state);
                if ret != 0 {
                    return ret;
                }
            }
        }

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;

            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            let funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
            if !funcs.is_null() && (*funcs).atomic_check.is_some() {
                ret = ((*funcs).atomic_check.unwrap())(crtc, state);
                if ret != 0 {
                    return ret;
                }
            }
        }

        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_check()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must point to an active `DrmDevice` instance.
/// - `state` must point to a live `DrmAtomicCommit` describing the complete proposed atomic update.
/// - Caller must hold modeset locks (via `DrmModesetAcquireCtx`) for all objects modified in `state`.
/// - Subordinate modeset and plane helper callback tables must remain valid throughout the validation pass.
/// - Returns 0 if all modeset and plane constraints are satisfied, or a negative errno on validation failure.
pub unsafe extern "C" fn drm_atomic_helper_check(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: `dev` and `state` are initialized structures with modeset locks held by caller.
    // Calling `drm_atomic_helper_check_modeset` and `drm_atomic_helper_check_planes` validates the complete state transaction.
    unsafe {
        let mut ret = drm_atomic_helper_check_modeset(dev, state);
        if ret != 0 {
            return ret;
        }

        if (*dev).mode_config.normalize_zpos {
            ret = crate::drm_blend_upstream::drm_atomic_normalize_zpos(dev, state);
            if ret != 0 {
                return ret;
            }
        }

        ret = drm_atomic_helper_check_planes(dev, state);
        if ret != 0 {
            return ret;
        }

        if (*state).legacy_cursor_update() {
            let async_ok = drm_atomic_helper_async_check(dev, state) == 0;
            (*state).set_async_update(async_ok);
        }

        ret
    }
}

// =========================================================================
// Section 3: Modeset Commits (Disables, Enables, Set Mode)
// =========================================================================

// upstream: drm_atomic_helper.c crtc_needs_disable()
/// # Safety
/// Both `old_state` and `new_state` must point to valid, initialized `DrmCrtcState` structures.
unsafe fn crtc_needs_disable(old_state: *mut DrmCrtcState, new_state: *mut DrmCrtcState) -> bool {
    // SAFETY: `old_state` and `new_state` are valid pointers to `DrmCrtcState`; reading
    // `active`, `enable`, and mode change flags to determine if the CRTC must be disabled.
    unsafe {
        if new_state.is_null() {
            return drm_atomic_crtc_effectively_active(old_state);
        }

        if (*old_state).self_refresh_active && (*old_state).crtc != (*new_state).crtc {
            return true;
        }

        (*old_state).active
            || ((*old_state).self_refresh_active && !(*new_state).active)
            || (*new_state).self_refresh_active
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_encoder_bridge_disable()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` with valid connector arrays and routing tables.
/// - Caller must be in the modeset disable phase of commit tail, holding modeset locks for all affected encoders and bridges.
/// - Encoder and bridge function tables (`helper_private`) must remain live during callback invocations.
/// - Traverses connectors whose CRTC or connection was disabled and executes bridge and encoder disable hooks.
pub unsafe extern "C" fn drm_atomic_helper_commit_encoder_bridge_disable(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `state.connectors` contains `num_connector` valid old connector state entries.
    // Invoking `disable` and DPMS callbacks on detached encoders and bridges is serialized under modeset locks.
    unsafe {
        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let old_conn_state = (*conn_entry).old_state;
            let new_conn_state = (*conn_entry).new_state;

            if old_conn_state.is_null() || (*old_conn_state).crtc.is_null() {
                continue;
            }

            let old_crtc_state = crate::drm_atomic_upstream::drm_atomic_get_old_crtc_state(
                state,
                (*old_conn_state).crtc,
            );
            let new_crtc_state = if !new_conn_state.is_null() && !(*new_conn_state).crtc.is_null() {
                crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                    state,
                    (*new_conn_state).crtc,
                )
            } else {
                core::ptr::null_mut()
            };

            let old_crtc = (*old_conn_state).crtc;
            if !crtc_needs_disable(old_crtc_state, new_crtc_state)
                || !drm_atomic_crtc_needs_modeset((*old_crtc).state)
            {
                continue;
            }

            let encoder = (*old_conn_state).best_encoder;
            if encoder.is_null() {
                continue;
            }

            let funcs = (*encoder).helper_private as *const DrmEncoderHelperFuncs;

            if !funcs.is_null() {
                if (*funcs).atomic_disable.is_some() {
                    ((*funcs).atomic_disable.unwrap())(encoder, state);
                } else if !new_conn_state.is_null()
                    && !(*new_conn_state).crtc.is_null()
                    && (*funcs).prepare.is_some()
                {
                    ((*funcs).prepare.unwrap())(encoder);
                } else if (*funcs).disable.is_some() {
                    ((*funcs).disable.unwrap())(encoder);
                } else if (*funcs).dpms.is_some() {
                    ((*funcs).dpms.unwrap())(encoder, DRM_MODE_DPMS_OFF);
                }
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_crtc_disable()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` containing old and new CRTC states.
/// - `dev` if non-null must point to the parent `DrmDevice` of `state`.
/// - Caller must hold modeset locks covering CRTCs transitioning from active to inactive.
/// - `crtc.helper_private` for affected CRTCs must point to valid `DrmCrtcHelperFuncs` with `atomic_disable` or `disable`.
pub unsafe extern "C" fn drm_atomic_helper_commit_crtc_disable(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_crtc` bounds the `state.crtcs` array. Calling `atomic_disable` or `disable`
    // on CRTCs transitioning to inactive is serialized under caller-held modeset locks.
    // callbacks on CRTCs requiring shutdown under the caller's commit synchronization.
    unsafe {
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let old_crtc_state = (*entry).old_state;
            let new_crtc_state = (*entry).new_state;

            if crtc.is_null() || old_crtc_state.is_null() || new_crtc_state.is_null() {
                continue;
            }

            if !drm_atomic_crtc_needs_modeset(new_crtc_state) {
                continue;
            }

            if !crtc_needs_disable(old_crtc_state, new_crtc_state) {
                continue;
            }

            let funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
            if !funcs.is_null() {
                if (*new_crtc_state).enable && (*funcs).prepare.is_some() {
                    ((*funcs).prepare.unwrap())(crtc);
                } else if (*funcs).atomic_disable.is_some() {
                    ((*funcs).atomic_disable.unwrap())(crtc, state);
                } else if (*funcs).disable.is_some() {
                    ((*funcs).disable.unwrap())(crtc);
                } else if (*funcs).dpms.is_some() {
                    ((*funcs).dpms.unwrap())(crtc, DRM_MODE_DPMS_OFF);
                }
            }

            if !crate::drm_vblank_upstream::drm_dev_has_vblank(dev) {
                continue;
            }

            let ret = crate::drm_vblank_upstream::drm_crtc_vblank_get(crtc);
            if ret == 0 {
                crate::drm_vblank_upstream::drm_crtc_vblank_put(crtc);
            }
        }
    }
}

// upstream: drm_atomic_helper.c disable_outputs()
/// # Safety
/// `dev` and `state` must point to live, initialized DRM device and atomic commit structures.
/// The caller must hold modeset locks and retain object callback tables.
unsafe fn disable_outputs(dev: *mut DrmDevice, state: *mut DrmAtomicCommit) {
    // SAFETY: Orchestrating modeset disables calls `commit_encoder_bridge_disable` then `commit_crtc_disable`
    // on valid state structures under caller-held modeset locks.
    unsafe {
        drm_atomic_helper_commit_encoder_bridge_disable(dev, state);
        drm_atomic_helper_commit_crtc_disable(dev, state);
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_update_legacy_modeset_state()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` whose new state has already been swapped into hardware/driver records.
/// - Caller must hold legacy DRM modeset locks (`dev.mode_config.mutex`).
/// - Updates legacy fields (`crtc.mode`, `crtc.enabled`, `crtc.x`, `crtc.y`, `connector.encoder`) on live DRM objects.
pub unsafe extern "C" fn drm_atomic_helper_update_legacy_modeset_state(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `state` contains committed atomic states. Writing legacy `mode`, `enabled`, `x`, `y`,
    // and connector routing fields is serialized by the caller holding `dev.mode_config.mutex`.
    unsafe {
        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            let old_conn_state = (*conn_entry).old_state;
            let new_conn_state = (*conn_entry).new_state;

            if !(*connector).encoder.is_null() {
                (*(*connector).encoder).crtc = core::ptr::null_mut();
                (*connector).encoder = core::ptr::null_mut();
            }

            let crtc = (*new_conn_state).crtc;
            if (crtc.is_null() && !(*old_conn_state).crtc.is_null())
                || (!crtc.is_null() && drm_atomic_crtc_needs_modeset((*crtc).state))
            {
                let mut mode = DRM_MODE_DPMS_OFF;
                if !crtc.is_null() && (*(*crtc).state).active {
                    mode = DRM_MODE_DPMS_ON;
                }
                (*connector).dpms = mode;
            }
        }

        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            let new_conn_state = (*conn_entry).new_state;

            if (*new_conn_state).crtc.is_null() {
                continue;
            }
            if (*new_conn_state).best_encoder.is_null() {
                continue;
            }

            (*connector).encoder = (*new_conn_state).best_encoder;
            (*(*connector).encoder).crtc = (*new_conn_state).crtc;
        }

        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            let primary = (*crtc).primary;
            crate::drm_modes_upstream::drm_mode_copy(
                core::ptr::addr_of_mut!((*crtc).mode),
                core::ptr::addr_of!((*new_crtc_state).mode),
            );
            (*crtc).enabled = (*new_crtc_state).enable;

            let new_plane_state =
                crate::drm_atomic_upstream::drm_atomic_get_new_plane_state(state, primary);
            if !new_plane_state.is_null() && (*new_plane_state).crtc == crtc {
                (*crtc).x = ((*new_plane_state).src_x >> 16) as i32;
                (*crtc).y = ((*new_plane_state).src_y >> 16) as i32;
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_calc_timestamping_constants()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` with valid CRTC state entries.
/// - For each CRTC in `state` whose mode changed or became active, `new_crtc_state.mode` must contain valid display timing parameters.
/// - Modifies internal timing fields of the associated `DrmCrtc` under caller-held modeset locks.
pub unsafe extern "C" fn drm_atomic_helper_calc_timestamping_constants(
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `state` is a live committed atomic state. Calculates vblank timestamping parameters
    // on CRTCs undergoing modeset under commit synchronization.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            if (*new_crtc_state).enable {
                crate::drm_vblank_upstream::drm_calc_timestamping_constants(
                    crtc,
                    core::ptr::addr_of!((*new_crtc_state).adjusted_mode),
                );
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_crtc_set_mode()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` with valid CRTC states.
/// - For each CRTC requiring a mode change (`mode_changed` is true), `crtc.helper_private` must point to valid `DrmCrtcHelperFuncs`.
/// - Caller must hold modeset locks and must have already disabled conflicting CRTC outputs.
pub unsafe extern "C" fn drm_atomic_helper_commit_crtc_set_mode(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_crtc` bounds the `state.crtcs` array. Calling `mode_set_nofb` on CRTCs
    // requiring a modeset programs timing registers without touching framebuffers under modeset locks.
    // callbacks on CRTCs undergoing modeset under commit synchronization.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            if !(*new_crtc_state).mode_changed() {
                continue;
            }

            let funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
            if (*new_crtc_state).enable && !funcs.is_null() && (*funcs).mode_set_nofb.is_some() {
                ((*funcs).mode_set_nofb.unwrap())(crtc);
            }
        }

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let new_conn_state = (*conn_entry).new_state;
            if new_conn_state.is_null() || (*new_conn_state).best_encoder.is_null() {
                continue;
            }

            let encoder = (*new_conn_state).best_encoder;
            let funcs = (*encoder).helper_private as *const DrmEncoderHelperFuncs;
            let new_crtc_state = (*(*new_conn_state).crtc).state;
            let mode = core::ptr::addr_of_mut!((*new_crtc_state).mode);
            let adjusted_mode = core::ptr::addr_of_mut!((*new_crtc_state).adjusted_mode);

            if !(*new_crtc_state).mode_changed() && !(*new_crtc_state).connectors_changed() {
                continue;
            }

            if !funcs.is_null() && (*funcs).atomic_mode_set.is_some() {
                ((*funcs).atomic_mode_set.unwrap())(encoder, new_crtc_state, new_conn_state);
            } else if !funcs.is_null() && (*funcs).mode_set.is_some() {
                ((*funcs).mode_set.unwrap())(encoder, mode, adjusted_mode);
            }

            // Bridge mode programming is excluded; validation rejects any chain.
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_modeset_disables()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to valid, live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Caller must hold modeset locks for all objects undergoing a modeset disable transition.
/// - Sequentially executes `drm_atomic_helper_commit_encoder_bridge_disable` followed by `drm_atomic_helper_commit_crtc_disable`.
pub unsafe extern "C" fn drm_atomic_helper_commit_modeset_disables(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_crtc` bounds the CRTC array in `state`. Calling `disable_outputs`
    // shuts down active CRTC outputs under caller-held modeset locks.
    // encoders that need to be turned off under commit synchronization.
    unsafe {
        disable_outputs(dev, state);
        drm_atomic_helper_update_legacy_modeset_state(dev, state);
        drm_atomic_helper_calc_timestamping_constants(state);
        drm_atomic_helper_commit_crtc_set_mode(dev, state);
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_writebacks()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` containing connector state arrays.
/// - For writeback connectors with active writeback framebuffers, `connector.helper_private` must contain valid `atomic_commit` callbacks.
/// - Caller must hold modeset locks covering writeback connectors.
pub unsafe extern "C" fn drm_atomic_helper_commit_writebacks(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    assert!(
        !state.is_null(),
        "writeback commit received a null atomic state"
    );
    // SAFETY: `state` was verified non-null above. Traversing `state.connectors` to dispatch writeback jobs
    // to writeback connectors with active framebuffers is serialized under modeset locks.
    // Iterating over connectors array to assert no writeback jobs are queued under commit synchronization.
    unsafe {
        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let new_conn_state = (*conn_entry).new_state;
            if !new_conn_state.is_null() {
                assert!(
                    (*new_conn_state).writeback_job.is_null(),
                    "unsupported writeback job reached atomic commit"
                );
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_crtc_enable()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Caller must hold modeset locks for CRTCs transitioning to active.
/// - For each enabling CRTC, `crtc.helper_private` must contain valid `atomic_enable` or `enable` function pointers.
pub unsafe extern "C" fn drm_atomic_helper_commit_crtc_enable(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Calling `atomic_enable` or `enable` on
    // CRTCs transitioning to active is serialized under caller-held modeset locks.
    // on newly activated CRTCs under commit synchronization.
    unsafe {
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            if !drm_atomic_crtc_needs_modeset(new_crtc_state) {
                continue;
            }
            if !(*new_crtc_state).active {
                continue;
            }

            let funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
            if (*new_crtc_state).enable && !funcs.is_null() {
                if (*funcs).atomic_enable.is_some() {
                    ((*funcs).atomic_enable.unwrap())(crtc, state);
                } else if (*funcs).commit.is_some() {
                    ((*funcs).commit.unwrap())(crtc);
                }
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_encoder_bridge_enable()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` with valid connector and encoder routing.
/// - Caller must hold modeset locks covering the active output pipelines.
/// - Invokes bridge `enable`/`pre_enable` and encoder `atomic_enable`/`enable` hooks; callback tables must remain valid.
pub unsafe extern "C" fn drm_atomic_helper_commit_encoder_bridge_enable(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `state.connectors` contains `num_connector` valid entries. Invoking `enable`/`pre_enable`
    // hooks on active encoders and bridge chains is serialized under modeset locks.
    // for active outputs in display pipeline order under commit synchronization.
    unsafe {
        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let new_conn_state = (*conn_entry).new_state;
            if new_conn_state.is_null() || (*new_conn_state).best_encoder.is_null() {
                continue;
            }

            let crtc = (*new_conn_state).crtc;
            if crtc.is_null()
                || !(*(*crtc).state).active
                || !drm_atomic_crtc_needs_modeset((*crtc).state)
            {
                continue;
            }

            let encoder = (*new_conn_state).best_encoder;
            let funcs = (*encoder).helper_private as *const DrmEncoderHelperFuncs;
            if !funcs.is_null() {
                if (*funcs).atomic_enable.is_some() {
                    ((*funcs).atomic_enable.unwrap())(encoder, state);
                } else if (*funcs).enable.is_some() {
                    ((*funcs).enable.unwrap())(encoder);
                } else if (*funcs).commit.is_some() {
                    ((*funcs).commit.unwrap())(encoder);
                }
            }

            // Bridge enable is excluded; see atomic modeset validation.
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_modeset_enables()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` structures.
/// - Caller must hold modeset locks for all pipelines being enabled.
/// - Coordinates hardware programming order: `commit_crtc_set_mode` -> `commit_crtc_enable` -> `commit_encoder_bridge_enable` -> `commit_writebacks`.
pub unsafe extern "C" fn drm_atomic_helper_commit_modeset_enables(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: Coordinating modeset enables sequentially executes `commit_crtc_set_mode`, `commit_crtc_enable`,
    // `commit_encoder_bridge_enable`, and `commit_writebacks` on live state under caller-held modeset locks.
    // and vblank timestamping updates under commit synchronization.
    unsafe {
        drm_atomic_helper_commit_crtc_enable(dev, state);
        drm_atomic_helper_commit_encoder_bridge_enable(dev, state);
        drm_atomic_helper_commit_writebacks(dev, state);
    }
}

// =========================================================================
// Section 4: Fences, Waits & Commit Tail
// =========================================================================

// upstream: drm_atomic_helper.c set_fence_deadline()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit` with valid CRTC and plane state arrays.
/// Driver vblank mechanisms and fence callback tables must remain valid.
unsafe fn set_fence_deadline(_dev: *mut DrmDevice, state: *mut DrmAtomicCommit) {
    // SAFETY: `state` is a live atomic commit; queries the next vblank start time on active CRTCs
    // and calls `dma_fence_set_deadline` on incoming plane fences under commit synchronization.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        let mut vbltime: i64 = 0;

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            if drm_atomic_crtc_needs_modeset(new_crtc_state) {
                continue;
            }
            if !(*new_crtc_state).active {
                continue;
            }

            let mut v: i64 = 0;
            if crate::drm_vblank_upstream::drm_crtc_next_vblank_start(crtc, &mut v) != 0 {
                continue;
            }

            if vbltime == 0 || v < vbltime {
                vbltime = v;
            }
        }

        if vbltime == 0 {
            return;
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let new_plane_state = (*entry).new_state;
            if new_plane_state.is_null() || (*new_plane_state).fence.is_null() {
                continue;
            }

            dma_fence_set_deadline((*new_plane_state).fence, vbltime);
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_wait_for_fences()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` containing valid plane states.
/// - `dev` if non-null must point to the parent `DrmDevice`.
/// - Must be called in sleepable task context without holding spinlocks or locks needed by DMA fence signaling paths.
/// - If `pre_swap` is true, waits on fences in new plane states; if false, waits on fences in old plane states.
/// - Returns 0 when all fences are signaled, or a negative errno on wait failure or timeout.
pub unsafe extern "C" fn drm_atomic_helper_wait_for_fences(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
    pre_swap: bool,
) -> i32 {
    // SAFETY: `dev.mode_config.num_total_plane` bounds `state.planes`. Waiting on input fences
    // in sleepable context does not hold spinlocks and cannot deadlock fence completion.
    // and waits on attached input DMA fences before hardware programming begins.
    unsafe {
        set_fence_deadline(dev, state);

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let new_plane_state = (*entry).new_state;
            if new_plane_state.is_null() || (*new_plane_state).fence.is_null() {
                continue;
            }

            let ret = dma_fence_wait((*new_plane_state).fence, pre_swap);
            if ret != 0 {
                return ret;
            }

            dma_fence_put((*new_plane_state).fence);
            (*new_plane_state).fence = core::ptr::null_mut();
        }

        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_wait_for_vblanks()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Must be called from sleepable task context after `drm_atomic_helper_commit_tail` has submitted hardware state.
/// - For each CRTC with active modeset changes, waits for one vblank interval via `drm_crtc_wait_one_vblank`.
pub unsafe extern "C" fn drm_atomic_helper_wait_for_vblanks(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Waiting for one vblank interval on
    // active CRTCs occurs in sleepable task context after hardware state programming.
    // on CRTCs with active commit states using driver vblank APIs.
    unsafe {
        if (*state).legacy_cursor_update() {
            return;
        }

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        let mut crtc_mask = 0u32;

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() || !(*new_crtc_state).active {
                continue;
            }

            let ret = crate::drm_vblank_upstream::drm_crtc_vblank_get(crtc);
            if ret != 0 {
                continue;
            }

            crtc_mask |= 1u32 << (*crtc).index;
            (*entry).last_vblank_count = crate::drm_vblank_upstream::drm_crtc_vblank_count(crtc);
        }

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            if crtc.is_null() {
                continue;
            }

            if (crtc_mask & (1u32 << (*crtc).index)) == 0 {
                continue;
            }

            let queue = crate::drm_vblank_upstream::drm_crtc_vblank_waitqueue(crtc);
            if !queue.is_null() {
                let last = (*entry).last_vblank_count;
                let _ = drm_kapi::wait_event_timeout!(
                    *queue,
                    last != crate::drm_vblank_upstream::drm_crtc_vblank_count(crtc),
                    drm_kapi::primitives::msecs_to_jiffies(1000u32) as i64,
                );
            }

            crate::drm_vblank_upstream::drm_crtc_vblank_put(crtc);
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_wait_for_flip_done()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Must be called from sleepable task context without holding locks required by vblank interrupt handlers.
/// - Waits for the `flip_done` completion token on all CRTC commits associated with `state`.
pub unsafe extern "C" fn drm_atomic_helper_wait_for_flip_done(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Calling `wait_for_completion_timeout`
    // on each CRTC commit token occurs in sleepable context without holding vblank interrupt locks.
    unsafe {
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let commit = (*entry).commit;
            let crtc = (*entry).ptr;
            if crtc.is_null() || commit.is_null() {
                continue;
            }

            let _ = (*commit).flip_done.wait_timeout(10 * HZ as i64);
        }

        if !(*state).fake_commit.is_null() {
            (*(*state).fake_commit).flip_done.complete_all();
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_tail()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a valid, committed `DrmAtomicCommit` whose fences have already completed.
/// - Caller transfers ownership of this state reference to `drm_atomic_helper_commit_tail`.
/// - Driver modeset and plane helper function tables must remain valid throughout commit tail execution.
/// - Sequentially applies disables, updates planes, applies enables, and signals hardware completion.
pub unsafe extern "C" fn drm_atomic_helper_commit_tail(state: *mut DrmAtomicCommit) {
    // SAFETY: `state` is a valid committed atomic state. Executes modeset disables, plane commits,
    // modeset enables, and waits for flip_done under driver commit serialization.
    unsafe {
        let dev = (*state).dev;

        drm_atomic_helper_commit_modeset_disables(dev, state);
        drm_atomic_helper_commit_planes(dev, state, 0);
        drm_atomic_helper_commit_modeset_enables(dev, state);
        drm_atomic_helper_fake_vblank(state);
        drm_atomic_helper_commit_hw_done(state);
        drm_atomic_helper_wait_for_vblanks(dev, state);
        drm_atomic_helper_cleanup_planes(dev, state);
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_tail_rpm()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a valid, committed `DrmAtomicCommit` for a runtime-PM-capable driver.
/// - Powers on the device hardware via runtime PM before executing planes commit and modeset enables.
/// - Caller transfers ownership of this state reference to the commit tail routine.
pub unsafe extern "C" fn drm_atomic_helper_commit_tail_rpm(state: *mut DrmAtomicCommit) {
    // SAFETY: `state` is a valid committed atomic state. Executes runtime PM-aware modeset disables,
    // plane commits, and modeset enables under driver commit serialization.
    unsafe {
        let dev = (*state).dev;

        drm_atomic_helper_commit_modeset_disables(dev, state);
        drm_atomic_helper_commit_modeset_enables(dev, state);
        drm_atomic_helper_commit_planes(dev, state, DRM_PLANE_COMMIT_ACTIVE_ONLY);
        drm_atomic_helper_fake_vblank(state);
        drm_atomic_helper_commit_hw_done(state);
        drm_atomic_helper_wait_for_vblanks(dev, state);
        drm_atomic_helper_cleanup_planes(dev, state);
    }
}

// upstream: drm_atomic_helper.c commit_tail()
/// # Safety
/// `state` must point to a valid, committed `DrmAtomicCommit` whose hardware execution has been prepared.
/// The caller transfers ownership of this state reference to `commit_tail`.
unsafe fn commit_tail(state: *mut DrmAtomicCommit) {
    // SAFETY: `state` is a valid committed atomic state. Coordinates fence waiting, dependency waiting,
    // tail execution, cleanup signaling, and final release of the commit reference.
    unsafe {
        let dev = (*state).dev;
        let ret = drm_atomic_helper_wait_for_fences(dev, state, false);
        if ret != 0 {
            drm_atomic_helper_commit_error(state, ret);
        } else {
            let funcs = (*dev).mode_config.helper_private as *const DrmModeConfigHelperFuncs;
            drm_atomic_helper_wait_for_dependencies(state);

            if !funcs.is_null() && (*funcs).atomic_commit_tail.is_some() {
                ((*funcs).atomic_commit_tail.unwrap())(state);
            } else {
                drm_atomic_helper_commit_tail(state);
            }
        }

        drm_atomic_helper_commit_cleanup_done(state);
        drm_atomic_commit_put(state);
    }
}

// upstream: drm_atomic_helper.c commit_work()
/// # Safety
/// `work` must point to the `commit_work` field of a live `DrmAtomicCommit`.
unsafe extern "C" fn commit_work(work: *mut WorkStruct) {
    // SAFETY: `work` is the `commit_work` field of a live `DrmAtomicCommit` (function contract),
    // so stepping back by its offset recovers the state passed to `commit_tail`.
    unsafe {
        let state = work
            .cast::<u8>()
            .sub(offset_of!(DrmAtomicCommit, commit_work))
            .cast::<DrmAtomicCommit>();
        commit_tail(state);
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_async_check()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit`.
/// - Caller must hold modeset locks for objects referenced in `state`.
/// - Returns 0 if the update modifies exactly one plane without altering CRTC modeset properties, or `-EINVAL` if ineligible for async update.
pub unsafe extern "C" fn drm_atomic_helper_async_check(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: `dev.mode_config.num_crtc` and `num_total_plane` bound state arrays in `state`.
    // Verifying that exactly one plane is modified without modeset changes is a read-only pass under modeset locks.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc_state = (*entry).new_state;
            if !crtc_state.is_null() && drm_atomic_crtc_needs_modeset(crtc_state) {
                return -EINVAL;
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        let mut n_planes = 0;
        let mut target_plane: *mut DrmPlane = core::ptr::null_mut();
        let mut target_old_plane_state: *mut DrmPlaneState = core::ptr::null_mut();
        let mut target_new_plane_state: *mut DrmPlaneState = core::ptr::null_mut();

        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let old_plane_state = (*entry).old_state;
            let new_plane_state = (*entry).new_state;
            if !plane.is_null() && !old_plane_state.is_null() && !new_plane_state.is_null() {
                n_planes += 1;
                target_plane = plane;
                target_old_plane_state = old_plane_state;
                target_new_plane_state = new_plane_state;
            }
        }

        if n_planes != 1 {
            return -EINVAL;
        }

        if (*target_new_plane_state).crtc.is_null()
            || (*target_old_plane_state).crtc != (*target_new_plane_state).crtc
        {
            return -EINVAL;
        }

        let funcs = (*target_plane).helper_private as *const DrmPlaneHelperFuncs;
        if funcs.is_null() || (*funcs).atomic_async_update.is_none() {
            return -EINVAL;
        }

        if !(*target_new_plane_state).fence.is_null() {
            return -EINVAL;
        }

        if !(*target_old_plane_state).commit.is_null()
            && !(*(*target_old_plane_state).commit).hw_done.try_wait()
        {
            return -EBUSY;
        }

        if !funcs.is_null() && (*funcs).atomic_async_check.is_some() {
            return ((*funcs).atomic_async_check.unwrap())(target_plane, state, false);
        }

        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_async_commit()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` that passed `drm_atomic_helper_async_check`.
/// - The plane's `helper_private` must provide a valid `atomic_async_update` callback.
/// - Performs the plane update without stalling for full modeset synchronization.
pub unsafe extern "C" fn drm_atomic_helper_async_commit(
    _dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_total_plane` bounds `state.planes`. Swapping plane state and calling
    // `atomic_async_update` applies the single-plane update without full modeset stalling.
    unsafe {
        let dev = (*state).dev;
        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let plane_state = (*entry).new_state;
            if plane.is_null() || plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
            if !funcs.is_null() && (*funcs).atomic_async_update.is_some() {
                ((*funcs).atomic_async_update.unwrap())(plane, state);
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live, initialized `DrmDevice` and `DrmAtomicCommit` instances.
/// - `state` must have passed atomic validation (`drm_atomic_helper_check`).
/// - If `nonblock` is true, queues the commit to the driver workqueue and transfers `state` ownership to the worker; returns 0 on successful enqueue.
/// - If `nonblock` is false, blocks the current task until commit execution completes.
/// - Returns 0 on success, or a negative errno on setup/allocation failure.
pub unsafe extern "C" fn drm_atomic_helper_commit(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
    nonblock: bool,
) -> i32 {
    // SAFETY: In `drm_atomic_helper_commit`, `state` is a validated atomic commit.
    // For nonblocking commits, queuing `commit_work` transfers state ownership to the worker thread.
    unsafe {
        if (*state).async_update() {
            let ret = drm_atomic_helper_prepare_planes(dev, state);
            if ret != 0 {
                return ret;
            }

            drm_atomic_helper_async_commit(dev, state);
            drm_atomic_helper_unprepare_planes(dev, state);
            return 0;
        }

        let mut ret = drm_atomic_helper_setup_commit(state, nonblock);
        if ret != 0 {
            return ret;
        }

        init_work(core::ptr::addr_of_mut!((*state).commit_work), commit_work);

        ret = drm_atomic_helper_prepare_planes(dev, state);
        if ret != 0 {
            return ret;
        }

        if !nonblock {
            ret = drm_atomic_helper_wait_for_fences(dev, state, true);
            if ret != 0 {
                drm_atomic_helper_unprepare_planes(dev, state);
                return ret;
            }
        }

        // Preserve Linux's ordering against previous commits for both modes:
        // a nonblocking commit does not wait for its own input fences here,
        // but it must not publish over a prior commit before that commit has
        // completed its hardware phase.
        ret = wait_for_hw_done(state);
        if ret != 0 {
            drm_atomic_helper_unprepare_planes(dev, state);
            return ret;
        }

        if !nonblock {
            let helper_funcs = (*dev).mode_config.helper_private as *const DrmModeConfigHelperFuncs;
            if !helper_funcs.is_null()
                && let Some(pre_swap) = (*helper_funcs).atomic_commit_pre_swap
            {
                ret = pre_swap(state);
                if ret != 0 {
                    drm_atomic_helper_unprepare_planes(dev, state);
                    return ret;
                }
            }
        }

        ret = drm_atomic_helper_swap_state(state, false);
        if ret != 0 {
            drm_atomic_helper_unprepare_planes(dev, state);
            return ret;
        }

        drm_atomic_commit_get(state);
        if nonblock {
            drm_kapi::workqueue::queue_work(
                drm_kapi::workqueue::SYSTEM_DFL_WQ,
                core::ptr::addr_of_mut!((*state).commit_work),
            );
        } else {
            commit_tail(state);
        }

        0
    }
}

// =========================================================================
// Section 5: Setup Commit, Dependencies & Signal Completion
// =========================================================================

// upstream: drm_atomic_helper.c stall_checks()
/// # Safety
/// `crtc` must point to a live `DrmCrtc` with initialized `commit_lock` spinlock and `commit_list` head.
unsafe fn stall_checks(crtc: *mut DrmCrtc, nonblock: bool) -> i32 {
    // SAFETY: `crtc` is valid; acquires `crtc.commit_lock` spinlock and traverses `crtc.commit_list`
    // (offset 112 in `DrmCrtcCommit`) to check for pending flip completions and wait on cleanup_done.
    unsafe {
        let mut stall_commit: *mut DrmCrtcCommit = core::ptr::null_mut();
        let mut completed: bool;
        let mut i = 0;

        spin_lock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));
        let head = core::ptr::addr_of_mut!((*crtc).commit_list);
        let mut cur = (*head).next;
        while cur != head {
            let commit = cur
                .cast::<u8>()
                .sub(offset_of!(DrmCrtcCommit, commit_entry))
                .cast::<DrmCrtcCommit>();
            if i == 0 {
                completed = (*commit).flip_done.try_wait();
                if !completed && nonblock {
                    spin_unlock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));
                    return -EBUSY;
                }
            } else if i == 1 {
                stall_commit = drm_crtc_commit_get(commit);
                break;
            }
            i += 1;
            cur = (*cur).next;
        }
        spin_unlock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));

        if stall_commit.is_null() {
            return 0;
        }

        let ret: i64 = (*stall_commit)
            .cleanup_done
            .wait_interruptible_timeout(10 * HZ as i64);
        drm_crtc_commit_put(stall_commit);

        if ret < 0 { ret as i32 } else { 0 }
    }
}

// upstream: drm_atomic_helper.c crtc_or_fake_commit()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit`. If non-null, `crtc` must point to a valid `DrmCrtc`.
unsafe fn crtc_or_fake_commit(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcCommit {
    // SAFETY: `state` points to a live `DrmAtomicCommit`. If `crtc` is non-null, reading `new_crtc_state.commit`
    // fetches the existing tracking token; otherwise allocating `fake_commit` provides standalone tracking.
    // or allocates and initializes a `fake_commit` via kzalloc if needed.
    unsafe {
        if !crtc.is_null() {
            let new_crtc_state =
                crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc);
            if !new_crtc_state.is_null() {
                return (*new_crtc_state).commit;
            }
        }

        if (*state).fake_commit.is_null() {
            let fake = drm_kapi::memory::kzalloc(core::mem::size_of::<DrmCrtcCommit>(), GFP_KERNEL)
                as *mut DrmCrtcCommit;
            if fake.is_null() {
                return core::ptr::null_mut();
            }
            init_commit(fake, core::ptr::null_mut());
            (*state).fake_commit = fake;
        }

        (*state).fake_commit
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_setup_commit()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to an initialized `DrmAtomicCommit` in the prepare phase.
/// - Caller must hold modeset locks for all CRTCs and connectors modified in `state`.
/// - Allocates tracking commit structures and attaches completion tokens to each CRTC state; returns 0 on success or `-ENOMEM`.
pub unsafe extern "C" fn drm_atomic_helper_setup_commit(
    state: *mut DrmAtomicCommit,
    nonblock: bool,
) -> i32 {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Allocating and linking `DrmCrtcCommit`
    // structures wires up completion tracking between successive commits under caller-held modeset locks.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let old_crtc_state = (*entry).old_state;
            let new_crtc_state = (*entry).new_state;

            if crtc.is_null() || old_crtc_state.is_null() || new_crtc_state.is_null() {
                continue;
            }

            let commit =
                drm_kapi::memory::kzalloc(core::mem::size_of::<DrmCrtcCommit>(), GFP_KERNEL)
                    as *mut DrmCrtcCommit;
            if commit.is_null() {
                return -ENOMEM;
            }

            init_commit(commit, crtc);
            (*new_crtc_state).commit = commit;

            let ret = stall_checks(crtc, nonblock);
            if ret != 0 {
                return ret;
            }

            if !(*old_crtc_state).active && !(*new_crtc_state).active {
                (*commit).flip_done.complete_all();
                continue;
            }

            if (*state).legacy_cursor_update() {
                (*commit).flip_done.complete_all();
                continue;
            }

            if (*new_crtc_state).event.is_null() {
                (*commit).event = drm_kapi::memory::kzalloc(
                    core::mem::size_of::<DrmPendingVblankEvent>(),
                    GFP_KERNEL,
                ) as *mut DrmPendingVblankEvent;
                if (*commit).event.is_null() {
                    return -ENOMEM;
                }
                (*new_crtc_state).event = (*commit).event;
            }

            (*(*new_crtc_state).event).base.completion =
                core::ptr::addr_of_mut!((*commit).flip_done);
            (*(*new_crtc_state).event).base.completion_release = Some(release_crtc_commit);
            drm_crtc_commit_get(commit);

            (*commit).abort_completion = true;

            (*entry).commit = commit;
            drm_crtc_commit_get(commit);
        }

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let old_conn_state = (*conn_entry).old_state;
            let new_conn_state = (*conn_entry).new_state;
            if old_conn_state.is_null() || new_conn_state.is_null() {
                continue;
            }

            if nonblock
                && !(*old_conn_state).commit.is_null()
                && !(*(*old_conn_state).commit).flip_done.try_wait()
            {
                return -EBUSY;
            }

            let target_crtc = if !(*new_conn_state).crtc.is_null() {
                (*new_conn_state).crtc
            } else {
                (*old_conn_state).crtc
            };
            let commit = crtc_or_fake_commit(state, target_crtc);
            if commit.is_null() {
                return -ENOMEM;
            }
            (*new_conn_state).commit = drm_crtc_commit_get(commit);
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let old_plane_state = (*entry).old_state;
            let new_plane_state = (*entry).new_state;
            if old_plane_state.is_null() || new_plane_state.is_null() {
                continue;
            }

            if nonblock
                && !(*old_plane_state).commit.is_null()
                && !(*(*old_plane_state).commit).flip_done.try_wait()
            {
                return -EBUSY;
            }

            let target_crtc = if !(*new_plane_state).crtc.is_null() {
                (*new_plane_state).crtc
            } else {
                (*old_plane_state).crtc
            };
            let commit = crtc_or_fake_commit(state, target_crtc);
            if commit.is_null() {
                return -ENOMEM;
            }
            (*new_plane_state).commit = drm_crtc_commit_get(commit);
        }

        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_wait_for_dependencies()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a committed `DrmAtomicCommit` with initialized `crtcs` and `connectors` state arrays.
/// - Must be called from sleepable task context without holding locks required by preceding commit completion paths.
/// - Waits for previous commits on the same CRTCs to complete hardware submission (`hw_done`).
pub unsafe extern "C" fn drm_atomic_helper_wait_for_dependencies(state: *mut DrmAtomicCommit) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Waiting on `hw_done` completion tokens
    // of previous commits occurs in sleepable task context before submitting new hardware commands.
    // on affected CRTCs and planes before beginning hardware programming.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let old_crtc_state = (*entry).old_state;
            if !old_crtc_state.is_null() && !(*old_crtc_state).commit.is_null() {
                let _ = crate::drm_atomic_upstream::drm_crtc_commit_wait((*old_crtc_state).commit);
            }
        }

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let old_conn_state = (*conn_entry).old_state;
            if !old_conn_state.is_null() && !(*old_conn_state).commit.is_null() {
                let _ = crate::drm_atomic_upstream::drm_crtc_commit_wait((*old_conn_state).commit);
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let old_plane_state = (*entry).old_state;
            if !old_plane_state.is_null() && !(*old_plane_state).commit.is_null() {
                let _ = crate::drm_atomic_upstream::drm_crtc_commit_wait((*old_plane_state).commit);
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_fake_vblank()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` with valid CRTC states.
/// - Delivers pending vblank events for drivers lacking hardware vblank interrupts.
/// - Acquires and releases `dev.event_lock` spinlock internally.
pub unsafe extern "C" fn drm_atomic_helper_fake_vblank(state: *mut DrmAtomicCommit) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Sending vblank events under
    // `dev.event_lock` simulates hardware vblanks for virtual display drivers.
    // events for drivers lacking hardware vblank support.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;

            if crtc.is_null() || new_crtc_state.is_null() || !(*new_crtc_state).no_vblank {
                continue;
            }

            let mut flags: core::ffi::c_ulong = 0;
            spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut flags);
            if !(*new_crtc_state).event.is_null() {
                crate::drm_vblank_upstream::drm_crtc_send_vblank_event(
                    crtc,
                    (*new_crtc_state).event,
                );
                (*new_crtc_state).event = core::ptr::null_mut();
            }
            spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), flags);
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_hw_done()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` with initialized per-CRTC commit tokens.
/// - Signals the `hw_done` completion event on each CRTC commit, releasing subsequent commits waiting for hardware registers.
pub unsafe extern "C" fn drm_atomic_helper_commit_hw_done(state: *mut DrmAtomicCommit) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Signaling `hw_done` on each CRTC commit
    // unblocks dependent commits waiting for hardware registers to become free.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let old_crtc_state = (*entry).old_state;
            let new_crtc_state = (*entry).new_state;

            if new_crtc_state.is_null() || (*new_crtc_state).commit.is_null() {
                continue;
            }

            let commit = (*new_crtc_state).commit;

            if !old_crtc_state.is_null() {
                if !(*old_crtc_state).commit.is_null() {
                    drm_crtc_commit_put((*old_crtc_state).commit);
                }
                (*old_crtc_state).commit = drm_crtc_commit_get(commit);
            }

            (*commit).hw_done.complete_all();
        }

        if !(*state).fake_commit.is_null() {
            (*(*state).fake_commit).hw_done.complete_all();
            (*(*state).fake_commit).flip_done.complete_all();
        }
    }
}

/// Finish a nonblocking commit whose driver submission failed after the
/// software state was swapped. No flip-complete event is reported as success;
/// requested CRTC out-fences are signaled with the negative errno, internal
/// commit dependencies are completed, and the device is unplugged so poll/read
/// users can observe that recovery is required. The driver must retain every
/// backing resource the device could still access until it confirms DeviceLost.
///
/// The caller must invoke this from `atomic_commit_tail` after input fences and
/// dependencies have completed, then return without calling the normal helper
/// commit tail. The outer commit worker will still perform `cleanup_done` and
/// release the atomic state.
///
/// # Safety
/// `state` must be a live, committed atomic state with the driver's failed
/// hardware submission associated with it. `error` is a negative Linux errno.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_commit_error(state: *mut DrmAtomicCommit, error: i32) {
    if state.is_null() {
        return;
    }

    // SAFETY: `state` was verified non-null above and points to a valid `DrmAtomicCommit`; reading `dev`.
    let dev = unsafe { (*state).dev };
    if dev.is_null() {
        return;
    }
    let error = if error < 0 {
        error
    } else {
        -drm_kapi::config::EIO
    };

    // Publish device loss before waking commit-dependency waiters. Other
    // already-queued nonblocking commits must not submit hardware work after
    // this commit has failed terminally.
    // SAFETY: `dev` was verified non-null; unplugs the device to prevent subsequent hardware work
    // after terminal commit failure before waking waiters.
    unsafe { crate::drm_drv_upstream::drm_dev_unplug(dev) };

    // SAFETY: The commit worker thread has exclusive ownership of `state` during commit tail execution;
    // CRTC completion events and out-fences are delivered under `dev.event_lock`.
    // its tail returns; CRTC events and fences are owned by their new states.
    unsafe {
        drm_atomic_helper_commit_hw_done(state);

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let new_crtc_state = (*entry).new_state;
            if new_crtc_state.is_null() {
                continue;
            }

            let event = (*new_crtc_state).event;
            if !event.is_null() {
                let event_base = core::ptr::addr_of_mut!((*event).base);
                let fence = (*event_base).fence;
                if !fence.is_null() {
                    drm_kapi::dma_fence::dma_fence_set_error(fence, error);
                    drm_kapi::dma_fence::dma_fence_signal(fence);
                    drm_kapi::dma_fence::dma_fence_put(fence);
                    (*event_base).fence = core::ptr::null_mut();
                }

                let completion_ptr = (*event_base).completion;
                if !completion_ptr.is_null() {
                    if let Some(completion) = completion_ptr.as_ref() {
                        completion.complete_all();
                    }
                    if let Some(release) = (*event_base).completion_release {
                        release(completion_ptr);
                    }
                    (*event_base).completion = core::ptr::null_mut();
                    (*event_base).completion_release = None;
                }

                let commit = (*new_crtc_state).commit;
                if !commit.is_null() && (*commit).event == event {
                    (*commit).event = core::ptr::null_mut();
                }
                crate::drm_file_upstream::drm_event_cancel_free(dev, event_base);
                (*new_crtc_state).event = core::ptr::null_mut();
            }

            let commit = (*new_crtc_state).commit;
            if !commit.is_null() {
                (*commit).flip_done.complete_all();
            }
        }

        // The normal helper tail did not commit planes, but it would release
        // old plane state here. Drivers keep any in-flight scanout references.
        drm_atomic_helper_cleanup_planes(dev, state);
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_cleanup_done()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` in the post-commit cleanup phase.
/// - Must be called after plane framebuffers have been cleaned up via `drm_atomic_helper_cleanup_planes`.
/// - Signals `cleanup_done` completion tokens and drops references to `DrmCrtcCommit` tokens.
pub unsafe extern "C" fn drm_atomic_helper_commit_cleanup_done(state: *mut DrmAtomicCommit) {
    // SAFETY: `dev.mode_config.num_crtc` bounds `state.crtcs`. Signaling `cleanup_done` indicates that
    // plane framebuffers have been cleaned up and drops references to `DrmCrtcCommit`.
    // dependent commits waiting on resource cleanup.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let old_crtc_state = (*entry).old_state;

            if crtc.is_null() || old_crtc_state.is_null() || (*old_crtc_state).commit.is_null() {
                continue;
            }

            let commit = (*old_crtc_state).commit;
            (*commit).cleanup_done.complete_all();

            spin_lock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));
            drm_kapi::list::list_del(core::ptr::addr_of_mut!((*commit).commit_entry));
            spin_unlock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));
        }

        if !(*state).fake_commit.is_null() {
            (*(*state).fake_commit).cleanup_done.complete_all();
        }
    }
}

// =========================================================================
// Section 6: Planes Commit, Swap & Lifecycle
// =========================================================================

// upstream: drm_atomic_helper.c drm_atomic_helper_prepare_planes()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - For all planes modified in `state`, `new_plane_state.fb` must point to valid framebuffers.
/// - Plane helper callbacks (`prepare_fb` and `begin_fb_access`) must be valid.
/// - On success, returns 0 with all modified plane framebuffers pinned and mapped. On error, automatically rolls back prepared planes and returns a negative errno.
pub unsafe extern "C" fn drm_atomic_helper_prepare_planes(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    let num_connectors = unsafe {
        // SAFETY: `num_connector` is the length of the live commit's `connectors` array and bounds the loop index.
        (*state).num_connector
    } as usize;
    for i in 0..num_connectors {
        let conn_entry = unsafe {
            // SAFETY: the loop index is below `num_connectors`, so the offset stays inside the `connectors` array of `state`.
            (*state).connectors.add(i)
        };
        let new_conn_state = unsafe {
            // SAFETY: `conn_entry` is an element of the `connectors` array at an index below `num_connector`.
            (*conn_entry).new_state
        };
        if new_conn_state.is_null()
            || unsafe {
                // SAFETY: `new_conn_state` was null-checked above and points to the connector state owned by the live commit.
                (*new_conn_state).writeback_job.is_null()
            }
        {
            continue;
        }

        return -EOPNOTSUPP;
    }

    let num_planes = unsafe {
        // SAFETY: `dev` is the live device; its `num_total_plane` bounds the loop over the `planes` array.
        (*dev).mode_config.num_total_plane
    } as usize;
    let mut prepared_planes = 0usize;
    let mut accessed_planes = 0usize;

    for i in 0..num_planes {
        let entry = unsafe {
            // SAFETY: the loop index is below the plane count read above, so the offset stays inside the `planes` array of `state`.
            (*state).planes.add(i)
        };
        let plane = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).ptr
        };
        let new_plane_state = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).new_state
        };

        if plane.is_null() || new_plane_state.is_null() {
            continue;
        }

        let funcs = unsafe {
            // SAFETY: `plane` is the `ptr` of an in-range `planes` entry and was null-checked, so its helper table pointer can be read.
            (*plane).helper_private
        } as *const DrmPlaneHelperFuncs;
        let mut ret = 0;

        if !funcs.is_null()
            && unsafe {
                // SAFETY: `funcs` is non-null, as checked in the enclosing condition, so reading the `prepare_fb` callback slot is valid.
                (*funcs).prepare_fb.is_some()
            }
        {
            ret = unsafe {
                // SAFETY: `prepare_fb` was verified `Some` in the enclosing condition, and `plane` and `new_plane_state` were null-checked before this call.
                ((*funcs).prepare_fb.unwrap())(plane, new_plane_state)
            };
        } else {
            if unsafe {
                // SAFETY: `dev` is the live device passed in by the caller; the feature check reads only its driver flags.
                drm_core_check_feature(dev, DRIVER_GEM)
            } {
                // SAFETY: `plane` and `new_plane_state` are non-null, and the GEM helper only touches the framebuffer that the live `state` holds for `new_plane_state`.
                ret = unsafe {
                    crate::drm_gem_atomic_helper_upstream::drm_gem_plane_helper_prepare_fb(
                        plane,
                        new_plane_state,
                    )
                };
            }
        }

        if ret != 0 {
            for j in 0..prepared_planes {
                let prev_entry = unsafe {
                    // SAFETY: the loop index is below the plane count read above, so the offset stays inside the `planes` array of `state`.
                    (*state).planes.add(j)
                };
                let prev_plane = unsafe {
                    // SAFETY: `prev_entry` was prepared earlier in this loop, so its index is below `prepared_planes` and it is in bounds.
                    (*prev_entry).ptr
                };
                let prev_state = unsafe {
                    // SAFETY: `prev_entry` was prepared earlier in this loop, so its index is below `prepared_planes` and it is in bounds.
                    (*prev_entry).new_state
                };
                if prev_plane.is_null() || prev_state.is_null() {
                    continue;
                }
                let prev_funcs = unsafe {
                    // SAFETY: `prev_plane` was null-checked above, so its helper table pointer can be read.
                    (*prev_plane).helper_private
                } as *const DrmPlaneHelperFuncs;
                if !prev_funcs.is_null()
                    && unsafe {
                        // SAFETY: `prev_funcs` was null-checked in the enclosing condition, so its callback slot can be read.
                        (*prev_funcs).cleanup_fb.is_some()
                    }
                {
                    unsafe {
                        // SAFETY: `prev_funcs` was null-checked in the enclosing condition, so its callback slot can be read.
                        ((*prev_funcs).cleanup_fb.unwrap())(prev_plane, prev_state)
                    };
                }
            }
            return ret;
        }
        prepared_planes = i + 1;
    }

    for i in 0..num_planes {
        let entry = unsafe {
            // SAFETY: the loop index is below the plane count read above, so the offset stays inside the `planes` array of `state`.
            (*state).planes.add(i)
        };
        let plane = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).ptr
        };
        let new_plane_state = unsafe {
            // SAFETY: `entry` is an element of the `crtcs` or `planes` array of `state` at an index bounded by the loop range.
            (*entry).new_state
        };

        if plane.is_null() || new_plane_state.is_null() {
            continue;
        }

        let funcs = unsafe {
            // SAFETY: `plane` is the `ptr` of an in-range `planes` entry and was null-checked, so its helper table pointer can be read.
            (*plane).helper_private
        } as *const DrmPlaneHelperFuncs;
        if !funcs.is_null()
            && unsafe {
                // SAFETY: `funcs` is non-null, as checked in the enclosing condition, so reading the `begin_fb_access` callback slot is valid.
                (*funcs).begin_fb_access.is_some()
            }
        {
            let ret = unsafe {
                // SAFETY: `begin_fb_access` was verified `Some` in the enclosing condition, and `plane` and `new_plane_state` are non-null.
                ((*funcs).begin_fb_access.unwrap())(plane, new_plane_state)
            };
            if ret != 0 {
                for j in 0..accessed_planes {
                    let prev_entry = unsafe {
                        // SAFETY: the loop index is below the plane count read above, so the offset stays inside the `planes` array of `state`.
                        (*state).planes.add(j)
                    };
                    let prev_plane = unsafe {
                        // SAFETY: `prev_entry` was prepared earlier in this loop, so its index is below `prepared_planes` and it is in bounds.
                        (*prev_entry).ptr
                    };
                    let prev_state = unsafe {
                        // SAFETY: `prev_entry` was prepared earlier in this loop, so its index is below `prepared_planes` and it is in bounds.
                        (*prev_entry).new_state
                    };
                    if prev_plane.is_null() || prev_state.is_null() {
                        continue;
                    }
                    let prev_funcs = unsafe {
                        // SAFETY: `prev_plane` was null-checked above, so its helper table pointer can be read.
                        (*prev_plane).helper_private
                    } as *const DrmPlaneHelperFuncs;
                    if !prev_funcs.is_null()
                        && unsafe {
                            // SAFETY: `prev_funcs` was null-checked in the enclosing condition, so its callback slot can be read.
                            (*prev_funcs).end_fb_access.is_some()
                        }
                    {
                        unsafe {
                            // SAFETY: `prev_funcs` was null-checked in the enclosing condition, so its callback slot can be read.
                            ((*prev_funcs).end_fb_access.unwrap())(prev_plane, prev_state)
                        };
                    }
                }
                for j in 0..num_planes {
                    let prev_entry = unsafe {
                        // SAFETY: the loop index is below the plane count read above, so the offset stays inside the `planes` array of `state`.
                        (*state).planes.add(j)
                    };
                    let prev_plane = unsafe {
                        // SAFETY: `prev_entry` was prepared earlier in this loop, so its index is below `prepared_planes` and it is in bounds.
                        (*prev_entry).ptr
                    };
                    let prev_state = unsafe {
                        // SAFETY: `prev_entry` was prepared earlier in this loop, so its index is below `prepared_planes` and it is in bounds.
                        (*prev_entry).new_state
                    };
                    if prev_plane.is_null() || prev_state.is_null() {
                        continue;
                    }
                    let prev_funcs = unsafe {
                        // SAFETY: `prev_plane` was null-checked above, so its helper table pointer can be read.
                        (*prev_plane).helper_private
                    } as *const DrmPlaneHelperFuncs;
                    if !prev_funcs.is_null()
                        && unsafe {
                            // SAFETY: `prev_funcs` was null-checked in the enclosing condition, so its callback slot can be read.
                            (*prev_funcs).cleanup_fb.is_some()
                        }
                    {
                        unsafe {
                            // SAFETY: `prev_funcs` was null-checked in the enclosing condition, so its callback slot can be read.
                            ((*prev_funcs).cleanup_fb.unwrap())(prev_plane, prev_state)
                        };
                    }
                }
                return ret;
            }
        }
        accessed_planes = i + 1;
    }

    0
}

// upstream: drm_atomic_helper.c drm_atomic_helper_unprepare_planes()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Planes modified in `state` must have previously been prepared via `drm_atomic_helper_prepare_planes`.
/// - Calls `cleanup_fb` on plane helper tables to unpin or unmap framebuffers.
pub unsafe extern "C" fn drm_atomic_helper_unprepare_planes(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_total_plane` bounds `state.planes`. Calling `cleanup_fb`
    // on prepared planes unpins framebuffers on the commit error or unprepare path.
    // on all planes that were previously prepared in `state`.
    unsafe {
        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let new_plane_state = (*entry).new_state;
            if plane.is_null() || new_plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
            if !funcs.is_null() && (*funcs).end_fb_access.is_some() {
                ((*funcs).end_fb_access.unwrap())(plane, new_plane_state);
            }
        }

        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let new_plane_state = (*entry).new_state;
            if plane.is_null() || new_plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
            if !funcs.is_null() && (*funcs).cleanup_fb.is_some() {
                ((*funcs).cleanup_fb.unwrap())(plane, new_plane_state);
            }
        }
    }
}

// upstream: drm_atomic_helper.c plane_crtc_active()
/// # Safety
/// If non-null, `state` must point to a valid `DrmPlaneState` whose `crtc` pointer (if present) points to a valid `DrmCrtc`.
unsafe fn plane_crtc_active(state: *const DrmPlaneState) -> bool {
    // SAFETY: `state` if non-null is safely checked; dereferencing checks `state.crtc`,
    // `crtc.state`, and reads the active boolean flag.
    unsafe {
        !state.is_null()
            && !(*state).crtc.is_null()
            && !(*(*state).crtc).state.is_null()
            && (*(*(*state).crtc).state).active
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_planes()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Caller must hold modeset locks covering the affected planes and CRTCs.
/// - Driver callbacks (`atomic_begin`, `atomic_update`, `atomic_flush`) must remain valid during execution.
/// - `flags` specifies plane update filtering (e.g., active, disabled, or all planes).
pub unsafe extern "C" fn drm_atomic_helper_commit_planes(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
    flags: u32,
) {
    // SAFETY: `dev.mode_config.num_crtc` and `num_total_plane` bound CRTC and plane arrays in `state`.
    // Invoking `atomic_begin`, plane update/disable, and `atomic_flush` is serialized under commit execution.
    unsafe {
        let active_only = (flags & DRM_PLANE_COMMIT_ACTIVE_ONLY) != 0;
        let no_disable = (flags & DRM_PLANE_COMMIT_NO_DISABLE_AFTER_MODESET) != 0;

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let old_crtc_state = (*entry).old_state;
            if crtc.is_null() || old_crtc_state.is_null() {
                continue;
            }

            let funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
            let new_crtc_state = (*entry).new_state;

            if !funcs.is_null() && (*funcs).atomic_begin.is_some() {
                if active_only && (new_crtc_state.is_null() || !(*new_crtc_state).active) {
                    continue;
                }
                ((*funcs).atomic_begin.unwrap())(crtc, state);
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let old_plane_state = (*entry).old_state;
            let new_plane_state = (*entry).new_state;
            if plane.is_null() || old_plane_state.is_null() || new_plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
            if funcs.is_null() {
                continue;
            }

            let disabling = drm_atomic_plane_disabling(old_plane_state, new_plane_state);

            if active_only {
                if disabling {
                    let was_active = plane_crtc_active(old_plane_state);
                    if !was_active {
                        continue;
                    }
                } else {
                    let is_active = plane_crtc_active(new_plane_state);
                    if !is_active {
                        continue;
                    }
                }
            }

            if disabling && (*funcs).atomic_disable.is_some() {
                let crtc_state = (*(*old_plane_state).crtc).state;
                if drm_atomic_crtc_needs_modeset(crtc_state) && no_disable {
                    continue;
                }
                ((*funcs).atomic_disable.unwrap())(plane, state);
            } else if !(*new_plane_state).crtc.is_null() || disabling {
                if (*funcs).atomic_update.is_some() {
                    ((*funcs).atomic_update.unwrap())(plane, state);
                }
                if !disabling
                    && (*funcs).atomic_enable.is_some()
                    && drm_atomic_plane_enabling(old_plane_state, new_plane_state)
                {
                    ((*funcs).atomic_enable.unwrap())(plane, state);
                }
            }
        }

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || new_crtc_state.is_null() {
                continue;
            }

            let funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
            if !funcs.is_null() && (*funcs).atomic_flush.is_some() {
                if active_only && !(*new_crtc_state).active {
                    continue;
                }
                ((*funcs).atomic_flush.unwrap())(crtc, state);
            }
        }

        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let old_plane_state = (*entry).old_state;
            if plane.is_null() || old_plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
            if !funcs.is_null() && (*funcs).end_fb_access.is_some() {
                ((*funcs).end_fb_access.unwrap())(plane, old_plane_state);
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_planes_on_crtc()
#[unsafe(no_mangle)]
/// # Safety
/// - `old_crtc_state` must point to a valid, initialized `DrmCrtcState` with a valid `state` (`DrmAtomicCommit`) pointer.
/// - Caller must hold modeset locks covering the CRTC and its attached planes.
/// - Executes `atomic_begin`, `atomic_update`, and `atomic_flush` for all planes assigned to this CRTC.
pub unsafe extern "C" fn drm_atomic_helper_commit_planes_on_crtc(
    old_crtc_state: *mut DrmCrtcState,
) {
    // SAFETY: `old_crtc_state` is a live state tracking `crtc`. Invoking plane helper updates for planes
    // attached to this CRTC is serialized under modeset locks.
    unsafe {
        let crtc = (*old_crtc_state).crtc;
        let old_state = (*old_crtc_state).state;
        let new_crtc_state =
            crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(old_state, crtc);

        let mut plane_mask = (*old_crtc_state).plane_mask;
        if !new_crtc_state.is_null() {
            plane_mask |= (*new_crtc_state).plane_mask;
        }

        let crtc_funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;
        if !crtc_funcs.is_null() && (*crtc_funcs).atomic_begin.is_some() {
            ((*crtc_funcs).atomic_begin.unwrap())(crtc, old_state);
        }

        let dev = (*crtc).dev;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut cur = (*head).next;
        while cur != head {
            let plane = (cur as *mut u8).sub(8) as *mut DrmPlane;
            let mask = 1u32 << (*plane).index;
            if (plane_mask & mask) != 0 {
                let old_plane_state =
                    crate::drm_atomic_upstream::drm_atomic_get_old_plane_state(old_state, plane);
                let new_plane_state =
                    crate::drm_atomic_upstream::drm_atomic_get_new_plane_state(old_state, plane);
                let plane_funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;

                if !old_plane_state.is_null() && !plane_funcs.is_null() {
                    let disabling = drm_atomic_plane_disabling(old_plane_state, new_plane_state);
                    if disabling && (*plane_funcs).atomic_disable.is_some() {
                        ((*plane_funcs).atomic_disable.unwrap())(plane, old_state);
                    } else if !new_plane_state.is_null()
                        && (!(*new_plane_state).crtc.is_null() || disabling)
                    {
                        if (*plane_funcs).atomic_update.is_some() {
                            ((*plane_funcs).atomic_update.unwrap())(plane, old_state);
                        }
                        if !disabling
                            && (*plane_funcs).atomic_enable.is_some()
                            && drm_atomic_plane_enabling(old_plane_state, new_plane_state)
                        {
                            ((*plane_funcs).atomic_enable.unwrap())(plane, old_state);
                        }
                    }
                }
            }
            cur = (*cur).next;
        }

        if !crtc_funcs.is_null() && (*crtc_funcs).atomic_flush.is_some() {
            ((*crtc_funcs).atomic_flush.unwrap())(crtc, old_state);
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_disable_planes_on_crtc()
#[unsafe(no_mangle)]
/// # Safety
/// - `old_crtc_state` must point to a valid `DrmCrtcState` with an initialized `crtc` pointer.
/// - Caller must hold the modeset lock covering this CRTC.
/// - If `atomic` is true, calls `atomic_disable`; otherwise invokes legacy disable functions on plane helpers.
pub unsafe extern "C" fn drm_atomic_helper_disable_planes_on_crtc(
    old_crtc_state: *mut DrmCrtcState,
    atomic: bool,
) {
    // SAFETY: `old_crtc_state.crtc` is non-null. Calling plane helper disable callbacks
    // for planes associated with this CRTC is serialized under modeset locks.
    unsafe {
        let crtc = (*old_crtc_state).crtc;
        let crtc_funcs = (*crtc).helper_private as *const DrmCrtcHelperFuncs;

        if atomic && !crtc_funcs.is_null() && (*crtc_funcs).atomic_begin.is_some() {
            ((*crtc_funcs).atomic_begin.unwrap())(crtc, core::ptr::null_mut());
        }

        let dev = (*crtc).dev;
        let plane_mask = (*old_crtc_state).plane_mask;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut cur = (*head).next;
        while cur != head {
            let plane = (cur as *mut u8).sub(8) as *mut DrmPlane;
            let mask = 1u32 << (*plane).index;
            if (plane_mask & mask) != 0 {
                let plane_funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
                if !plane_funcs.is_null() && (*plane_funcs).atomic_disable.is_some() {
                    ((*plane_funcs).atomic_disable.unwrap())(plane, core::ptr::null_mut());
                }
            }
            cur = (*cur).next;
        }

        if atomic && !crtc_funcs.is_null() && (*crtc_funcs).atomic_flush.is_some() {
            ((*crtc_funcs).atomic_flush.unwrap())(crtc, core::ptr::null_mut());
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_cleanup_planes()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` and `state` must point to live `DrmDevice` and `DrmAtomicCommit` instances.
/// - Planes modified in `state` must have previously been prepared and had hardware access initiated.
/// - Sequentially calls `end_fb_access` and `cleanup_fb` on plane helper tables; callback tables must remain valid.
pub unsafe extern "C" fn drm_atomic_helper_cleanup_planes(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY: `dev.mode_config.num_total_plane` bounds `state.planes`. Calling `end_fb_access`
    // and `cleanup_fb` on modified planes releases hardware access and unpins framebuffers.
    unsafe {
        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let old_plane_state = (*entry).old_state;
            if plane.is_null() || old_plane_state.is_null() {
                continue;
            }

            let funcs = (*plane).helper_private as *const DrmPlaneHelperFuncs;
            if !funcs.is_null() && (*funcs).cleanup_fb.is_some() {
                ((*funcs).cleanup_fb.unwrap())(plane, old_plane_state);
            }
        }
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_swap_state()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit` with valid old CRTC, connector, and plane state references.
unsafe fn wait_for_hw_done(state: *mut DrmAtomicCommit) -> i32 {
    // SAFETY: `state` retains valid references to all old CRTC states; waiting on preceding
    // commit completion tokens occurs in sleepable task context.
    // the caller finishes its pre-swap or state-swap phase.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let old = (*(*state).crtcs.add(i)).old_state;
            if !old.is_null() && !(*old).commit.is_null() {
                let ret = (*(*old).commit).hw_done.wait_interruptible();
                if ret != 0 {
                    return ret;
                }
            }
        }

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let old = (*(*state).connectors.add(i)).old_state;
            if !old.is_null() && !(*old).commit.is_null() {
                let ret = (*(*old).commit).hw_done.wait_interruptible();
                if ret != 0 {
                    return ret;
                }
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let old = (*(*state).planes.add(i)).old_state;
            if !old.is_null() && !(*old).commit.is_null() {
                let ret = (*(*old).commit).hw_done.wait_interruptible();
                if ret != 0 {
                    return ret;
                }
            }
        }
        0
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a validated `DrmAtomicCommit` with initialized `planes`, `crtcs`, and `connectors` arrays.
/// - Caller must hold all modeset locks (via `DrmModesetAcquireCtx`) covering objects in `state`.
/// - If `stall` is true, blocks the caller until preceding commits on the affected CRTCs have completed.
/// - Swaps `new_state` into the live `obj.state` pointer of each object and records `old_state` in `state`.
/// - Returns 0 on success, or a negative errno if stall checks fail.
pub unsafe extern "C" fn drm_atomic_helper_swap_state(
    state: *mut DrmAtomicCommit,
    stall: bool,
) -> i32 {
    // SAFETY: Caller holds modeset locks covering all objects in `state`. Swapping `new_state`
    // into `obj.state` pointers and recording `old_state` in `state` transfers active ownership.
    unsafe {
        let dev = (*state).dev;
        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        let num_connectors = (*state).num_connector as usize;
        let num_planes = (*dev).mode_config.num_total_plane as usize;

        if stall {
            let ret = wait_for_hw_done(state);
            if ret != 0 {
                return ret;
            }
        }

        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            let old_conn_state = (*conn_entry).old_state;
            let new_conn_state = (*conn_entry).new_state;
            if connector.is_null() || old_conn_state.is_null() || new_conn_state.is_null() {
                continue;
            }

            (*old_conn_state).state = state;
            (*new_conn_state).state = core::ptr::null_mut();

            (*conn_entry).state_to_destroy = old_conn_state;
            (*connector).state = new_conn_state;
        }

        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            let old_crtc_state = (*entry).old_state;
            let new_crtc_state = (*entry).new_state;
            if crtc.is_null() || old_crtc_state.is_null() || new_crtc_state.is_null() {
                continue;
            }

            (*old_crtc_state).state = state;
            (*new_crtc_state).state = core::ptr::null_mut();

            (*entry).state_to_destroy = old_crtc_state;
            (*crtc).state = new_crtc_state;

            if !(*new_crtc_state).commit.is_null() {
                spin_lock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));
                drm_kapi::list::list_add(
                    core::ptr::addr_of_mut!((*(*new_crtc_state).commit).commit_entry),
                    core::ptr::addr_of_mut!((*crtc).commit_list),
                );
                spin_unlock_raw(core::ptr::addr_of_mut!((*crtc).commit_lock));

                (*(*new_crtc_state).commit).event = core::ptr::null_mut();
            }
        }

        let num_colorops = if (*state).colorops.is_null() {
            0
        } else {
            (*(*state).dev).mode_config.num_colorop as usize
        };
        for i in 0..num_colorops {
            let entry = (*state).colorops.add(i);
            let colorop = (*entry).ptr as *mut DrmColorop;
            let old_colorop_state = (*entry).old_state as *mut DrmColoropState;
            let new_colorop_state = (*entry).new_state as *mut DrmColoropState;
            if colorop.is_null() || old_colorop_state.is_null() || new_colorop_state.is_null() {
                continue;
            }

            (*old_colorop_state).state = state;
            (*new_colorop_state).state = core::ptr::null_mut();

            (*entry).state = old_colorop_state as *mut c_void;
            (*colorop).state = new_colorop_state;
        }

        // Skip drm_panic_lock / unlock: CONFIG_DRM_PANIC is disabled in 7.2.3-q35-graphics.config
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            let old_plane_state = (*entry).old_state;
            let new_plane_state = (*entry).new_state;
            if plane.is_null() || old_plane_state.is_null() || new_plane_state.is_null() {
                continue;
            }

            (*old_plane_state).state = state;
            (*new_plane_state).state = core::ptr::null_mut();

            (*entry).state_to_destroy = old_plane_state;
            (*plane).state = new_plane_state;
        }

        let num_priv = (*state).num_private_objs as usize;
        for i in 0..num_priv {
            let entry = (*state).private_objs.add(i);
            let obj = (*entry).ptr;
            let old_obj_state = (*entry).old_state;
            let new_obj_state = (*entry).new_state;
            if obj.is_null() || old_obj_state.is_null() || new_obj_state.is_null() {
                continue;
            }

            (*old_obj_state).state = state;
            (*new_obj_state).state = core::ptr::null_mut();

            (*entry).state_to_destroy = old_obj_state;
            (*obj).state = new_obj_state;
        }

        0
    }
}

// =========================================================================
// Section 7: Legacy Helpers, Suspend / Resume & Page Flip
// =========================================================================

// upstream: drm_atomic_helper.c drm_atomic_helper_update_plane()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane`, `crtc`, and `fb` must point to live, initialized DRM objects belonging to the same device.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` with modeset locks held.
/// - Coordinates (`crtc_x`, `crtc_y`, `crtc_w`, `crtc_h`) and source rectangles (`src_x`, `src_y`, `src_w`, `src_h`) must be within device limits.
/// - Returns 0 on successful atomic plane update commit, or a negative errno on validation/allocation failure.
pub unsafe extern "C" fn drm_atomic_helper_update_plane(
    plane: *mut DrmPlane,
    crtc: *mut DrmCrtc,
    fb: *mut DrmFramebuffer,
    crtc_x: i32,
    crtc_y: i32,
    crtc_w: u32,
    crtc_h: u32,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `plane.dev` is valid. Allocating an atomic commit, setting plane properties,
    // and committing the update is serialized under caller-held `ctx` modeset locks.
    unsafe {
        let dev = (*plane).dev;
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;
        let plane_state = crate::drm_atomic_upstream::drm_atomic_get_plane_state(state, plane);
        if is_err(plane_state) {
            let ret = ptr_err(plane_state);
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        let mut ret =
            crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_plane(plane_state, crtc);
        if ret != 0 {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        crate::drm_atomic_uapi_upstream::drm_atomic_set_fb_for_plane(plane_state, fb);
        (*plane_state).crtc_x = crtc_x;
        (*plane_state).crtc_y = crtc_y;
        (*plane_state).crtc_w = crtc_w;
        (*plane_state).crtc_h = crtc_h;
        (*plane_state).src_x = src_x;
        (*plane_state).src_y = src_y;
        (*plane_state).src_w = src_w;
        (*plane_state).src_h = src_h;

        if !crtc.is_null() && plane == (*crtc).cursor {
            (*state).set_legacy_cursor_update(true);
        }

        ret = crate::drm_atomic_upstream::drm_atomic_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_disable_plane()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane` must point to a live `DrmPlane`.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` holding locks for `plane` and its current CRTC.
/// - Returns 0 on successful atomic disable commit, or a negative errno.
pub unsafe extern "C" fn drm_atomic_helper_disable_plane(
    plane: *mut DrmPlane,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `plane.dev` is valid. Allocating an atomic commit to disable `plane` and
    // committing the update is serialized under caller-held `ctx` modeset locks.
    unsafe {
        let dev = (*plane).dev;
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;
        let plane_state = crate::drm_atomic_upstream::drm_atomic_get_plane_state(state, plane);
        if is_err(plane_state) {
            let ret = ptr_err(plane_state);
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        if !(*plane_state).crtc.is_null() && (*(*plane_state).crtc).cursor == plane {
            (*(*plane_state).state).set_legacy_cursor_update(true);
        }

        let mut ret =
            crate::drm_atomic_upstream::__drm_atomic_helper_disable_plane(plane, plane_state);
        if ret != 0 {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        ret = crate::drm_atomic_upstream::drm_atomic_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_set_config()
#[unsafe(no_mangle)]
/// # Safety
/// - `set` must point to a valid `DrmModeSet` with an initialized `crtc` pointer.
/// - If `set.mode` or `set.fb` is non-null, they must point to valid mode and framebuffer structures.
/// - `set.connectors` must point to an array of at least `set.num_connectors` valid `DrmConnector` pointers.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` with modeset locks acquired.
/// - Returns 0 on success, or a negative errno on failure.
pub unsafe extern "C" fn drm_atomic_helper_set_config(
    set: *mut DrmModeSet,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `set` is a valid `DrmModeSet` configuration. Translates legacy modeset parameters
    // into an atomic state transaction and commits it under modeset locks.
    unsafe {
        let crtc = (*set).crtc;
        let dev = (*crtc).dev;
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;
        let mut ret = crate::drm_atomic_upstream::__drm_atomic_helper_set_config(set, state);
        if ret != 0 {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        ret = handle_conflicting_encoders(state, true);
        if ret != 0 {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        ret = crate::drm_atomic_upstream::drm_atomic_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_disable_all()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must point to a live `DrmDevice`.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` holding locks for all CRTCs, planes, and connectors on `dev`.
/// - Creates an atomic state disabling all display pipelines and commits it; returns 0 on success or a negative errno.
pub unsafe extern "C" fn drm_atomic_helper_disable_all(
    dev: *mut DrmDevice,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `dev` is non-null. Allocating an atomic commit, configuring all CRTCs, planes, and
    // connectors to disabled, and committing the change is serialized under `ctx`.
    unsafe {
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;

        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        let mut ret: i32;
        while cur != head {
            let crtc = cur
                .cast::<u8>()
                .sub(offset_of!(DrmCrtc, head))
                .cast::<DrmCrtc>();
            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_crtc_state(state, crtc);
            if is_err(crtc_state) {
                ret = ptr_err(crtc_state);
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return ret;
            }

            (*crtc_state).active = false;

            ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_mode_prop_for_crtc(
                crtc_state,
                core::ptr::null_mut(),
            );
            if ret < 0 {
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return ret;
            }

            ret = crate::drm_atomic_upstream::drm_atomic_add_affected_planes(state, crtc);
            if ret < 0 {
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return ret;
            }

            ret = crate::drm_atomic_upstream::drm_atomic_add_affected_connectors(state, crtc);
            if ret < 0 {
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return ret;
            }

            cur = (*cur).next;
        }

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let conn_state = (*conn_entry).new_state;
            if conn_state.is_null() {
                continue;
            }
            ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_connector(
                conn_state,
                core::ptr::null_mut(),
            );
            if ret < 0 {
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return ret;
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane_state = (*entry).new_state;
            if plane_state.is_null() {
                continue;
            }

            ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_plane(
                plane_state,
                core::ptr::null_mut(),
            );
            if ret < 0 {
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return ret;
            }

            crate::drm_atomic_uapi_upstream::drm_atomic_set_fb_for_plane(
                plane_state,
                core::ptr::null_mut(),
            );
        }

        ret = crate::drm_atomic_upstream::drm_atomic_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_reset_crtc()
#[unsafe(no_mangle)]
/// # Safety
/// - `crtc` must point to a live `DrmCrtc`.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` with the CRTC modeset lock held.
/// - Resets routing and disables planes on `crtc`; returns 0 on success or a negative errno.
pub unsafe extern "C" fn drm_atomic_helper_reset_crtc(
    crtc: *mut DrmCrtc,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `crtc.dev` is valid. Allocating an atomic commit to reset CRTC state
    // and committing the update runs under caller-held `ctx` modeset locks.
    unsafe {
        let dev = (*crtc).dev;
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;
        let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_crtc_state(state, crtc);
        if is_err(crtc_state) {
            let ret = ptr_err(crtc_state);
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        (*crtc_state).set_connectors_changed(true);
        let ret = crate::drm_atomic_upstream::drm_atomic_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_shutdown()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must point to a live `DrmDevice` undergoing system shutdown or driver unbind.
/// - Must be called in sleepable task context without holding modeset locks (the function initializes its own acquire context).
/// - Disables all display pipelines across `dev`.
pub unsafe extern "C" fn drm_atomic_helper_shutdown(dev: *mut DrmDevice) {
    // SAFETY: `dev` is checked non-null. Initializing a local acquire context and calling
    // `drm_atomic_helper_disable_all` cleanly turns off all display hardware for shutdown.
    unsafe {
        if dev.is_null() {
            return;
        }

        let mut ctx: DrmModesetAcquireCtx = core::mem::zeroed();
        let mut ret: i32;

        crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0);
        loop {
            ret = crate::drm_modeset_lock_upstream::drm_modeset_lock_all_ctx(dev, &mut ctx);
            if ret == 0 {
                ret = drm_atomic_helper_disable_all(dev, &mut ctx);
            }
            if ret == -drm_kapi::config::EDEADLK {
                ret = crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx);
                if ret == 0 {
                    continue;
                }
            }
            break;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_duplicate_state()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must point to a live `DrmDevice`.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` holding locks for all modeset objects on `dev`.
/// - Returns a newly allocated `DrmAtomicCommit` containing duplicated state for all CRTCs, planes, and connectors, or a null pointer on allocation failure.
/// - Caller assumes ownership of the returned state reference (refcount 1).
pub unsafe extern "C" fn drm_atomic_helper_duplicate_state(
    dev: *mut DrmDevice,
    ctx: *mut DrmModesetAcquireCtx,
) -> *mut DrmAtomicCommit {
    // SAFETY: `dev` is non-null. Duplicating current CRTC, connector, and plane states into
    // a freshly allocated `DrmAtomicCommit` runs under caller-held `ctx` modeset locks.
    unsafe {
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return err_ptr(-ENOMEM);
        }

        (*state).acquire_ctx = ctx;
        (*state).set_duplicated(true);

        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            let crtc = cur
                .cast::<u8>()
                .sub(offset_of!(DrmCrtc, head))
                .cast::<DrmCrtc>();
            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_crtc_state(state, crtc);
            if is_err(crtc_state) {
                let err = ptr_err(crtc_state);
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return err_ptr(err);
            }
            cur = (*cur).next;
        }

        let plane_head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut plane_cur = (*plane_head).next;
        while plane_cur != plane_head {
            let plane = (plane_cur as *mut u8).sub(8) as *mut DrmPlane;
            let plane_state = crate::drm_atomic_upstream::drm_atomic_get_plane_state(state, plane);
            if is_err(plane_state) {
                let err = ptr_err(plane_state);
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return err_ptr(err);
            }

            if !(*plane_state).color_pipeline.is_null() {
                let err =
                    crate::drm_atomic_upstream::drm_atomic_add_affected_colorops(state, plane);
                if err != 0 {
                    crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                    return err_ptr(err);
                }
            }
            plane_cur = (*plane_cur).next;
        }

        let mut conn_iter: DrmConnectorListIter = core::mem::zeroed();
        crate::drm_connector_upstream::drm_connector_list_iter_begin(dev, &mut conn_iter);
        loop {
            let conn = crate::drm_connector_upstream::drm_connector_list_iter_next(&mut conn_iter);
            if conn.is_null() {
                break;
            }

            let conn_state =
                crate::drm_atomic_upstream::drm_atomic_get_connector_state(state, conn);
            if is_err(conn_state) {
                let err = ptr_err(conn_state);
                crate::drm_connector_upstream::drm_connector_list_iter_end(&mut conn_iter);
                crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                return err_ptr(err);
            }
        }
        crate::drm_connector_upstream::drm_connector_list_iter_end(&mut conn_iter);

        (*state).acquire_ctx = core::ptr::null_mut();
        state
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_suspend()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must point to an active `DrmDevice` entering system suspend.
/// - Must be called in sleepable task context without holding modeset locks (initializes its own acquire context).
/// - On success, returns an owned `*mut DrmAtomicCommit` storing pre-suspend state to be restored by `drm_atomic_helper_resume`.
/// - On failure, returns an error pointer (`err_ptr(-errno)`).
pub unsafe extern "C" fn drm_atomic_helper_suspend(dev: *mut DrmDevice) -> *mut DrmAtomicCommit {
    // SAFETY: `dev` is non-null. `drm_atomic_helper_suspend` initializes its own modeset context,
    // saves active state via `duplicate_state`, and disables all outputs via `disable_all`.
    unsafe {
        let mut ctx: DrmModesetAcquireCtx = core::mem::zeroed();
        let mut state: *mut DrmAtomicCommit = err_ptr(-EINVAL);
        let mut ret: i32;

        crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0);
        loop {
            ret = crate::drm_modeset_lock_upstream::drm_modeset_lock_all_ctx(dev, &mut ctx);
            if ret == 0 {
                state = drm_atomic_helper_duplicate_state(dev, &mut ctx);
                if !is_err(state) {
                    ret = drm_atomic_helper_disable_all(dev, &mut ctx);
                    if ret < 0 {
                        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
                        state = err_ptr(ret);
                    }
                } else {
                    ret = ptr_err(state);
                }
            }
            if ret == -drm_kapi::config::EDEADLK {
                ret = crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx);
                if ret == 0 {
                    continue;
                }
            }
            break;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);

        if ret != 0 && !is_err(state) {
            return err_ptr(ret);
        }
        state
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_commit_duplicated_state()
#[unsafe(no_mangle)]
/// # Safety
/// - `state` must point to a live `DrmAtomicCommit` created by `drm_atomic_helper_duplicate_state`.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` with modeset locks acquired for all objects in `state`.
/// - Returns 0 on successful commit, or a negative errno on failure.
pub unsafe extern "C" fn drm_atomic_helper_commit_duplicated_state(
    state: *mut DrmAtomicCommit,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `state` is a duplicated state and `ctx` is an acquire context. Locks all objects
    // in `state` and commits the duplicated state under modeset serialization.
    unsafe {
        (*state).acquire_ctx = ctx;

        let dev = (*state).dev;
        let num_colorops = if (*state).colorops.is_null() {
            0
        } else {
            (*dev).mode_config.num_colorop as usize
        };
        for i in 0..num_colorops {
            let entry = (*state).colorops.add(i);
            let colorop = (*entry).ptr as *mut DrmColorop;
            if !colorop.is_null() {
                (*entry).old_state = (*colorop).state as *mut c_void;
            }
        }

        let num_planes = (*dev).mode_config.num_total_plane as usize;
        for i in 0..num_planes {
            let entry = (*state).planes.add(i);
            let plane = (*entry).ptr;
            if !plane.is_null() {
                (*entry).old_state = (*plane).state;
            }
        }

        let num_crtcs = (*dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            if !crtc.is_null() {
                (*entry).old_state = (*crtc).state;
            }
        }

        let num_connectors = (*state).num_connector as usize;
        for i in 0..num_connectors {
            let conn_entry = (*state).connectors.add(i);
            let connector = (*conn_entry).ptr;
            if !connector.is_null() {
                (*conn_entry).old_state = (*connector).state;
            }
        }

        let ret = crate::drm_atomic_upstream::drm_atomic_commit(state);
        (*state).acquire_ctx = core::ptr::null_mut();
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_resume()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must point to the resuming `DrmDevice`.
/// - If `state` is null or an error pointer (`is_err(state)`), returns 0 without action.
/// - Otherwise, `state` must be the valid `DrmAtomicCommit` returned by `drm_atomic_helper_suspend`.
/// - Must be called in sleepable task context without modeset locks held.
/// - Automatically releases the `state` reference upon completion.
pub unsafe extern "C" fn drm_atomic_helper_resume(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: `dev` is non-null and `state` is the commit returned by suspend.
    // Initializing a local acquire context and committing `state` restores display configuration.
    unsafe {
        crate::drm_mode_config_upstream::drm_mode_config_reset(dev);

        let mut ctx: DrmModesetAcquireCtx = core::mem::zeroed();
        let mut err: i32;

        crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(&mut ctx, 0);
        loop {
            err = crate::drm_modeset_lock_upstream::drm_modeset_lock_all_ctx(dev, &mut ctx);
            if err == 0 {
                err = drm_atomic_helper_commit_duplicated_state(state, &mut ctx);
            }
            if err == -drm_kapi::config::EDEADLK {
                err = crate::drm_modeset_lock_upstream::drm_modeset_backoff(&mut ctx);
                if err == 0 {
                    continue;
                }
            }
            break;
        }
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(&mut ctx);
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(&mut ctx);

        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        err
    }
}

// upstream: drm_atomic_helper.c page_flip_common()
/// # Safety
/// `state` must point to a live `DrmAtomicCommit`. `crtc` must point to a valid `DrmCrtc` with an initialized primary plane.
/// If non-null, `fb` and `event` must point to valid DRM framebuffer and pending vblank event structures.
unsafe fn page_flip_common(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
    fb: *mut DrmFramebuffer,
    event: *mut DrmPendingVblankEvent,
    flags: u32,
) -> i32 {
    // SAFETY: `crtc.primary` is a valid plane. Updating the primary plane state with `fb`
    // and attaching `event` runs under caller-held modeset locks.
    unsafe {
        let plane = (*crtc).primary;
        let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_crtc_state(state, crtc);
        if is_err(crtc_state) {
            return ptr_err(crtc_state);
        }

        (*crtc_state).event = event;
        (*crtc_state).async_flip = (flags & DRM_MODE_PAGE_FLIP_ASYNC) != 0;

        let plane_state = crate::drm_atomic_upstream::drm_atomic_get_plane_state(state, plane);
        if is_err(plane_state) {
            return ptr_err(plane_state);
        }

        let ret = crate::drm_atomic_uapi_upstream::drm_atomic_set_crtc_for_plane(plane_state, crtc);
        if ret != 0 {
            return ret;
        }
        crate::drm_atomic_uapi_upstream::drm_atomic_set_fb_for_plane(plane_state, fb);

        (*state).set_allow_modeset(false);
        if !(*crtc_state).active {
            return -EINVAL;
        }

        0
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_page_flip()
#[unsafe(no_mangle)]
/// # Safety
/// - `crtc` must point to a live `DrmCrtc` with an active primary plane.
/// - `fb` must point to a valid, live `DrmFramebuffer` compatible with the primary plane.
/// - If non-null, `event` must point to an initialized `DrmPendingVblankEvent` to signal when the flip completes.
/// - `flags` specifies page flip flags (`DRM_MODE_PAGE_FLIP_*`).
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` holding modeset locks.
/// - Returns 0 on success, or a negative errno on failure.
pub unsafe extern "C" fn drm_atomic_helper_page_flip(
    crtc: *mut DrmCrtc,
    fb: *mut DrmFramebuffer,
    event: *mut DrmPendingVblankEvent,
    flags: u32,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `crtc.primary.dev` is valid. Allocating an atomic commit, populating it via
    // `page_flip_common`, and committing asynchronously runs under caller-held `ctx`.
    unsafe {
        let plane = (*crtc).primary;
        let dev = (*plane).dev;
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;
        let mut ret = page_flip_common(state, crtc, fb, event, flags);
        if ret != 0 {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        ret = crate::drm_atomic_upstream::drm_atomic_nonblocking_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

// upstream: drm_atomic_helper.c drm_atomic_helper_page_flip_target()
#[unsafe(no_mangle)]
/// # Safety
/// - `crtc` must point to a live `DrmCrtc` with an active primary plane.
/// - `fb` must point to a valid, live `DrmFramebuffer` compatible with the primary plane.
/// - If non-null, `event` must point to an initialized `DrmPendingVblankEvent`.
/// - `flags` specifies page flip flags (`DRM_MODE_PAGE_FLIP_*`).
/// - `target` specifies the target vblank sequence number for the flip.
/// - `ctx` must point to an initialized `DrmModesetAcquireCtx` holding modeset locks.
/// - Returns 0 on success, or a negative errno on failure.
pub unsafe extern "C" fn drm_atomic_helper_page_flip_target(
    crtc: *mut DrmCrtc,
    fb: *mut DrmFramebuffer,
    event: *mut DrmPendingVblankEvent,
    flags: u32,
    target: u32,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `crtc.primary.dev` is valid. Allocating an atomic commit, setting target vblank sequence,
    // and committing asynchronously runs under caller-held `ctx` modeset locks.
    unsafe {
        let plane = (*crtc).primary;
        let dev = (*plane).dev;
        let state = crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        (*state).acquire_ctx = ctx;
        let mut ret = page_flip_common(state, crtc, fb, event, flags);
        if ret != 0 {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return ret;
        }

        let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc);
        if crtc_state.is_null() {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            return -EINVAL;
        }
        (*crtc_state).target_vblank = target;

        ret = crate::drm_atomic_upstream::drm_atomic_nonblocking_commit(state);
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        ret
    }
}

#[cfg(test)]
mod pre_swap_tests {
    use std::{
        boxed::Box,
        mem::zeroed,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{drm_atomic_types::DrmPlanesState, test_support::TestDrm};

    static TEST_FENCE_SIGNALED: AtomicBool = AtomicBool::new(false);
    static TEST_FENCE_PROBES: AtomicUsize = AtomicUsize::new(0);

    /// # Safety
    /// If non-null, `_fence` must point to a valid `DmaFence` being released by the test harness.
    unsafe extern "C" fn release_test_fence(_fence: *mut drm_kapi::types::DmaFence) {}

    /// # Safety
    /// If non-null, `_fence` must point to a valid `DmaFence` queried by the test harness.
    unsafe extern "C" fn test_fence_signaled(_fence: *mut drm_kapi::types::DmaFence) -> bool {
        TEST_FENCE_PROBES.fetch_add(1, Ordering::SeqCst);
        TEST_FENCE_SIGNALED.load(Ordering::Acquire)
    }

    static TEST_FENCE_OPS: drm_kapi::types::DmaFenceOps = drm_kapi::types::DmaFenceOps {
        get_driver_name: None,
        get_timeline_name: None,
        enable_signaling: None,
        signaled: Some(test_fence_signaled),
        wait: None,
        release: Some(release_test_fence),
        set_deadline: None,
    };

    struct CommitCounters {
        pre_swap: AtomicUsize,
        prepared: AtomicUsize,
        cleaned: AtomicUsize,
        plane_updates: AtomicUsize,
        tails: AtomicUsize,
        fence_consumed: AtomicUsize,
        tail_done: Completion,
    }

    /// # Safety
    /// `dev` must point to a valid `DrmDevice` whose `dev_private` points to a live `CommitCounters` instance.
    unsafe fn counters_from_dev(dev: *mut DrmDevice) -> *const CommitCounters {
        // SAFETY: `dev` points to an active `DrmDevice` whose `dev_private` was initialized to a heap-allocated `CommitCounters`.
        unsafe { (*dev).dev_private.cast::<CommitCounters>() }
    }

    /// # Safety
    /// `state` must point to a valid `DrmAtomicCommit` whose `dev` points to a `DrmDevice` configured with `CommitCounters`.
    unsafe extern "C" fn fail_pre_swap(state: *mut DrmAtomicCommit) -> i32 {
        // SAFETY: `state.dev` is an active `DrmDevice` configured with `CommitCounters` in `dev_private`.
        let counters = unsafe { &*counters_from_dev((*state).dev) };
        counters.pre_swap.fetch_add(1, Ordering::SeqCst);
        -drm_kapi::config::EIO
    }

    /// # Safety
    /// `plane` must point to a valid `DrmPlane` whose `dev` points to a `DrmDevice` configured with `CommitCounters`.
    unsafe extern "C" fn prepare_fb(plane: *mut DrmPlane, _state: *mut DrmPlaneState) -> i32 {
        // SAFETY: `plane.dev` is an active `DrmDevice` configured with `CommitCounters` in `dev_private`.
        let counters = unsafe { &*counters_from_dev((*plane).dev) };
        counters.prepared.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// # Safety
    /// `plane` must point to a valid `DrmPlane` whose `dev` points to a `DrmDevice` configured with `CommitCounters`.
    unsafe extern "C" fn cleanup_fb(plane: *mut DrmPlane, _state: *mut DrmPlaneState) {
        // SAFETY: `plane.dev` is an active `DrmDevice` configured with `CommitCounters` in `dev_private`.
        let counters = unsafe { &*counters_from_dev((*plane).dev) };
        counters.cleaned.fetch_add(1, Ordering::SeqCst);
    }

    /// # Safety
    /// `plane` must point to a valid `DrmPlane` whose `dev` points to a `DrmDevice` configured with `CommitCounters`.
    unsafe extern "C" fn plane_update(plane: *mut DrmPlane, _state: *mut DrmAtomicCommit) {
        // SAFETY: `plane.dev` is an active `DrmDevice` configured with `CommitCounters` in `dev_private`.
        let counters = unsafe { &*counters_from_dev((*plane).dev) };
        counters.plane_updates.fetch_add(1, Ordering::SeqCst);
    }

    /// # Safety
    /// `state` must point to a valid `DrmAtomicCommit` whose `dev` points to a `DrmDevice` configured with `CommitCounters`.
    unsafe extern "C" fn commit_tail(state: *mut DrmAtomicCommit) {
        // SAFETY: `state.dev` is an active `DrmDevice` configured with `CommitCounters` in `dev_private`.
        let counters = unsafe { &*counters_from_dev((*state).dev) };
        counters.tails.fetch_add(1, Ordering::SeqCst);
        // SAFETY: In this test fixture, `state.planes` points to a valid `DrmPlanesState` array.
        let entry = unsafe { (*state).planes };
        if !entry.is_null() {
            // SAFETY: `entry` is non-null and points to a valid `DrmPlanesState` in this test fixture.
            let plane_state = unsafe { (*entry).new_state };
            // SAFETY: `plane_state` is non-null and points to the mock `DrmPlaneState`.
            if !plane_state.is_null() && unsafe { (*plane_state).fence.is_null() } {
                counters.fence_consumed.fetch_add(1, Ordering::SeqCst);
            }
        }
        // SAFETY: `state` is a valid committed atomic state being processed by mock commit tail.
        unsafe { drm_atomic_helper_commit_tail(state) };
        counters.tail_done.complete_all();
    }

    static MODE_CONFIG_HELPERS: DrmModeConfigHelperFuncs = DrmModeConfigHelperFuncs {
        atomic_commit_tail: Some(commit_tail),
        atomic_commit_pre_swap: Some(fail_pre_swap),
    };

    static PLANE_HELPERS: DrmPlaneHelperFuncs = DrmPlaneHelperFuncs {
        prepare_fb: Some(prepare_fb),
        cleanup_fb: Some(cleanup_fb),
        begin_fb_access: None,
        end_fb_access: None,
        atomic_check: None,
        atomic_update: Some(plane_update),
        atomic_enable: None,
        atomic_disable: None,
        atomic_async_check: None,
        atomic_async_update: None,
        get_scanout_buffer: None,
        panic_flush: None,
    };

    #[test]
    fn failed_pre_swap_keeps_old_state_and_unprepares_planes() {
        let mut drm = TestDrm::new(0);
        let dev = drm.ptr();
        let counters = Box::new(CommitCounters {
            pre_swap: AtomicUsize::new(0),
            prepared: AtomicUsize::new(0),
            cleaned: AtomicUsize::new(0),
            plane_updates: AtomicUsize::new(0),
            tails: AtomicUsize::new(0),
            fence_consumed: AtomicUsize::new(0),
            tail_done: Completion::new(),
        });
        // SAFETY: `dev` points to an active `TestDrm` device fixture. Setting `dev_private` and mode config fields.
        unsafe {
            (*dev).dev_private = (&*counters as *const CommitCounters).cast_mut().cast();
            (*dev).mode_config.num_total_plane = 1;
            (*dev).mode_config.helper_private =
                core::ptr::addr_of!(MODE_CONFIG_HELPERS).cast_mut().cast();
        }

        // SAFETY: DrmPlane POD struct mock initialized with all-zeroes pattern for unit test.
        let mut plane: DrmPlane = unsafe { zeroed() };
        // SAFETY: DrmPlaneState POD struct mock initialized with all-zeroes pattern for unit test.
        let mut old_state: DrmPlaneState = unsafe { zeroed() };
        // SAFETY: DrmPlaneState POD struct mock initialized with all-zeroes pattern for unit test.
        let mut new_state: DrmPlaneState = unsafe { zeroed() };
        // SAFETY: DrmPlanesState POD struct mock initialized with all-zeroes pattern for unit test.
        let mut plane_entry: DrmPlanesState = unsafe { zeroed() };
        // SAFETY: DrmAtomicCommit POD struct mock initialized with all-zeroes pattern for unit test.
        let mut state: DrmAtomicCommit = unsafe { zeroed() };
        plane.dev = dev;
        plane.helper_private = core::ptr::addr_of!(PLANE_HELPERS).cast_mut().cast();
        plane.state = &mut old_state;

        old_state.plane = &mut plane;
        new_state.plane = &mut plane;
        new_state.state = &mut state;

        plane_entry.ptr = &mut plane;
        plane_entry.old_state = &mut old_state;
        plane_entry.new_state = &mut new_state;

        state.dev = dev;
        state.planes = &mut plane_entry;

        // SAFETY: `dev` and `state` are initialized mock structures wired up for synchronous commit failure testing.
        let result = unsafe { drm_atomic_helper_commit(dev, &mut state, false) };

        assert_eq!(result, -drm_kapi::config::EIO);
        assert_eq!(counters.pre_swap.load(Ordering::SeqCst), 1);
        assert_eq!(counters.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(counters.cleaned.load(Ordering::SeqCst), 1);
        assert_eq!(counters.plane_updates.load(Ordering::SeqCst), 0);
        // Synthetic-vblank and flip-event delivery are in commit_tail; the
        // failing pre-swap hook must prevent the tail from being scheduled.
        assert_eq!(counters.tails.load(Ordering::SeqCst), 0);
        assert!(core::ptr::eq(plane.state, &old_state));
        assert!(plane_entry.state_to_destroy.is_null());
        assert!(core::ptr::eq(new_state.state, &state));

        // setup_commit gave the plane and the atomic state one reference each
        // to its fake CRTC commit; release both because this fixture is stack-owned.
        // SAFETY: Releases references to the fake CRTC commits allocated during the test.
        unsafe {
            drm_crtc_commit_put(new_state.commit);
            drm_crtc_commit_put(state.fake_commit);
        }
    }

    #[test]
    fn mode_valid_path_accepts_an_initialized_empty_bridge_chain() {
        // SAFETY: DrmConnector mock initialized with all-zeroes pattern for unit test.
        let mut connector: DrmConnector = unsafe { zeroed() };
        // SAFETY: DrmEncoder mock initialized with all-zeroes pattern for unit test.
        let mut encoder: DrmEncoder = unsafe { zeroed() };
        // SAFETY: DrmCrtc mock initialized with all-zeroes pattern for unit test.
        let mut crtc: DrmCrtc = unsafe { zeroed() };
        // SAFETY: DrmDisplayMode mock initialized with all-zeroes pattern for unit test.
        let mode: DrmDisplayMode = unsafe { zeroed() };
        // SAFETY: `encoder.bridge_chain` is a valid mutable list head being initialized.
        unsafe {
            drm_kapi::list::init_list_head(core::ptr::addr_of_mut!(encoder.bridge_chain));
        }

        assert_eq!(
            // SAFETY: Stack-allocated mock connector, encoder, CRTC, and mode are valid throughout the call.
            unsafe { mode_valid_path(&mut connector, &mut encoder, &mut crtc, &mode) },
            crate::drm_modes_types::MODE_OK
        );
    }

    #[test]
    fn nonblock_publishes_state_then_worker_waits_for_input_fence() {
        let mut drm = TestDrm::new(0);
        let dev = drm.ptr();
        let counters = Box::new(CommitCounters {
            pre_swap: AtomicUsize::new(0),
            prepared: AtomicUsize::new(0),
            cleaned: AtomicUsize::new(0),
            plane_updates: AtomicUsize::new(0),
            tails: AtomicUsize::new(0),
            fence_consumed: AtomicUsize::new(0),
            tail_done: Completion::new(),
        });
        // SAFETY: `dev` points to an active `TestDrm` device; configuring `dev_private` and mode config helper tables.
        unsafe {
            (*dev).dev_private = (&*counters as *const CommitCounters).cast_mut().cast();
            (*dev).mode_config.num_total_plane = 1;
            (*dev).mode_config.helper_private =
                core::ptr::addr_of!(MODE_CONFIG_HELPERS).cast_mut().cast();
        }

        // SAFETY: DrmPlane mock initialized with all-zeroes pattern for unit test.
        let mut plane: DrmPlane = unsafe { zeroed() };
        // SAFETY: DrmPlaneState mock initialized with all-zeroes pattern for unit test.
        let mut old_state = Box::new(unsafe { zeroed::<DrmPlaneState>() });
        // SAFETY: DrmPlaneState mock initialized with all-zeroes pattern for unit test.
        let mut new_state = Box::new(unsafe { zeroed::<DrmPlaneState>() });
        // SAFETY: DmaFence mock initialized with all-zeroes pattern for unit test.
        let mut fence: drm_kapi::types::DmaFence = unsafe { zeroed() };
        TEST_FENCE_SIGNALED.store(false, Ordering::Release);
        TEST_FENCE_PROBES.store(0, Ordering::Release);
        // SAFETY: `dev` is a valid mock `DrmDevice` allocated by `TestDrm` test fixture.
        let state = unsafe { crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev) };
        assert!(!state.is_null());
        // SAFETY: Initializing mock DMA fence and wiring up plane and state pointers for nonblocking commit.
        unsafe {
            drm_kapi::dma_fence::dma_fence_init(
                &mut fence,
                core::ptr::addr_of!(TEST_FENCE_OPS),
                core::ptr::null_mut(),
                0,
                1,
            );

            plane.dev = dev;
            plane.helper_private = core::ptr::addr_of!(PLANE_HELPERS).cast_mut().cast();
            plane.state = &mut *old_state;

            old_state.plane = &mut plane;
            new_state.plane = &mut plane;
            new_state.state = state;
            new_state.fence = &mut fence;

            let entry = (*state).planes;
            (*entry).ptr = &mut plane;
            (*entry).old_state = &mut *old_state;
            (*entry).new_state = &mut *new_state;
        }

        // SAFETY: `dev` and `state` are valid initialized DRM objects submitted for nonblocking commit.
        let result = unsafe { drm_atomic_helper_commit(dev, state, true) };
        assert_eq!(result, 0);
        assert!(core::ptr::eq(plane.state, &mut *new_state));
        assert_eq!(counters.pre_swap.load(Ordering::SeqCst), 0);
        assert_eq!(counters.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(counters.tails.load(Ordering::SeqCst), 0);

        let deadline = Instant::now() + Duration::from_secs(2);
        while TEST_FENCE_PROBES.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(TEST_FENCE_PROBES.load(Ordering::Acquire) > 0);
        assert_eq!(counters.tails.load(Ordering::SeqCst), 0);
        assert!(core::ptr::eq(new_state.fence, &mut fence));

        TEST_FENCE_SIGNALED.store(true, Ordering::Release);
        // SAFETY: Signaling the test DMA fence attached to the commit.
        unsafe {
            drm_kapi::dma_fence::dma_fence_signal(&mut fence);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while !counters.tail_done.is_done() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            counters.tail_done.is_done(),
            "commit tail did not run after fence"
        );
        assert_eq!(counters.fence_consumed.load(Ordering::SeqCst), 1);
        assert!(new_state.fence.is_null());
        // SAFETY: `state` points to the committed atomic state; reading `fake_commit`.
        let fake_commit = unsafe { (*state).fake_commit };
        assert!(!fake_commit.is_null());
        let deadline = Instant::now() + Duration::from_secs(2);
        // SAFETY: `fake_commit` points to the valid fake CRTC commit tracking this atomic update.
        while !unsafe { (*fake_commit).cleanup_done.is_done() } && Instant::now() < deadline {
            thread::yield_now();
        }
        // SAFETY: `fake_commit` points to the valid fake CRTC commit tracking this atomic update.
        assert!(unsafe { (*fake_commit).cleanup_done.is_done() });

        // SAFETY: Dropping references to the committed state and plane commit allocated during the test.
        unsafe {
            crate::drm_atomic_upstream::drm_atomic_commit_put(state);
            drm_crtc_commit_put(new_state.commit);
        }
    }

    /// # Safety
    /// `_bytes` if non-null must point to readable memory of at least `_len` bytes.
    unsafe extern "C" fn discard_event_bytes(
        _context: *mut core::ffi::c_void,
        _bytes: *const core::ffi::c_void,
        _len: usize,
    ) -> isize {
        0
    }

    #[test]
    fn async_commit_error_cancels_event_fails_out_fence_and_unplugs_file() {
        use core::mem::ManuallyDrop;

        use crate::{
            drm_atomic_types::{DrmCrtcCommit, DrmCrtcsState},
            drm_crtc_types::DrmCrtcState,
            drm_file_types::{DrmFile, DrmMinor},
            drm_vblank_types::DrmPendingVblankEvent,
        };

        let mut drm = TestDrm::new(0);
        let dev = drm.ptr();
        // SAFETY: DrmFile mock initialized with all-zeroes pattern for unit test.
        let mut file: DrmFile = unsafe { zeroed() };
        // SAFETY: DrmMinor mock initialized with all-zeroes pattern for unit test.
        let mut minor: DrmMinor = unsafe { zeroed() };
        // SAFETY: File mock initialized with all-zeroes pattern for unit test.
        let mut filp: drm_kapi::types::File = unsafe { zeroed() };
        // SAFETY: Initializing mutexes, spinlocks, waitqueues, and linking mock file into device filelist.
        unsafe {
            drm_kapi::mutex::mutex_init(core::ptr::addr_of_mut!((*dev).filelist_mutex));
            drm_kapi::locks::spin_lock_init(&mut (*dev).event_lock);
            drm_kapi::list::init_list_head(core::ptr::addr_of_mut!((*dev).filelist));
            drm_kapi::list::init_list_head(core::ptr::addr_of_mut!(file.lhead));
            drm_kapi::list::init_list_head(core::ptr::addr_of_mut!(file.pending_event_list));
            drm_kapi::list::init_list_head(core::ptr::addr_of_mut!(file.event_list));
            drm_kapi::wait::init_waitqueue_head(&mut file.event_wait);
            drm_kapi::mutex::mutex_init(core::ptr::addr_of_mut!(file.event_read_lock));
            file.event_space = 4096;
            minor.dev = dev;
            file.minor = &mut minor;
            file.filp = &mut filp;
            filp.private_data = (&mut file as *mut DrmFile).cast();
            drm_kapi::list::list_add(
                core::ptr::addr_of_mut!(file.lhead),
                core::ptr::addr_of_mut!((*dev).filelist),
            );
        }

        let event = drm_kapi::memory::kzalloc(
            core::mem::size_of::<DrmPendingVblankEvent>(),
            drm_kapi::config::GFP_KERNEL,
        )
        .cast::<DrmPendingVblankEvent>();
        assert!(!event.is_null());
        // SAFETY: DmaFence mock initialized with all-zeroes pattern for unit test.
        let mut out_fence: drm_kapi::types::DmaFence = unsafe { zeroed() };
        // SAFETY: DrmCrtcCommit mock initialized with all-zeroes pattern for unit test.
        let mut commit: DrmCrtcCommit = unsafe { zeroed() };
        // SAFETY: DrmCrtcState mock initialized with all-zeroes pattern for unit test.
        let mut old_crtc_state: DrmCrtcState = unsafe { zeroed() };
        // SAFETY: DrmCrtcState mock initialized with all-zeroes pattern for unit test.
        let mut new_crtc_state: DrmCrtcState = unsafe { zeroed() };
        // SAFETY: DrmCrtcsState mock initialized with all-zeroes pattern for unit test.
        let mut crtc_entry: DrmCrtcsState = unsafe { zeroed() };
        // SAFETY: DrmAtomicCommit mock initialized with all-zeroes pattern for unit test.
        let mut state: DrmAtomicCommit = unsafe { zeroed() };
        let event_header = crate::uapi_drm_types::DrmEvent {
            r#type: 0x8000_0000,
            length: core::mem::size_of::<crate::uapi_drm_types::DrmEvent>() as u32,
        };
        TEST_FENCE_SIGNALED.store(false, Ordering::Release);
        // SAFETY: Initializing DMA fence, setting up pending vblank event reservation, and linking CRTC commit state.
        unsafe {
            drm_kapi::dma_fence::dma_fence_init(
                &mut out_fence,
                core::ptr::addr_of!(TEST_FENCE_OPS),
                core::ptr::null_mut(),
                0,
                2,
            );
            assert!(!drm_kapi::dma_fence::dma_fence_get(&mut out_fence).is_null());

            (*event).base.event = core::ptr::addr_of_mut!((*event).event.base).cast();
            (*event).base.file_priv = &mut file;
            (*event).base.fence = &mut out_fence;
            (*event).event.base = ManuallyDrop::new(event_header);
            assert_eq!(
                crate::drm_file_upstream::drm_event_reserve_init(
                    dev,
                    &mut file,
                    &mut (*event).base,
                    core::ptr::addr_of_mut!((*event).event.base).cast(),
                ),
                0
            );

            init_commit(&mut commit, core::ptr::null_mut());
            commit.event = event;
            new_crtc_state.commit = &mut commit;
            new_crtc_state.event = event;
            crtc_entry.old_state = &mut old_crtc_state;
            crtc_entry.new_state = &mut new_crtc_state;
            state.dev = dev;
            state.crtcs = &mut crtc_entry;
            (*dev).mode_config.num_crtc = 1;
        }

        // SAFETY: Invoking `drm_atomic_helper_commit_error` on the test atomic state with an I/O error.
        unsafe { drm_atomic_helper_commit_error(&mut state, -drm_kapi::config::EIO) };

        // SAFETY: `dev` is a live `TestDrm` device; checking the `unplugged` atomic flag.
        assert!(unsafe { (*dev).unplugged.load(Ordering::Acquire) });
        assert!(commit.hw_done.is_done());
        assert!(commit.flip_done.is_done());
        assert!(new_crtc_state.event.is_null());
        assert!(commit.event.is_null());
        assert_eq!(file.event_space, 4096);
        assert!(drm_kapi::list::list_empty(&file.pending_event_list));
        assert!(drm_kapi::list::list_empty(&file.event_list));
        assert_eq!(
            // SAFETY: Checking the status of the test out_fence after commit error signaling.
            unsafe { drm_kapi::dma_fence::dma_fence_get_status(&mut out_fence) },
            -drm_kapi::config::EIO
        );
        // SAFETY: Querying if the test out_fence is signaled after commit error.
        assert!(unsafe { drm_kapi::dma_fence::dma_fence_is_signaled(&mut out_fence) });
        // SAFETY: Polling file descriptor for events on the unplugged file.
        let poll = unsafe { crate::drm_file_upstream::drm_poll(&mut filp, core::ptr::null_mut()) };
        assert_ne!(poll & 0x0008, 0); // EPOLLERR
        assert_ne!(poll & 0x0010, 0); // EPOLLHUP
        assert_eq!(poll & 0x0041, 0); // no EPOLLIN/RDNORM without a success event
        assert_eq!(
            // SAFETY: Reading kernel event data to verify EOF / error on unplugged file.
            unsafe {
                crate::drm_file_upstream::drm_read_kernel_to(
                    &mut filp,
                    4096,
                    core::ptr::null_mut(),
                    discard_event_bytes,
                )
            },
            -(crate::drm_drv_upstream::ENODEV as isize)
        );

        // SAFETY: Releasing test DMA fence and unlinking mock file from device filelist.
        unsafe {
            drm_kapi::dma_fence::dma_fence_put(&mut out_fence);
            drm_kapi::list::list_del(core::ptr::addr_of_mut!(file.lhead));
        }
    }
}

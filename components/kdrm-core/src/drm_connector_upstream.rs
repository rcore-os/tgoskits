// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_connector.c`.
//!
//! Connector lifecycle (`__drm_mode_object_add` based init, dynamic and
//! managed variants, deferred free through `connector_free_work`), registration
//! state machine, the connector type/status/force/subpixel name tables, the
//! connector list iterator, and the standard, DVI-I, DP, TV, scaling, VRR,
//! colorspace, HDR and privacy-screen property sets, plus the connector and
//! tile-group ioctls.
//!
//! Deviations (see also the notes at each affected function):
//! - Variadic format helpers are not expanded; plain names are duplicated.
//! - The debugfs and sysfs hooks are omitted (CONFIG_DEBUG_FS / sysfs off).
//! - Linux `video=` boot-option discovery is not part of this component.
//!   Hosts may use the shared mode parser before publishing connector policy.
//! - `i2c_adapter` arguments are passed as opaque `*mut c_void`.

use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_void},
    mem::{MaybeUninit, offset_of, size_of},
    sync::atomic::{AtomicBool, Ordering},
};

use drm_kapi::{
    assert::warn_on,
    config::{EFAULT, EINVAL, ENOENT, ENOMEM, EOPNOTSUPP, GFP_KERNEL},
    drm_err,
    idr::{Ida, ida_alloc_range, ida_destroy, ida_free, ida_init},
    list::{init_list_head, list_add_tail, list_del, list_del_init, list_empty},
    locks::{spin_lock_irq, spin_lock_irqsave, spin_unlock_irq, spin_unlock_irqrestore},
    memory::{kfree, kzalloc},
    mm_native::copy_to_user_c,
    mutex::mutex_init,
    types::{ListHead, LlistNode, Mutex, WorkStruct},
};

use crate::{
    drm_connector_types::*,
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_encoder_upstream::{drm_encoder_mask, kasprintf_name_id},
    drm_file_types::DrmFile,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{
        __drm_mode_object_add, DRIVER_ATOMIC, DRIVER_MODESET, drm_core_check_feature,
        drm_drv_uses_atomic_modeset, drm_mode_object_find, drm_mode_object_get,
        drm_mode_object_get_properties, drm_mode_object_put, drm_mode_object_register,
        drm_mode_object_unregister, drm_object_attach_property, drm_object_property_set_value,
        idr_alloc, idr_remove, mutex_lock, mutex_unlock, obj_to_connector, put_user,
    },
    drm_modes_types::DrmDisplayMode,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_property_types::{DrmPropEnumList, DrmProperty},
    drm_property_upstream::{
        drm_property_add_enum, drm_property_create, drm_property_create_bool,
        drm_property_create_enum, drm_property_create_range, drm_property_destroy,
        drm_property_replace_global_blob,
    },
    uapi_drm_consts::*,
    uapi_drm_mode_types::*,
};

/// `list_entry(link, struct drm_encoder, head)`.
/// # Safety
///
/// `link` must point to the `head` member of a valid `DrmEncoder` object.
unsafe fn encoder_from_head(link: *mut ListHead) -> *mut DrmEncoder {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a valid `DrmEncoder`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmEncoder, head))
            .cast::<DrmEncoder>()
    }
}

/// `DRM_CONNECTOR_HDMI_VENDOR_LEN` / `_PRODUCT_LEN` (drm_connector.h).
const DRM_CONNECTOR_HDMI_VENDOR_LEN: usize = 8;
const DRM_CONNECTOR_HDMI_PRODUCT_LEN: usize = 16;

// Cross-batch callees: mode parsing and EDID/mode helpers (drm_modes.c,
// drm_edid.c), managed release (drm_managed.c), leases and DRM core, and the
// peer component interfaces. Boot-option discovery remains host-owned.
// # Safety
//
// External C ABI symbol declarations linked from kernel or runtime modules.
unsafe extern "C" {
    fn __drmm_add_action_or_reset(
        dev: *mut DrmDevice,
        action: unsafe extern "C" fn(*mut DrmDevice, *mut c_void),
        data: *mut c_void,
        name: *const c_char,
    ) -> i32;
    fn drm_modeset_lock(lock: *mut DrmModesetLock, ctx: *mut DrmModesetAcquireCtx) -> i32;
    fn drm_modeset_unlock(lock: *mut DrmModesetLock);
    fn drm_is_current_master(file_priv: *mut DrmFile) -> bool;
    fn drm_mode_destroy(dev: *mut DrmDevice, mode: *mut DrmDisplayMode);
    fn drm_mode_match(
        mode1: *const DrmDisplayMode,
        mode2: *const DrmDisplayMode,
        match_flags: u32,
    ) -> bool;
    fn drm_mode_convert_to_umode(out: *mut DrmModeModeinfo, mode: *const DrmDisplayMode);
    fn drm_privacy_screen_get_state(
        priv_: *mut c_void,
        sw_state: *mut DrmPrivacyScreenStatus,
        hw_state: *mut DrmPrivacyScreenStatus,
    );
    fn drm_privacy_screen_set_sw_state(priv_: *mut c_void, sw_state: DrmPrivacyScreenStatus)
    -> i32;
    fn drm_get_panel_orientation_quirk(width: i32, height: i32) -> DrmPanelOrientation;
}

/// Logs a DRM KMS debug message (`DRM_DEBUG_KMS()`) through the kapi logger.
macro_rules! drm_dbg_kms {
    ($device:expr, $format:expr $(, $argument:expr)* $(,)?) => {
        drm_kapi::drm_dbg!($device, $format $(, $argument)*)
    };
}

/// `drm_connector_enum_list[]` names, indexed by connector type.
// upstream: drm_connector.c drm_connector_enum_list[]
const DRM_CONNECTOR_ENUM_NAMES: [&[u8]; 21] = [
    b"Unknown\0",
    b"VGA\0",
    b"DVI-I\0",
    b"DVI-D\0",
    b"DVI-A\0",
    b"Composite\0",
    b"SVIDEO\0",
    b"LVDS\0",
    b"Component\0",
    b"DIN\0",
    b"DP\0",
    b"HDMI-A\0",
    b"HDMI-B\0",
    b"TV\0",
    b"eDP\0",
    b"Virtual\0",
    b"DSI\0",
    b"DPI\0",
    b"Writeback\0",
    b"SPI\0",
    b"USB\0",
];

/// The NUL-terminated name of connector type `i` (`drm_connector_enum_list[i].name`).
fn connector_type_name_c(i: usize) -> *const c_char {
    DRM_CONNECTOR_ENUM_NAMES[i].as_ptr().cast()
}

/// The name of connector type `i` without its terminator, for `kasprintf()`.
fn connector_type_name(i: usize) -> &'static [u8] {
    let name = DRM_CONNECTOR_ENUM_NAMES[i];
    &name[..name.len() - 1]
}

/// Per-connector-type IDA table (`drm_conn_prop_enum_list.ida`) and the
/// global connector list used by the fwnode lookup, both guarded by the
/// init flag below (Linux initialises them at module load).
struct ConnectorGlobals {
    ida: UnsafeCell<MaybeUninit<[Ida; 21]>>,
    list: UnsafeCell<MaybeUninit<ListHead>>,
    list_lock: UnsafeCell<MaybeUninit<Mutex>>,
    ready: AtomicBool,
}

// SAFETY: every access is serialised by the kernel-style locks stored in the
// same structure, mirroring the static C objects they replace.
unsafe impl Sync for ConnectorGlobals {}

static CONNECTOR_GLOBALS: ConnectorGlobals = ConnectorGlobals {
    ida: UnsafeCell::new(MaybeUninit::zeroed()),
    list: UnsafeCell::new(MaybeUninit::zeroed()),
    list_lock: UnsafeCell::new(MaybeUninit::zeroed()),
    ready: AtomicBool::new(false),
};

/// Initialises the global IDA table, list and lock on first use.
/// # Safety
///
/// Global initialization function; accesses global IDA instances under atomic one-time execution.
unsafe fn connector_globals_ensure() {
    if CONNECTOR_GLOBALS.ready.load(Ordering::Acquire) {
        return;
    }
    let ida = CONNECTOR_GLOBALS.ida.get().cast::<Ida>();
    for i in 0..21 {
        // SAFETY: `ida` points to the `[Ida; 21]` table in CONNECTOR_GLOBALS and `i < 21`, so `ida.add(i)` is in bounds, per the function's one-time initialisation contract.
        ida_init(unsafe { &mut *ida.add(i) });
    }
    // SAFETY: `CONNECTOR_GLOBALS.list` is static storage for a `ListHead` that is initialised in place here, before the ready flag is set, per the function's one-time initialisation contract.
    unsafe { init_list_head(CONNECTOR_GLOBALS.list.get().cast()) };
    // SAFETY: `CONNECTOR_GLOBALS.list_lock` is static storage for a `Mutex` that is initialised in place here, before any use, per the function's one-time initialisation contract.
    unsafe { mutex_init(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>()) };
    CONNECTOR_GLOBALS.ready.store(true, Ordering::Release);
}

// upstream: drm_connector.c drm_connector_ida_init()
#[unsafe(no_mangle)]
/// # Safety
/// No pointer parameters. Must run before any connector IDA allocation; the static
/// `CONNECTOR_GLOBALS` table is initialised on first use, so repeat calls are harmless.
pub unsafe extern "C" fn drm_connector_ida_init() {
    // SAFETY: connector_globals_ensure() only initialises the static CONNECTOR_GLOBALS table; no caller pointer is dereferenced.
    unsafe {
        connector_globals_ensure();
    }
}

// upstream: drm_connector.c drm_connector_ida_destroy()
#[unsafe(no_mangle)]
/// # Safety
/// No pointer parameters. Must be called only after all connectors have been released,
/// since it destroys the 21 per-type IDAs in `CONNECTOR_GLOBALS` and any live ID would be lost.
pub unsafe extern "C" fn drm_connector_ida_destroy() {
    // SAFETY: The loop bound 21 matches the 21 `Ida` slots of the static `ida` array, so `ida.add(i)` stays in bounds.
    unsafe {
        connector_globals_ensure();
        let ida = CONNECTOR_GLOBALS.ida.get().cast::<Ida>();
        for i in 0..21 {
            ida_destroy(&mut *ida.add(i));
        }
    }
}

/// `drm_connector_enum_list[type].ida`.
/// # Safety
///
/// `connector_type` must be a valid DRM connector type value.
unsafe fn connector_type_ida(connector_type: i32) -> *mut Ida {
    // SAFETY: Ensure connector global IDA allocators are initialized before table lookup.
    unsafe {
        connector_globals_ensure();
        CONNECTOR_GLOBALS
            .ida
            .get()
            .cast::<Ida>()
            .add(connector_type as usize)
    }
}

/// `ida_alloc_max()` (include/linux/idr.h): `[0, max]`.
/// # Safety
///
/// `ida` must point to an initialized, valid `Ida` allocator.
unsafe fn ida_alloc_max(ida: *mut Ida, max: u32, gfp: u32) -> i32 {
    // SAFETY: `ida` is a valid pointer to an initialized `Ida` allocator.
    unsafe { ida_alloc_range(&mut *ida, 0, max, gfp) }
}

/// `ida_alloc_min()` (include/linux/idr.h): `[min, INT_MAX]`.
/// # Safety
///
/// `ida` must point to an initialized, valid `Ida` allocator.
unsafe fn ida_alloc_min(ida: *mut Ida, min: u32, gfp: u32) -> i32 {
    // SAFETY: `ida` is a valid pointer to an initialized `Ida` allocator.
    unsafe { ida_alloc_range(&mut *ida, min, i32::MAX as u32, gfp) }
}

// upstream: drm_connector.c drm_get_connector_type_name()
#[unsafe(no_mangle)]
/// # Safety
/// `r#type` is a plain integer. Out-of-range values yield NULL rather than an error;
/// the returned string is static and must not be freed by the caller.
pub unsafe extern "C" fn drm_get_connector_type_name(r#type: u32) -> *const c_char {
    if (r#type as usize) < DRM_CONNECTOR_ENUM_NAMES.len() {
        connector_type_name_c(r#type as usize)
    } else {
        core::ptr::null()
    }
}

/// `container_of(kref, struct drm_connector, base.refcount)`.
/// # Safety
///
/// `kref` must point to the `kref` member embedded in a valid `DrmConnector`.
unsafe fn connector_from_kref(kref: *mut drm_kapi::types::Kref) -> *mut DrmConnector {
    // SAFETY: per the function's safety contract, `kref` is the `kref` member of a valid `DrmConnector`, so stepping back by the `base.refcount` and `base` offsets stays within that object.
    unsafe {
        kref.cast::<u8>()
            .sub(offset_of!(DrmModeObject, refcount))
            .sub(offset_of!(DrmConnector, base))
            .cast::<DrmConnector>()
    }
}

/// `container_of(link, struct drm_connector, free_node)` for llist members.
/// # Safety
///
/// `node` must point to the `free_node` member embedded in a valid `DrmConnector`.
unsafe fn connector_from_free_node(node: *mut LlistNode) -> *mut DrmConnector {
    // SAFETY: per the function's safety contract, `node` is the `free_node` member of a valid `DrmConnector`, so stepping back by its offset stays within that object.
    unsafe {
        node.cast::<u8>()
            .sub(offset_of!(DrmConnector, free_node))
            .cast::<DrmConnector>()
    }
}

/// `container_of(link, struct drm_connector, head)`.
/// # Safety
///
/// `link` must point to the `head` member embedded in a valid `DrmConnector`.
unsafe fn connector_from_head(link: *mut ListHead) -> *mut DrmConnector {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a valid `DrmConnector`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmConnector, head))
            .cast::<DrmConnector>()
    }
}

// upstream: drm_connector.c drm_connector_free()
/// # Safety
///
/// `kref` must point to the `kref` member of an allocated `DrmConnector` whose refcount reached zero.
unsafe extern "C" fn drm_connector_free(kref: *mut drm_kapi::types::Kref) {
    // SAFETY: `kref` is embedded in a live `DrmConnector`; retrieve pointer and schedule cleanup.
    unsafe {
        let connector = connector_from_kref(kref);
        let dev = (*connector).dev;

        drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*connector).base));
        if let Some(destroy) = (*(*connector).funcs).destroy {
            destroy(connector);
        }
    }
}

// upstream: drm_connector.c drm_connector_free_work_fn()
/// # Safety
/// `work` must be the `connector_free_work` member of a live `DrmModeConfig` embedded in
/// a `DrmDevice`, as queued by the connector free path. The caller hands ownership of that
/// work item to this function; it takes `connector_list_lock` before draining the free list.
pub unsafe extern "C" fn drm_connector_free_work_fn(work: *mut WorkStruct) {
    // SAFETY: `work` is the `connector_free_work` field of a live DrmModeConfig inside the DrmDevice that scheduled it, so the offset_of subtraction stays inside that allocation.
    unsafe {
        // container_of(work, struct drm_device, mode_config.connector_free_work)
        let config = work
            .cast::<u8>()
            .sub(offset_of!(
                crate::drm_mode_config_types::DrmModeConfig,
                connector_free_work
            ))
            .cast::<crate::drm_mode_config_types::DrmModeConfig>();
        let dev = (config.cast::<u8>())
            .sub(offset_of!(DrmDevice, mode_config))
            .cast::<DrmDevice>();
        let config_ptr = core::ptr::addr_of_mut!((*dev).mode_config);

        let mut flags: u64 = 0;
        spin_lock_irqsave(&mut (*config_ptr).connector_list_lock, &mut flags);
        let freed = drm_kapi::list::llist_del_all(core::ptr::addr_of_mut!(
            (*config_ptr).connector_free_list
        ));
        spin_unlock_irqrestore(&mut (*config_ptr).connector_list_lock, flags);

        // llist_for_each_entry_safe(connector, n, freed, free_node)
        let mut node = freed;
        while !node.is_null() {
            let next = (*node).next;
            let connector = connector_from_free_node(node);
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*connector).base));
            if let Some(destroy) = (*(*connector).funcs).destroy {
                destroy(connector);
            }
            node = next;
        }
    }
}

/// Linux `drm_connector_init_only()`: object, IDs, names and default
/// properties, without linking the connector into the device list.
// upstream: drm_connector.c drm_connector_init_only()
/// # Safety
///
/// `dev`, `connector`, and `funcs` must point to valid, live memory for their respective types.
unsafe fn drm_connector_init_only(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorFuncs,
    connector_type: i32,
    ddc: *mut c_void,
) -> i32 {
    // SAFETY: Initialize connector mode object, mutexes, lists, and properties under caller ownership.
    unsafe {
        if connector_type == DRM_MODE_CONNECTOR_WRITEBACK as i32 {
            return -drm_kapi::config::EOPNOTSUPP;
        }
        let config = core::ptr::addr_of_mut!((*dev).mode_config);
        let mut ret: i32;
        let connector_ida = connector_type_ida(connector_type);

        warn_on(
            drm_drv_uses_atomic_modeset(dev)
                && ((*funcs).atomic_destroy_state.is_none()
                    || (*funcs).atomic_duplicate_state.is_none()),
        );

        ret = __drm_mode_object_add(
            dev,
            core::ptr::addr_of_mut!((*connector).base),
            DRM_MODE_OBJECT_CONNECTOR,
            false,
            Some(drm_connector_free),
        );
        if ret != 0 {
            return ret;
        }

        (*connector).base.properties = core::ptr::addr_of_mut!((*connector).properties);
        (*connector).dev = dev;
        (*connector).funcs = funcs.cast_mut();

        ret = ida_alloc_max(
            core::ptr::addr_of_mut!((*config).connector_ida),
            31,
            GFP_KERNEL,
        );
        if ret < 0 {
            drm_dbg_kms!(
                dev,
                "Failed to allocate %s connector index: %d\n",
                connector_type_name_c(connector_type as usize),
                ret
            );
            // out_put:
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*connector).base));
            return ret;
        }
        (*connector).index = ret as u32;

        (*connector).connector_type = connector_type;
        (*connector).connector_type_id = ida_alloc_min(connector_ida, 1, GFP_KERNEL);
        if (*connector).connector_type_id < 0 {
            ret = (*connector).connector_type_id;
            // out_put_id:
            ida_free(&mut (*config).connector_ida, (*connector).index);
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*connector).base));
            return ret;
        }

        (*connector).name = kasprintf_name_id(
            connector_type_name(connector_type as usize),
            (*connector).connector_type_id as u32,
        );
        if (*connector).name.is_null() {
            ret = -ENOMEM;
            // out_put_type_id:
            ida_free(&mut *connector_ida, (*connector).connector_type_id as u32);
            ida_free(&mut (*config).connector_ida, (*connector).index);
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*connector).base));
            return ret;
        }

        (*connector).ddc = ddc;
        init_list_head(core::ptr::addr_of_mut!((*connector).head));
        init_list_head(core::ptr::addr_of_mut!(
            (*connector).global_connector_list_entry
        ));
        init_list_head(core::ptr::addr_of_mut!((*connector).probed_modes));
        init_list_head(core::ptr::addr_of_mut!((*connector).modes));
        mutex_init(core::ptr::addr_of_mut!((*connector).mutex));
        mutex_init(core::ptr::addr_of_mut!((*connector).cec.mutex));
        mutex_init(core::ptr::addr_of_mut!((*connector).eld_mutex));
        mutex_init(core::ptr::addr_of_mut!((*connector).edid_override_mutex));
        mutex_init(core::ptr::addr_of_mut!((*connector).hdmi.infoframes.lock));
        mutex_init(core::ptr::addr_of_mut!((*connector).hdmi_audio.lock));

        (*connector).edid_blob_ptr = core::ptr::null_mut();
        (*connector).epoch_counter = 0;
        (*connector).tile_blob_ptr = core::ptr::null_mut();
        (*connector).status = CONNECTOR_STATUS_UNKNOWN;
        (*connector).display_info.panel_orientation = DRM_MODE_PANEL_ORIENTATION_UNKNOWN;

        if connector_type != DRM_MODE_CONNECTOR_VIRTUAL as i32
            && connector_type != DRM_MODE_CONNECTOR_WRITEBACK as i32
        {
            drm_connector_attach_edid_property(connector);
        }

        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*config).dpms_property,
            0,
        );
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*config).link_status_property,
            0,
        );
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*config).non_desktop_property,
            0,
        );
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*config).tile_property,
            0,
        );
        if drm_core_check_feature(dev, DRIVER_ATOMIC) {
            drm_object_attach_property(
                core::ptr::addr_of_mut!((*connector).base),
                (*config).prop_crtc_id,
                0,
            );
        }

        (*connector).debugfs_entry = core::ptr::null_mut();

        0
    }
}

// upstream: drm_connector.c drm_connector_add()
/// # Safety
///
/// `connector` must point to an initialized, live `DrmConnector`.
unsafe fn drm_connector_add(connector: *mut DrmConnector) {
    // SAFETY: `connector` is valid; acquire mode_config lock and register connector into device list.
    unsafe {
        let dev = (*connector).dev;
        let config = core::ptr::addr_of_mut!((*dev).mode_config);

        if warn_on(!list_empty(&(*connector).head)) {
            return;
        }

        spin_lock_irq(&mut (*config).connector_list_lock);
        list_add_tail(
            core::ptr::addr_of_mut!((*connector).head),
            core::ptr::addr_of_mut!((*config).connector_list),
        );
        (*config).num_connector += 1;
        spin_unlock_irq(&mut (*config).connector_list_lock);
    }
}

// upstream: drm_connector.c drm_connector_remove()
/// # Safety
///
/// `connector` must point to an initialized, live `DrmConnector`.
unsafe fn drm_connector_remove(connector: *mut DrmConnector) {
    // SAFETY: `connector` is valid; acquire mode_config lock and unregister connector from device list.
    unsafe {
        let dev = (*connector).dev;

        if list_empty(&(*connector).head) {
            return;
        }

        spin_lock_irq(&mut (*dev).mode_config.connector_list_lock);
        list_del_init(core::ptr::addr_of_mut!((*connector).head));
        (*dev).mode_config.num_connector -= 1;
        spin_unlock_irq(&mut (*dev).mode_config.connector_list_lock);
    }
}

// upstream: drm_connector.c drm_connector_init_and_add()
/// # Safety
///
/// `dev`, `connector`, and `funcs` must be valid, live pointers.
unsafe fn drm_connector_init_and_add(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorFuncs,
    connector_type: i32,
    ddc: *mut c_void,
) -> i32 {
    // SAFETY: Initialize connector fields and register into device mode_config.
    unsafe {
        let ret = drm_connector_init_only(dev, connector, funcs, connector_type, ddc);
        if ret != 0 {
            return ret;
        }
        drm_connector_add(connector);
        0
    }
}

// upstream: drm_connector.c drm_connector_init()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice` and `connector` writable storage for a `DrmConnector`
/// that the caller owns and that is not yet registered. `funcs` must be non-null with a
/// `destroy` callback set; no DDC adapter is attached.
pub unsafe extern "C" fn drm_connector_init(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorFuncs,
    connector_type: i32,
) -> i32 {
    // SAFETY: Validate callbacks and initialize connector with dynamic registration flag set.
    unsafe {
        if warn_on(!(!funcs.is_null() && (*funcs).destroy.is_some())) {
            return -EINVAL;
        }
        drm_connector_init_and_add(dev, connector, funcs, connector_type, core::ptr::null_mut())
    }
}

// upstream: drm_connector.c drm_connector_dynamic_init()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be live, `connector` must be writable and unregistered, and `funcs` must be
/// non-null with `destroy` set. `ddc` may be NULL; when non-null it is stored in
/// `connector.ddc` without being owned, so the adapter must outlive the connector.
pub unsafe extern "C" fn drm_connector_dynamic_init(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorFuncs,
    connector_type: i32,
    ddc: *mut c_void,
) -> i32 {
    // SAFETY: Validate callbacks and initialize connector with dynamic registration flag set.
    unsafe {
        if warn_on(!(!funcs.is_null() && (*funcs).destroy.is_some())) {
            return -EINVAL;
        }
        drm_connector_init_only(dev, connector, funcs, connector_type, ddc)
    }
}

// upstream: drm_connector.c drm_connector_init_with_ddc()
#[unsafe(no_mangle)]
/// # Safety
/// Same requirements for `dev`, `connector` and `funcs` as `drm_connector_init`.
/// `ddc` may be NULL; when non-null it is stored unowned in `connector.ddc` and must stay
/// valid until `drm_connector_cleanup` runs.
pub unsafe extern "C" fn drm_connector_init_with_ddc(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorFuncs,
    connector_type: i32,
    ddc: *mut c_void,
) -> i32 {
    // SAFETY: Validate callbacks and initialize connector with optional DDC adapter.
    unsafe {
        if warn_on(!(!funcs.is_null() && (*funcs).destroy.is_some())) {
            return -EINVAL;
        }
        drm_connector_init_and_add(dev, connector, funcs, connector_type, ddc)
    }
}

// upstream: drm_connector.c drm_connector_cleanup_action()
/// # Safety
///
/// `ptr` must point to an initialized `DrmConnector` registered with devm.
unsafe extern "C" fn drm_connector_cleanup_action(_dev: *mut DrmDevice, ptr: *mut c_void) {
    // SAFETY: Devres cleanup action callback; clean up connector resources.
    unsafe {
        drm_connector_cleanup(ptr.cast::<DrmConnector>());
    }
}

// upstream: drm_connector.c drmm_connector_init()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be live and `connector` writable storage owned by the caller. `funcs` may be
/// NULL, but if non-null its `destroy` callback must be unset, because the device-managed
/// cleanup action replaces it. On success that action is registered on `dev` with
/// `connector` as its argument, so `connector` must stay allocated until `dev` is released.
/// `ddc` may be NULL.
pub unsafe extern "C" fn drmm_connector_init(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    funcs: *const DrmConnectorFuncs,
    connector_type: i32,
    ddc: *mut c_void,
) -> i32 {
    // SAFETY: Managed connector initialization; registers cleanup action with device.
    unsafe {
        if warn_on(!funcs.is_null() && (*funcs).destroy.is_some()) {
            return -EINVAL;
        }

        let ret = drm_connector_init_and_add(dev, connector, funcs, connector_type, ddc);
        if ret != 0 {
            return ret;
        }

        let ret = __drmm_add_action_or_reset(
            dev,
            drm_connector_cleanup_action,
            connector.cast(),
            c"drm_connector_cleanup_action".as_ptr().cast(),
        );
        if ret != 0 {
            return ret;
        }

        0
    }
}

/// `strtomem_pad()`: copies `src` into a fixed byte array, zero padding.
// upstream: include/linux/string.h strtomem_pad()
/// # Safety
///
/// `dst` must point to writable memory of at least `len` bytes; `src` must point to a valid null-terminated string or be null.
unsafe fn strtomem_pad(dst: *mut u8, len: usize, src: *const c_char) {
    let src = src.cast::<u8>();
    // SAFETY: `dst` points to writable buffer of `len` bytes; copy string bytes and zero-pad remainder.
    unsafe {
        let mut i = 0usize;
        while i < len {
            let c = *src.add(i);
            if c == 0 {
                break;
            }
            *dst.add(i) = c;
            i += 1;
        }
        while i < len {
            *dst.add(i) = 0;
            i += 1;
        }
    }
}

/// `strlen()` on a NUL-terminated C string.
/// # Safety
///
/// `s` must point to a valid null-terminated C string.
unsafe fn c_strlen(s: *const c_char) -> usize {
    // SAFETY: `s` points to a valid null-terminated C string; compute its length.
    unsafe {
        let mut len = 0usize;
        while *s.add(len) != 0 {
            len += 1;
        }
        len
    }
}

// upstream: drm_connector.c drmm_connector_hdmi_init()
#[unsafe(no_mangle)]
/// # Safety
/// `dev`, `connector` and `hdmi_funcs` must be live. `vendor` and `product` must be
/// NUL-terminated strings, and are rejected with -EINVAL if NULL or longer than the
/// HDMI vendor/product limits; they are copied into the connector's HDMI state. `funcs`
/// and `ddc` follow the same rules as in `drmm_connector_init`.
pub unsafe extern "C" fn drmm_connector_hdmi_init(
    dev: *mut DrmDevice,
    connector: *mut DrmConnector,
    vendor: *const c_char,
    product: *const c_char,
    funcs: *const DrmConnectorFuncs,
    hdmi_funcs: *const DrmConnectorHdmiFuncs,
    connector_type: i32,
    ddc: *mut c_void,
    supported_formats: u64,
    max_bpc: u32,
) -> i32 {
    // SAFETY: Initialize HDMI connector properties, color spaces, and audio capabilities.
    unsafe {
        if vendor.is_null() || product.is_null() {
            return -EINVAL;
        }

        if c_strlen(vendor) > DRM_CONNECTOR_HDMI_VENDOR_LEN
            || c_strlen(product) > DRM_CONNECTOR_HDMI_PRODUCT_LEN
        {
            return -EINVAL;
        }

        if !(connector_type == DRM_MODE_CONNECTOR_HDMIA as i32
            || connector_type == DRM_MODE_CONNECTOR_HDMIB as i32)
        {
            return -EINVAL;
        }

        if supported_formats == 0
            || supported_formats & (1u64 << DRM_OUTPUT_COLOR_FORMAT_RGB444) == 0
        {
            return -EINVAL;
        }

        if (*connector).ycbcr_420_allowed
            != (supported_formats & (1u64 << DRM_OUTPUT_COLOR_FORMAT_YCBCR420) != 0)
        {
            return -EINVAL;
        }

        if !(max_bpc == 8 || max_bpc == 10 || max_bpc == 12) {
            return -EINVAL;
        }

        if (*hdmi_funcs).avi.clear_infoframe.is_none()
            || (*hdmi_funcs).avi.write_infoframe.is_none()
            || (*hdmi_funcs).hdmi.clear_infoframe.is_none()
            || (*hdmi_funcs).hdmi.write_infoframe.is_none()
        {
            return -EINVAL;
        }

        let ret = drmm_connector_init(dev, connector, funcs, connector_type, ddc);
        if ret != 0 {
            return ret;
        }

        (*connector).hdmi.supported_formats = supported_formats;
        strtomem_pad(
            (*connector).hdmi.vendor.as_mut_ptr(),
            DRM_CONNECTOR_HDMI_VENDOR_LEN,
            vendor,
        );
        strtomem_pad(
            (*connector).hdmi.product.as_mut_ptr(),
            DRM_CONNECTOR_HDMI_PRODUCT_LEN,
            product,
        );

        if let Some(reset) = (*(*connector).funcs).reset {
            reset(connector);
        }

        drm_connector_attach_max_bpc_property(connector, 8, max_bpc as i32);
        (*connector).max_bpc = max_bpc;

        if max_bpc > 8 {
            drm_connector_attach_hdr_output_metadata_property(connector);
        }

        (*connector).hdmi.funcs = hdmi_funcs.cast_mut();

        0
    }
}

// upstream: drm_connector.c drm_connector_attach_edid_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be a live connector whose `dev` back-pointer is set. The EDID property
/// is read from `dev.mode_config` and attached to `connector.base`; the caller holds
/// no lock here and must not race with property creation on the same device.
pub unsafe extern "C" fn drm_connector_attach_edid_property(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live and its `dev` back-pointer is set at init; `mode_config` is accessed in place through `addr_of_mut!`.
    unsafe {
        let config = core::ptr::addr_of_mut!((*(*connector).dev).mode_config);
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*config).edid_property,
            0,
        );
    }
}

// upstream: drm_connector.c drm_connector_attach_encoder()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and must not already have an encoder set; if it does, the
/// function warns and returns -EINVAL without changing state. `encoder` must be a live
/// `DrmEncoder` whose index is used to build the possible-encoder mask; the connector
/// does not take ownership of it.
pub unsafe extern "C" fn drm_connector_attach_encoder(
    connector: *mut DrmConnector,
    encoder: *mut DrmEncoder,
) -> i32 {
    // SAFETY: `connector` is live; `encoder` is only forwarded to `drm_encoder_mask`, and the caller keeps it alive for the call.
    unsafe {
        if warn_on(!(*connector).encoder.is_null()) {
            return -EINVAL;
        }
        (*connector).possible_encoders |= crate::drm_encoder_upstream::drm_encoder_mask(encoder);
        0
    }
}

// upstream: drm_connector.c drm_connector_has_possible_encoder()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and `encoder` must be a live `DrmEncoder`. Neither pointer is
/// stored; the function only reads the connector's possible-encoder mask and the encoder's
/// index bit.
pub unsafe extern "C" fn drm_connector_has_possible_encoder(
    connector: *mut DrmConnector,
    encoder: *mut DrmEncoder,
) -> bool {
    // SAFETY: `connector` is valid; check if encoder mask intersects possible encoders.
    unsafe {
        (*connector).possible_encoders & crate::drm_encoder_upstream::drm_encoder_mask(encoder) != 0
    }
}

// upstream: drm_connector.c drm_mode_remove()
/// # Safety
///
/// `connector` and `mode` must point to live display mode objects.
unsafe fn drm_mode_remove(connector: *mut DrmConnector, mode: *mut DrmDisplayMode) {
    // SAFETY: `mode` is an intrusive list node in connector mode list; unlink it.
    unsafe {
        list_del(core::ptr::addr_of_mut!((*mode).head));
        drm_mode_destroy((*connector).dev, mode);
    }
}

// upstream: drm_connector.c drm_connector_cec_phys_addr_invalidate()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and its `cec.mutex` must be initialised. The function takes
/// `cec.mutex` itself and calls the driver's `phys_addr_invalidate` callback, if any, with
/// the same connector pointer while holding that lock.
pub unsafe extern "C" fn drm_connector_cec_phys_addr_invalidate(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live; `cec.mutex` is held around the read of `cec.funcs` and the callback, which receives the same connector pointer.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*connector).cec.mutex));
        if !(*connector).cec.funcs.is_null()
            && let Some(phys_addr_invalidate) = (*(*connector).cec.funcs).phys_addr_invalidate
        {
            phys_addr_invalidate(connector);
        }
        mutex_unlock(core::ptr::addr_of_mut!((*connector).cec.mutex));
    }
}

// upstream: drm_connector.c drm_connector_cec_phys_addr_set()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with an initialised `cec.mutex`. The physical address is read
/// from `display_info` under `cec.mutex`, and the driver's `phys_addr_set` callback is
/// invoked while that lock is held; the caller must not hold `cec.mutex` beforehand.
pub unsafe extern "C" fn drm_connector_cec_phys_addr_set(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live; `source_physical_address` is read under `cec.mutex`, which also guards the `phys_addr_set` callback.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*connector).cec.mutex));
        let addr = (*connector).display_info.source_physical_address;
        if !(*connector).cec.funcs.is_null()
            && let Some(phys_addr_set) = (*(*connector).cec.funcs).phys_addr_set
        {
            phys_addr_set(connector, addr);
        }
        mutex_unlock(core::ptr::addr_of_mut!((*connector).cec.mutex));
    }
}

// upstream: drm_connector.c drm_connector_cleanup()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be a live connector previously set up by one of the init functions,
/// with no outstanding references. If it is still registered, it is unregistered first.
/// On return the connector's memory is no longer used by the DRM core, but the caller
/// still owns the storage and frees it.
pub unsafe extern "C" fn drm_connector_cleanup(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live and owns the `dev` back-pointer read here; `unregister` runs only while the connector is still registered.
    unsafe {
        let dev = (*connector).dev;

        if warn_on((*connector).registration_state == DRM_CONNECTOR_REGISTERED) {
            drm_connector_unregister(connector);
        }

        // platform_device_unregister(codec_pdev): HDMI audio codec devices are
        // not created on this target, so the pointer is NULL here.
        // privacy_screen_put(): the privacy screen is never attached on this target.
        // fwnode_handle_put(): fwnode is NULL on this target.

        if !(*connector).tile_group.is_null() {
            drm_mode_put_tile_group(dev, (*connector).tile_group);
            (*connector).tile_group = core::ptr::null_mut();
        }

        let head = core::ptr::addr_of_mut!((*connector).probed_modes);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            drm_mode_remove(
                connector,
                cur.cast::<u8>()
                    .sub(offset_of!(DrmDisplayMode, head))
                    .cast(),
            );
            cur = next;
        }

        let head = core::ptr::addr_of_mut!((*connector).modes);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            drm_mode_remove(
                connector,
                cur.cast::<u8>()
                    .sub(offset_of!(DrmDisplayMode, head))
                    .cast(),
            );
            cur = next;
        }

        ida_free(
            &mut *connector_type_ida((*connector).connector_type),
            (*connector).connector_type_id as u32,
        );
        ida_free(&mut (*dev).mode_config.connector_ida, (*connector).index);

        kfree((*connector).display_info.bus_formats);
        kfree((*connector).display_info.vics);

        drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*connector).base));

        kfree((*connector).name);
        (*connector).name = core::ptr::null_mut();

        drm_connector_remove(connector);

        warn_on(
            !(*connector).state.is_null() && (*(*connector).funcs).atomic_destroy_state.is_none(),
        );
        if !(*connector).state.is_null()
            && let Some(atomic_destroy_state) = (*(*connector).funcs).atomic_destroy_state
        {
            atomic_destroy_state(connector, (*connector).state);
        }

        mutex_destroy_c(core::ptr::addr_of_mut!((*connector).hdmi_audio.lock));
        mutex_destroy_c(core::ptr::addr_of_mut!((*connector).hdmi.infoframes.lock));
        mutex_destroy_c(core::ptr::addr_of_mut!((*connector).mutex));

        core::ptr::write_bytes(connector.cast::<u8>(), 0, size_of::<DrmConnector>());

        // drm_sysfs_hotplug_event(dev) when registered: sysfs is not built.
    }
}

/// `mutex_destroy()` on a raw mutex pointer.
/// # Safety
///
/// `lock` must point to an initialized `Mutex` being destroyed.
unsafe fn mutex_destroy_c(lock: *mut Mutex) {
    // SAFETY: `lock` points to an initialized `Mutex`; finalize it.
    unsafe {
        drm_kapi::mutex::mutex_destroy(&mut *lock);
    }
}

// upstream: drm_connector.c drm_connector_register()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and initialised. The function takes `connector.mutex` and
/// then `list_lock`, and returns early with 0 if the device is not yet registered. The
/// caller must not hold either lock. On error the registration state is rolled back.
pub unsafe extern "C" fn drm_connector_register(connector: *mut DrmConnector) -> i32 {
    // SAFETY: `connector` is live and `dev` is its back-pointer; `connector.mutex` is taken before the state check and released on every exit path through `'unlock`.
    unsafe {
        let mut ret = 0;

        if !(*(*connector).dev).registered {
            return 0;
        }

        mutex_lock(core::ptr::addr_of_mut!((*connector).mutex));
        'unlock: {
            if (*connector).registration_state != DRM_CONNECTOR_INITIALIZING {
                break 'unlock;
            }

            // sysfs/debugfs connector registration is not built on this target.

            if let Some(late_register) = (*(*connector).funcs).late_register {
                ret = late_register(connector);
                if ret != 0 {
                    break 'unlock;
                }
            }

            drm_mode_object_register((*connector).dev, core::ptr::addr_of_mut!((*connector).base));
            (*connector).registration_state = DRM_CONNECTOR_REGISTERED;

            // privacy-screen notifier registration: no privacy screen on this target.

            connector_globals_ensure();
            mutex_lock(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>());
            list_add_tail(
                core::ptr::addr_of_mut!((*connector).global_connector_list_entry),
                CONNECTOR_GLOBALS.list.get().cast::<ListHead>(),
            );
            mutex_unlock(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>());
        }
        mutex_unlock(core::ptr::addr_of_mut!((*connector).mutex));
        ret
    }
}

// upstream: drm_connector.c drm_connector_dynamic_register()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and must have `funcs` set with a `destroy` callback, since
/// the connector is added to the global list before registration. The caller must not
/// hold `list_lock` or the connector mutex.
pub unsafe extern "C" fn drm_connector_dynamic_register(connector: *mut DrmConnector) -> i32 {
    // SAFETY: `connector` is live; `funcs` is checked non-null before `destroy` is inspected, and the add/register helpers take their own locks.
    unsafe {
        let ok = !(*connector).funcs.is_null() && (*(*connector).funcs).destroy.is_some();
        if warn_on(!ok) {
            return -EINVAL;
        }
        drm_connector_add(connector);
        drm_connector_register(connector)
    }
}

// upstream: drm_connector.c drm_connector_unregister()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live. If it is not in the registered state the call is a no-op
/// after taking `connector.mutex`. Otherwise it removes the connector from the global
/// list under `list_lock` and must not be called while the caller holds either lock.
pub unsafe extern "C" fn drm_connector_unregister(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live; `connector.mutex` serialises the state check and `list_lock` guards the unlink from the global connector list.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*connector).mutex));
        if (*connector).registration_state != DRM_CONNECTOR_REGISTERED {
            mutex_unlock(core::ptr::addr_of_mut!((*connector).mutex));
            return;
        }

        connector_globals_ensure();
        mutex_lock(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>());
        list_del_init(core::ptr::addr_of_mut!(
            (*connector).global_connector_list_entry
        ));
        mutex_unlock(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>());

        // privacy-screen notifier removal and sysfs/debugfs teardown: not built.

        if let Some(early_unregister) = (*(*connector).funcs).early_unregister {
            early_unregister(connector);
        }

        (*connector).registration_state = DRM_CONNECTOR_UNREGISTERED;
        mutex_unlock(core::ptr::addr_of_mut!((*connector).mutex));
    }
}

// upstream: drm_connector.c drm_connector_unregister_all()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live device. Each connector returned by the list iterator is a live
/// member of `dev`'s connector list, and is unregistered while the iterator is active.
/// The caller must not hold the connector list lock.
pub unsafe extern "C" fn drm_connector_unregister_all(dev: *mut DrmDevice) {
    // SAFETY: `dev` is live; `conn_iter` is zeroed stack storage set up by `list_iter_begin`, and each connector from `list_iter_next` is a live member of `dev`.
    unsafe {
        let mut conn_iter = core::mem::MaybeUninit::<DrmConnectorListIter>::zeroed();
        let conn_iter = conn_iter.as_mut_ptr();
        drm_connector_list_iter_begin(dev, conn_iter);
        loop {
            let connector = drm_connector_list_iter_next(conn_iter);
            if connector.is_null() {
                break;
            }
            drm_connector_unregister(connector);
        }
        drm_connector_list_iter_end(conn_iter);
    }
}

// upstream: drm_connector.c drm_connector_register_all()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live device. The function iterates its connector list and registers
/// each connector; on the first error the already-registered ones are unregistered again,
/// so the caller must not depend on partial state.
pub unsafe extern "C" fn drm_connector_register_all(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `conn_iter` is zeroed stack storage passed to the list iterator, which holds the connector list lock while yielding each connector.
    unsafe {
        let mut conn_iter = core::mem::MaybeUninit::<DrmConnectorListIter>::zeroed();
        let conn_iter = conn_iter.as_mut_ptr();
        let mut ret = 0;

        drm_connector_list_iter_begin(dev, conn_iter);
        loop {
            let connector = drm_connector_list_iter_next(conn_iter);
            if connector.is_null() {
                break;
            }
            ret = drm_connector_register(connector);
            if ret != 0 {
                break;
            }
        }
        drm_connector_list_iter_end(conn_iter);

        if ret != 0 {
            drm_connector_unregister_all(dev);
        }
        ret
    }
}

/// One `struct drm_prop_enum_list` entry as a Sync table row; the name is
/// NUL-terminated for C consumers.
type EnumEntry = (i32, &'static [u8]);

/// Builds a C `drm_prop_enum_list[]` view of a static table on the stack.
// upstream: include/drm/drm_property.h struct drm_prop_enum_list
fn to_prop_enum_list<const N: usize>(table: &[EnumEntry; N]) -> [DrmPropEnumList; N] {
    core::array::from_fn(|i| DrmPropEnumList {
        r#type: table[i].0,
        name: table[i].1.as_ptr().cast::<c_char>() as *mut c_char,
    })
}

/// `DRM_ENUM_NAME_FN()`: the name for `val`, or "(null)".
// upstream: include/drm/drm_property.h DRM_ENUM_NAME_FN()
fn enum_name_fn<const N: usize>(table: &[EnumEntry; N], val: i32) -> *const c_char {
    for &(ty, name) in table.iter() {
        if ty == val {
            return name.as_ptr().cast();
        }
    }
    c"(null)".as_ptr().cast()
}

// upstream: drm_connector.c drm_get_connector_status_name()
#[unsafe(no_mangle)]
pub extern "C" fn drm_get_connector_status_name(status: DrmConnectorStatus) -> *const c_char {
    if status == CONNECTOR_STATUS_CONNECTED {
        c"connected".as_ptr().cast()
    } else if status == CONNECTOR_STATUS_DISCONNECTED {
        c"disconnected".as_ptr().cast()
    } else {
        c"unknown".as_ptr().cast()
    }
}

// upstream: drm_connector.c drm_get_connector_force_name()
#[unsafe(no_mangle)]
pub extern "C" fn drm_get_connector_force_name(force: DrmConnectorForce) -> *const c_char {
    match force {
        DRM_FORCE_UNSPECIFIED => c"unspecified".as_ptr().cast(),
        DRM_FORCE_OFF => c"off".as_ptr().cast(),
        DRM_FORCE_ON => c"on".as_ptr().cast(),
        DRM_FORCE_ON_DIGITAL => c"digital".as_ptr().cast(),
        _ => c"unknown".as_ptr().cast(),
    }
}

// upstream: drm_connector.c drm_connector_list_iter_begin()
#[unsafe(no_mangle)]
/// # Safety
/// `iter` must be writable storage owned by the caller and `dev` must be a live device
/// that outlives the iteration. Only `iter.dev` and `iter.conn` are written; `dev` is not
/// dereferenced here.
pub unsafe extern "C" fn drm_connector_list_iter_begin(
    dev: *mut DrmDevice,
    iter: *mut DrmConnectorListIter,
) {
    // SAFETY: `iter` is caller-provided writable stack storage; only its `dev` and `conn` fields are written, and `dev` is stored as a raw pointer without being dereferenced.
    unsafe {
        (*iter).dev = dev;
        (*iter).conn = core::ptr::null_mut();
    }
}

// upstream: drm_connector.c __drm_connector_put_safe()
/// # Safety
///
/// `conn` must point to a live `DrmConnector` holding a valid reference.
unsafe fn __drm_connector_put_safe(conn: *mut DrmConnector) {
    // SAFETY: Drop reference to previous connector under mode_config connector list lock.
    unsafe {
        let config = core::ptr::addr_of_mut!((*(*conn).dev).mode_config);

        if !drm_kapi::memory::refcount_dec_and_test(&(*conn).base.refcount.refcount) {
            return;
        }
        drm_kapi::list::llist_add(
            core::ptr::addr_of_mut!((*conn).free_node),
            core::ptr::addr_of_mut!((*config).connector_free_list),
        );
        drm_kapi::workqueue::queue_work(
            drm_kapi::workqueue::SYSTEM_DFL_WQ,
            core::ptr::addr_of_mut!((*config).connector_free_work),
        );
    }
}

// upstream: drm_connector.c drm_connector_list_iter_next()
#[unsafe(no_mangle)]
/// # Safety
/// `iter` must be a valid iterator set up by `drm_connector_list_iter_begin` and not yet
/// ended. `iter.conn` is the previous connector, if any, and holds a reference that is
/// dropped inside the call. The returned pointer is a connector still on `dev`'s list, or
/// NULL at the end of the list.
pub unsafe extern "C" fn drm_connector_list_iter_next(
    iter: *mut DrmConnectorListIter,
) -> *mut DrmConnector {
    // SAFETY: `iter` is valid and `iter.dev` was set by `drm_connector_list_iter_begin`; `old_conn` is dereferenced only when non-null, and the list is walked under `connector_list_lock`.
    unsafe {
        let old_conn = (*iter).conn;
        let config = core::ptr::addr_of_mut!((*(*iter).dev).mode_config);
        let mut flags: u64 = 0;

        spin_lock_irqsave(&mut (*config).connector_list_lock, &mut flags);
        let list_head_ptr = core::ptr::addr_of_mut!((*config).connector_list);
        let mut lhead: *mut ListHead = if !old_conn.is_null() {
            core::ptr::addr_of_mut!((*old_conn).head)
        } else {
            list_head_ptr
        };
        loop {
            if (*lhead).next == list_head_ptr {
                (*iter).conn = core::ptr::null_mut();
                break;
            }
            lhead = (*lhead).next;
            (*iter).conn = connector_from_head(lhead);
            if drm_kapi::memory::kref_get_unless_zero(&(*(*iter).conn).base.refcount) {
                break;
            }
        }

        if !old_conn.is_null() {
            __drm_connector_put_safe(old_conn);
        }
        spin_unlock_irqrestore(&mut (*config).connector_list_lock, flags);

        (*iter).conn
    }
}

// upstream: drm_connector.c drm_connector_list_iter_end()
#[unsafe(no_mangle)]
/// # Safety
/// `iter` must be a valid iterator from `drm_connector_list_iter_begin`. If it still holds
/// a connector reference it is released under `connector_list_lock`. After the call `iter`
/// must not be passed to the iterator again without a new `begin`.
pub unsafe extern "C" fn drm_connector_list_iter_end(iter: *mut DrmConnectorListIter) {
    // SAFETY: `iter` is valid and `iter.dev` is still set; the reference in `iter.conn` is dropped under `connector_list_lock`, the lock the iterator took.
    unsafe {
        let config = core::ptr::addr_of_mut!((*(*iter).dev).mode_config);
        let mut flags: u64 = 0;

        (*iter).dev = core::ptr::null_mut();
        if !(*iter).conn.is_null() {
            spin_lock_irqsave(&mut (*config).connector_list_lock, &mut flags);
            __drm_connector_put_safe((*iter).conn);
            spin_unlock_irqrestore(&mut (*config).connector_list_lock, flags);
        }
    }
}

// upstream: drm_connector.c drm_get_subpixel_order_name()
static DRM_SUBPIXEL_ENUM_LIST: [EnumEntry; 6] = [
    (SUBPIXELUNKNOWN as i32, b"Unknown\0"),
    (SUBPIXELHORIZONTALRGB as i32, b"Horizontal RGB\0"),
    (SUBPIXELHORIZONTALBGR as i32, b"Horizontal BGR\0"),
    (SUBPIXELVERTICALRGB as i32, b"Vertical RGB\0"),
    (SUBPIXELVERTICALBGR as i32, b"Vertical BGR\0"),
    (SUBPIXELNONE as i32, b"None\0"),
];

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_subpixel_order_name(order: SubpixelOrder) -> *const c_char {
    DRM_SUBPIXEL_ENUM_LIST[order as usize].1.as_ptr().cast()
}

// upstream: drm_connector.c drm_dpms_enum_list[] / drm_get_dpms_name()
static DRM_DPMS_ENUM_LIST: [EnumEntry; 4] = [
    (DRM_MODE_DPMS_ON as i32, b"On\0"),
    (DRM_MODE_DPMS_STANDBY as i32, b"Standby\0"),
    (DRM_MODE_DPMS_SUSPEND as i32, b"Suspend\0"),
    (DRM_MODE_DPMS_OFF as i32, b"Off\0"),
];

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_dpms_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_DPMS_ENUM_LIST, val)
}

// upstream: drm_connector.c drm_link_status_enum_list[]
static DRM_LINK_STATUS_ENUM_LIST: [EnumEntry; 2] = [
    (DRM_MODE_LINK_STATUS_GOOD as i32, b"Good\0"),
    (DRM_MODE_LINK_STATUS_BAD as i32, b"Bad\0"),
];

// upstream: drm_connector.c drm_panel_type_enum_list[]
static DRM_PANEL_TYPE_ENUM_LIST: [EnumEntry; 2] = [
    (DRM_MODE_PANEL_TYPE_UNKNOWN as i32, b"unknown\0"),
    (DRM_MODE_PANEL_TYPE_OLED as i32, b"OLED\0"),
];

// upstream: drm_connector.c drm_display_info_set_bus_formats()
#[unsafe(no_mangle)]
/// # Safety
/// `info` must be a live `DrmDisplayInfo`. `formats` may be NULL only when `num_formats`
/// is 0; otherwise it must point to `num_formats` readable `u32` values, which are copied
/// into a new allocation. The previous `info.bus_formats` buffer is freed and replaced,
/// so the caller must not keep references to the old array.
pub unsafe extern "C" fn drm_display_info_set_bus_formats(
    info: *mut DrmDisplayInfo,
    formats: *const u32,
    num_formats: u32,
) -> i32 {
    // SAFETY: Allocate kernel buffer and update supported bus formats array on display info.
    unsafe {
        let mut fmts: *mut u32 = core::ptr::null_mut();

        if formats.is_null() && num_formats != 0 {
            return -EINVAL;
        }

        if !formats.is_null() && num_formats != 0 {
            let bytes = size_of::<u32>() * num_formats as usize;
            fmts = drm_kapi::memory::kmalloc(bytes, GFP_KERNEL).cast::<u32>();
            if fmts.is_null() {
                return -ENOMEM;
            }
            core::ptr::copy_nonoverlapping(formats, fmts, num_formats as usize);
        }

        kfree((*info).bus_formats);
        (*info).bus_formats = fmts;
        (*info).num_bus_formats = num_formats;

        0
    }
}

// upstream: drm_connector.c scaling/aspect/content/orientation tables
static DRM_SCALING_MODE_ENUM_LIST: [EnumEntry; 4] = [
    (DRM_MODE_SCALE_NONE as i32, b"None\0"),
    (DRM_MODE_SCALE_FULLSCREEN as i32, b"Full\0"),
    (DRM_MODE_SCALE_CENTER as i32, b"Center\0"),
    (DRM_MODE_SCALE_ASPECT as i32, b"Full aspect\0"),
];

static DRM_ASPECT_RATIO_ENUM_LIST: [EnumEntry; 3] = [
    (DRM_MODE_PICTURE_ASPECT_NONE as i32, b"Automatic\0"),
    (DRM_MODE_PICTURE_ASPECT_4_3 as i32, b"4:3\0"),
    (DRM_MODE_PICTURE_ASPECT_16_9 as i32, b"16:9\0"),
];

static DRM_CONTENT_TYPE_ENUM_LIST: [EnumEntry; 5] = [
    (DRM_MODE_CONTENT_TYPE_NO_DATA as i32, b"No Data\0"),
    (DRM_MODE_CONTENT_TYPE_GRAPHICS as i32, b"Graphics\0"),
    (DRM_MODE_CONTENT_TYPE_PHOTO as i32, b"Photo\0"),
    (DRM_MODE_CONTENT_TYPE_CINEMA as i32, b"Cinema\0"),
    (DRM_MODE_CONTENT_TYPE_GAME as i32, b"Game\0"),
];

static DRM_PANEL_ORIENTATION_ENUM_LIST: [EnumEntry; 4] = [
    (DRM_MODE_PANEL_ORIENTATION_NORMAL, b"Normal\0"),
    (DRM_MODE_PANEL_ORIENTATION_BOTTOM_UP, b"Upside Down\0"),
    (DRM_MODE_PANEL_ORIENTATION_LEFT_UP, b"Left Side Up\0"),
    (DRM_MODE_PANEL_ORIENTATION_RIGHT_UP, b"Right Side Up\0"),
];

static DRM_DVI_I_SELECT_ENUM_LIST: [EnumEntry; 3] = [
    (DRM_MODE_SUBCONNECTOR_AUTOMATIC as i32, b"Automatic\0"),
    (DRM_MODE_SUBCONNECTOR_DVID as i32, b"DVI-D\0"),
    (DRM_MODE_SUBCONNECTOR_DVIA as i32, b"DVI-A\0"),
];

static DRM_DVI_I_SUBCONNECTOR_ENUM_LIST: [EnumEntry; 3] = [
    (DRM_MODE_SUBCONNECTOR_UNKNOWN as i32, b"Unknown\0"),
    (DRM_MODE_SUBCONNECTOR_DVID as i32, b"DVI-D\0"),
    (DRM_MODE_SUBCONNECTOR_DVIA as i32, b"DVI-A\0"),
];

static DRM_TV_MODE_ENUM_LIST: [EnumEntry; 8] = [
    (DRM_MODE_TV_MODE_NTSC as i32, b"NTSC\0"),
    (DRM_MODE_TV_MODE_NTSC_443 as i32, b"NTSC-443\0"),
    (DRM_MODE_TV_MODE_NTSC_J as i32, b"NTSC-J\0"),
    (DRM_MODE_TV_MODE_PAL as i32, b"PAL\0"),
    (DRM_MODE_TV_MODE_PAL_M as i32, b"PAL-M\0"),
    (DRM_MODE_TV_MODE_PAL_N as i32, b"PAL-N\0"),
    (DRM_MODE_TV_MODE_SECAM as i32, b"SECAM\0"),
    (DRM_MODE_TV_MODE_MONOCHROME as i32, b"Mono\0"),
];

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_tv_mode_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_TV_MODE_ENUM_LIST, val)
}

// upstream: drm_connector.c drm_get_tv_mode_from_name()
#[unsafe(no_mangle)]
/// # Safety
/// `name` must be readable for `len` bytes when `len` matches a known TV mode name
/// length; it is compared, not stored. The pointer may be unaligned and need not be
/// NUL-terminated.
pub unsafe extern "C" fn drm_get_tv_mode_from_name(name: *const c_char, len: usize) -> i32 {
    // SAFETY: `name` must be readable for `len` bytes; the slice is formed only after the length check against the static item name matches.
    unsafe {
        for &(ty, item) in DRM_TV_MODE_ENUM_LIST.iter() {
            // strlen(item->name) == len && !strncmp(item->name, name, len)
            let item_len = item.len() - 1;
            if item_len == len
                && core::slice::from_raw_parts(name.cast::<u8>(), len) == &item[..len]
            {
                return ty;
            }
        }
        -EINVAL
    }
}

static DRM_TV_SELECT_ENUM_LIST: [EnumEntry; 5] = [
    (DRM_MODE_SUBCONNECTOR_AUTOMATIC as i32, b"Automatic\0"),
    (DRM_MODE_SUBCONNECTOR_COMPOSITE as i32, b"Composite\0"),
    (DRM_MODE_SUBCONNECTOR_SVIDEO as i32, b"SVIDEO\0"),
    (DRM_MODE_SUBCONNECTOR_COMPONENT as i32, b"Component\0"),
    (DRM_MODE_SUBCONNECTOR_SCART as i32, b"SCART\0"),
];

static DRM_TV_SUBCONNECTOR_ENUM_LIST: [EnumEntry; 5] = [
    (DRM_MODE_SUBCONNECTOR_UNKNOWN as i32, b"Unknown\0"),
    (DRM_MODE_SUBCONNECTOR_COMPOSITE as i32, b"Composite\0"),
    (DRM_MODE_SUBCONNECTOR_SVIDEO as i32, b"SVIDEO\0"),
    (DRM_MODE_SUBCONNECTOR_COMPONENT as i32, b"Component\0"),
    (DRM_MODE_SUBCONNECTOR_SCART as i32, b"SCART\0"),
];

static DRM_DP_SUBCONNECTOR_ENUM_LIST: [EnumEntry; 7] = [
    (DRM_MODE_SUBCONNECTOR_UNKNOWN as i32, b"Unknown\0"),
    (DRM_MODE_SUBCONNECTOR_VGA as i32, b"VGA\0"),
    (DRM_MODE_SUBCONNECTOR_DVID as i32, b"DVI-D\0"),
    (DRM_MODE_SUBCONNECTOR_HDMIA as i32, b"HDMI\0"),
    (DRM_MODE_SUBCONNECTOR_DISPLAYPORT as i32, b"DP\0"),
    (DRM_MODE_SUBCONNECTOR_WIRELESS as i32, b"Wireless\0"),
    (DRM_MODE_SUBCONNECTOR_NATIVE as i32, b"Native\0"),
];

// upstream: drm_connector.c colorspace_names[] (sparse, indexed by colorimetry)
const COLORSPACE_NAMES: [&[u8]; DRM_MODE_COLORIMETRY_COUNT as usize] = [
    b"Default\0",
    b"SMPTE_170M_YCC\0",
    b"BT709_YCC\0",
    b"XVYCC_601\0",
    b"XVYCC_709\0",
    b"SYCC_601\0",
    b"opYCC_601\0",
    b"opRGB\0",
    b"BT2020_CYCC\0",
    b"BT2020_RGB\0",
    b"BT2020_YCC\0",
    b"DCI-P3_RGB_D65\0",
    b"DCI-P3_RGB_Theater\0",
    b"RGB_WIDE_FIXED\0",
    b"RGB_WIDE_FLOAT\0",
    b"BT601_YCC\0",
];

// upstream: drm_connector.c drm_get_colorspace_name()
#[unsafe(no_mangle)]
pub extern "C" fn drm_get_colorspace_name(colorspace: DrmColorspace) -> *const c_char {
    if (colorspace as usize) < COLORSPACE_NAMES.len() {
        return COLORSPACE_NAMES[colorspace as usize].as_ptr().cast();
    }
    c"(null)".as_ptr().cast()
}

/// `hdmi_colorspaces` mask (drm_connector.c).
const HDMI_COLORSPACES: u32 = (1 << DRM_MODE_COLORIMETRY_SMPTE_170M_YCC)
    | (1 << DRM_MODE_COLORIMETRY_BT709_YCC)
    | (1 << DRM_MODE_COLORIMETRY_XVYCC_601)
    | (1 << DRM_MODE_COLORIMETRY_XVYCC_709)
    | (1 << DRM_MODE_COLORIMETRY_SYCC_601)
    | (1 << DRM_MODE_COLORIMETRY_OPYCC_601)
    | (1 << DRM_MODE_COLORIMETRY_OPRGB)
    | (1 << DRM_MODE_COLORIMETRY_BT2020_CYCC)
    | (1 << DRM_MODE_COLORIMETRY_BT2020_RGB)
    | (1 << DRM_MODE_COLORIMETRY_BT2020_YCC)
    | (1 << DRM_MODE_COLORIMETRY_DCI_P3_RGB_D65)
    | (1 << DRM_MODE_COLORIMETRY_DCI_P3_RGB_THEATER);

/// `dp_colorspaces` mask (drm_connector.c).
const DP_COLORSPACES: u32 = (1 << DRM_MODE_COLORIMETRY_RGB_WIDE_FIXED)
    | (1 << DRM_MODE_COLORIMETRY_RGB_WIDE_FLOAT)
    | (1 << DRM_MODE_COLORIMETRY_OPRGB)
    | (1 << DRM_MODE_COLORIMETRY_DCI_P3_RGB_D65)
    | (1 << DRM_MODE_COLORIMETRY_BT2020_RGB)
    | (1 << DRM_MODE_COLORIMETRY_BT601_YCC)
    | (1 << DRM_MODE_COLORIMETRY_BT709_YCC)
    | (1 << DRM_MODE_COLORIMETRY_XVYCC_601)
    | (1 << DRM_MODE_COLORIMETRY_XVYCC_709)
    | (1 << DRM_MODE_COLORIMETRY_SYCC_601)
    | (1 << DRM_MODE_COLORIMETRY_OPYCC_601)
    | (1 << DRM_MODE_COLORIMETRY_BT2020_CYCC)
    | (1 << DRM_MODE_COLORIMETRY_BT2020_YCC);

// upstream: drm_connector.c broadcast_rgb_names[]
static BROADCAST_RGB_NAMES: [EnumEntry; 3] = [
    (DRM_HDMI_BROADCAST_RGB_AUTO as i32, b"Automatic\0"),
    (DRM_HDMI_BROADCAST_RGB_FULL as i32, b"Full\0"),
    (DRM_HDMI_BROADCAST_RGB_LIMITED as i32, b"Limited 16:235\0"),
];

// upstream: drm_connector.c drm_hdmi_connector_get_broadcast_rgb_name()
#[unsafe(no_mangle)]
pub extern "C" fn drm_hdmi_connector_get_broadcast_rgb_name(
    broadcast_rgb: DrmHdmiBroadcastRgb,
) -> *const c_char {
    if broadcast_rgb as usize >= BROADCAST_RGB_NAMES.len() {
        return core::ptr::null();
    }
    BROADCAST_RGB_NAMES[broadcast_rgb as usize]
        .1
        .as_ptr()
        .cast()
}

// upstream: drm_connector.c output_format_str[]
const OUTPUT_FORMAT_STR: [Option<&[u8]>; 4] = [
    Some(b"RGB\0"),
    Some(b"YUV 4:4:4\0"),
    Some(b"YUV 4:2:2\0"),
    Some(b"YUV 4:2:0\0"),
];

// upstream: drm_connector.c drm_hdmi_connector_get_output_format_name()
#[unsafe(no_mangle)]
pub extern "C" fn drm_hdmi_connector_get_output_format_name(
    fmt: DrmOutputColorFormat,
) -> *const c_char {
    if fmt as usize >= OUTPUT_FORMAT_STR.len() {
        return core::ptr::null();
    }
    match OUTPUT_FORMAT_STR[fmt as usize] {
        Some(name) => name.as_ptr().cast(),
        None => core::ptr::null(),
    }
}

/// Creates a property and stores it in `mode_config`, or returns `-ENOMEM`.
/// Mirrors the repeated `prop = ...; if (!prop) return -ENOMEM; config.x = prop;`
/// blocks of the C file.
macro_rules! store_prop_or_enomem {
    ($slot:expr, $create:expr) => {{
        let prop: *mut DrmProperty = $create;
        if prop.is_null() {
            return -ENOMEM;
        }
        $slot = prop;
    }};
}

// upstream: drm_connector.c drm_connector_create_standard_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice` with its `mode_config` initialised. The created
/// properties are stored in `dev.mode_config`, so the caller must serialise this call
/// against other property creation on the same device.
pub unsafe extern "C" fn drm_connector_create_standard_properties(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` is a place pointer into its `mode_config`, so no temporary reference is created while the properties are stored.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        store_prop_or_enomem!(
            (*mc).edid_property,
            drm_property_create(
                dev,
                DRM_MODE_PROP_BLOB | DRM_MODE_PROP_IMMUTABLE,
                c"EDID".as_ptr().cast(),
                0
            )
        );
        store_prop_or_enomem!(
            (*mc).dpms_property,
            drm_property_create_enum(
                dev,
                0,
                c"DPMS".as_ptr().cast(),
                to_prop_enum_list(&DRM_DPMS_ENUM_LIST).as_ptr(),
                DRM_DPMS_ENUM_LIST.len() as i32
            )
        );
        store_prop_or_enomem!(
            (*mc).path_property,
            drm_property_create(
                dev,
                DRM_MODE_PROP_BLOB | DRM_MODE_PROP_IMMUTABLE,
                c"PATH".as_ptr().cast(),
                0
            )
        );
        store_prop_or_enomem!(
            (*mc).tile_property,
            drm_property_create(
                dev,
                DRM_MODE_PROP_BLOB | DRM_MODE_PROP_IMMUTABLE,
                c"TILE".as_ptr().cast(),
                0
            )
        );
        store_prop_or_enomem!(
            (*mc).link_status_property,
            drm_property_create_enum(
                dev,
                0,
                c"link-status".as_ptr().cast(),
                to_prop_enum_list(&DRM_LINK_STATUS_ENUM_LIST).as_ptr(),
                DRM_LINK_STATUS_ENUM_LIST.len() as i32
            )
        );
        store_prop_or_enomem!(
            (*mc).panel_type_property,
            drm_property_create_enum(
                dev,
                DRM_MODE_PROP_IMMUTABLE,
                c"panel_type".as_ptr().cast(),
                to_prop_enum_list(&DRM_PANEL_TYPE_ENUM_LIST).as_ptr(),
                DRM_PANEL_TYPE_ENUM_LIST.len() as i32
            )
        );
        store_prop_or_enomem!(
            (*mc).non_desktop_property,
            drm_property_create_bool(dev, DRM_MODE_PROP_IMMUTABLE, c"non-desktop".as_ptr().cast())
        );
        store_prop_or_enomem!(
            (*mc).hdr_output_metadata_property,
            drm_property_create(
                dev,
                DRM_MODE_PROP_BLOB,
                c"HDR_OUTPUT_METADATA".as_ptr().cast(),
                0
            )
        );

        0
    }
}

// upstream: drm_connector.c drm_mode_create_dvi_i_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. If the DVI-I select property already exists the
/// call returns 0 without creating anything. Otherwise both DVI-I properties are stored in
/// `dev.mode_config`; the caller must serialise property creation on `dev`.
pub unsafe extern "C" fn drm_mode_create_dvi_i_properties(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` points into its `mode_config`, and the cached DVI-I select property is checked before any creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).dvi_i_select_subconnector_property.is_null() {
            return 0;
        }

        let dvi_i_selector = drm_property_create_enum(
            dev,
            0,
            c"select subconnector".as_ptr().cast(),
            to_prop_enum_list(&DRM_DVI_I_SELECT_ENUM_LIST).as_ptr(),
            DRM_DVI_I_SELECT_ENUM_LIST.len() as i32,
        );
        (*mc).dvi_i_select_subconnector_property = dvi_i_selector;

        let dvi_i_subconnector = drm_property_create_enum(
            dev,
            DRM_MODE_PROP_IMMUTABLE,
            c"subconnector".as_ptr().cast(),
            to_prop_enum_list(&DRM_DVI_I_SUBCONNECTOR_ENUM_LIST).as_ptr(),
            DRM_DVI_I_SUBCONNECTOR_ENUM_LIST.len() as i32,
        );
        (*mc).dvi_i_subconnector_property = dvi_i_subconnector;

        0
    }
}

// upstream: drm_connector.c drm_connector_attach_dp_subconnector_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and `connector.dev` must be its live device. The DP
/// subconnector property is created in `mode_config` only when its slot is still NULL, then
/// attached to `connector.base`. The caller serialises this against other property setup.
pub unsafe extern "C" fn drm_connector_attach_dp_subconnector_property(
    connector: *mut DrmConnector,
) {
    // SAFETY: `connector.dev` is a live device back-pointer; `mode_config` is a place pointer into it, and the DP subconnector slot is created only when null.
    unsafe {
        let mode_config = core::ptr::addr_of_mut!((*(*connector).dev).mode_config);

        if (*mode_config).dp_subconnector_property.is_null() {
            (*mode_config).dp_subconnector_property = drm_property_create_enum(
                (*connector).dev,
                DRM_MODE_PROP_IMMUTABLE,
                c"subconnector".as_ptr().cast(),
                to_prop_enum_list(&DRM_DP_SUBCONNECTOR_ENUM_LIST).as_ptr(),
                DRM_DP_SUBCONNECTOR_ENUM_LIST.len() as i32,
            );
        }

        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*mode_config).dp_subconnector_property,
            DRM_MODE_SUBCONNECTOR_UNKNOWN as u64,
        );
    }
}

// upstream: drm_connector.c drm_connector_attach_content_type_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and `connector.dev` must be its live device. The content-type
/// property is created on the device if missing and then attached to `connector.base`.
/// Returns 0 when the property was created or already present.
pub unsafe extern "C" fn drm_connector_attach_content_type_property(
    connector: *mut DrmConnector,
) -> i32 {
    // SAFETY: `connector` is live; `dev` is its back-pointer, and `content_type_property` is read after the create helper has populated it.
    unsafe {
        if drm_mode_create_content_type_property((*connector).dev) == 0 {
            drm_object_attach_property(
                core::ptr::addr_of_mut!((*connector).base),
                (*(*connector).dev).mode_config.content_type_property,
                DRM_MODE_CONTENT_TYPE_NO_DATA as u64,
            );
        }
        0
    }
}

// upstream: drm_connector.c drm_connector_attach_tv_margin_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live, with `dev` set and the TV margin properties already created
/// on that device. Each property slot is attached to `connector.base`; a NULL slot is
/// attached as-is, so the caller must create the margin properties first.
pub unsafe extern "C" fn drm_connector_attach_tv_margin_properties(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live and `dev` is its back-pointer; each margin property is attached to the connector's own `base` object by address.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*(*connector).dev).mode_config);
        let base = core::ptr::addr_of_mut!((*connector).base);
        drm_object_attach_property(base, (*mc).tv_left_margin_property, 0);
        drm_object_attach_property(base, (*mc).tv_right_margin_property, 0);
        drm_object_attach_property(base, (*mc).tv_top_margin_property, 0);
        drm_object_attach_property(base, (*mc).tv_bottom_margin_property, 0);
    }
}

// upstream: drm_connector.c drm_mode_create_tv_margin_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. The four margin range properties are created only if
/// the left-margin slot is NULL; an allocation failure returns -ENOMEM with the partial
/// results left in `dev.mode_config`. The caller serialises property creation on `dev`.
pub unsafe extern "C" fn drm_mode_create_tv_margin_properties(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` is in-place access to its `mode_config`, and the cached `tv_left_margin_property` slot prevents duplicate creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).tv_left_margin_property.is_null() {
            return 0;
        }

        (*mc).tv_left_margin_property =
            drm_property_create_range(dev, 0, c"left margin".as_ptr().cast(), 0, 100);
        if (*mc).tv_left_margin_property.is_null() {
            return -ENOMEM;
        }

        (*mc).tv_right_margin_property =
            drm_property_create_range(dev, 0, c"right margin".as_ptr().cast(), 0, 100);
        if (*mc).tv_right_margin_property.is_null() {
            return -ENOMEM;
        }

        (*mc).tv_top_margin_property =
            drm_property_create_range(dev, 0, c"top margin".as_ptr().cast(), 0, 100);
        if (*mc).tv_top_margin_property.is_null() {
            return -ENOMEM;
        }

        (*mc).tv_bottom_margin_property =
            drm_property_create_range(dev, 0, c"bottom margin".as_ptr().cast(), 0, 100);
        if (*mc).tv_bottom_margin_property.is_null() {
            return -ENOMEM;
        }

        0
    }
}

/// Creates a TV range property (`0..=100`) into `slot`; returns true on failure.
/// # Safety
///
/// `dev` and `slot` must be valid, live pointers.
unsafe fn tv_range_failed(dev: *mut DrmDevice, slot: *mut *mut DrmProperty, name: &[u8]) -> bool {
    // SAFETY: `dev` is valid; create TV range property for mode_config.
    unsafe {
        *slot = drm_property_create_range(dev, 0, name.as_ptr().cast(), 0, 100);
        (*slot).is_null()
    }
}

// upstream: drm_connector.c drm_mode_create_tv_properties_legacy()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. `modes` must point to `num_modes` valid,
/// NUL-terminated strings, which are used as enum names; they must outlive the created
/// property, as the enum list keeps the pointers. Returns early with 0 if the legacy
/// subconnector property is already present.
pub unsafe extern "C" fn drm_mode_create_tv_properties_legacy(
    dev: *mut DrmDevice,
    num_modes: u32,
    modes: *const *const c_char,
) -> i32 {
    // SAFETY: `dev` is valid; create legacy TV properties in mode_config.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).tv_select_subconnector_property.is_null() {
            return 0;
        }

        // Each failing step jumps to `nomem`, which returns -ENOMEM.
        let tv_selector = drm_property_create_enum(
            dev,
            0,
            c"select subconnector".as_ptr().cast(),
            to_prop_enum_list(&DRM_TV_SELECT_ENUM_LIST).as_ptr(),
            DRM_TV_SELECT_ENUM_LIST.len() as i32,
        );
        if tv_selector.is_null() {
            return -ENOMEM;
        }
        (*mc).tv_select_subconnector_property = tv_selector;

        let tv_subconnector = drm_property_create_enum(
            dev,
            DRM_MODE_PROP_IMMUTABLE,
            c"subconnector".as_ptr().cast(),
            to_prop_enum_list(&DRM_TV_SUBCONNECTOR_ENUM_LIST).as_ptr(),
            DRM_TV_SUBCONNECTOR_ENUM_LIST.len() as i32,
        );
        if tv_subconnector.is_null() {
            return -ENOMEM;
        }
        (*mc).tv_subconnector_property = tv_subconnector;

        if drm_mode_create_tv_margin_properties(dev) != 0 {
            return -ENOMEM;
        }

        if num_modes != 0 {
            (*mc).legacy_tv_mode_property = drm_property_create(
                dev,
                DRM_MODE_PROP_ENUM,
                c"mode".as_ptr().cast(),
                num_modes as i32,
            );
            if (*mc).legacy_tv_mode_property.is_null() {
                return -ENOMEM;
            }
            for i in 0..num_modes {
                drm_property_add_enum(
                    (*mc).legacy_tv_mode_property,
                    i as u64,
                    *modes.add(i as usize),
                );
            }
        }

        if tv_range_failed(
            dev,
            core::ptr::addr_of_mut!((*mc).tv_brightness_property),
            b"brightness\0",
        ) {
            return -ENOMEM;
        }
        if tv_range_failed(
            dev,
            core::ptr::addr_of_mut!((*mc).tv_contrast_property),
            b"contrast\0",
        ) {
            return -ENOMEM;
        }
        if tv_range_failed(
            dev,
            core::ptr::addr_of_mut!((*mc).tv_flicker_reduction_property),
            b"flicker reduction\0",
        ) {
            return -ENOMEM;
        }
        if tv_range_failed(
            dev,
            core::ptr::addr_of_mut!((*mc).tv_overscan_property),
            b"overscan\0",
        ) {
            return -ENOMEM;
        }
        if tv_range_failed(
            dev,
            core::ptr::addr_of_mut!((*mc).tv_saturation_property),
            b"saturation\0",
        ) {
            return -ENOMEM;
        }
        if tv_range_failed(
            dev,
            core::ptr::addr_of_mut!((*mc).tv_hue_property),
            b"hue\0",
        ) {
            return -ENOMEM;
        }

        0
    }
}

// upstream: drm_connector.c drm_mode_create_tv_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. Each TV mode bit set in `supported_tv_modes` adds an
/// entry to a local enum list built from the static TV mode names. The created property is
/// stored in `dev.mode_config`, and the function returns 0 early if it already exists.
pub unsafe extern "C" fn drm_mode_create_tv_properties(
    dev: *mut DrmDevice,
    supported_tv_modes: u32,
) -> i32 {
    // SAFETY: `dev` is live; `mc` is in-place access to its `mode_config`, and `tv_mode_property` is checked for null before creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).tv_mode_property.is_null() {
            return 0;
        }

        let mut tv_mode_list: [DrmPropEnumList; DRM_MODE_TV_MODE_MAX as usize] =
            core::array::from_fn(|_| DrmPropEnumList {
                r#type: 0,
                name: core::ptr::null_mut(),
            });
        let mut len = 0usize;
        for i in 0..DRM_MODE_TV_MODE_MAX {
            if supported_tv_modes & (1u32 << i) == 0 {
                continue;
            }
            tv_mode_list[len].r#type = i as i32;
            tv_mode_list[len].name = drm_get_tv_mode_name(i as i32) as *mut c_char;
            len += 1;
        }

        let tv_mode = drm_property_create_enum(
            dev,
            0,
            c"TV mode".as_ptr().cast(),
            tv_mode_list.as_ptr(),
            len as i32,
        );
        if tv_mode.is_null() {
            return -ENOMEM;
        }
        (*mc).tv_mode_property = tv_mode;

        drm_mode_create_tv_properties_legacy(dev, 0, core::ptr::null())
    }
}

// upstream: drm_connector.c drm_mode_create_scaling_mode_property()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. The scaling-mode enum property is created and stored
/// in `dev.mode_config` unless it is already present, in which case 0 is returned. The
/// caller serialises property creation on `dev`.
pub unsafe extern "C" fn drm_mode_create_scaling_mode_property(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` is in-place access to its `mode_config`, and the cached `scaling_mode_property` slot is checked before creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).scaling_mode_property.is_null() {
            return 0;
        }

        let scaling_mode = drm_property_create_enum(
            dev,
            0,
            c"scaling mode".as_ptr().cast(),
            to_prop_enum_list(&DRM_SCALING_MODE_ENUM_LIST).as_ptr(),
            DRM_SCALING_MODE_ENUM_LIST.len() as i32,
        );
        (*mc).scaling_mode_property = scaling_mode;

        0
    }
}

// upstream: drm_connector.c drm_connector_attach_vrr_capable_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and `connector.dev` must be its live device. The VRR-capable
/// boolean property is created and stored in `connector.vrr_capable_property` only if the
/// slot is NULL, and then attached to `connector.base`.
pub unsafe extern "C" fn drm_connector_attach_vrr_capable_property(
    connector: *mut DrmConnector,
) -> i32 {
    // SAFETY: `connector` is live and `dev` is its back-pointer; `vrr_capable_property` is created only when its slot is null, then attached to `base`.
    unsafe {
        let dev = (*connector).dev;

        if (*connector).vrr_capable_property.is_null() {
            let prop = drm_property_create_bool(
                dev,
                DRM_MODE_PROP_IMMUTABLE,
                c"vrr_capable".as_ptr().cast(),
            );
            if prop.is_null() {
                return -ENOMEM;
            }
            (*connector).vrr_capable_property = prop;
            drm_object_attach_property(core::ptr::addr_of_mut!((*connector).base), prop, 0);
        }

        0
    }
}

// upstream: drm_connector.c drm_connector_attach_scaling_mode_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with a valid `dev`. `scaling_mode_mask` must have at least two
/// bits set and only bits that name existing scaling modes; otherwise -EINVAL is returned
/// and nothing is created. The property is attached to `connector.base`.
pub unsafe extern "C" fn drm_connector_attach_scaling_mode_property(
    connector: *mut DrmConnector,
    scaling_mode_mask: u32,
) -> i32 {
    // SAFETY: `connector` is valid; attach scaling mode property.
    unsafe {
        let dev = (*connector).dev;
        let valid_scaling_mode_mask: u32 = (1u32 << DRM_SCALING_MODE_ENUM_LIST.len()) - 1;

        if warn_on(
            scaling_mode_mask.count_ones() < 2 || scaling_mode_mask & !valid_scaling_mode_mask != 0,
        ) {
            return -EINVAL;
        }

        let scaling_mode_property = drm_property_create(
            dev,
            DRM_MODE_PROP_ENUM,
            c"scaling mode".as_ptr().cast(),
            scaling_mode_mask.count_ones() as i32,
        );
        if scaling_mode_property.is_null() {
            return -ENOMEM;
        }

        for (i, &(ty, name)) in DRM_SCALING_MODE_ENUM_LIST.iter().enumerate() {
            if (1u32 << i) & scaling_mode_mask == 0 {
                continue;
            }

            let ret = drm_property_add_enum(scaling_mode_property, ty as u64, name.as_ptr().cast());
            if ret != 0 {
                drm_property_destroy(dev, scaling_mode_property);
                return ret;
            }
        }

        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            scaling_mode_property,
            0,
        );
        (*connector).scaling_mode_property = scaling_mode_property;

        0
    }
}

// upstream: drm_connector.c drm_mode_create_aspect_ratio_property()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. The aspect-ratio enum property is created in
/// `dev.mode_config` only when its slot is NULL; an allocation failure returns -ENOMEM.
/// The caller serialises property creation on `dev`.
pub unsafe extern "C" fn drm_mode_create_aspect_ratio_property(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` points into its `mode_config`, and `aspect_ratio_property` is checked before creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).aspect_ratio_property.is_null() {
            return 0;
        }

        (*mc).aspect_ratio_property = drm_property_create_enum(
            dev,
            0,
            c"aspect ratio".as_ptr().cast(),
            to_prop_enum_list(&DRM_ASPECT_RATIO_ENUM_LIST).as_ptr(),
            DRM_ASPECT_RATIO_ENUM_LIST.len() as i32,
        );
        if (*mc).aspect_ratio_property.is_null() {
            return -ENOMEM;
        }

        0
    }
}

// upstream: drm_connector.c drm_mode_create_colorspace_property()
/// # Safety
///
/// `connector` must point to a live `DrmConnector`.
unsafe fn drm_mode_create_colorspace_property(
    connector: *mut DrmConnector,
    supported_colorspaces: u32,
) -> i32 {
    // SAFETY: `connector` is valid; create and attach color space property.
    unsafe {
        let dev = (*connector).dev;
        let colorspaces = supported_colorspaces | (1u32 << DRM_MODE_COLORIMETRY_DEFAULT);
        let mut enum_list: [DrmPropEnumList; DRM_MODE_COLORIMETRY_COUNT as usize] =
            core::array::from_fn(|_| DrmPropEnumList {
                r#type: 0,
                name: core::ptr::null_mut(),
            });

        if !(*connector).colorspace_property.is_null() {
            return 0;
        }

        if supported_colorspaces == 0 {
            drm_err!(
                dev,
                "No supported colorspaces provded on [CONNECTOR:%d:%s]\n",
                (*connector).base.id,
                (*connector).name
            );
            return -EINVAL;
        }

        if supported_colorspaces & !((1u32 << DRM_MODE_COLORIMETRY_COUNT) - 1) != 0 {
            drm_err!(
                dev,
                "Unknown colorspace provded on [CONNECTOR:%d:%s]\n",
                (*connector).base.id,
                (*connector).name
            );
            return -EINVAL;
        }

        let mut len = 0usize;
        for i in 0..DRM_MODE_COLORIMETRY_COUNT {
            if colorspaces & (1u32 << i) == 0 {
                continue;
            }
            enum_list[len].r#type = i as i32;
            enum_list[len].name =
                COLORSPACE_NAMES[i as usize].as_ptr().cast::<c_char>() as *mut c_char;
            len += 1;
        }

        (*connector).colorspace_property = drm_property_create_enum(
            dev,
            DRM_MODE_PROP_ENUM,
            c"Colorspace".as_ptr().cast(),
            enum_list.as_ptr(),
            len as i32,
        );
        if (*connector).colorspace_property.is_null() {
            return -ENOMEM;
        }

        0
    }
}

// upstream: drm_connector.c drm_mode_create_hdmi_colorspace_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. `supported_colorspaces` of 0 selects the full
/// HDMI colorspace set; otherwise its bits are masked against it. The colorspace property
/// is created by `drm_mode_create_colorspace_property` and stored on the connector.
pub unsafe extern "C" fn drm_mode_create_hdmi_colorspace_property(
    connector: *mut DrmConnector,
    supported_colorspaces: u32,
) -> i32 {
    // SAFETY: `connector` is valid; attach HDMI color space property.
    unsafe {
        let colorspaces = if supported_colorspaces != 0 {
            supported_colorspaces & HDMI_COLORSPACES
        } else {
            HDMI_COLORSPACES
        };
        drm_mode_create_colorspace_property(connector, colorspaces)
    }
}

// upstream: drm_connector.c drm_mode_create_dp_colorspace_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. A zero `supported_colorspaces` selects the whole
/// DisplayPort colorspace set; otherwise its bits are masked against it. The property is
/// created by `drm_mode_create_colorspace_property` and stored on the connector.
pub unsafe extern "C" fn drm_mode_create_dp_colorspace_property(
    connector: *mut DrmConnector,
    supported_colorspaces: u32,
) -> i32 {
    // SAFETY: `connector` is valid; attach DisplayPort color space property.
    unsafe {
        let colorspaces = if supported_colorspaces != 0 {
            supported_colorspaces & DP_COLORSPACES
        } else {
            DP_COLORSPACES
        };
        drm_mode_create_colorspace_property(connector, colorspaces)
    }
}

// upstream: drm_connector.c drm_mode_create_content_type_property()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. The content-type enum property is created in
/// `dev.mode_config` only when the slot is NULL; allocation failure returns -ENOMEM.
/// The caller serialises property creation on `dev`.
pub unsafe extern "C" fn drm_mode_create_content_type_property(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` points into its `mode_config`, and `content_type_property` is checked for null to avoid a second creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).content_type_property.is_null() {
            return 0;
        }

        (*mc).content_type_property = drm_property_create_enum(
            dev,
            0,
            c"content type".as_ptr().cast(),
            to_prop_enum_list(&DRM_CONTENT_TYPE_ENUM_LIST).as_ptr(),
            DRM_CONTENT_TYPE_ENUM_LIST.len() as i32,
        );
        if (*mc).content_type_property.is_null() {
            return -ENOMEM;
        }

        0
    }
}

// upstream: drm_connector.c drm_mode_create_suggested_offset_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. The suggested X and Y range properties are created
/// in `dev.mode_config` only if either slot is NULL, and the function returns 0 when both
/// already exist. The caller serialises property creation on `dev`.
pub unsafe extern "C" fn drm_mode_create_suggested_offset_properties(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is live; `mc` points into its `mode_config`, and both suggested x/y slots are checked before creation.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        if !(*mc).suggested_x_property.is_null() && !(*mc).suggested_y_property.is_null() {
            return 0;
        }

        (*mc).suggested_x_property = drm_property_create_range(
            dev,
            DRM_MODE_PROP_IMMUTABLE,
            c"suggested X".as_ptr().cast(),
            0,
            0xffff_ffff,
        );
        (*mc).suggested_y_property = drm_property_create_range(
            dev,
            DRM_MODE_PROP_IMMUTABLE,
            c"suggested Y".as_ptr().cast(),
            0,
            0xffff_ffff,
        );

        if (*mc).suggested_x_property.is_null() || (*mc).suggested_y_property.is_null() {
            return -ENOMEM;
        }

        0
    }
}

// upstream: drm_connector.c drm_connector_set_path_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. `path` must be a NUL-terminated string; its
/// bytes are copied into a new global blob that replaces `connector.path_blob_ptr`, so
/// `path` need not outlive the call. The blob is attached to `connector.base`.
pub unsafe extern "C" fn drm_connector_set_path_property(
    connector: *mut DrmConnector,
    path: *const c_char,
) -> i32 {
    // SAFETY: `connector` is live; `path` must be NUL-terminated, which `c_strlen` walks to, and the global blob is built from a copy, so `path` is read only during this call.
    unsafe {
        let dev = (*connector).dev;
        drm_property_replace_global_blob(
            dev,
            core::ptr::addr_of_mut!((*connector).path_blob_ptr),
            c_strlen(path) + 1,
            path.cast(),
            core::ptr::addr_of_mut!((*connector).base),
            (*dev).mode_config.path_property,
        )
    }
}

/// Appends the decimal form of `value` to `buf` at `*len` (no bounds growth
/// beyond the 256-byte `tile[]` of the C code; the caller caps `*len`).
/// # Safety
///
/// `buf` must point to writable buffer of sufficient capacity.
unsafe fn tile_push_u32(buf: *mut u8, len: &mut usize, value: u32) {
    let mut digits = [0u8; 10];
    let mut n = value;
    let mut count = 0usize;
    loop {
        digits[count] = b'0' + (n % 10) as u8;
        count += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for i in (0..count).rev() {
        if *len + 1 < 256 {
            // SAFETY: `*len + 1 < 256` was checked above, so `*len <= 254`; `buf` must have that capacity per the function's safety contract (the only caller passes a 256-byte `tile` array).
            unsafe { *buf.add(*len) = digits[i] };
            *len += 1;
        }
    }
}

// upstream: drm_connector.c drm_connector_set_tile_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. When `has_tile` is false the tile blob is
/// cleared; otherwise the tile descriptor is encoded into a local buffer and replaces
/// `connector.tile_blob_ptr`. No caller-owned pointer is retained.
pub unsafe extern "C" fn drm_connector_set_tile_property(connector: *mut DrmConnector) -> i32 {
    // SAFETY: `connector` is live; `tile_blob_ptr` is the connector's own slot, passed by address, and `has_tile` selects between replacing and clearing the blob.
    unsafe {
        let dev = (*connector).dev;

        if !(*connector).has_tile {
            return drm_property_replace_global_blob(
                dev,
                core::ptr::addr_of_mut!((*connector).tile_blob_ptr),
                0,
                core::ptr::null(),
                core::ptr::addr_of_mut!((*connector).base),
                (*dev).mode_config.tile_property,
            );
        }

        // snprintf(tile, 256, "%d:%d:%d:%d:%d:%d:%d:%d", ...)
        let mut tile = [0u8; 256];
        let mut len = 0usize;
        let fields = [
            (*(*connector).tile_group).id as u32,
            (*connector).tile_is_single_monitor as u32,
            (*connector).num_h_tile as u32,
            (*connector).num_v_tile as u32,
            (*connector).tile_h_loc as u32,
            (*connector).tile_v_loc as u32,
            (*connector).tile_h_size as u32,
            (*connector).tile_v_size as u32,
        ];
        for (i, &field) in fields.iter().enumerate() {
            if i != 0 && len + 1 < 256 {
                tile[len] = b':';
                len += 1;
            }
            tile_push_u32(tile.as_mut_ptr(), &mut len, field);
        }
        tile[len] = 0;

        drm_property_replace_global_blob(
            dev,
            core::ptr::addr_of_mut!((*connector).tile_blob_ptr),
            len + 1,
            tile.as_ptr().cast(),
            core::ptr::addr_of_mut!((*connector).base),
            (*dev).mode_config.tile_property,
        )
    }
}

// upstream: drm_connector.c drm_connector_set_link_status_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live and `connector.state` must point to a valid connector state.
/// The write to `state.link_status` happens while `dev.mode_config.connection_mutex` is
/// held, so the caller must not already hold that mutex.
pub unsafe extern "C" fn drm_connector_set_link_status_property(
    connector: *mut DrmConnector,
    link_status: u64,
) {
    // SAFETY: `connector` is live and `dev` its back-pointer; `connection_mutex` is taken before `connector.state` is written, and `state` is read only under that lock.
    unsafe {
        let dev = (*connector).dev;
        drm_modeset_lock(
            core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
            core::ptr::null_mut(),
        );
        (*(*connector).state).link_status = link_status as u32;
        drm_modeset_unlock(core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex));
    }
}

// upstream: drm_connector.c drm_connector_attach_max_bpc_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. `min` and `max` bound the range property,
/// which is created only when `connector.max_bpc_property` is NULL and then attached to
/// `connector.base`. The caller serialises property creation on the device.
pub unsafe extern "C" fn drm_connector_attach_max_bpc_property(
    connector: *mut DrmConnector,
    min: i32,
    max: i32,
) -> i32 {
    // SAFETY: `connector` is live; `dev` is its back-pointer, and `max_bpc_property` is cached on the connector and reused when already created.
    unsafe {
        let dev = (*connector).dev;

        let mut prop = (*connector).max_bpc_property;
        if prop.is_null() {
            prop = drm_property_create_range(
                dev,
                0,
                c"max bpc".as_ptr().cast(),
                min as u64,
                max as u64,
            );
            if prop.is_null() {
                return -ENOMEM;
            }
            (*connector).max_bpc_property = prop;
        }

        drm_object_attach_property(core::ptr::addr_of_mut!((*connector).base), prop, max as u64);
        (*(*connector).state).max_requested_bpc = max as u8;
        (*(*connector).state).max_bpc = max as u8;

        0
    }
}

// upstream: drm_connector.c drm_connector_attach_hdr_output_metadata_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. The HDR output metadata property must already
/// exist in `dev.mode_config`; it is attached to `connector.base` with value 0 and this
/// function does not create it.
pub unsafe extern "C" fn drm_connector_attach_hdr_output_metadata_property(
    connector: *mut DrmConnector,
) {
    // SAFETY: `connector` is live; `dev` is its back-pointer, and `hdr_output_metadata_property` comes from the device's `mode_config`, set up before connectors attach.
    unsafe {
        let dev = (*connector).dev;
        let prop = (*dev).mode_config.hdr_output_metadata_property;
        drm_object_attach_property(core::ptr::addr_of_mut!((*connector).base), prop, 0);
    }
}

// upstream: drm_connector.c drm_connector_attach_broadcast_rgb_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. The Broadcast RGB enum property is created and
/// cached in `connector.broadcast_rgb_property` only when the slot is NULL, then attached
/// to `connector.base`.
pub unsafe extern "C" fn drm_connector_attach_broadcast_rgb_property(
    connector: *mut DrmConnector,
) -> i32 {
    // SAFETY: `connector` is live; `dev` is its back-pointer, and `broadcast_rgb_property` is cached on the connector and created only when null.
    unsafe {
        let dev = (*connector).dev;

        let mut prop = (*connector).broadcast_rgb_property;
        if prop.is_null() {
            prop = drm_property_create_enum(
                dev,
                DRM_MODE_PROP_ENUM,
                c"Broadcast RGB".as_ptr().cast(),
                to_prop_enum_list(&BROADCAST_RGB_NAMES).as_ptr(),
                BROADCAST_RGB_NAMES.len() as i32,
            );
            if prop.is_null() {
                return -EINVAL;
            }
            (*connector).broadcast_rgb_property = prop;
        }

        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            prop,
            DRM_HDMI_BROADCAST_RGB_AUTO as u64,
        );
        0
    }
}

// upstream: drm_connector.c drm_connector_attach_colorspace_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live. Its `colorspace_property` must already have been created by
/// one of the colorspace setup functions, since it is attached to `connector.base` as-is
/// with the default colorimetry value.
pub unsafe extern "C" fn drm_connector_attach_colorspace_property(
    connector: *mut DrmConnector,
) -> i32 {
    // SAFETY: `connector` is live; `colorspace_property` is read from the connector itself and attached to its `base` object by address.
    unsafe {
        let prop = (*connector).colorspace_property;
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            prop,
            DRM_MODE_COLORIMETRY_DEFAULT as u64,
        );
        0
    }
}

// upstream: drm_connector.c drm_connector_atomic_hdr_metadata_equal()
#[unsafe(no_mangle)]
/// # Safety
/// `old_state` and `new_state` must both be live connector states. Their
/// `hdr_output_metadata` blob pointers may be NULL; if either is NULL the result is whether
/// both are NULL. Otherwise the blob lengths and bytes are compared read-only, so the blobs
/// must remain valid for the duration of the call.
pub unsafe extern "C" fn drm_connector_atomic_hdr_metadata_equal(
    old_state: *mut DrmConnectorState,
    new_state: *mut DrmConnectorState,
) -> bool {
    // SAFETY: Check whether HDR output metadata blob changed between old and new state.
    unsafe {
        let old_blob = (*old_state).hdr_output_metadata;
        let new_blob = (*new_state).hdr_output_metadata;

        if old_blob.is_null() || new_blob.is_null() {
            return old_blob == new_blob;
        }

        if (*old_blob).length != (*new_blob).length {
            return false;
        }

        let len = (*old_blob).length;
        core::slice::from_raw_parts((*old_blob).data.cast::<u8>(), len)
            == core::slice::from_raw_parts((*new_blob).data.cast::<u8>(), len)
    }
}

// upstream: drm_connector.c drm_connector_set_vrr_capable_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `vrr_capable_property` created beforehand. If it is NULL
/// the call does nothing; otherwise `capable` is written as the property value on
/// `connector.base`.
pub unsafe extern "C" fn drm_connector_set_vrr_capable_property(
    connector: *mut DrmConnector,
    capable: bool,
) {
    // SAFETY: `connector` is live; `vrr_capable_property` is checked non-null before it is passed to `drm_object_property_set_value` with the connector's `base`.
    unsafe {
        if (*connector).vrr_capable_property.is_null() {
            return;
        }
        drm_object_property_set_value(
            core::ptr::addr_of_mut!((*connector).base),
            (*connector).vrr_capable_property,
            capable as u64,
        );
    }
}

// upstream: drm_connector.c drm_connector_set_panel_orientation()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set and a valid `display_info`. The orientation is
/// written only when the current value is UNKNOWN and the new value is not UNKNOWN; the
/// property is updated on `connector.base`. The caller serialises this against other
/// display-info writers.
pub unsafe extern "C" fn drm_connector_set_panel_orientation(
    connector: *mut DrmConnector,
    panel_orientation: DrmPanelOrientation,
) -> i32 {
    // SAFETY: `connector` is valid; set panel orientation property value.
    unsafe {
        let dev = (*connector).dev;
        let info = core::ptr::addr_of_mut!((*connector).display_info);

        if (*info).panel_orientation != DRM_MODE_PANEL_ORIENTATION_UNKNOWN {
            return 0;
        }

        if panel_orientation == DRM_MODE_PANEL_ORIENTATION_UNKNOWN {
            return 0;
        }

        (*info).panel_orientation = panel_orientation;

        let mut prop = (*dev).mode_config.panel_orientation_property;
        if prop.is_null() {
            prop = drm_property_create_enum(
                dev,
                DRM_MODE_PROP_IMMUTABLE,
                c"panel orientation".as_ptr().cast(),
                to_prop_enum_list(&DRM_PANEL_ORIENTATION_ENUM_LIST).as_ptr(),
                DRM_PANEL_ORIENTATION_ENUM_LIST.len() as i32,
            );
            if prop.is_null() {
                return -ENOMEM;
            }
            (*dev).mode_config.panel_orientation_property = prop;
        }

        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            prop,
            (*info).panel_orientation as u64,
        );
        0
    }
}

// upstream: drm_connector.c drm_connector_set_panel_orientation_with_quirk()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must satisfy the requirements of `drm_connector_set_panel_orientation`.
/// `width` and `height` are plain panel dimensions used to look up a built-in quirk, which
/// may override `panel_orientation` before it is applied.
pub unsafe extern "C" fn drm_connector_set_panel_orientation_with_quirk(
    connector: *mut DrmConnector,
    mut panel_orientation: DrmPanelOrientation,
    width: i32,
    height: i32,
) -> i32 {
    // SAFETY: Lookup panel orientation quirk and set panel orientation if found.
    unsafe {
        let orientation_quirk = drm_get_panel_orientation_quirk(width, height);
        if orientation_quirk != DRM_MODE_PANEL_ORIENTATION_UNKNOWN {
            panel_orientation = orientation_quirk;
        }
        drm_connector_set_panel_orientation(connector, panel_orientation)
    }
}

// upstream: drm_connector.c drm_connector_set_orientation_from_panel()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must satisfy the requirements of `drm_connector_set_panel_orientation`.
/// `_panel` is ignored and is not dereferenced; the orientation is set to UNKNOWN.
pub unsafe extern "C" fn drm_connector_set_orientation_from_panel(
    connector: *mut DrmConnector,
    _panel: *mut c_void,
) -> i32 {
    // Deviation: struct drm_panel is not ported in this batch, so
    // panel->funcs->get_orientation() cannot be called; the orientation is
    // reported as UNKNOWN, which is what the C code returns for a NULL panel.
    // SAFETY: Set panel orientation property to unknown.
    unsafe { drm_connector_set_panel_orientation(connector, DRM_MODE_PANEL_ORIENTATION_UNKNOWN) }
}

/// `drm_connector_create_privacy_screen_properties()`: the sw/hw state enums.
// upstream: drm_connector.c privacy_screen_enum[]
static PRIVACY_SCREEN_ENUM: [EnumEntry; 4] = [
    (PRIVACY_SCREEN_DISABLED as i32, b"Disabled\0"),
    (PRIVACY_SCREEN_ENABLED as i32, b"Enabled\0"),
    (PRIVACY_SCREEN_DISABLED_LOCKED as i32, b"Disabled-locked\0"),
    (PRIVACY_SCREEN_ENABLED_LOCKED as i32, b"Enabled-locked\0"),
];

// upstream: drm_connector.c drm_connector_create_privacy_screen_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. The software and hardware state enum properties
/// are created only if the software slot is NULL; they are stored on the connector and
/// the caller serialises this with other property setup on the device.
pub unsafe extern "C" fn drm_connector_create_privacy_screen_properties(
    connector: *mut DrmConnector,
) {
    // SAFETY: `connector` is live and `dev` is its back-pointer; the sw_state property is created only when its slot is null.
    unsafe {
        if !(*connector).privacy_screen_sw_state_property.is_null() {
            return;
        }

        (*connector).privacy_screen_sw_state_property = drm_property_create_enum(
            (*connector).dev,
            DRM_MODE_PROP_ENUM,
            c"privacy-screen sw-state".as_ptr().cast(),
            to_prop_enum_list(&PRIVACY_SCREEN_ENUM).as_ptr(),
            2,
        );
        (*connector).privacy_screen_hw_state_property = drm_property_create_enum(
            (*connector).dev,
            DRM_MODE_PROP_IMMUTABLE | DRM_MODE_PROP_ENUM,
            c"privacy-screen hw-state".as_ptr().cast(),
            to_prop_enum_list(&PRIVACY_SCREEN_ENUM).as_ptr(),
            PRIVACY_SCREEN_ENUM.len() as i32,
        );
    }
}

// upstream: drm_connector.c drm_connector_attach_privacy_screen_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live. The privacy-screen properties must have been created by
/// `drm_connector_create_privacy_screen_properties`; if the software-state slot is NULL the
/// function returns without attaching anything.
pub unsafe extern "C" fn drm_connector_attach_privacy_screen_properties(
    connector: *mut DrmConnector,
) {
    // SAFETY: `connector` is live; the sw_state property slot is checked non-null before it is attached to the connector's `base`.
    unsafe {
        if (*connector).privacy_screen_sw_state_property.is_null() {
            return;
        }
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*connector).privacy_screen_sw_state_property,
            PRIVACY_SCREEN_DISABLED as u64,
        );
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            (*connector).privacy_screen_hw_state_property,
            PRIVACY_SCREEN_DISABLED as u64,
        );
    }
}

// upstream: drm_connector.c drm_connector_update_privacy_screen_properties()
/// # Safety
///
/// `connector` must point to a live `DrmConnector`.
unsafe fn drm_connector_update_privacy_screen_properties(
    connector: *mut DrmConnector,
    set_sw_state: bool,
) {
    // SAFETY: Update privacy screen hardware and software state properties.
    unsafe {
        let mut sw_state: DrmPrivacyScreenStatus = 0;
        let mut hw_state: DrmPrivacyScreenStatus = 0;
        drm_privacy_screen_get_state((*connector).privacy_screen, &mut sw_state, &mut hw_state);

        if set_sw_state {
            (*(*connector).state).privacy_screen_sw_state = sw_state;
        }
        drm_object_property_set_value(
            core::ptr::addr_of_mut!((*connector).base),
            (*connector).privacy_screen_hw_state_property,
            hw_state as u64,
        );
    }
}

// upstream: drm_connector.c drm_connector_privacy_screen_notifier()
/// # Safety
///
/// `nb` must point to the embedded notifier block of a `DrmConnector`.
unsafe extern "C" fn drm_connector_privacy_screen_notifier(
    nb: *mut drm_kapi::gem_memory::NotifierBlock,
    _action: u64,
    _data: *mut c_void,
) -> i32 {
    // SAFETY: Notifier callback; update privacy screen properties from notifier event.
    unsafe {
        // container_of(nb, struct drm_connector, privacy_screen_notifier)
        let connector = nb
            .cast::<u8>()
            .sub(offset_of!(DrmConnector, privacy_screen_notifier))
            .cast::<DrmConnector>();
        let dev = (*connector).dev;

        drm_modeset_lock(
            core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
            core::ptr::null_mut(),
        );
        drm_connector_update_privacy_screen_properties(connector, true);
        drm_modeset_unlock(core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex));

        // drm_sysfs_connector_property_event() is not built on this target.

        NOTIFY_DONE
    }
}

/// `NOTIFY_DONE` (include/linux/notifier.h).
const NOTIFY_DONE: i32 = 0;

// upstream: drm_connector.c drm_connector_attach_privacy_screen_provider()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live. `priv_` is stored unowned in `connector.privacy_screen`, so the
/// privacy-screen provider must stay alive until it is detached. The notifier is wired
/// before the properties are created and updated.
pub unsafe extern "C" fn drm_connector_attach_privacy_screen_provider(
    connector: *mut DrmConnector,
    priv_: *mut c_void,
) {
    // SAFETY: `connector` is live; `priv_` is stored as the raw provider pointer, which the privacy-screen provider keeps alive until detach; the notifier is wired before properties are updated.
    unsafe {
        (*connector).privacy_screen = priv_;
        (*connector).privacy_screen_notifier.notifier_call =
            Some(drm_connector_privacy_screen_notifier);

        drm_connector_create_privacy_screen_properties(connector);
        drm_connector_update_privacy_screen_properties(connector, true);
        drm_connector_attach_privacy_screen_properties(connector);
    }
}

// upstream: drm_connector.c drm_connector_update_privacy_screen()
#[unsafe(no_mangle)]
/// # Safety
/// `connector_state` must be a live state whose `connector` back-pointer is valid. If the
/// connector has no privacy screen the call returns; otherwise the software state is
/// pushed to `connector.privacy_screen` and the properties are refreshed.
pub unsafe extern "C" fn drm_connector_update_privacy_screen(
    connector_state: *const DrmConnectorState,
) {
    // SAFETY: `connector_state` is live and its `connector` back-pointer is valid; `privacy_screen` is checked for null before the update runs.
    unsafe {
        let connector = (*connector_state).connector;

        if (*connector).privacy_screen.is_null() {
            return;
        }

        let ret = drm_privacy_screen_set_sw_state(
            (*connector).privacy_screen,
            (*connector_state).privacy_screen_sw_state,
        );
        if ret != 0 {
            drm_err!((*connector).dev, "Error updating privacy-screen sw_state\n");
            return;
        }

        drm_connector_update_privacy_screen_properties(connector, false);
    }
}

// upstream: drm_connector.c drm_connector_set_obj_prop()
#[unsafe(no_mangle)]
/// # Safety
/// `obj` must be a live connector mode object (it is converted with `obj_to_connector`),
/// and `property` must be a live property on the device. The DPMS property is routed to
/// the driver's `dpms` callback and others to `set_property`; the caller must hold the
/// modeset locks that the driver callbacks require.
pub unsafe extern "C" fn drm_connector_set_obj_prop(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    value: u64,
) -> i32 {
    // SAFETY: Set connector mode object property value using legacy property setter.
    unsafe {
        let mut ret = -EINVAL;
        let connector = obj_to_connector(obj);

        if property == (*(*connector).dev).mode_config.dpms_property {
            if let Some(dpms) = (*(*connector).funcs).dpms {
                ret = dpms(connector, value as i32);
            }
        } else if let Some(set_property) = (*(*connector).funcs).set_property {
            ret = set_property(connector, property, value);
        }

        if ret == 0 {
            drm_object_property_set_value(
                core::ptr::addr_of_mut!((*connector).base),
                property,
                value,
            );
        }
        ret
    }
}

// upstream: drm_connector.c drm_connector_property_set_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. `data` must point to a readable and writable
/// `DrmModeConnectorSetProperty` copied in from userspace by the ioctl layer, and
/// `file_priv` must be the open `DrmFile` of the calling client. The struct is read, not
/// retained.
pub unsafe extern "C" fn drm_connector_property_set_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Connector property set ioctl handler; validate arguments and dispatch to driver.
    unsafe {
        let conn_set_prop = data.cast::<DrmModeConnectorSetProperty>();
        let mut obj_set_prop = DrmModeObjSetProperty {
            value: (*conn_set_prop).value,
            prop_id: (*conn_set_prop).prop_id,
            obj_id: (*conn_set_prop).connector_id,
            obj_type: DRM_MODE_OBJECT_CONNECTOR,
        };
        crate::drm_mode_object_upstream::drm_mode_obj_set_property_ioctl(
            dev,
            core::ptr::addr_of_mut!(obj_set_prop).cast(),
            file_priv,
        )
    }
}

// upstream: drm_connector.c drm_connector_get_encoder()
/// # Safety
///
/// `connector` must point to a live `DrmConnector`.
unsafe fn drm_connector_get_encoder(connector: *mut DrmConnector) -> *mut DrmEncoder {
    // SAFETY: `connector` is valid; read current encoder from connector atomic state.
    unsafe {
        if !(*connector).state.is_null() {
            return (*(*connector).state).best_encoder;
        }
        (*connector).encoder
    }
}

/// `DRM_MODE_FLAG_3D_MASK` (include/uapi/drm/drm_mode.h).
const DRM_MODE_FLAG_3D_MASK: u32 = 0x1f << 14;

/// `DRM_MODE_MATCH_*` bits (include/drm/drm_modes.h).
const DRM_MODE_MATCH_TIMINGS: u32 = 1 << 0;
const DRM_MODE_MATCH_CLOCK: u32 = 1 << 1;
const DRM_MODE_MATCH_FLAGS: u32 = 1 << 2;
const DRM_MODE_MATCH_3D_FLAGS: u32 = 1 << 3;

/// `drm_mode_is_stereo()` (include/drm/drm_modes.h).
// upstream: include/drm/drm_modes.h drm_mode_is_stereo()
/// # Safety
///
/// `mode` must point to a live `DrmDisplayMode`.
unsafe fn drm_mode_is_stereo(mode: *const DrmDisplayMode) -> bool {
    // SAFETY: `mode` is a valid pointer; test 3D display flags.
    unsafe { (*mode).flags & DRM_MODE_FLAG_3D_MASK != 0 }
}

/// `drm_connector_lookup()` (include/drm/drm_connector.h).
// upstream: include/drm/drm_connector.h drm_connector_lookup()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be live and `file_priv` must be the calling client's `DrmFile`. `id` is the
/// mode object ID to find; NULL is returned if no connector with that ID is visible to
/// `file_priv`. The returned connector is not referenced by this call.
pub unsafe extern "C" fn drm_connector_lookup(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
) -> *mut DrmConnector {
    // SAFETY: Look up connector mode object by ID in dev mode_config.
    unsafe {
        let mo = drm_mode_object_find(dev, file_priv, id, DRM_MODE_OBJECT_CONNECTOR);
        if mo.is_null() {
            core::ptr::null_mut()
        } else {
            obj_to_connector(mo)
        }
    }
}

// upstream: drm_connector.c drm_mode_expose_to_userspace()
/// # Safety
///
/// `mode` and `file_priv` must be valid pointers.
unsafe fn drm_mode_expose_to_userspace(
    mode: *const DrmDisplayMode,
    modes: *const ListHead,
    file_priv: *const DrmFile,
) -> bool {
    // SAFETY: Check whether display mode is stereo and permitted for caller.
    unsafe {
        if !(*file_priv)
            .stereo_allowed
            .load(core::sync::atomic::Ordering::Relaxed)
            && drm_mode_is_stereo(mode)
        {
            return false;
        }

        if !(*file_priv)
            .aspect_ratio_allowed
            .load(core::sync::atomic::Ordering::Relaxed)
        {
            let head = modes as *mut ListHead;
            let mut cur = (*head).next;
            while cur != head {
                let mode_itr = cur
                    .cast::<u8>()
                    .sub(offset_of!(DrmDisplayMode, head))
                    .cast::<DrmDisplayMode>();
                if (*mode_itr).expose_to_userspace
                    && drm_mode_match(
                        mode_itr,
                        mode,
                        DRM_MODE_MATCH_TIMINGS
                            | DRM_MODE_MATCH_CLOCK
                            | DRM_MODE_MATCH_FLAGS
                            | DRM_MODE_MATCH_3D_FLAGS,
                    )
                {
                    return false;
                }
                cur = (*cur).next;
            }
        }

        true
    }
}

// upstream: drm_connector.c drm_mode_getconnector()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice` and `file_priv` the calling client's `DrmFile`. `data`
/// must point to a `DrmModeGetConnector` from the ioctl layer, readable and writable; its
/// mode and encoder arrays are written back to user memory only when the caller provides
/// space for them.
pub unsafe extern "C" fn drm_mode_getconnector(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is the ioctl's `DrmModeGetConnector` argument, kept live by the ioctl layer for this call; `u_mode` is zeroed stack storage used as the copy-out buffer.
    unsafe {
        let out_resp = data.cast::<DrmModeGetConnector>();
        let mut mode_count: u32 = 0;
        let ret: i32;
        let mut u_mode: DrmModeModeinfo = core::mem::zeroed();

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let connector = drm_connector_lookup(dev, file_priv, (*out_resp).connector_id);
        if connector.is_null() {
            return -ENOENT;
        }

        let encoders_count = (*connector).possible_encoders.count_ones();
        'out: {
            if (*out_resp).count_encoders >= encoders_count && encoders_count != 0 {
                let mut copied: u32 = 0;
                let encoder_ptr = (*out_resp).encoders_ptr as usize as *mut u32;
                // drm_connector_for_each_possible_encoder()
                let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
                let mut cur = (*head).next;
                while cur != head {
                    let encoder = encoder_from_head(cur);
                    cur = (*cur).next;
                    if (*connector).possible_encoders & drm_encoder_mask(encoder) == 0 {
                        continue;
                    }
                    if put_user(encoder_ptr.add(copied as usize), (*encoder).base.id) != 0 {
                        ret = -EFAULT;
                        break 'out;
                    }
                    copied += 1;
                }
            }

            (*out_resp).count_encoders = encoders_count;
            (*out_resp).connector_id = (*connector).base.id;
            (*out_resp).connector_type = (*connector).connector_type as u32;
            (*out_resp).connector_type_id = (*connector).connector_type_id as u32;

            let is_current_master = drm_is_current_master(file_priv);

            mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
            if (*out_resp).count_modes == 0 {
                if is_current_master {
                    if let Some(fill_modes) = (*(*connector).funcs).fill_modes {
                        fill_modes(
                            connector,
                            (*dev).mode_config.max_width,
                            (*dev).mode_config.max_height,
                        );
                    }
                } else {
                    drm_dbg_kms!(
                        dev,
                        "User-space requested a forced probe on [CONNECTOR:%d:%s] but is not the \
                         DRM master, demoting to read-only probe\n",
                        (*connector).base.id,
                        (*connector).name
                    );
                }
            }

            (*out_resp).mm_width = (*connector).display_info.width_mm;
            (*out_resp).mm_height = (*connector).display_info.height_mm;
            (*out_resp).subpixel = (*connector).display_info.subpixel_order;
            (*out_resp).connection = (*connector).status;

            let modes_head = core::ptr::addr_of_mut!((*connector).modes);
            let mut cur = (*modes_head).next;
            while cur != modes_head {
                let mode = cur
                    .cast::<u8>()
                    .sub(offset_of!(DrmDisplayMode, head))
                    .cast::<DrmDisplayMode>();
                warn_on((*mode).expose_to_userspace);
                if drm_mode_expose_to_userspace(mode, modes_head, file_priv) {
                    (*mode).expose_to_userspace = true;
                    mode_count += 1;
                }
                cur = (*cur).next;
            }

            if (*out_resp).count_modes >= mode_count && mode_count != 0 {
                let mut copied: u32 = 0;
                let mode_ptr = (*out_resp).modes_ptr as usize as *mut DrmModeModeinfo;
                let mut cur = (*modes_head).next;
                while cur != modes_head {
                    let mode = cur
                        .cast::<u8>()
                        .sub(offset_of!(DrmDisplayMode, head))
                        .cast::<DrmDisplayMode>();
                    cur = (*cur).next;
                    if !(*mode).expose_to_userspace {
                        continue;
                    }
                    (*mode).expose_to_userspace = false;
                    drm_mode_convert_to_umode(&mut u_mode, mode);
                    if !(*file_priv)
                        .aspect_ratio_allowed
                        .load(core::sync::atomic::Ordering::Relaxed)
                    {
                        u_mode.flags &= !DRM_MODE_FLAG_PIC_AR_MASK;
                    }
                    if copy_to_user_c(
                        mode_ptr.add(copied as usize).cast(),
                        core::ptr::addr_of!(u_mode).cast(),
                        size_of::<DrmModeModeinfo>(),
                    ) != 0
                    {
                        ret = -EFAULT;
                        // list_for_each_entry_continue(): clear the rest.
                        let mut rest = cur;
                        while rest != modes_head {
                            let m = rest
                                .cast::<u8>()
                                .sub(offset_of!(DrmDisplayMode, head))
                                .cast::<DrmDisplayMode>();
                            (*m).expose_to_userspace = false;
                            rest = (*rest).next;
                        }
                        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
                        break 'out;
                    }
                    copied += 1;
                }
            } else {
                let mut cur = (*modes_head).next;
                while cur != modes_head {
                    let mode = cur
                        .cast::<u8>()
                        .sub(offset_of!(DrmDisplayMode, head))
                        .cast::<DrmDisplayMode>();
                    (*mode).expose_to_userspace = false;
                    cur = (*cur).next;
                }
            }

            (*out_resp).count_modes = mode_count;
            mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));

            drm_modeset_lock(
                core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
                core::ptr::null_mut(),
            );
            let encoder = drm_connector_get_encoder(connector);
            if !encoder.is_null() {
                (*out_resp).encoder_id = (*encoder).base.id;
            } else {
                (*out_resp).encoder_id = 0;
            }

            ret = drm_mode_object_get_properties(
                core::ptr::addr_of_mut!((*connector).base),
                (*file_priv)
                    .atomic
                    .load(core::sync::atomic::Ordering::Relaxed),
                (*file_priv)
                    .plane_color_pipeline
                    .load(core::sync::atomic::Ordering::Relaxed),
                (*out_resp).props_ptr as usize as *mut u32,
                (*out_resp).prop_values_ptr as usize as *mut u64,
                core::ptr::addr_of_mut!((*out_resp).count_props),
            );
            drm_modeset_unlock(core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex));
        }

        drm_connector_put(connector);
        ret
    }
}

/// `drm_connector_get()` (include/drm/drm_connector.h).
///
/// # Safety
///
/// `connector` must point to an initialized, live `DrmConnector` with a non-zero reference count.
// upstream: include/drm/drm_connector.h drm_connector_get()
#[unsafe(export_name = "drm_connector_get")]
pub(crate) unsafe extern "C" fn drm_connector_get(connector: *mut DrmConnector) {
    // SAFETY: `connector` is valid; increment reference count on mode object base.
    unsafe { drm_mode_object_get(core::ptr::addr_of_mut!((*connector).base)) }
}

/// `drm_connector_put()` (include/drm/drm_connector.h).
///
/// # Safety
///
/// `connector` must be null or point to an initialized, live `DrmConnector` with at least one reference owned by caller.
// upstream: include/drm/drm_connector.h drm_connector_put()
#[unsafe(export_name = "drm_connector_put")]
pub(crate) unsafe extern "C" fn drm_connector_put(connector: *mut DrmConnector) {
    // SAFETY: `connector` is valid; drop reference count on mode object base.
    unsafe { drm_mode_object_put(core::ptr::addr_of_mut!((*connector).base)) }
}

/// `drm_connector_find_by_fwnode()`.
///
/// Deviation: fwnode handles are not ported (`fwnode` is always NULL on this
/// target), so only the `connector->fwnode == fwnode` identity match is kept
/// and the `->secondary` fallback is omitted.
// upstream: drm_connector.c drm_connector_find_by_fwnode()
#[unsafe(no_mangle)]
/// # Safety
/// `fwnode` is used only as an identity key and is never dereferenced; NULL is rejected
/// with an error pointer. The matching connector is returned with a reference taken, which
/// the caller releases with `drm_connector_put`.
pub unsafe extern "C" fn drm_connector_find_by_fwnode(fwnode: *mut c_void) -> *mut DrmConnector {
    // SAFETY: `fwnode` is compared only as an identity key and never dereferenced; the global list is walked under `list_lock`, which is held until the walk ends.
    unsafe {
        if fwnode.is_null() {
            return err_ptr_c(-ENODEV);
        }

        let mut found: *mut DrmConnector = err_ptr_c(-ENODEV);
        connector_globals_ensure();
        mutex_lock(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>());
        let head = CONNECTOR_GLOBALS.list.get().cast::<ListHead>();
        let mut cur = (*head).next;
        while cur != head {
            let connector = cur
                .cast::<u8>()
                .sub(offset_of!(DrmConnector, global_connector_list_entry))
                .cast::<DrmConnector>();
            if (*connector).fwnode == fwnode {
                drm_connector_get(connector);
                found = connector;
                break;
            }
            cur = (*cur).next;
        }
        mutex_unlock(CONNECTOR_GLOBALS.list_lock.get().cast::<Mutex>());
        found
    }
}

/// `ENODEV` error pointer helper for the connector lookup.
const ENODEV: i32 = 19;

// upstream: drm_connector.c drm_connector_oob_hotplug_event()
#[unsafe(no_mangle)]
/// # Safety
/// `connector_fwnode` is passed to `drm_connector_find_by_fwnode` and is not dereferenced
/// here. A connector found by that lookup is referenced for the call, its driver callback
/// is invoked with `status`, and the reference is dropped before return.
pub unsafe extern "C" fn drm_connector_oob_hotplug_event(
    connector_fwnode: *mut c_void,
    status: DrmConnectorStatus,
) {
    // SAFETY: `connector_fwnode` is used only as a lookup key; the result of `drm_connector_find_by_fwnode` is checked with `is_err_c` before its `funcs` table is read.
    unsafe {
        let connector = drm_connector_find_by_fwnode(connector_fwnode);
        if is_err_c(connector) {
            return;
        }

        if let Some(oob_hotplug_event) = (*(*connector).funcs).oob_hotplug_event {
            oob_hotplug_event(connector, status);
        }

        drm_connector_put(connector);
    }
}

/// `is_err()` in pointer form.
fn is_err_c<T>(ptr: *mut T) -> bool {
    drm_kapi::config::is_err(ptr)
}

/// `err_ptr()` in pointer form.
fn err_ptr_c<T>(error: i32) -> *mut T {
    drm_kapi::config::err_ptr::<T>(error)
}

// upstream: drm_connector.c drm_tile_group_free()
/// # Safety
///
/// `kref` must point to the `refcount` member of an allocated `DrmTileGroup`.
unsafe extern "C" fn drm_tile_group_free(kref: *mut drm_kapi::types::Kref) {
    // SAFETY: `kref` is embedded in `DrmTileGroup`; free tile group resources and memory.
    unsafe {
        let tg = kref.cast::<DrmTileGroup>();
        let dev = (*tg).dev;

        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex));
        idr_remove(
            core::ptr::addr_of_mut!((*dev).mode_config.tile_idr),
            (*tg).id,
        );
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex));

        kfree(tg);
    }
}

// upstream: drm_connector.c drm_mode_put_tile_group()
#[unsafe(no_mangle)]
/// # Safety
/// `tg` must be a live tile group with a non-zero refcount; the caller drops one reference
/// it owns. `drm_tile_group_free` runs and frees the group when the count reaches zero, so
/// `tg` must not be used afterwards by the caller. `_dev` is unused.
pub unsafe extern "C" fn drm_mode_put_tile_group(_dev: *mut DrmDevice, tg: *mut DrmTileGroup) {
    // SAFETY: `tg` is a live tile group whose `refcount` is taken by `addr_of_mut!`, so no reference is formed; `drm_tile_group_free` runs only when the count reaches zero.
    unsafe {
        drm_kapi::memory::kref_put(core::ptr::addr_of_mut!((*tg).refcount), drm_tile_group_free);
    }
}

// upstream: drm_connector.c drm_mode_get_tile_group()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. `topology_id` must point to at least 9 readable bytes
/// that are compared against stored groups. A match is returned with a new reference
/// that the caller releases with `drm_mode_put_tile_group`.
pub unsafe extern "C" fn drm_mode_get_tile_group(
    dev: *mut DrmDevice,
    topology_id: *const u8,
) -> *mut DrmTileGroup {
    // SAFETY: `dev` is live; the tile IDR is walked under `idr_mutex`, which is locked before the walk and protects the `found_tg` result.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex));
        let mut found_tg: *mut DrmTileGroup = core::ptr::null_mut();
        drm_kapi::idr::idr_for_each(
            core::ptr::addr_of_mut!((*dev).mode_config.tile_idr),
            |_id, entry| {
                if !found_tg.is_null() {
                    return;
                }
                let tg = entry.cast::<DrmTileGroup>();
                if core::slice::from_raw_parts((*tg).group_data.as_ptr(), 9)
                    == core::slice::from_raw_parts(topology_id, 9)
                    && drm_kapi::memory::kref_get_unless_zero(&(*tg).refcount)
                {
                    found_tg = tg;
                }
            },
        );
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex));
        found_tg
    }
}

// upstream: drm_connector.c drm_mode_create_tile_group()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must be a live `DrmDevice`. `topology_id` must point to at least 9 readable bytes
/// which are copied into the new group; the caller keeps ownership of its buffer. The
/// returned group starts with refcount 1 and is owned by the caller; NULL signals an
/// allocation or IDR failure.
pub unsafe extern "C" fn drm_mode_create_tile_group(
    dev: *mut DrmDevice,
    topology_id: *const u8,
) -> *mut DrmTileGroup {
    // SAFETY: `kzalloc` returns zeroed storage for one `DrmTileGroup`; `tg` is dereferenced only after the null check passes.
    unsafe {
        let mut tg = kzalloc(size_of::<DrmTileGroup>(), GFP_KERNEL).cast::<DrmTileGroup>();
        if tg.is_null() {
            return core::ptr::null_mut();
        }

        drm_kapi::memory::kref_init(core::ptr::addr_of_mut!((*tg).refcount));
        core::ptr::copy_nonoverlapping(topology_id, (*tg).group_data.as_mut_ptr(), 9);
        (*tg).dev = dev;

        mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex));
        let ret = idr_alloc(
            core::ptr::addr_of_mut!((*dev).mode_config.tile_idr),
            tg.cast(),
            1,
            0,
        );
        if ret >= 0 {
            (*tg).id = ret;
        } else {
            kfree(tg);
            tg = core::ptr::null_mut();
        }
        mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.idr_mutex));

        tg
    }
}

// upstream: drm_connector.c drm_connector_attach_panel_type_property()
#[unsafe(no_mangle)]
/// # Safety
/// `connector` must be live with `dev` set. The panel-type property is read from
/// `dev.mode_config` and attached to `connector.base` with the unknown value; nothing is
/// created here, so a NULL property makes the function a no-op.
pub unsafe extern "C" fn drm_connector_attach_panel_type_property(connector: *mut DrmConnector) {
    // SAFETY: `connector` is live and `dev` is its back-pointer; `panel_type_property` is read from `mode_config` and attached only when non-null.
    unsafe {
        let dev = (*connector).dev;
        let prop = (*dev).mode_config.panel_type_property;
        if prop.is_null() {
            return;
        }
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*connector).base),
            prop,
            DRM_MODE_PANEL_TYPE_UNKNOWN as u64,
        );
    }
}

// upstream: drm_connector.c drm_get_dvi_i_select_name() and siblings (DRM_ENUM_NAME_FN)
#[unsafe(no_mangle)]
pub extern "C" fn drm_get_dvi_i_select_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_DVI_I_SELECT_ENUM_LIST, val)
}

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_dvi_i_subconnector_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_DVI_I_SUBCONNECTOR_ENUM_LIST, val)
}

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_tv_select_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_TV_SELECT_ENUM_LIST, val)
}

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_tv_subconnector_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_TV_SUBCONNECTOR_ENUM_LIST, val)
}

#[unsafe(no_mangle)]
pub extern "C" fn drm_get_dp_subconnector_name(val: i32) -> *const c_char {
    enum_name_fn(&DRM_DP_SUBCONNECTOR_ENUM_LIST, val)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support::TestDrm, uapi_drm_consts::DRM_MODE_CONNECTOR_DISPLAYPORT};

    /// # Safety
    ///
    /// Test destroy callback; safe no-op.
    unsafe extern "C" fn noop_destroy(_connector: *mut DrmConnector) {}

    fn funcs() -> DrmConnectorFuncs {
        DrmConnectorFuncs {
            dpms: None,
            reset: None,
            detect: None,
            force: None,
            fill_modes: None,
            set_property: None,
            late_register: None,
            early_unregister: None,
            destroy: Some(noop_destroy),
            atomic_duplicate_state: None,
            atomic_destroy_state: None,
            atomic_set_property: None,
            atomic_get_property: None,
            atomic_print_state: None,
            oob_hotplug_event: None,
            debugfs_init: None,
        }
    }

    fn name_of(connector: *mut DrmConnector) -> &'static str {
        // SAFETY: Unit test converts connector name to CStr for assertion.
        unsafe {
            core::ffi::CStr::from_ptr((*connector).name)
                .to_str()
                .unwrap()
        }
    }

    fn type_id_from_name(connector: *mut DrmConnector) -> i32 {
        name_of(connector)
            .strip_prefix("DP-")
            .expect("DisplayPort connector name must use the DP-N form")
            .parse()
            .expect("DisplayPort connector name must end in its numeric type ID")
    }

    #[test]
    fn connector_init_assigns_name_ids_and_stays_initializing() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        let f = funcs();
        // SAFETY: Stack-allocated mock `DrmConnector` zero-initialized for unit test.
        let mut conn: DrmConnector = unsafe { core::mem::zeroed() };
        // SAFETY: Unit test initializes connector with dummy callbacks and verifies error handling.
        unsafe {
            let ret = drm_connector_init(dev, &mut conn, &f, DRM_MODE_CONNECTOR_DISPLAYPORT as i32);
            assert_eq!(ret, 0);
            assert!(conn.connector_type_id > 0);
            assert_eq!(type_id_from_name(&mut conn), conn.connector_type_id);
            assert_eq!(conn.index, 0);
            assert_eq!(conn.registration_state, DRM_CONNECTOR_INITIALIZING);
            assert_eq!((*dev).mode_config.num_connector, 1);
            assert_eq!(conn.status, CONNECTOR_STATUS_UNKNOWN);
            assert_eq!(
                conn.display_info.panel_orientation,
                DRM_MODE_PANEL_ORIENTATION_UNKNOWN
            );
            // The standard connector properties were attached.
            assert!(conn.properties.count >= 4);

            drm_connector_cleanup(&mut conn);
            assert_eq!((*dev).mode_config.num_connector, 0);
        }
        t.cleanup_mode_config();
    }

    #[test]
    fn same_type_connectors_get_increasing_type_ids() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        let f = funcs();
        // SAFETY: Stack-allocated mock `DrmConnector` zero-initialized for unit test.
        let mut a: DrmConnector = unsafe { core::mem::zeroed() };
        // SAFETY: `b` is a second zero-initialised stack connector, so its list node is distinct from `a`.
        let mut b: DrmConnector = unsafe { core::mem::zeroed() };
        // SAFETY: Unit test exercises connector list iteration on mock connectors.
        unsafe {
            assert_eq!(
                drm_connector_init(dev, &mut a, &f, DRM_MODE_CONNECTOR_DISPLAYPORT as i32),
                0
            );
            assert_eq!(
                drm_connector_init(dev, &mut b, &f, DRM_MODE_CONNECTOR_DISPLAYPORT as i32),
                0
            );
            // The per-type IDA is process-global and can already contain IDs
            // allocated by parallel tests. Prove uniqueness and stable
            // name-to-ID mapping without assuming it starts empty.
            assert!(a.connector_type_id > 0);
            assert!(b.connector_type_id > 0);
            assert_ne!(a.connector_type_id, b.connector_type_id);
            assert_eq!(type_id_from_name(&mut a), a.connector_type_id);
            assert_eq!(type_id_from_name(&mut b), b.connector_type_id);
            assert_ne!(name_of(&mut a), name_of(&mut b));
            assert_eq!(b.index, 1);

            // The list iterator walks both connectors and then ends.
            let mut iter: DrmConnectorListIter = core::mem::zeroed();
            drm_connector_list_iter_begin(dev, &mut iter);
            let mut seen = 0;
            loop {
                let c = drm_connector_list_iter_next(&mut iter);
                if c.is_null() {
                    break;
                }
                // The iterator owns the reference it returned; no put here.
                seen += 1;
            }
            drm_connector_list_iter_end(&mut iter);
            assert_eq!(seen, 2);

            drm_connector_cleanup(&mut a);
            drm_connector_cleanup(&mut b);
        }
        t.cleanup_mode_config();
    }

    #[test]
    fn register_state_machine_follows_device_registration() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        let f = funcs();
        // SAFETY: Stack-allocated mock `DrmConnector` zero-initialized for unit test.
        let mut conn: DrmConnector = unsafe { core::mem::zeroed() };
        // SAFETY: Unit test exercises connector registration and cleanup on mock connector.
        unsafe {
            assert_eq!(
                drm_connector_init(dev, &mut conn, &f, DRM_MODE_CONNECTOR_DISPLAYPORT as i32),
                0
            );

            // Device not registered yet: the connector stays INITIALIZING.
            assert_eq!(drm_connector_register(&mut conn), 0);
            assert_eq!(conn.registration_state, DRM_CONNECTOR_INITIALIZING);

            (*dev).registered = true;
            assert_eq!(drm_connector_register(&mut conn), 0);
            assert_eq!(conn.registration_state, DRM_CONNECTOR_REGISTERED);

            drm_connector_unregister(&mut conn);
            assert_eq!(conn.registration_state, DRM_CONNECTOR_UNREGISTERED);
            // Unregistering twice is a no-op.
            drm_connector_unregister(&mut conn);
            assert_eq!(conn.registration_state, DRM_CONNECTOR_UNREGISTERED);

            (*dev).registered = false;
            drm_connector_cleanup(&mut conn);
        }
        t.cleanup_mode_config();
    }

    #[test]
    fn subpixel_and_status_names_match_the_c_tables() {
        // SAFETY: Unit test parses C string pointers to test connector type name lookup.
        unsafe {
            let name = |p: *const c_char| core::ffi::CStr::from_ptr(p).to_str().unwrap();
            assert_eq!(
                name(drm_get_connector_status_name(CONNECTOR_STATUS_CONNECTED)),
                "connected"
            );
            assert_eq!(
                name(drm_get_connector_status_name(CONNECTOR_STATUS_UNKNOWN)),
                "unknown"
            );
            assert_eq!(
                name(drm_get_connector_force_name(DRM_FORCE_ON_DIGITAL)),
                "digital"
            );
            assert_eq!(name(drm_get_subpixel_order_name(SUBPIXELNONE)), "None");
            assert_eq!(
                name(drm_get_colorspace_name(DRM_MODE_COLORIMETRY_BT601_YCC)),
                "BT601_YCC"
            );
            assert_eq!(
                name(drm_get_connector_type_name(DRM_MODE_CONNECTOR_DISPLAYPORT)),
                "DP"
            );
        }
    }
}

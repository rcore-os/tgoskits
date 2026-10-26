// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_crtc.c`.
//!
//! CRTC object init/cleanup (plain and managed), the legacy GETCRTC and
//! SETCRTC ioctls, the internal set-config path, fences, and the scaling
//! filter and sharpness properties. Functions are named and ordered as in the
//! C file.
//!
//! Deviations (documented in the batch report):
//! - `drm_crtc_init_with_planes()` / `drmm_crtc_*` take the already formatted
//!   CRTC name; C variadic formatting cannot be expressed in this ABI.
//! - `CONFIG_DEBUG_FS` is off for this configuration, so the CRC source
//!   bookkeeping is compiled out, as `drm_crtc_crc_init()` does in C.

// The Linux ports state the invariant of each C-ABI call once in the module
// header, as the GT ports do; per-block SAFETY prose is not repeated.

use core::{
    ffi::{c_char, c_void},
    mem::{offset_of, size_of, zeroed},
};

use drm_kapi::{
    assert::warn_on,
    config::{
        EFAULT, EINVAL, ENOENT, ENOMEM, EOPNOTSUPP, ERANGE, GFP_KERNEL, err_ptr, is_err, ptr_err,
    },
};

/// `EACCES` (errno 13; not present in the kapi errno table).
const EACCES: i32 = 13;
use drm_kapi::{
    list::{init_list_head, list_add_tail, list_del},
    locks::spin_lock_init,
    types::{DmaFence, ListHead},
};

use crate::{
    drm_color_mgmt_upstream::{
        drm_core_check_feature, drm_drv_uses_atomic_modeset, with_modeset_lock_all,
    },
    drm_connector_types::DrmConnector,
    drm_crtc_types::{DrmCrtc, DrmCrtcFuncs, DrmCrtcState, DrmModeSet},
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{DRIVER_ATOMIC, DRIVER_MODESET},
    drm_modes_types::DrmDisplayMode,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_plane_types::DrmPlane,
    drm_property_types::DrmProperty,
    uapi_drm_consts::{
        DRM_MODE_FLAG_PIC_AR_MASK, DRM_MODE_FLAG_PIC_AR_NONE, DRM_MODE_OBJECT_CRTC,
        DRM_MODE_ROTATE_90, DRM_MODE_ROTATE_270,
    },
    uapi_drm_mode_types::{DrmModeCrtc, DrmModeModeinfo},
};

/// `DRM_SCALING_FILTER_DEFAULT` (include/drm/drm_plane.h).
const DRM_SCALING_FILTER_DEFAULT: u32 = 0;
/// `DRM_PLANE_TYPE_PRIMARY` / `DRM_PLANE_TYPE_CURSOR` (include/drm/drm_plane.h).
const DRM_PLANE_TYPE_PRIMARY: u32 = 1;
const DRM_PLANE_TYPE_CURSOR: u32 = 2;

/// `struct dma_fence_ops` (include/linux/dma-fence.h), canonical in tk-linux-kapi
/// next to the fence core that dispatches through it.
pub use drm_kapi::types::DmaFenceOps;

unsafe extern "C" {
    // upstream: drm_mode_object.c
    fn drm_mode_object_add(dev: *mut DrmDevice, obj: *mut DrmModeObject, obj_type: u32) -> i32;
    fn drm_mode_object_unregister(dev: *mut DrmDevice, obj: *mut DrmModeObject);
    fn drm_mode_object_find(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        id: u32,
        obj_type: u32,
    ) -> *mut DrmModeObject;
    fn drm_mode_object_get(obj: *mut DrmModeObject);
    fn drm_mode_object_put(obj: *mut DrmModeObject);
    fn drm_object_attach_property(
        obj: *mut DrmModeObject,
        property: *mut DrmProperty,
        init_val: u64,
    );
    fn drm_object_property_set_value(
        obj: *mut DrmModeObject,
        property: *mut DrmProperty,
        val: u64,
    ) -> i32;
    // upstream: drm_property.c
    fn drm_property_create_range(
        dev: *mut DrmDevice,
        flags: u32,
        name: *const c_char,
        min: u64,
        max: u64,
    ) -> *mut DrmProperty;
    // upstream: drm_modeset_lock.c
    fn drm_modeset_lock_init(lock: *mut DrmModesetLock);
    fn drm_modeset_lock(lock: *mut DrmModesetLock, ctx: *mut DrmModesetAcquireCtx) -> i32;
    fn drm_modeset_unlock(lock: *mut DrmModesetLock);
    // upstream: drm_framebuffer.c
    fn drm_framebuffer_lookup(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        id: u32,
    ) -> *mut DrmFramebuffer;
    fn drm_framebuffer_check_src_coords(
        src_x: u32,
        src_y: u32,
        src_w: u32,
        src_h: u32,
        fb: *const DrmFramebuffer,
    ) -> i32;
    // upstream: drm_modes.c
    fn drm_mode_create(dev: *mut DrmDevice) -> *mut DrmDisplayMode;
    fn drm_mode_destroy(dev: *mut DrmDevice, mode: *mut DrmDisplayMode);
    fn drm_mode_convert_umode(
        dev: *mut DrmDevice,
        out: *mut DrmDisplayMode,
        in_: *const DrmModeModeinfo,
    ) -> i32;
    fn drm_mode_convert_to_umode(out: *mut DrmModeModeinfo, in_: *const DrmDisplayMode);
    fn drm_mode_get_hv_timing(mode: *const DrmDisplayMode, hdisplay: *mut i32, vdisplay: *mut i32);
    // upstream: drm_connector.c
    fn drm_connector_lookup(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        id: u32,
    ) -> *mut DrmConnector;
    // upstream: drm_lease.c
    fn drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool;
    // upstream: kernel/locking/ww_mutex.c ww_mutex_destroy() not used; drm_modeset_lock_fini() only warns
    // upstream: drivers/dma-buf/dma-fence.c
    fn dma_fence_init(
        fence: *mut DmaFence,
        ops: *const DmaFenceOps,
        lock: *mut drm_kapi::types::Spinlock,
        context: u64,
        seqno: u64,
    );
    fn dma_fence_context_alloc(num: u32) -> u64;
}

// upstream: include/drm/drm_mode_object.h drm_mode_object_get() / drm_mode_object_put() via fb/connector helpers
/// # Safety
/// `fb` must point to an initialized `DrmFramebuffer`.
#[inline]
unsafe fn drm_framebuffer_get(fb: *mut DrmFramebuffer) {
    // SAFETY: `fb` is non-null and valid. Taking a mutable pointer to embedded `base` mode object
    // and calling `drm_mode_object_get` increments its reference count.
    unsafe { drm_mode_object_get(core::ptr::addr_of_mut!((*fb).base)) }
}

// upstream: include/drm/drm_framebuffer.h drm_framebuffer_put()
/// # Safety
/// `fb` must point to an initialized `DrmFramebuffer` with an active reference.
#[inline]
unsafe fn drm_framebuffer_put(fb: *mut DrmFramebuffer) {
    // SAFETY: `fb` is non-null and valid. Taking a mutable pointer to embedded `base` mode object
    // and calling `drm_mode_object_put` decrements its reference count.
    unsafe { drm_mode_object_put(core::ptr::addr_of_mut!((*fb).base)) }
}

// upstream: include/drm/drm_connector.h drm_connector_put()
/// # Safety
/// `connector` must point to an initialized `DrmConnector` with an active reference.
#[inline]
unsafe fn drm_connector_put(connector: *mut DrmConnector) {
    // SAFETY: `connector` is non-null and valid. Calling `drm_mode_object_put` on its embedded `base`
    // mode object decrements its reference count.
    unsafe { drm_mode_object_put(core::ptr::addr_of_mut!((*connector).base)) }
}

// upstream: include/drm/drm_crtc.h drm_crtc_index()
/// # Safety
/// `crtc` must point to an initialized, readable `DrmCrtc`.
#[inline]
pub(crate) unsafe fn drm_crtc_index(crtc: *const DrmCrtc) -> u32 {
    // SAFETY: `crtc` is guaranteed valid and readable per caller precondition.
    unsafe { (*crtc).index }
}

// upstream: include/drm/drm_crtc.h drm_crtc_mask()
/// # Safety
/// `crtc` must point to an initialized, readable `DrmCrtc`.
#[inline]
pub(crate) unsafe fn drm_crtc_mask(crtc: *const DrmCrtc) -> u32 {
    // SAFETY: Calling `drm_crtc_index` on valid `crtc` returns a valid index (< 32 per CRTC init).
    unsafe { 1u32 << drm_crtc_index(crtc) }
}

// upstream: include/drm/drm_crtc.h obj_to_crtc()
/// # Safety
/// `obj` must point to a `DrmModeObject` embedded inside an initialized `DrmCrtc`.
#[inline]
unsafe fn obj_to_crtc(obj: *mut DrmModeObject) -> *mut DrmCrtc {
    // SAFETY: `obj` is embedded at offset `offset_of!(DrmCrtc, base)`. Subtracting this byte offset
    // reconstructs the pointer to the containing `DrmCrtc`.
    unsafe { (obj as *mut u8).sub(offset_of!(DrmCrtc, base)) as *mut DrmCrtc }
}

// upstream: include/drm/drm_crtc.h drm_crtc_find()
/// Looks up a CRTC from its userspace object ID.
/// # Safety
/// `dev` must point to an initialized `DrmDevice`. `file_priv` must be a valid open `DrmFile` or null.
pub(crate) unsafe fn drm_crtc_find(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
) -> *mut DrmCrtc {
    // SAFETY: `drm_mode_object_find` looks up the object ID with type `DRM_MODE_OBJECT_CRTC`.
    // If found, the object is guaranteed to be embedded inside a `DrmCrtc`, making `obj_to_crtc` sound.
    unsafe {
        let mo = drm_mode_object_find(dev, file_priv, id, DRM_MODE_OBJECT_CRTC);
        if mo.is_null() {
            core::ptr::null_mut()
        } else {
            obj_to_crtc(mo)
        }
    }
}

/// Iterates `dev->mode_config.crtc_list` (drm_for_each_crtc()).
// upstream: include/drm/drm_crtc.h drm_for_each_crtc()
/// # Safety
/// `dev` must point to an initialized `DrmDevice`. Callers must ensure `crtc_list` is synchronized
/// against concurrent modifications (e.g., holding `mode_config.mutex`).
unsafe fn for_each_crtc(dev: *mut DrmDevice, mut f: impl FnMut(*mut DrmCrtc) -> bool) {
    // SAFETY: `dev` is valid and live. Iterating through circular linked list `crtc_list`
    // using `head` offsets correctly recovers each `DrmCrtc` container pointer via pointer subtraction.
    unsafe {
        let head: *mut ListHead = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            let crtc = (cur as *mut u8).sub(offset_of!(DrmCrtc, head)) as *mut DrmCrtc;
            if !f(crtc) {
                break;
            }
            cur = (*cur).next;
        }
    }
}

/// Returns the CRTC with the given index, or null.
// upstream: drm_crtc.c drm_crtc_from_index()
/// # Safety
/// `dev` must point to an initialized `DrmDevice` with synchronized `mode_config.crtc_list`.
pub unsafe fn drm_crtc_from_index(dev: *mut DrmDevice, idx: i32) -> *mut DrmCrtc {
    // SAFETY: Traversing the list with `for_each_crtc` is safe under caller synchronization.
    unsafe {
        let mut found: *mut DrmCrtc = core::ptr::null_mut();
        for_each_crtc(dev, |crtc| {
            if idx == (*crtc).index as i32 {
                found = crtc;
                return false;
            }
            true
        });
        found
    }
}

/// Disables the CRTC through the legacy set-config path.
// upstream: drm_crtc.c drm_crtc_force_disable()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must point to an initialized, live `DrmCrtc` associated with an initialized non-atomic `DrmDevice`.
/// Callers must hold modeset locks.
pub unsafe extern "C" fn drm_crtc_force_disable(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: `set` is zero-initialized on the stack and its `crtc` field assigned to `crtc`.
    // Calling `drm_mode_set_config_internal` executes the legacy set_config hook to disable output.
    unsafe {
        let mut set: DrmModeSet = zeroed();
        set.crtc = crtc;
        warn_on(drm_drv_uses_atomic_modeset((*crtc).dev));
        drm_mode_set_config_internal(&mut set)
    }
}

/// Registers the CRTCs' debugfs and late-register hooks.
// upstream: drm_crtc.c drm_crtc_register_all()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_register_all(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is valid. Iterating through `dev.mode_config.crtc_list` and invoking the optional
    // `late_register` callback for each registered CRTC is safe during device registration.
    unsafe {
        let mut ret: i32 = 0;
        let mut err: Option<i32> = None;
        for_each_crtc(dev, |crtc| {
            if let Some(late_register) = (*(*crtc).funcs).late_register {
                ret = late_register(crtc);
            }
            if ret != 0 {
                err = Some(ret);
                return false;
            }
            true
        });
        err.unwrap_or(0)
    }
}

/// Runs the early-unregister hooks and removes the CRTCs' debugfs entries.
// upstream: drm_crtc.c drm_crtc_unregister_all()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_unregister_all(dev: *mut DrmDevice) {
    // SAFETY: `dev` is valid. Iterating through `dev.mode_config.crtc_list` and invoking the optional
    // `early_unregister` callback for each registered CRTC is safe during device unregistration.
    unsafe {
        for_each_crtc(dev, |crtc| {
            if let Some(early_unregister) = (*(*crtc).funcs).early_unregister {
                early_unregister(crtc);
            }
            true
        });
    }
}

// upstream: drm_crtc.c drm_crtc_crc_init()
// CONFIG_DEBUG_FS is off: the CRC source string and waitqueue are not used.
/// # Safety
/// `_crtc` may be null or point to a `DrmCrtc`.
unsafe fn drm_crtc_crc_init(_crtc: *mut DrmCrtc) -> i32 {
    0
}

// upstream: drm_crtc.c drm_crtc_crc_fini()
/// # Safety
/// `_crtc` may be null or point to a `DrmCrtc`.
unsafe fn drm_crtc_crc_fini(_crtc: *mut DrmCrtc) {}

// upstream: drm_crtc.c fence_to_crtc()
/// # Safety
/// `fence` must be non-null and point to a `DmaFence` initialized with `DRM_CRTC_FENCE_OPS`.
unsafe fn fence_to_crtc(fence: *mut DmaFence) -> *mut DrmCrtc {
    // SAFETY: The fence's `ops` is checked to match `DRM_CRTC_FENCE_OPS`. Its `lock.extern_lock`
    // was initialized with the address of `DrmCrtc.fence_lock`. Subtracting `offset_of!(DrmCrtc, fence_lock)`
    // safely recovers the outer `DrmCrtc` pointer.
    unsafe {
        // BUG_ON(rcu_access_pointer(fence->ops) != &drm_crtc_fence_ops)
        drm_kapi::BUG_ON!(
            (*fence).ops != &DRM_CRTC_FENCE_OPS as *const DmaFenceOps as *const c_void
        );
        ((*fence).lock.extern_lock as *mut u8).sub(offset_of!(DrmCrtc, fence_lock)) as *mut DrmCrtc
    }
}

// upstream: drm_crtc.c drm_crtc_fence_get_driver_name()
/// # Safety
/// `fence` must point to a valid `DmaFence` associated with a live `DrmCrtc`.
unsafe extern "C" fn drm_crtc_fence_get_driver_name(fence: *mut DmaFence) -> *const c_char {
    // SAFETY: `fence_to_crtc` retrieves the containing CRTC. `(*crtc).dev` and its `driver`
    // are initialized and live, so reading `(*driver).name` yields a valid string pointer.
    unsafe {
        let crtc = fence_to_crtc(fence);
        (*(*(*crtc).dev).driver).name as *const c_char
    }
}

// upstream: drm_crtc.c drm_crtc_fence_get_timeline_name()
/// # Safety
/// `fence` must point to a valid `DmaFence` associated with a live `DrmCrtc`.
unsafe extern "C" fn drm_crtc_fence_get_timeline_name(fence: *mut DmaFence) -> *const c_char {
    // SAFETY: `fence_to_crtc` retrieves the containing CRTC. Returning the address of
    // `(*crtc).timeline_name` array is valid while `crtc` remains live.
    unsafe {
        let crtc = fence_to_crtc(fence);
        (*crtc).timeline_name.as_ptr()
    }
}

// upstream: drm_crtc.c drm_crtc_fence_ops
static DRM_CRTC_FENCE_OPS: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(drm_crtc_fence_get_driver_name),
    get_timeline_name: Some(drm_crtc_fence_get_timeline_name),
    enable_signaling: None,
    signaled: None,
    wait: None,
    release: None,
    set_deadline: None,
};

/// Allocates a fence on the CRTC's fence context.
// upstream: drm_crtc.c drm_crtc_create_fence()
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc`.
pub unsafe fn drm_crtc_create_fence(crtc: *mut DrmCrtc) -> *mut DmaFence {
    // SAFETY: Allocating `DmaFence` with `kzalloc`. If non-null, initializing it via `dma_fence_init`
    // with `DRM_CRTC_FENCE_OPS` and `(*crtc).fence_lock` correctly initializes the fence and attaches
    // it to `(*crtc).fence_context`.
    unsafe {
        // kzalloc_obj(*fence)
        let fence = drm_kapi::memory::kzalloc(size_of::<DmaFence>(), GFP_KERNEL) as *mut DmaFence;
        if fence.is_null() {
            return core::ptr::null_mut();
        }
        (*crtc).fence_seqno += 1;
        dma_fence_init(
            fence,
            &DRM_CRTC_FENCE_OPS,
            core::ptr::addr_of_mut!((*crtc).fence_lock),
            (*crtc).fence_context as u64,
            (*crtc).fence_seqno,
        );
        fence
    }
}

/// Copies `src` (a NUL-terminated name) into a fresh kernel allocation.
// upstream: drm_crtc.c kstrdup()/kasprintf() result for the CRTC name
/// # Safety
/// `src` must point to a valid null-terminated C string.
unsafe fn kstrdup_name(src: *const c_char) -> *mut c_char {
    // SAFETY: Stepping through `src` until 0 measures the string length without out-of-bounds reads.
    // `kzalloc` allocates `len + 1` bytes; copying `len` bytes from `src` to `dst` leaves the buffer
    // null-terminated and safely allocated in kernel memory.
    unsafe {
        let mut len = 0usize;
        while *src.add(len) != 0 {
            len += 1;
        }
        let dst = drm_kapi::memory::kzalloc(len + 1, GFP_KERNEL) as *mut c_char;
        if dst.is_null() {
            return dst;
        }
        core::ptr::copy_nonoverlapping(src, dst, len);
        dst
    }
}

/// Formats "crtc-%d" (the default name) into a fresh kernel allocation.
// upstream: drm_crtc.c kasprintf(GFP_KERNEL, "crtc-%d", config->num_crtc)
/// # Safety
/// Caller must ensure kernel memory allocator is operational.
unsafe fn kasprintf_crtc_default(num: i32) -> *mut c_char {
    // SAFETY: Fixed-size buffers `buf` (16 bytes) and `digits` (12 bytes) are guaranteed large enough
    // for "crtc-" prefix and any 32-bit integer formatted in base-10. Allocating `len + 1` bytes and copying
    // produces a valid null-terminated heap string.
    unsafe {
        let mut buf = [0u8; 16];
        let mut len = 0usize;
        let prefix = b"crtc-";
        buf[..prefix.len()].copy_from_slice(prefix);
        len += prefix.len();
        let mut digits = [0u8; 12];
        let mut n = num as i64;
        let neg = n < 0;
        if neg {
            n = -n;
        }
        let mut nd = 0usize;
        loop {
            digits[nd] = b'0' + (n % 10) as u8;
            nd += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        if neg {
            buf[len] = b'-';
            len += 1;
        }
        while nd > 0 {
            nd -= 1;
            buf[len] = digits[nd];
            len += 1;
        }
        let dst = drm_kapi::memory::kzalloc(len + 1, GFP_KERNEL) as *mut c_char;
        if dst.is_null() {
            return dst;
        }
        core::ptr::copy_nonoverlapping(buf.as_ptr(), dst.cast::<u8>(), len);
        dst
    }
}

/// `snprintf(crtc->timeline_name, sizeof(crtc->timeline_name), "CRTC:%d-%s", id, name)`.
// upstream: drm_crtc.c __drm_crtc_init_with_planes() timeline name
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc`.
unsafe fn format_timeline_name(crtc: *mut DrmCrtc) {
    // SAFETY: `crtc` is valid and non-null. The inner `put` closure enforces bounds `pos + 1 < size`,
    // and `out[pos] = 0` guarantees null-termination within `(*crtc).timeline_name`.
    unsafe {
        let out = &mut (*crtc).timeline_name;
        let size = out.len();
        let mut pos = 0usize;
        let put = |b: u8, out: &mut [c_char], pos: &mut usize| {
            if *pos + 1 < size {
                out[*pos] = b as c_char;
                *pos += 1;
            }
        };
        for &b in b"CRTC:" {
            put(b, out, &mut pos);
        }
        let mut digits = [0u8; 12];
        let mut n = (*crtc).base.id as u64;
        let mut nd = 0usize;
        loop {
            digits[nd] = b'0' + (n % 10) as u8;
            nd += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        while nd > 0 {
            nd -= 1;
            put(digits[nd], out, &mut pos);
        }
        put(b'-', out, &mut pos);
        let mut name = (*crtc).name as *const u8;
        if !name.is_null() {
            while *name != 0 {
                put(*name, out, &mut pos);
                name = name.add(1);
            }
        }
        out[pos] = 0;
    }
}

/// Common CRTC init used by the plain and managed variants.
// upstream: drm_crtc.c __drm_crtc_init_with_planes()
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
/// `crtc` must point to writable memory of at least `size_of::<DrmCrtc>()`.
/// `funcs` must point to an initialized `DrmCrtcFuncs` table.
/// `primary` and `cursor` must either be null or point to valid `DrmPlane` instances.
/// If `name` is non-null, it must be a valid null-terminated C string.
unsafe fn __drm_crtc_init_with_planes(
    dev: *mut DrmDevice,
    crtc: *mut DrmCrtc,
    primary: *mut DrmPlane,
    cursor: *mut DrmPlane,
    funcs: *const DrmCrtcFuncs,
    name: *const c_char,
) -> i32 {
    // SAFETY: Initializing fields of `crtc` (`commit_list`, `commit_lock`, `mutex`), registering
    // `(*crtc).base` as `DRM_MODE_OBJECT_CRTC`, allocating the heap name, setting up fence context,
    // linking into `(*dev).mode_config.crtc_list`, assigning primary/cursor planes, and attaching
    // atomic properties. Unwinding unregisters mode object if allocation or CRC init fails.
    unsafe {
        let config = core::ptr::addr_of_mut!((*dev).mode_config);

        warn_on(!primary.is_null() && (*primary).r#type != DRM_PLANE_TYPE_PRIMARY);
        warn_on(!cursor.is_null() && (*cursor).r#type != DRM_PLANE_TYPE_CURSOR);

        // crtc index is used with 32bit bitmasks
        if warn_on((*config).num_crtc >= 32) {
            return -EINVAL;
        }

        warn_on(
            drm_drv_uses_atomic_modeset(dev)
                && ((*funcs).atomic_destroy_state.is_none()
                    || (*funcs).atomic_duplicate_state.is_none()),
        );

        (*crtc).dev = dev;
        (*crtc).funcs = funcs as *mut DrmCrtcFuncs;
        init_list_head(core::ptr::addr_of_mut!((*crtc).commit_list));
        spin_lock_init(&mut (*crtc).commit_lock);
        drm_modeset_lock_init(core::ptr::addr_of_mut!((*crtc).mutex));

        let ret = drm_mode_object_add(
            dev,
            core::ptr::addr_of_mut!((*crtc).base),
            DRM_MODE_OBJECT_CRTC,
        );
        if ret != 0 {
            return ret;
        }

        (*crtc).name = if !name.is_null() {
            kstrdup_name(name)
        } else {
            kasprintf_crtc_default((*config).num_crtc)
        };

        if (*crtc).name.is_null() {
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*crtc).base));
            return -ENOMEM;
        }

        (*crtc).fence_context = dma_fence_context_alloc(1) as u32;
        spin_lock_init(&mut (*crtc).fence_lock);
        format_timeline_name(crtc);

        (*crtc).base.properties = core::ptr::addr_of_mut!((*crtc).properties);

        list_add_tail(
            core::ptr::addr_of_mut!((*crtc).head),
            core::ptr::addr_of_mut!((*config).crtc_list),
        );
        (*crtc).index = (*config).num_crtc as u32;
        (*config).num_crtc += 1;

        (*crtc).primary = primary;
        (*crtc).cursor = cursor;

        if !primary.is_null() && (*primary).possible_crtcs == 0 {
            (*primary).possible_crtcs = drm_crtc_mask(crtc);
        }
        if !cursor.is_null() && (*cursor).possible_crtcs == 0 {
            (*cursor).possible_crtcs = drm_crtc_mask(crtc);
        }

        let ret = drm_crtc_crc_init(crtc);
        if ret != 0 {
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*crtc).base));
            return ret;
        }

        if drm_core_check_feature(dev, DRIVER_ATOMIC) {
            let base = core::ptr::addr_of_mut!((*crtc).base);
            drm_object_attach_property(base, (*config).prop_active, 0);
            drm_object_attach_property(base, (*config).prop_mode_id, 0);
            drm_object_attach_property(base, (*config).prop_out_fence_ptr, 0);
            drm_object_attach_property(base, (*config).prop_vrr_enabled, 0);
        }

        0
    }
}

/// Initializes a CRTC with explicit primary and cursor planes.
/// `name` is the already formatted CRTC name (NULL selects "crtc-%d").
// upstream: drm_crtc.c drm_crtc_init_with_planes()
#[unsafe(no_mangle)]
/// # Safety
/// Preconditions match `__drm_crtc_init_with_planes`. In addition, `(*funcs).destroy` must be provided.
pub unsafe extern "C" fn drm_crtc_init_with_planes(
    dev: *mut DrmDevice,
    crtc: *mut DrmCrtc,
    primary: *mut DrmPlane,
    cursor: *mut DrmPlane,
    funcs: *const DrmCrtcFuncs,
    name: *const c_char,
) -> i32 {
    // SAFETY: Forwarding to `__drm_crtc_init_with_planes` after verifying `(*funcs).destroy` hook is set.
    unsafe {
        warn_on((*funcs).destroy.is_none());
        __drm_crtc_init_with_planes(dev, crtc, primary, cursor, funcs, name)
    }
}

// upstream: drm_crtc.c drmm_crtc_init_with_planes_cleanup()
/// # Safety
/// `ptr` must point to a `DrmCrtc` initialized via `__drmm_crtc_init_with_planes`.
unsafe extern "C" fn drmm_crtc_init_with_planes_cleanup(_dev: *mut c_void, ptr: *mut c_void) {
    // SAFETY: `ptr` was passed as `crtc as *mut c_void` during action registration, so casting back
    // to `*mut DrmCrtc` and invoking `drm_crtc_cleanup` is valid.
    unsafe { drm_crtc_cleanup(ptr as *mut DrmCrtc) }
}

// upstream: drm_crtc.c __drmm_crtc_init_with_planes()
/// # Safety
/// Preconditions match `__drm_crtc_init_with_planes`, with `(*funcs).destroy` set to None.
unsafe fn __drmm_crtc_init_with_planes(
    dev: *mut DrmDevice,
    crtc: *mut DrmCrtc,
    primary: *mut DrmPlane,
    cursor: *mut DrmPlane,
    funcs: *const DrmCrtcFuncs,
    name: *const c_char,
) -> i32 {
    // SAFETY: Initializing CRTC via `__drm_crtc_init_with_planes` and binding `drmm_crtc_init_with_planes_cleanup`
    // to device lifetime via `drmm_add_action_or_reset` ensures automatic teardown.
    unsafe {
        warn_on(!funcs.is_null() && (*funcs).destroy.is_some());

        let ret = __drm_crtc_init_with_planes(dev, crtc, primary, cursor, funcs, name);
        if ret != 0 {
            return ret;
        }

        let ret = drm_kapi::kernel_core::drmm_add_action_or_reset(
            dev as *mut c_void,
            drmm_crtc_init_with_planes_cleanup,
            crtc as *mut c_void,
        );
        if ret != 0 {
            return ret;
        }
        0
    }
}

/// Managed variant of [`drm_crtc_init_with_planes`]: cleanup runs with the device.
// upstream: drm_crtc.c drmm_crtc_init_with_planes()
#[unsafe(no_mangle)]
/// # Safety
/// Preconditions match `__drmm_crtc_init_with_planes`.
pub unsafe extern "C" fn drmm_crtc_init_with_planes(
    dev: *mut DrmDevice,
    crtc: *mut DrmCrtc,
    primary: *mut DrmPlane,
    cursor: *mut DrmPlane,
    funcs: *const DrmCrtcFuncs,
    name: *const c_char,
) -> i32 {
    // SAFETY: Forwarding to `__drmm_crtc_init_with_planes` under matching preconditions.
    unsafe {
        let ret = __drmm_crtc_init_with_planes(dev, crtc, primary, cursor, funcs, name);
        if ret != 0 {
            return ret;
        }
        0
    }
}

/// Allocates a managed container of `size` bytes with the CRTC at `offset`.
/// Returns an error pointer on failure.
// upstream: drm_crtc.c __drmm_crtc_alloc_with_planes()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
/// `size` must be at least `offset + size_of::<DrmCrtc>()`.
/// `funcs` must point to a valid `DrmCrtcFuncs` table with `destroy` set to None.
pub unsafe extern "C" fn __drmm_crtc_alloc_with_planes(
    dev: *mut DrmDevice,
    size: usize,
    offset: usize,
    primary: *mut DrmPlane,
    cursor: *mut DrmPlane,
    funcs: *const DrmCrtcFuncs,
    name: *const c_char,
) -> *mut c_void {
    // SAFETY: Allocating `size` bytes managed by `dev`. Offsetting by `offset` yields a valid, properly
    // aligned `DrmCrtc` pointer within the allocated block. Initializing via `__drmm_crtc_init_with_planes`.
    unsafe {
        if warn_on(funcs.is_null() || (*funcs).destroy.is_some()) {
            return err_ptr::<c_void>(-EINVAL);
        }

        let container = drm_kapi::memory::drmm_kzalloc_c(dev as *mut c_void, size, GFP_KERNEL);
        if container.is_null() {
            return err_ptr::<c_void>(-ENOMEM);
        }

        let crtc = (container as *mut u8).add(offset) as *mut DrmCrtc;
        let ret = __drmm_crtc_init_with_planes(dev, crtc, primary, cursor, funcs, name);
        if ret != 0 {
            return err_ptr::<c_void>(ret);
        }
        container
    }
}

/// Tears down a CRTC initialized by one of the init functions above.
// upstream: drm_crtc.c drm_crtc_cleanup()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc` currently registered with its `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_cleanup(crtc: *mut DrmCrtc) {
    // SAFETY: `crtc` is valid and live. Freeing `gamma_store` via `kfree`, unregistering mode object,
    // removing `(*crtc).head` from device list, invoking `atomic_destroy_state` on `(*crtc).state` if present,
    // freeing `name` via `kfree`, and zeroing `crtc` with `write_bytes` cleanly resets the object.
    unsafe {
        let dev = (*crtc).dev;

        // Note that the crtc_list is considered to be static; should we remove
        // the drm_crtc at runtime we would have to decrement all the indices on
        // the drm_crtc after us in the crtc_list.
        drm_crtc_crc_fini(crtc);

        drm_kapi::memory::kfree((*crtc).gamma_store);
        (*crtc).gamma_store = core::ptr::null_mut();

        // drm_modeset_lock_fini(&crtc->mutex): WARN_ON(!list_empty(&lock->head))
        warn_on(!drm_kapi::list::list_empty(&(*crtc).mutex.head));

        drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*crtc).base));
        list_del(core::ptr::addr_of_mut!((*crtc).head));
        (*dev).mode_config.num_crtc -= 1;

        warn_on(!(*crtc).state.is_null() && (*(*crtc).funcs).atomic_destroy_state.is_none());
        if !(*crtc).state.is_null()
            && let Some(atomic_destroy_state) = (*(*crtc).funcs).atomic_destroy_state
        {
            atomic_destroy_state(crtc, (*crtc).state);
        }

        drm_kapi::memory::kfree((*crtc).name);

        // memset(crtc, 0, sizeof(*crtc))
        core::ptr::write_bytes(crtc, 0, 1);
    }
}

/// Legacy GETCRTC ioctl: reports the CRTC's framebuffer, origin, gamma size
/// and mode.
// upstream: drm_crtc.c drm_mode_getcrtc()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
/// `data` must point to a readable and writable `DrmModeCrtc` ioctl buffer.
/// `file_priv` must be a valid open `DrmFile` or null.
pub unsafe extern "C" fn drm_mode_getcrtc(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is cast to `DrmModeCrtc`. Lookups and reads are protected under `plane.mutex`
    // and `crtc.mutex`. Converting internal display mode to `DrmModeModeinfo` via `drm_mode_convert_to_umode`
    // writes valid timing information into `crtc_resp.mode`.
    unsafe {
        let crtc_resp = data as *mut DrmModeCrtc;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let crtc = drm_crtc_find(dev, file_priv, (*crtc_resp).crtc_id);
        if crtc.is_null() {
            return -ENOENT;
        }

        let plane = (*crtc).primary;
        (*crtc_resp).gamma_size = (*crtc).gamma_size;

        drm_modeset_lock(
            core::ptr::addr_of_mut!((*plane).mutex),
            core::ptr::null_mut(),
        );
        if !(*plane).state.is_null() && !(*(*plane).state).fb.is_null() {
            (*crtc_resp).fb_id = (*(*(*plane).state).fb).base.id;
        } else if (*plane).state.is_null() && !(*plane).fb.is_null() {
            (*crtc_resp).fb_id = (*(*plane).fb).base.id;
        } else {
            (*crtc_resp).fb_id = 0;
        }

        if !(*plane).state.is_null() {
            (*crtc_resp).x = (*(*plane).state).src_x >> 16;
            (*crtc_resp).y = (*(*plane).state).src_y >> 16;
        }
        drm_modeset_unlock(core::ptr::addr_of_mut!((*plane).mutex));

        drm_modeset_lock(
            core::ptr::addr_of_mut!((*crtc).mutex),
            core::ptr::null_mut(),
        );
        if !(*crtc).state.is_null() {
            if (*(*crtc).state).enable {
                drm_mode_convert_to_umode(
                    core::ptr::addr_of_mut!((*crtc_resp).mode),
                    core::ptr::addr_of!((*(*crtc).state).mode),
                );
                (*crtc_resp).mode_valid = 1;
            } else {
                (*crtc_resp).mode_valid = 0;
            }
        } else {
            (*crtc_resp).x = (*crtc).x as u32;
            (*crtc_resp).y = (*crtc).y as u32;
            if (*crtc).enabled {
                drm_mode_convert_to_umode(
                    core::ptr::addr_of_mut!((*crtc_resp).mode),
                    core::ptr::addr_of!((*crtc).mode),
                );
                (*crtc_resp).mode_valid = 1;
            } else {
                (*crtc_resp).mode_valid = 0;
            }
        }

        if !(*file_priv)
            .aspect_ratio_allowed
            .load(core::sync::atomic::Ordering::Relaxed)
        {
            (*crtc_resp).mode.flags &= !DRM_MODE_FLAG_PIC_AR_MASK;
        }

        drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));
        0
    }
}

// upstream: drm_crtc.c __drm_mode_set_config_internal()
/// # Safety
/// `set` must point to a valid `DrmModeSet` with an initialized `crtc`.
/// Appropriate modeset locks must be held across the device.
unsafe fn __drm_mode_set_config_internal(
    set: *mut DrmModeSet,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: Stashing `old_fb` across all primary planes in `dev.mode_config.crtc_list`, invoking
    // driver's legacy `set_config` hook, updating primary plane binding, and rebalancing framebuffer
    // refcounts with `drm_framebuffer_get` and `drm_framebuffer_put`.
    unsafe {
        let crtc = (*set).crtc;
        let dev = (*crtc).dev;

        warn_on(drm_drv_uses_atomic_modeset((*crtc).dev));

        // NOTE: ->set_config can also disable other crtcs (if we steal all
        // connectors from it), hence we need to refcount the fbs across all
        // crtcs. Atomic modeset will have saner semantics ...
        for_each_crtc(dev, |tmp| {
            let plane = (*tmp).primary;
            (*plane).old_fb = (*plane).fb;
            true
        });

        let fb = (*set).fb;
        let set_config = (*(*crtc).funcs).set_config.expect("set_config");
        let ret = set_config(set, ctx);

        if ret == 0 {
            let plane = (*crtc).primary;
            (*plane).crtc = if !fb.is_null() {
                crtc
            } else {
                core::ptr::null_mut()
            };
            (*plane).fb = fb;
        }

        for_each_crtc(dev, |tmp| {
            let plane = (*tmp).primary;
            if !(*plane).fb.is_null() {
                drm_framebuffer_get((*plane).fb);
            }
            if !(*plane).old_fb.is_null() {
                drm_framebuffer_put((*plane).old_fb);
            }
            (*plane).old_fb = core::ptr::null_mut();
            true
        });

        ret
    }
}

/// Sets a CRTC configuration on a non-atomic driver through the legacy path.
// upstream: drm_crtc.c drm_mode_set_config_internal()
#[unsafe(no_mangle)]
/// # Safety
/// `set` must point to an initialized `DrmModeSet` on a non-atomic driver with modeset locks held.
pub unsafe extern "C" fn drm_mode_set_config_internal(set: *mut DrmModeSet) -> i32 {
    // SAFETY: Forwarding to `__drm_mode_set_config_internal` with null acquire context.
    unsafe {
        warn_on(drm_drv_uses_atomic_modeset((*(*set).crtc).dev));
        __drm_mode_set_config_internal(set, core::ptr::null_mut())
    }
}

/// Checks that the viewport origin fits the framebuffer for the given mode.
// upstream: drm_crtc.c drm_crtc_check_viewport()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc`, `mode`, and `fb` must point to valid, initialized DRM structures.
pub unsafe extern "C" fn drm_crtc_check_viewport(
    crtc: *const DrmCrtc,
    x: i32,
    y: i32,
    mode: *const DrmDisplayMode,
    fb: *const DrmFramebuffer,
) -> i32 {
    // SAFETY: Reading timings from `mode`, inspecting rotation on `crtc.primary.state`, and
    // passing source coordinates to `drm_framebuffer_check_src_coords` is safe for valid input pointers.
    unsafe {
        let mut hdisplay: i32 = 0;
        let mut vdisplay: i32 = 0;
        drm_mode_get_hv_timing(mode, &mut hdisplay, &mut vdisplay);

        if !(*crtc).state.is_null()
            && ((*(*(*crtc).primary).state).rotation & (DRM_MODE_ROTATE_90 | DRM_MODE_ROTATE_270))
                != 0
        {
            core::mem::swap(&mut hdisplay, &mut vdisplay);
        }

        drm_framebuffer_check_src_coords(
            (x << 16) as u32,
            (y << 16) as u32,
            (hdisplay << 16) as u32,
            (vdisplay << 16) as u32,
            fb,
        )
    }
}

// upstream: drm_crtc.c get_user() of one u32 connector id from user memory
/// # Safety
/// `ptr` must point to user memory representing a `u32` value.
unsafe fn get_user_u32(ptr: *const u32) -> Result<u32, i32> {
    // SAFETY: `copy_from_user` safely handles page faulting and memory access validation
    // when reading `size_of::<u32>()` bytes into local stack variable `value`.
    unsafe {
        let mut value: u32 = 0;
        if drm_kapi::mm_native::copy_from_user(
            core::ptr::addr_of_mut!(value) as *mut c_void,
            ptr as *const c_void,
            size_of::<u32>(),
        ) != 0
        {
            return Err(-EFAULT);
        }
        Ok(value)
    }
}

/// Legacy SETCRTC ioctl: binds a framebuffer, mode and connector set to the
/// CRTC, or disables it when `mode_valid` is zero.
// upstream: drm_crtc.c drm_mode_setcrtc()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
/// `data` must point to a readable `DrmModeCrtc` ioctl buffer.
/// `file_priv` must be a valid open `DrmFile` or null.
pub unsafe extern "C" fn drm_mode_setcrtc(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Inside `with_modeset_lock_all`, CRTC and connector lookups are performed,
    // memory for `connector_set` is allocated with `kzalloc` and cleaned up with `kfree`,
    // and framebuffer and connector references are managed symmetrically on exit.
    unsafe {
        let config = core::ptr::addr_of_mut!((*dev).mode_config);
        let crtc_req = data as *mut DrmModeCrtc;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        // Universal plane src offsets are only 16.16, prevent havoc for drivers
        // using universal plane code internally.
        if ((*crtc_req).x & 0xffff0000) != 0 || ((*crtc_req).y & 0xffff0000) != 0 {
            return -ERANGE;
        }

        let crtc = drm_crtc_find(dev, file_priv, (*crtc_req).crtc_id);
        if crtc.is_null() {
            return -ENOENT;
        }

        let plane = (*crtc).primary;

        // allow disabling with the primary plane leased
        if (*crtc_req).mode_valid != 0 && !drm_lease_held(file_priv, (*plane).base.id as i32) {
            return -EACCES;
        }

        with_modeset_lock_all(dev, 1 /* DRM_MODESET_ACQUIRE_INTERRUPTIBLE */, |ctx| {
            let mut connector_set: *mut *mut DrmConnector = core::ptr::null_mut();
            let mut fb: *mut DrmFramebuffer = core::ptr::null_mut();
            let mut mode: *mut DrmDisplayMode = core::ptr::null_mut();
            let mut num_connectors: u32 = 0;
            let mut ret: i32;

            'out: {
                if (*crtc_req).mode_valid != 0 {
                    // If we have a mode we need a framebuffer. If we pass -1,
                    // set the mode with the currently bound fb.
                    if (*crtc_req).fb_id == u32::MAX {
                        let old_fb = if !(*plane).state.is_null() {
                            (*(*plane).state).fb
                        } else {
                            (*plane).fb
                        };
                        if old_fb.is_null() {
                            // DRM_DEBUG_KMS("CRTC doesn't have current FB\n")
                            ret = -EINVAL;
                            break 'out;
                        }
                        fb = old_fb;
                        // Make refcounting symmetric with the lookup path.
                        drm_framebuffer_get(fb);
                    } else {
                        fb = drm_framebuffer_lookup(dev, file_priv, (*crtc_req).fb_id);
                        if fb.is_null() {
                            ret = -ENOENT;
                            break 'out;
                        }
                    }

                    mode = drm_mode_create(dev);
                    if mode.is_null() {
                        ret = -ENOMEM;
                        break 'out;
                    }

                    if !(*file_priv)
                        .aspect_ratio_allowed
                        .load(core::sync::atomic::Ordering::Relaxed)
                        && ((*crtc_req).mode.flags & DRM_MODE_FLAG_PIC_AR_MASK)
                            != DRM_MODE_FLAG_PIC_AR_NONE
                    {
                        ret = -EINVAL;
                        break 'out;
                    }

                    ret = drm_mode_convert_umode(dev, mode, core::ptr::addr_of!((*crtc_req).mode));
                    if ret != 0 {
                        break 'out;
                    }

                    // Check whether the primary plane supports the fb pixel format.
                    // Drivers not implementing the universal planes API use a default
                    // formats list provided by the DRM core which doesn't match real
                    // hardware capabilities. Skip the check in that case.
                    if !(*plane).format_default
                        && !crate::drm_plane_upstream::drm_plane_has_format(
                            plane,
                            (*(*fb).format).format,
                            (*fb).modifier,
                        )
                    {
                        ret = -EINVAL;
                        break 'out;
                    }

                    ret = drm_crtc_check_viewport(
                        crtc,
                        (*crtc_req).x as i32,
                        (*crtc_req).y as i32,
                        mode,
                        fb,
                    );
                    if ret != 0 {
                        break 'out;
                    }
                }

                if (*crtc_req).count_connectors == 0 && !mode.is_null() {
                    ret = -EINVAL;
                    break 'out;
                }

                if (*crtc_req).count_connectors > 0 && (mode.is_null() || fb.is_null()) {
                    ret = -EINVAL;
                    break 'out;
                }

                if (*crtc_req).count_connectors > 0 {
                    // Avoid unbounded kernel memory allocation.
                    if (*crtc_req).count_connectors > (*config).num_connector as u32 {
                        ret = -EINVAL;
                        break 'out;
                    }

                    // kmalloc_objs(struct drm_connector *, count_connectors)
                    connector_set = drm_kapi::memory::kzalloc(
                        (*crtc_req).count_connectors as usize * size_of::<*mut DrmConnector>(),
                        GFP_KERNEL,
                    ) as *mut *mut DrmConnector;
                    if connector_set.is_null() {
                        ret = -ENOMEM;
                        break 'out;
                    }

                    for i in 0..(*crtc_req).count_connectors as usize {
                        *connector_set.add(i) = core::ptr::null_mut();
                        let set_connectors_ptr =
                            (*crtc_req).set_connectors_ptr as usize as *const u32;
                        let out_id = match get_user_u32(set_connectors_ptr.add(i)) {
                            Ok(v) => v,
                            Err(e) => {
                                ret = e;
                                break 'out;
                            }
                        };

                        let connector = drm_connector_lookup(dev, file_priv, out_id);
                        if connector.is_null() {
                            // DRM_DEBUG_KMS("Connector id %d unknown\n", out_id)
                            ret = -ENOENT;
                            break 'out;
                        }

                        *connector_set.add(i) = connector;
                        num_connectors += 1;
                    }
                }

                let mut set: DrmModeSet = zeroed();
                set.crtc = crtc;
                set.x = (*crtc_req).x;
                set.y = (*crtc_req).y;
                set.mode = mode;
                set.connectors = connector_set;
                set.num_connectors = num_connectors as usize;
                set.fb = fb;

                ret = if drm_drv_uses_atomic_modeset(dev) {
                    ((*(*crtc).funcs).set_config.expect("set_config"))(&mut set, ctx)
                } else {
                    __drm_mode_set_config_internal(&mut set, ctx)
                };
            }

            // out:
            if !fb.is_null() {
                drm_framebuffer_put(fb);
            }
            if !connector_set.is_null() {
                for i in 0..num_connectors as usize {
                    if !(*connector_set.add(i)).is_null() {
                        drm_connector_put(*connector_set.add(i));
                    }
                }
            }
            drm_kapi::memory::kfree(connector_set);
            drm_mode_destroy(dev, mode);

            ret
        })
    }
}

/// Sets a CRTC object property: the CRTC's `set_property` hook first, then
/// the stored value.
// upstream: drm_crtc.c drm_mode_crtc_set_obj_prop()
#[unsafe(no_mangle)]
/// # Safety
/// `obj` must point to a `DrmModeObject` embedded in an initialized `DrmCrtc`.
/// `property` must point to a valid `DrmProperty`.
pub unsafe extern "C" fn drm_mode_crtc_set_obj_prop(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    value: u64,
) -> i32 {
    // SAFETY: `obj_to_crtc` retrieves the outer CRTC. Calling `set_property` and updating property
    // value on `obj` is safe while modeset locks are held.
    unsafe {
        let mut ret = -EINVAL;
        let crtc = obj_to_crtc(obj);

        if let Some(set_property) = (*(*crtc).funcs).set_property {
            ret = set_property(crtc, property, value);
        }
        if ret == 0 {
            drm_object_property_set_value(obj, property, value);
        }
        ret
    }
}

/// Creates and attaches the CRTC scaling-filter enum property.
// upstream: drm_crtc.c drm_crtc_create_scaling_filter_property()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc` associated with an initialized `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_create_scaling_filter_property(
    crtc: *mut DrmCrtc,
    supported_filters: u32,
) -> i32 {
    // SAFETY: `(*crtc).dev` is valid. Creating scaling filter prop and attaching to `(*crtc).base`
    // attaches property ownership to the CRTC base object during initialization.
    unsafe {
        let prop = crate::drm_plane_upstream::drm_create_scaling_filter_prop(
            (*crtc).dev,
            supported_filters,
        );
        if is_err(prop) {
            return ptr_err(prop);
        }

        drm_object_attach_property(
            core::ptr::addr_of_mut!((*crtc).base),
            prop,
            DRM_SCALING_FILTER_DEFAULT as u64,
        );
        (*crtc).scaling_filter_property = prop;
        0
    }
}

/// Creates and attaches the SHARPNESS_STRENGTH range property (0..=255).
// upstream: drm_crtc.c drm_crtc_create_sharpness_strength_property()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc` associated with an initialized `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_create_sharpness_strength_property(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: Creating range property on `(*crtc).dev` and attaching to `(*crtc).base` attaches
    // property ownership during CRTC initialization.
    unsafe {
        let dev = (*crtc).dev;
        let prop = drm_property_create_range(dev, 0, c"SHARPNESS_STRENGTH".as_ptr(), 0, 255);
        if prop.is_null() {
            return -ENOMEM;
        }

        (*crtc).sharpness_strength_property = prop;
        drm_object_attach_property(core::ptr::addr_of_mut!((*crtc).base), prop, 0);
        0
    }
}

/// True when more than one encoder feeds the CRTC (clone mode).
// upstream: drm_crtc.c drm_crtc_in_clone_mode()
#[unsafe(no_mangle)]
/// # Safety
/// If non-null, `crtc_state` must point to a valid, readable `DrmCrtcState`.
pub unsafe extern "C" fn drm_crtc_in_clone_mode(crtc_state: *const DrmCrtcState) -> bool {
    // SAFETY: Checked for null. Reading `(*crtc_state).encoder_mask` is safe per caller precondition.
    unsafe {
        if crtc_state.is_null() {
            return false;
        }
        drm_kapi::primitives::hweight32((*crtc_state).encoder_mask) > 1
    }
}

#[cfg(test)]
mod tests {
    use core::mem::zeroed;
    use std::{boxed::Box, ffi::CStr, string::String};

    use super::*;
    use crate::test_support::TestDrm;

    /// # Safety
    /// Unused dummy destroy hook for test mocks.
    unsafe extern "C" fn noop_destroy(_crtc: *mut DrmCrtc) {}

    /// CRTC funcs with only the mandatory `destroy` hook set.
    fn funcs() -> *mut DrmCrtcFuncs {
        // SAFETY: Zeroed memory is valid for `DrmCrtcFuncs` function pointers.
        let f: &'static mut DrmCrtcFuncs = Box::leak(Box::new(unsafe { zeroed() }));
        f.destroy = Some(noop_destroy);
        f
    }

    fn name_of(p: *const c_char) -> String {
        // SAFETY: `p` is a valid null-terminated UTF-8/ASCII C string.
        unsafe { CStr::from_ptr(p).to_str().unwrap().into() }
    }

    #[test]
    fn crtc_mask_has_one_bit_per_index() {
        // SAFETY: Zeroed memory is a valid initial bit pattern for test dummy `DrmCrtc`.
        let mut c: DrmCrtc = unsafe { zeroed() };
        for i in 0..8u32 {
            c.index = i;
            // SAFETY: `c` has valid index.
            assert_eq!(unsafe { drm_crtc_mask(&c) }, 1 << i);
        }
    }

    #[test]
    fn clone_mode_needs_more_than_one_encoder() {
        // SAFETY: Zeroed memory is a valid initial bit pattern for test dummy `DrmCrtcState`.
        let mut st: DrmCrtcState = unsafe { zeroed() };
        // SAFETY: Testing null pointer handling.
        assert!(!unsafe { drm_crtc_in_clone_mode(core::ptr::null()) });
        st.encoder_mask = 0b01;
        // SAFETY: `st` is valid test state.
        assert!(!unsafe { drm_crtc_in_clone_mode(&st) });
        st.encoder_mask = 0b11;
        // SAFETY: `st` is valid test state.
        assert!(unsafe { drm_crtc_in_clone_mode(&st) });
    }

    #[test]
    fn crtc_init_assigns_index_name_and_lookup() {
        let mut drm = TestDrm::new(DRIVER_MODESET);
        drm.init_registry();
        let dev = drm.ptr();
        // SAFETY: Test harness exercises CRTC initialization and teardown sequence.
        unsafe {
            init_list_head(core::ptr::addr_of_mut!((*dev).mode_config.crtc_list));
            init_list_head(core::ptr::addr_of_mut!((*dev).mode_config.plane_list));
            let f = funcs();

            let c1: *mut DrmCrtc = Box::into_raw(Box::new(zeroed()));
            assert_eq!(
                drm_crtc_init_with_planes(
                    dev,
                    c1,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    f,
                    c"crtc-a".as_ptr().cast()
                ),
                0
            );
            assert_eq!((*c1).index, 0);
            assert_eq!(name_of((*c1).name), "crtc-a");
            assert_ne!((*c1).base.id, 0);
            assert_eq!(drm_crtc_find(dev, core::ptr::null_mut(), (*c1).base.id), c1);
            assert_eq!(
                name_of((*c1).timeline_name.as_ptr()),
                std::format!("CRTC:{}-crtc-a", (*c1).base.id)
            );

            // No name given: the default "crtc-<index>" is used.
            let c2: *mut DrmCrtc = Box::into_raw(Box::new(zeroed()));
            assert_eq!(
                drm_crtc_init_with_planes(
                    dev,
                    c2,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    f,
                    core::ptr::null()
                ),
                0
            );
            assert_eq!((*c2).index, 1);
            assert_eq!(name_of((*c2).name), "crtc-1");
            assert_eq!((*dev).mode_config.num_crtc, 2);
            assert_eq!(drm_crtc_find(dev, core::ptr::null_mut(), (*c2).base.id), c2);

            assert_eq!(drm_crtc_from_index(dev, 0), c1);
            assert_eq!(drm_crtc_from_index(dev, 1), c2);
            assert!(drm_crtc_from_index(dev, 2).is_null());
            assert_eq!(drm_crtc_mask(c2), 2);

            drm_crtc_cleanup(c1);
            assert_eq!((*dev).mode_config.num_crtc, 1);
            assert!(drm_crtc_from_index(dev, 0).is_null());
            assert_eq!(drm_crtc_from_index(dev, 1), c2);
        }
    }
}

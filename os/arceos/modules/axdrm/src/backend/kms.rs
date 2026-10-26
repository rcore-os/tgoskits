// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! One-output DRM KMS adapter over the registered RDIF display capability.

use alloc::{vec, vec::Vec};
use core::ptr;

use ax_gpu::{
    self,
    rdif_display::{DisplayError, DisplayState, Framebuffer, Mode, OutputId, Rect, ScanoutBuffer},
    rdif_gpu::{Completion, PixelFormat},
};
use drm_kapi::config::err_ptr;
use kdrm_core::{
    drm_atomic_helper_upstream::{
        drm_atomic_helper_check, drm_atomic_helper_commit_error, drm_atomic_helper_commit_tail,
    },
    drm_atomic_types::DrmAtomicCommit,
    drm_atomic_upstream::{
        drm_atomic_add_affected_planes, drm_atomic_get_new_crtc_state,
        drm_atomic_get_new_plane_state, drm_atomic_get_old_plane_state,
    },
    drm_connector_types::{
        CONNECTOR_STATUS_CONNECTED, CONNECTOR_STATUS_DISCONNECTED, DrmConnector,
    },
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_device_types::DrmDevice,
    drm_framebuffer_types::DrmFramebuffer,
    drm_gem_types::DrmGemObject,
    drm_gem_upstream::{drm_gem_object_get, drm_gem_object_put},
    drm_modes_types::{DrmDisplayMode, DrmModeStatus, MODE_OK, MODE_VIRTUAL_X, MODE_VIRTUAL_Y},
    drm_modes_upstream::{drm_cvt_mode, drm_mode_probed_add},
    drm_plane_types::DrmPlaneState,
    drm_simple_kms_helper_types_upstream::DrmSimpleDisplayPipe,
    drm_vblank_helper_upstream::{
        drm_crtc_vblank_helper_disable_vblank_timer, drm_crtc_vblank_helper_enable_vblank_timer,
    },
    drm_vblank_types::DrmPendingVblankEvent,
    uapi_drm_consts::{
        DRM_FORMAT_ARGB8888, DRM_FORMAT_BGR888, DRM_FORMAT_RGB565, DRM_FORMAT_RGB888,
        DRM_FORMAT_XBGR8888, DRM_FORMAT_XRGB8888,
    },
};

use crate::{
    backend::{fence, gem},
    device::private_from_device,
};

const NEG_EINVAL: i32 = -drm_kapi::config::EINVAL;
const NEG_EBUSY: i32 = -drm_kapi::config::EBUSY;
const NEG_ENODEV: i32 = -drm_kapi::config::ENODEV;
const NEG_EIO: i32 = -drm_kapi::config::EIO;
const NEG_ETIMEDOUT: i32 = -drm_kapi::config::ETIMEDOUT;
const NEG_EOPNOTSUPP: i32 = -drm_kapi::config::EOPNOTSUPP;
const MIN_MODE: u32 = 32;
const MAX_MODE: u32 = 8192;
const ASYNC_BUSY_RETRIES: usize = 100;

pub(crate) struct PrecommittedDisplay {
    pub(crate) atomic: *mut DrmAtomicCommit,
    pub(crate) ready: bool,
    pub(crate) completion: Completion,
    pub(crate) object: *mut DrmGemObject,
    pub(crate) framebuffer: *mut DrmFramebuffer,
    pub(crate) crtc: *mut DrmCrtc,
    pub(crate) event: *mut DrmPendingVblankEvent,
}

/// # Safety
///
/// `connector` may be null (in which case this function does nothing). If non-null,
/// it must point to an initialized `DrmConnector` belonging to this driver entering cleanup.
pub(crate) unsafe extern "C" fn connector_destroy(connector: *mut DrmConnector) {
    if connector.is_null() {
        return;
    }
    // SAFETY: connector was checked non-null above; cleaning up connector resources via drm_connector_cleanup.
    unsafe { kdrm_core::drm_connector_upstream::drm_connector_cleanup(connector) };
}

/// Adapt kdrm-core's const framebuffer-create helper to the mutable callback
/// signature used by this revision of `DrmModeConfigFuncs`.
///
/// # Safety
///
/// `dev`, `file`, and `cmd` must point to live, allocated DRM objects for the duration of the call.
/// `format` may be null (returns `-EINVAL`). If non-null, it must point to an initialized `DrmFormatInfo`.
pub(crate) unsafe extern "C" fn fb_create(
    dev: *mut DrmDevice,
    file: *mut kdrm_core::drm_file_types::DrmFile,
    format: *mut kdrm_core::drm_fourcc_types::DrmFormatInfo,
    cmd: *mut kdrm_core::uapi_drm_mode_types::DrmModeFbCmd2,
) -> *mut DrmFramebuffer {
    // SAFETY: The DRM core passes live pointers for the duration of the
    // callback; drm_gem_fb_create only reads both argument structures.
    let Some(format_info) = (unsafe { format.as_ref() }) else {
        return err_ptr::<DrmFramebuffer>(NEG_EINVAL);
    };
    let pixel_format = match pixel_format_from_fourcc(format_info.format) {
        Some(format) => format,
        None => {
            // SAFETY: unsupported pixel format fallback; delegating to drm_gem_fb_create with caller-supplied dev, file, format info, and mode command.
            return unsafe {
                kdrm_core::drm_gem_framebuffer_helper_upstream::drm_gem_fb_create(
                    dev,
                    file,
                    format.cast_const(),
                    cmd.cast_const(),
                )
            };
        }
    };
    // SAFETY: format matched a supported PixelFormat; delegating to gem::create_framebuffer with caller-supplied device and mode command pointers.
    unsafe {
        gem::create_framebuffer(
            dev,
            file,
            format.cast_const(),
            cmd.cast_const(),
            pixel_format,
        )
    }
}

fn pixel_format_from_fourcc(format: u32) -> Option<PixelFormat> {
    match format {
        DRM_FORMAT_XRGB8888 => Some(PixelFormat::Xrgb8888),
        DRM_FORMAT_ARGB8888 => Some(PixelFormat::Argb8888),
        DRM_FORMAT_XBGR8888 => Some(PixelFormat::Xbgr8888),
        DRM_FORMAT_RGB565 => Some(PixelFormat::Rgb565),
        DRM_FORMAT_RGB888 => Some(PixelFormat::Rgb888),
        DRM_FORMAT_BGR888 => Some(PixelFormat::Bgr888),
        _ => None,
    }
}

/// # Safety
///
/// `connector` may be null (returns `CONNECTOR_STATUS_DISCONNECTED`). If non-null,
/// it must point to an initialized `DrmConnector` whose `dev` field points to a live `DrmDevice`.
pub(crate) unsafe extern "C" fn connector_detect(
    connector: *mut DrmConnector,
    _force: bool,
) -> u32 {
    if connector.is_null() {
        return CONNECTOR_STATUS_DISCONNECTED;
    }
    // SAFETY: connector was checked non-null above; reading its owning dev field.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `dev_private` remains retained by the DRM core release callback.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return CONNECTOR_STATUS_DISCONNECTED;
    };
    let connected = output_info(private.output).is_some_and(|info| info.connected);
    private
        .connected
        .store(connected, core::sync::atomic::Ordering::Release);
    if connected {
        CONNECTOR_STATUS_CONNECTED
    } else {
        CONNECTOR_STATUS_DISCONNECTED
    }
}

/// # Safety
///
/// `connector` may be null (returns 0). If non-null, it must point to an initialized
/// `DrmConnector` whose `dev` field points to this driver's `DrmDevice`. Probed modes
/// are added directly to `connector`.
pub(crate) unsafe extern "C" fn connector_get_modes(connector: *mut DrmConnector) -> i32 {
    if connector.is_null() {
        return 0;
    }
    // SAFETY: connector was checked non-null above; reading (*connector).dev to look up device private state.
    let dev = unsafe { (*connector).dev };
    // SAFETY: the core-owned device pointer remains valid during callbacks.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return 0;
    };
    let Some(info) = output_info(private.output).filter(|info| info.connected) else {
        private
            .connected
            .store(false, core::sync::atomic::Ordering::Release);
        return 0;
    };
    private
        .connected
        .store(true, core::sync::atomic::Ordering::Release);
    let mut count = 0;
    for mode in info.modes.iter().copied() {
        if !valid_dimensions(mode.width, mode.height) {
            continue;
        }
        let refresh_hz = if mode.refresh_millihz == 0 {
            60
        } else {
            mode.refresh_millihz.saturating_add(500) / 1000
        };
        // SAFETY: dev is the live core device and dimensions are bounded above.
        let display_mode = unsafe {
            drm_cvt_mode(
                dev,
                mode.width as i32,
                mode.height as i32,
                refresh_hz.clamp(1, 240) as i32,
                false,
                false,
                false,
            )
        };
        if display_mode.is_null() {
            continue;
        }
        if info.preferred_mode == Some(mode) {
            // SAFETY: `display_mode` is newly allocated and owned by DRM.
            unsafe {
                (*display_mode).r#type |= kdrm_core::uapi_drm_consts::DRM_MODE_TYPE_PREFERRED as u8;
            }
        }
        // SAFETY: the probe helper takes ownership and links the fresh mode.
        unsafe { drm_mode_probed_add(connector, display_mode) };
        count += 1;
    }
    count
}

/// # Safety
///
/// `mode` may be null (returns `MODE_VIRTUAL_X`). If non-null, it must point to
/// a readable `DrmDisplayMode`. `_connector` is unused and may be null.
pub(crate) unsafe extern "C" fn connector_mode_valid(
    _connector: *mut DrmConnector,
    mode: *mut DrmDisplayMode,
) -> DrmModeStatus {
    // SAFETY: the same validation handles null and uses only bounded mode fields.
    unsafe { pipe_mode_valid(ptr::null_mut(), mode) }
}

/// # Safety
///
/// `connector` may be null (returns null). If non-null, it must point to an initialized
/// `DrmConnector` whose `dev` points to this driver's `DrmDevice`. Returns a pointer
/// to the encoder embedded in the device's simple display pipe.
pub(crate) unsafe extern "C" fn best_encoder(
    connector: *mut DrmConnector,
) -> *mut kdrm_core::drm_encoder_types::DrmEncoder {
    if connector.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: connector is live for the callback duration.
    let dev = unsafe { (*connector).dev };
    // SAFETY: the device-private owner keeps the embedded pipe stable.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return ptr::null_mut();
    };
    // SAFETY: pipe is initialized before registration and remains pinned in Arc.
    unsafe { ptr::addr_of_mut!((*private.pipe.get()).encoder) }
}

/// # Safety
///
/// `mode` may be null (returns `MODE_VIRTUAL_X`). If non-null, it must point to
/// a readable `DrmDisplayMode`. `_dev` is unused and may be null.
pub(crate) unsafe extern "C" fn mode_valid(
    _dev: *mut DrmDevice,
    mode: *mut DrmDisplayMode,
) -> DrmModeStatus {
    // SAFETY: mode validation is shared with the pipe callback.
    unsafe { pipe_mode_valid(ptr::null_mut(), mode) }
}

/// # Safety
///
/// `mode` may be null (returns `MODE_VIRTUAL_X`). If non-null, it must point to
/// an aligned, readable `DrmDisplayMode`. `_pipe` is unused and may be null.
pub(crate) unsafe extern "C" fn pipe_mode_valid(
    _pipe: *mut DrmSimpleDisplayPipe,
    mode: *const DrmDisplayMode,
) -> DrmModeStatus {
    if mode.is_null() {
        return MODE_VIRTUAL_X;
    }
    // SAFETY: mode is a valid core-owned mode record.
    let (width, height) = unsafe { ((*mode).hdisplay as u32, (*mode).vdisplay as u32) };
    if !(MIN_MODE..=MAX_MODE).contains(&width) {
        MODE_VIRTUAL_X
    } else if !(MIN_MODE..=MAX_MODE).contains(&height) {
        MODE_VIRTUAL_Y
    } else {
        MODE_OK
    }
}

/// # Safety
///
/// `pipe` may be null (in which case this function does nothing). If non-null, it must point
/// to an initialized `DrmSimpleDisplayPipe` whose `crtc.dev` points to this driver's `DrmDevice`.
pub(crate) unsafe extern "C" fn pipe_enable(
    pipe: *mut DrmSimpleDisplayPipe,
    _crtc_state: *mut DrmCrtcState,
    _plane_state: *mut DrmPlaneState,
) {
    if pipe.is_null() {
        return;
    }
    // SAFETY: pipe was checked non-null above; reading (*pipe).crtc.dev to resolve device private state.
    let dev = unsafe { (*pipe).crtc.dev };
    // SAFETY: DRM retains the associated device private state.
    if let Some(private) = unsafe { private_from_device(dev) } {
        private
            .connected
            .store(true, core::sync::atomic::Ordering::Release);
        // CRTC-only mode changes may not issue a primary-plane update. In
        // that case simple-KMS reaches enable after the successful pre-swap
        // commit and it is the remaining completion handoff point.
        if let Some(staged) = take_precommitted(private, ptr::null_mut()) {
            apply_precommitted(private, staged);
        }
    }
}

/// # Safety
///
/// `pipe` may be null (in which case this function does nothing). If non-null, it must point
/// to an initialized `DrmSimpleDisplayPipe` whose `crtc.dev` points to this driver's `DrmDevice`.
pub(crate) unsafe extern "C" fn pipe_disable(pipe: *mut DrmSimpleDisplayPipe) {
    if pipe.is_null() {
        return;
    }
    // SAFETY: pipe was checked non-null above; reading (*pipe).crtc.dev to look up device private state.
    let dev = unsafe { (*pipe).crtc.dev };
    // SAFETY: the device-private owner remains live during core callbacks.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return;
    };
    if let Some(staged) = take_precommitted(private, ptr::null_mut()) {
        apply_precommitted(private, staged);
    } else {
        log::error!("simple-KMS disable ran without a successful pre-swap RDIF commit");
    }
}

/// # Safety
///
/// `pipe` may be null (in which case this function does nothing). If non-null, it must point
/// to an initialized `DrmSimpleDisplayPipe` whose `crtc.dev` points to this driver's `DrmDevice`.
/// `old_plane_state` may be null or point to the previous `DrmPlaneState` passed by simple-KMS helpers.
pub(crate) unsafe extern "C" fn pipe_update(
    pipe: *mut DrmSimpleDisplayPipe,
    old_plane_state: *mut DrmPlaneState,
) {
    if pipe.is_null() {
        return;
    }
    // SAFETY: pipe and state are live records provided by the core callback.
    let dev = unsafe { (*pipe).crtc.dev };
    // SAFETY: the DRM core keeps the device private Arc alive for callbacks.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return;
    };
    // The simple-KMS helper passes the OLD plane state to `update`. Its state
    // pointer identifies the transaction staged by pre-swap; it must not be
    // used to program the new scanout.
    // SAFETY: `old_plane_state` is provided by the core callback; dereferencing its state pointer is valid.
    let atomic = unsafe {
        old_plane_state
            .as_ref()
            .map_or(ptr::null_mut(), |state| state.state)
    };
    if let Some(staged) = take_precommitted(private, atomic) {
        apply_precommitted(private, staged);
        return;
    }

    // A modeset's CRTC-disable callback can consume a precommitted active
    // state before simple-KMS visits the primary plane. Do not replay that
    // already-swapped framebuffer after the completion was attached.
    // SAFETY: `pipe` is a live simple-display-pipe pointer provided by the core callback.
    let (new_object, new_framebuffer) = unsafe {
        let plane_state = (*pipe).plane.state;
        let framebuffer = plane_state
            .as_ref()
            .map_or(ptr::null_mut(), |state| state.fb);
        let object = framebuffer.as_ref().map_or(ptr::null_mut(), |fb| fb.obj[0]);
        (object, framebuffer)
    };
    if *private.last_scanout.lock() == new_object
        && *private.last_scanout_fb.lock() == new_framebuffer
    {
        return;
    }
    log::error!("simple-KMS plane update had no staged pre-swap RDIF completion");
}

fn replace_last_scanout(
    private: &crate::device::DriverPrivate,
    new_object: *mut DrmGemObject,
    new_framebuffer: *mut DrmFramebuffer,
) -> (*mut DrmGemObject, *mut DrmFramebuffer) {
    // The framebuffer's atomic state pins `new_object` while its GEM reference
    // is acquired; the private mutex serializes replacement with disable.
    // SAFETY: `new_object` is a live GEM object pointer pinned by the atomic transaction.
    unsafe { drm_gem_object_get(new_object) };
    // SAFETY: `new_framebuffer` is a live framebuffer pointer pinned by the atomic transaction.
    unsafe { kdrm_core::drm_framebuffer_upstream::drm_framebuffer_get(new_framebuffer) };
    let old_object = core::mem::replace(&mut *private.last_scanout.lock(), new_object);
    let old_framebuffer = core::mem::replace(&mut *private.last_scanout_fb.lock(), new_framebuffer);
    (old_object, old_framebuffer)
}

/// # Safety
///
/// `pipe` may be null (returns `-EINVAL`). If non-null, it must point to an initialized
/// `DrmSimpleDisplayPipe` containing an initialized `crtc`.
pub(crate) unsafe extern "C" fn pipe_enable_vblank(pipe: *mut DrmSimpleDisplayPipe) -> i32 {
    if pipe.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: pipe was checked non-null above; enabling vblank timer on (*pipe).crtc.
    unsafe { drm_crtc_vblank_helper_enable_vblank_timer(ptr::addr_of_mut!((*pipe).crtc)) }
}

/// # Safety
///
/// `pipe` may be null (in which case this function does nothing). If non-null, it must point
/// to an initialized `DrmSimpleDisplayPipe` containing an initialized `crtc`.
pub(crate) unsafe extern "C" fn pipe_disable_vblank(pipe: *mut DrmSimpleDisplayPipe) {
    if pipe.is_null() {
        return;
    }
    // SAFETY: pipe was checked non-null above; disabling vblank timer on (*pipe).crtc.
    unsafe { drm_crtc_vblank_helper_disable_vblank_timer(ptr::addr_of_mut!((*pipe).crtc)) };
}

/// # Safety
///
/// `dev` must point to an initialized `DrmDevice` associated with this driver.
/// `state` must point to an active `DrmAtomicCommit` undergoing validation before commit.
pub(crate) unsafe extern "C" fn atomic_check(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: This callback follows the drm_mode_config atomic-check contract.
    let ret = unsafe { drm_atomic_helper_check(dev, state) };
    if ret != 0 {
        return ret;
    }
    // SAFETY: dev is live, and mode_config's embedded pipe stays stable.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return NEG_ENODEV;
    };
    let output = output_info(private.output).filter(|info| info.connected);
    let modes = output.as_ref().map(|info| info.modes.as_slice());
    // SAFETY: the core passed a live atomic state whose new CRTC/plane records
    // remain valid until this check returns.
    let display_state = match unsafe {
        display_state_from_atomic(private.output, private.pipe.get(), state, modes)
    } {
        Ok(state) => state,
        Err(error) => return error,
    };
    match ax_gpu::with_display(|display| display.check(&display_state)) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) | Err(error) => display_error_to_errno(error),
    }
}

/// simple-KMS's default CRTC check additionally requires the primary plane to
/// be present whenever the CRTC is enabled. VirtGPU supports an active CRTC
/// with a detached/blank primary plane, so retain the helper's affected-plane
/// expansion without imposing that driver-specific restriction.
///
/// # Safety
///
/// `crtc` and `state` may be null (returns `-EINVAL`). If non-null, `crtc` must point
/// to an initialized `DrmCrtc` and `state` must point to the enclosing `DrmAtomicCommit`.
pub(crate) unsafe extern "C" fn crtc_atomic_check(
    crtc: *mut DrmCrtc,
    state: *mut DrmAtomicCommit,
) -> i32 {
    if crtc.is_null() || state.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: kdrm-core invokes this while checking the live atomic state; the
    // generic plane helper has already validated every updated plane.
    unsafe { drm_atomic_add_affected_planes(state, crtc) }
}

/// Stage the fallible RDIF display operation. Blocking commits call this from
/// the pre-swap hook so failure leaves the old atomic state published; the
/// NONBLOCK worker calls the same operation after publication and reports a
/// failure through kdrm-core's asynchronous commit-error helper. Pipe callbacks
/// only transfer references and completion events after a successful stage.
///
/// # Safety
///
/// `atomic` may be null (returns `-EINVAL`). If non-null, it must point to an initialized
/// `DrmAtomicCommit` prior to state swap.
pub(crate) unsafe extern "C" fn atomic_commit_pre_swap(atomic: *mut DrmAtomicCommit) -> i32 {
    // The synchronous path calls this before publishing state, preserving
    // Linux's failure-before-swap behavior. NONBLOCK calls the same operation
    // from `atomic_commit_tail` after its worker has waited input fences; that
    // path reports errors through the core's asynchronous commit-error helper.
    // SAFETY: `atomic` is a live commit state passed to this pre-swap callback.
    match unsafe { stage_display_commit(atomic) } {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

/// # Safety
///
/// `atomic` may be null (returns `Err(-EINVAL)`). If non-null, it must point to an active
/// `DrmAtomicCommit` whose new CRTC and plane states are staged for display.
unsafe fn stage_display_commit(atomic: *mut DrmAtomicCommit) -> Result<(), i32> {
    if atomic.is_null() {
        return Err(NEG_EINVAL);
    }
    // SAFETY: The atomic helper retains the live device and state throughout
    // its pre-swap callback, under the modeset commit serialization.
    let dev = unsafe { (*atomic).dev };
    // SAFETY: `dev` is retained by the atomic helper; device private data remains pinned.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        return Err(NEG_ENODEV);
    };
    // SAFETY: `dev` is a live DRM device pointer retained for this commit transaction.
    if unsafe { (*dev).unplugged.load(core::sync::atomic::Ordering::Acquire) } {
        return Err(NEG_ENODEV);
    }
    let pipe = private.pipe.get();
    // SAFETY: querying new CRTC state for (*pipe).crtc from active commit `atomic`.
    let crtc_state =
        unsafe { drm_atomic_get_new_crtc_state(atomic, ptr::addr_of_mut!((*pipe).crtc)) };
    // SAFETY: querying new plane state for (*pipe).plane from active commit `atomic`.
    let plane_state =
        unsafe { drm_atomic_get_new_plane_state(atomic, ptr::addr_of_mut!((*pipe).plane)) };
    if crtc_state.is_null() && plane_state.is_null() {
        return Ok(());
    }
    // SAFETY: querying old plane state for (*pipe).plane from active commit `atomic`.
    let old_plane_state =
        unsafe { drm_atomic_get_old_plane_state(atomic, ptr::addr_of_mut!((*pipe).plane)) };
    // SAFETY: borrowing mutable pointer to (*pipe).crtc to test CRTC association.
    let crtc = unsafe { ptr::addr_of_mut!((*pipe).crtc) };
    if !atomic_change_affects_pipe(crtc_state, old_plane_state, plane_state, crtc) {
        return Ok(());
    }
    let output = output_info(private.output).filter(|info| info.connected);
    let modes = output.as_ref().map(|info| info.modes.as_slice());
    // SAFETY: `pipe` and `atomic` are live structures for the duration of this commit.
    let display_state = unsafe { display_state_from_atomic(private.output, pipe, atomic, modes) }?;
    let (object, framebuffer) = if display_state.mode.is_some() {
        // SAFETY: `plane_state` is obtained from the atomic state or the live pipe.
        let plane_state = unsafe {
            plane_state
                .as_ref()
                .or_else(|| (*pipe).plane.state.as_ref())
        };
        let fb = plane_state.map_or(ptr::null_mut(), |plane| plane.fb);
        // SAFETY: `fb` is validated by DRM atomic state or checked for null.
        let object = unsafe { fb.as_ref().map_or(ptr::null_mut(), |fb| fb.obj[0]) };
        (object, fb)
    } else {
        (ptr::null_mut(), ptr::null_mut())
    };

    // Reserve the one staging slot before touching RDIF. Core serializes this
    // hook behind the prior commit's hw_done, so a ready stage is an invariant
    // violation rather than a reason to replace an unconsumed completion.
    {
        let mut staged = private.precommitted.lock();
        if staged.is_some() {
            return Err(NEG_EBUSY);
        }
        *staged = Some(PrecommittedDisplay {
            atomic,
            ready: false,
            completion: Completion::Complete,
            object,
            framebuffer,
            crtc: ptr::null_mut(),
            event: ptr::null_mut(),
        });
    }

    let committed = ax_gpu::with_display(|display| {
        display.check(&display_state)?;
        display.commit(&display_state)
    });
    let completion = match committed {
        Ok(Ok(completion)) => completion,
        Ok(Err(error)) | Err(error) => {
            *private.precommitted.lock() = None;
            return Err(display_error_to_errno(error));
        }
    };

    // SAFETY: borrowing mutable pointer to (*pipe).crtc to assign completion event.
    let crtc = unsafe { ptr::addr_of_mut!((*pipe).crtc) };
    // SAFETY: `crtc_state` if non-null points to the atomic CRTC state owned by `atomic`.
    let event = unsafe {
        if crtc_state.is_null() {
            ptr::null_mut()
        } else {
            let event = (*crtc_state).event;
            (*crtc_state).event = ptr::null_mut();
            event
        }
    };
    let mut staged = private.precommitted.lock();
    // This slot was reserved above, and kdrm-core serializes pre-swap callbacks
    // on this device. Once RDIF commit succeeds there must be no recoverable
    // error path that leaves hardware changed but aborts the software swap.
    let staged = staged
        .as_mut()
        .expect("pre-swap completion slot remains reserved");
    assert_eq!(
        staged.atomic, atomic,
        "pre-swap completion identity changed"
    );
    staged.completion = completion;
    staged.crtc = crtc;
    staged.event = event;
    staged.ready = true;
    Ok(())
}

/// The generic atomic worker waits fences before entering this callback. A
/// blocking commit already reserved and filled its stage before state swap;
/// NONBLOCK reaches here with no stage, so it performs the fallible RDIF
/// operation only after userspace has returned and input fences have signaled.
///
/// # Safety
///
/// `atomic` may be null (in which case this function does nothing). If non-null,
/// it must point to an active `DrmAtomicCommit` undergoing commit tail processing
/// after fences have signaled.
pub(crate) unsafe extern "C" fn atomic_commit_tail(atomic: *mut DrmAtomicCommit) {
    if atomic.is_null() {
        return;
    }
    // SAFETY: The core commit worker retains the live state and DRM device for
    // this callback's entire duration.
    let dev = unsafe { (*atomic).dev };
    // SAFETY: the device private structure remains pinned for the duration of this commit worker.
    let Some(private) = (unsafe { private_from_device(dev) }) else {
        // SAFETY: This is an already-published async commit; the core helper
        // cancels events, reports the error, completes dependencies and
        // unplugs the live device without claiming successful presentation.
        unsafe { drm_atomic_helper_commit_error(atomic, NEG_ENODEV) };
        return;
    };

    let already_committed = private
        .precommitted
        .lock()
        .as_ref()
        .is_some_and(|staged| staged.atomic == atomic && staged.ready);
    if already_committed {
        // Synchronous pre-swap commit: preserve its already successful
        // completion token and let simple-KMS transfer the staged references.
        // SAFETY: The core's standard helper tail completes this successful
        // (already submitted) blocking commit and its normal dependency work.
        unsafe { drm_atomic_helper_commit_tail(atomic) };
        release_unconsumed_stage(private, atomic);
        return;
    }

    // A previous asynchronous commit may have failed and unplugged this DRM
    // device while this commit was already queued. Do not issue any further
    // RDIF command in that state; the core helper completes this commit as an
    // error and the prior committed scanout remains untouched.
    // SAFETY: `dev` is a live DRM device pointer retained for this commit worker.
    if unsafe { (*dev).unplugged.load(core::sync::atomic::Ordering::Acquire) } {
        fail_async_commit(atomic, NEG_ENODEV);
        return;
    }

    if !atomic_affects_pipe(atomic, private) {
        // No scanout operation is needed for this transaction.
        // SAFETY: Input fences were processed by the common commit worker.
        unsafe { drm_atomic_helper_commit_tail(atomic) };
        return;
    }

    for retry in 0..=ASYNC_BUSY_RETRIES {
        // SAFETY: `atomic` is a live commit state passed to this commit worker.
        match unsafe { stage_display_commit(atomic) } {
            Ok(()) => {
                // Hardware submission succeeded; core KMS callbacks consume
                // the stage and its RDIF completion before fake-vblank logic.
                // SAFETY: Standard KMS tail now performs only the successful
                // software-state handoff and cleanup.
                unsafe { drm_atomic_helper_commit_tail(atomic) };
                release_unconsumed_stage(private, atomic);
                return;
            }
            Err(errno) if errno == NEG_EBUSY && retry < ASYNC_BUSY_RETRIES => {
                // Backpressure is transient. Keep the retry bounded so a
                // permanently stalled device produces a visible async error.
                drm_kapi::wait::msleep(1);
            }
            Err(errno) if errno == NEG_EBUSY => {
                fail_async_commit(atomic, NEG_ETIMEDOUT);
                return;
            }
            Err(errno) => {
                fail_async_commit(atomic, errno);
                return;
            }
        }
    }

    // The loop returns from each arm; this is only a defensive unreachable
    // guard in case its retry bound changes.
    fail_async_commit(atomic, NEG_ETIMEDOUT);
}

fn atomic_affects_pipe(
    atomic: *mut DrmAtomicCommit,
    private: &crate::device::DriverPrivate,
) -> bool {
    let pipe = private.pipe.get();
    // SAFETY: querying new CRTC state for (*pipe).crtc from atomic commit to check affected pipe.
    let crtc_state =
        unsafe { drm_atomic_get_new_crtc_state(atomic, ptr::addr_of_mut!((*pipe).crtc)) };
    // SAFETY: querying old plane state for (*pipe).plane from atomic commit to check affected pipe.
    let old_plane_state =
        unsafe { drm_atomic_get_old_plane_state(atomic, ptr::addr_of_mut!((*pipe).plane)) };
    // SAFETY: querying new plane state for (*pipe).plane from atomic commit to check affected pipe.
    let new_plane_state =
        unsafe { drm_atomic_get_new_plane_state(atomic, ptr::addr_of_mut!((*pipe).plane)) };
    // SAFETY: borrowing mutable pointer to (*pipe).crtc for pipe change check.
    atomic_change_affects_pipe(crtc_state, old_plane_state, new_plane_state, unsafe {
        ptr::addr_of_mut!((*pipe).crtc)
    })
}

fn fail_async_commit(atomic: *mut DrmAtomicCommit, errno: i32) {
    // SAFETY: The core worker owns `atomic`; this helper transfers the
    // asynchronous error to events/out-fences, cleans old planes, completes
    // dependencies, and prevents subsequent userspace commits.
    unsafe { drm_atomic_helper_commit_error(atomic, errno) };
}

/// Simple-KMS hands the stage over from its enable, disable or plane-update
/// callback, but a transaction can submit to RDIF without reaching any of them:
/// for example a mode change on a CRTC that stays inactive while its plane is
/// unbound. Hand such a stage over here so its completion and event are
/// delivered and the slot does not block every later commit with `EBUSY`.
fn release_unconsumed_stage(private: &crate::device::DriverPrivate, atomic: *mut DrmAtomicCommit) {
    if let Some(staged) = take_precommitted(private, atomic) {
        apply_precommitted(private, staged);
    }
}

fn take_precommitted(
    private: &crate::device::DriverPrivate,
    atomic: *mut DrmAtomicCommit,
) -> Option<PrecommittedDisplay> {
    take_ready_precommitted(&mut private.precommitted.lock(), atomic)
}

fn take_ready_precommitted(
    staged: &mut Option<PrecommittedDisplay>,
    atomic: *mut DrmAtomicCommit,
) -> Option<PrecommittedDisplay> {
    let ready = staged
        .as_ref()
        .is_some_and(|entry| entry.ready && (atomic.is_null() || entry.atomic == atomic));
    ready.then(|| staged.take()).flatten()
}

fn atomic_change_affects_pipe(
    crtc_state: *mut DrmCrtcState,
    old_plane_state: *mut DrmPlaneState,
    new_plane_state: *mut DrmPlaneState,
    crtc: *mut DrmCrtc,
) -> bool {
    if !crtc_state.is_null() {
        return true;
    }
    if crtc.is_null() {
        return false;
    }
    // SAFETY: `old_plane_state` and `new_plane_state` are valid pointers or null from DRM core.
    unsafe {
        old_plane_state
            .as_ref()
            .is_some_and(|state| state.crtc == crtc)
            || new_plane_state
                .as_ref()
                .is_some_and(|state| state.crtc == crtc)
    }
}

fn apply_precommitted(private: &crate::device::DriverPrivate, staged: PrecommittedDisplay) {
    let (old_object, old_framebuffer) =
        replace_last_scanout(private, staged.object, staged.framebuffer);
    // The RDIF display completion (or terminal error contract) owns the old
    // framebuffer as well as its GEM object: imported per-framebuffer 2D
    // resources are released by the framebuffer's final destroy callback.
    // SAFETY: `old_object`, `old_framebuffer`, `staged.crtc`, and `staged.event` are valid DRM pointers.
    unsafe {
        fence::track_read_completion(
            old_object,
            old_framebuffer,
            staged.completion,
            staged.crtc,
            staged.event,
        );
        drm_gem_object_put(old_object);
        kdrm_core::drm_framebuffer_upstream::drm_framebuffer_put(old_framebuffer);
    }
}

/// # Safety
///
/// `pipe` and `atomic` may be null (returns `Err(-EINVAL)`). If non-null, `pipe` must point
/// to an initialized `DrmSimpleDisplayPipe` and `atomic` must point to the active `DrmAtomicCommit`.
unsafe fn display_state_from_atomic(
    output: OutputId,
    pipe: *mut DrmSimpleDisplayPipe,
    atomic: *mut DrmAtomicCommit,
    modes: Option<&[Mode]>,
) -> Result<DisplayState, i32> {
    if pipe.is_null() || atomic.is_null() {
        return Err(NEG_EINVAL);
    }
    // Plane-only transactions may not carry a new CRTC state. Use an atomic
    // state's replacement where present, otherwise validate against the live
    // committed state; the primary plane follows the same rule.
    // SAFETY: `atomic` and `pipe` are live structures, and CRTC/plane pointers within `pipe` are valid.
    let (crtc, plane) = unsafe {
        let crtc_ptr = drm_atomic_get_new_crtc_state(atomic, ptr::addr_of_mut!((*pipe).crtc));
        let plane_ptr = drm_atomic_get_new_plane_state(atomic, ptr::addr_of_mut!((*pipe).plane));
        (
            crtc_ptr.as_ref().or_else(|| (*pipe).crtc.state.as_ref()),
            plane_ptr.as_ref().or_else(|| (*pipe).plane.state.as_ref()),
        )
    };
    let (Some(crtc), Some(plane)) = (crtc, plane) else {
        return Err(NEG_ENODEV);
    };
    display_state_from_fields(output, crtc, plane, modes)
}

fn display_state_from_fields(
    output: OutputId,
    crtc: &DrmCrtcState,
    plane: &DrmPlaneState,
    modes: Option<&[Mode]>,
) -> Result<DisplayState, i32> {
    if !crtc.enable || !crtc.active || plane.crtc.is_null() || plane.fb.is_null() {
        return Ok(DisplayState {
            output,
            mode: None,
            framebuffer: None,
            damage: Vec::new(),
        });
    }
    // SAFETY: plane and CRTC state are part of a live DRM atomic transaction.
    let fb = unsafe { plane.fb.as_ref() }.ok_or(NEG_EINVAL)?;
    // SAFETY: `fb` is non-null and references the active DRM framebuffer validated above.
    let (format, buffer) = unsafe { framebuffer_resource(fb)? };
    let width = u32::from(crtc.adjusted_mode.hdisplay);
    let height = u32::from(crtc.adjusted_mode.vdisplay);
    let fb_width = fb.width;
    let fb_height = fb.height;
    let src_x = plane.src_x >> 16;
    let src_y = plane.src_y >> 16;
    let src_width = if plane.src_w == 0 {
        fb_width
    } else {
        plane.src_w >> 16
    };
    let src_height = if plane.src_h == 0 {
        fb_height
    } else {
        plane.src_h >> 16
    };
    let x_end = src_x.checked_add(src_width).ok_or(NEG_EINVAL)?;
    let y_end = src_y.checked_add(src_height).ok_or(NEG_EINVAL)?;
    if !valid_dimensions(width, height)
        || !valid_dimensions(fb_width, fb_height)
        || x_end > fb_width
        || y_end > fb_height
        || src_width != width
        || src_height != height
    {
        return Err(NEG_EINVAL);
    }
    let stride = fb.pitches[0];
    let offset = usize::try_from(fb.offsets[0]).map_err(|_| NEG_EINVAL)?;
    let Some(required) = (height as usize - 1)
        .checked_mul(stride as usize)
        .and_then(|bytes| bytes.checked_add(width as usize * format.bytes_per_pixel()))
        .and_then(|bytes| bytes.checked_add(offset))
    else {
        return Err(NEG_EINVAL);
    };
    // The GEM backing is fixed-size and is retained by the object through the
    // core framebuffer reference. Reject geometry exceeding that allocation.
    if !gem::framebuffer_storage_valid(fb.obj[0], required, fb_width, fb_height) {
        return Err(NEG_EINVAL);
    }
    // The DRM CVT mode is only a userspace/timer-facing representation. RDIF
    // output modes are authoritative and may report refresh=0 (unspecified),
    // so resolve back to the exact advertised RDIF tuple before check/commit.
    let modes = modes.ok_or(NEG_ENODEV)?;
    let mode = resolve_rdif_mode_in_modes(
        modes,
        width,
        height,
        mode_refresh_millihz(&crtc.adjusted_mode),
    )?;
    Ok(DisplayState {
        output,
        mode: Some(mode),
        framebuffer: Some(Framebuffer {
            buffer,
            width: fb_width,
            height: fb_height,
            stride,
            offset,
            format,
        }),
        damage: vec![Rect {
            x: src_x,
            y: src_y,
            width: src_width,
            height: src_height,
        }],
    })
}

/// # Safety
///
/// `fb.format`, if non-null, must point to an initialized `DrmFormatInfo`.
/// `fb.obj[0]` must point to a valid GEM object.
unsafe fn framebuffer_resource(fb: &DrmFramebuffer) -> Result<(PixelFormat, ScanoutBuffer), i32> {
    let format = if fb.format.is_null() {
        return Err(NEG_EINVAL);
    } else {
        // SAFETY: fb.format was verified non-null above; reading the fourcc format code.
        unsafe { (*fb.format).format }
    };
    let format = pixel_format_from_fourcc(format).ok_or(NEG_EOPNOTSUPP)?;
    // The simple pipe accepts one framebuffer plane for the supported linear
    // formats; modifiers and multi-plane layouts are rejected by core setup.
    let object = fb.obj[0];
    let buffer = if gem::is_imported(object) {
        ScanoutBuffer::Gpu(gem::framebuffer_scanout_resource(fb).ok_or(NEG_EOPNOTSUPP)?)
    } else {
        gem::scanout_buffer(object).ok_or(NEG_EINVAL)?
    };
    Ok((format, buffer))
}

fn output_info(output: OutputId) -> Option<ax_gpu::rdif_display::OutputInfo> {
    ax_gpu::with_display(|display| display.output(output))
        .ok()
        .and_then(Result::ok)
}

fn valid_dimensions(width: u32, height: u32) -> bool {
    (MIN_MODE..=MAX_MODE).contains(&width) && (MIN_MODE..=MAX_MODE).contains(&height)
}

fn resolve_rdif_mode_in_modes(
    modes: &[Mode],
    width: u32,
    height: u32,
    drm_refresh_millihz: u32,
) -> Result<Mode, i32> {
    let mut wildcard = None;
    let mut exactish = None;
    for mode in modes
        .iter()
        .copied()
        .filter(|mode| mode.width == width && mode.height == height)
    {
        if mode.refresh_millihz == 0 {
            wildcard.get_or_insert(mode);
        } else if exactish.is_none_or(|best: Mode| {
            mode.refresh_millihz.abs_diff(drm_refresh_millihz)
                < best.refresh_millihz.abs_diff(drm_refresh_millihz)
        }) {
            exactish = Some(mode);
        }
    }
    if let Some(mode) =
        exactish.filter(|mode| mode.refresh_millihz.abs_diff(drm_refresh_millihz) <= 1_500)
    {
        return Ok(mode);
    }
    wildcard.ok_or(NEG_EINVAL)
}

fn mode_refresh_millihz(mode: &DrmDisplayMode) -> u32 {
    let denominator = u64::from(mode.htotal).saturating_mul(u64::from(mode.vtotal));
    if denominator == 0 || mode.clock <= 0 {
        return 60_000;
    }
    (u64::from(mode.clock as u32)
        .saturating_mul(1_000_000)
        .checked_div(denominator)
        .unwrap_or(60_000))
    .min(u64::from(u32::MAX)) as u32
}

fn display_error_to_errno(error: DisplayError) -> i32 {
    match error {
        DisplayError::Unsupported | DisplayError::Gpu(ax_gpu::rdif_gpu::GpuError::Unsupported) => {
            NEG_EOPNOTSUPP
        }
        DisplayError::NotAvailable
        | DisplayError::DeviceLost
        | DisplayError::Gpu(
            ax_gpu::rdif_gpu::GpuError::NotAvailable | ax_gpu::rdif_gpu::GpuError::DeviceLost,
        ) => NEG_ENODEV,
        DisplayError::InvalidOutput
        | DisplayError::InvalidState
        | DisplayError::Gpu(
            ax_gpu::rdif_gpu::GpuError::InvalidArgument | ax_gpu::rdif_gpu::GpuError::InvalidHandle,
        ) => NEG_EINVAL,
        DisplayError::Busy
        | DisplayError::NotReady
        | DisplayError::Gpu(
            ax_gpu::rdif_gpu::GpuError::Busy | ax_gpu::rdif_gpu::GpuError::NotReady,
        ) => -drm_kapi::config::EBUSY,
        _ => NEG_EIO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_only_atomic_state_uses_committed_crtc_and_plane_fallback() {
        // The helper's `get_new_*` lookups return null for untouched objects.
        // Zeroed C-layout records are sufficient here: the fallback path sees
        // an inactive CRTC and returns a disabled state without dereferencing
        // any uninitialized object or invoking RDIF.
        // SAFETY: All-zero is valid for these C-layout DRM storage records;
        // the test only reads their null pointers and zero-valued bool fields.
        let mut pipe = unsafe { core::mem::zeroed::<DrmSimpleDisplayPipe>() };
        // SAFETY: Zeroing DrmCrtcState creates an inactive CRTC with null pointer fields.
        let mut committed_crtc = unsafe { core::mem::zeroed::<DrmCrtcState>() };
        // SAFETY: Zeroing DrmPlaneState creates an inactive plane with null buffer pointers.
        let mut committed_plane = unsafe { core::mem::zeroed::<DrmPlaneState>() };
        // SAFETY: Zeroing DrmAtomicCommit provides null array pointers to simulate untouched state.
        let mut atomic = unsafe { core::mem::zeroed::<DrmAtomicCommit>() };
        pipe.crtc.state = &mut committed_crtc;
        pipe.plane.state = &mut committed_plane;

        // SAFETY: `pipe` and `atomic` are zeroed mock structs safe for reading in this test.
        let display_state =
            unsafe { display_state_from_atomic(OutputId::new(0), &mut pipe, &mut atomic, None) }
                .expect("plane-only transaction should use the current committed states");

        assert!(display_state.mode.is_none());
        assert!(display_state.framebuffer.is_none());
    }

    #[test]
    fn unspecified_rdif_refresh_maps_to_advertised_mode_tuple() {
        let modes = [Mode {
            width: 1920,
            height: 1080,
            refresh_millihz: 0,
        }];
        let mode = resolve_rdif_mode_in_modes(&modes, 1920, 1080, 59_940)
            .expect("unspecified refresh accepts the advertised dimensions");
        assert_eq!(mode, modes[0]);
    }

    #[test]
    fn successful_precommit_transfers_fence_and_event_exactly_once() {
        let event = ptr::dangling_mut::<DrmPendingVblankEvent>();
        let object = ptr::dangling_mut::<DrmGemObject>();
        let framebuffer = ptr::dangling_mut::<DrmFramebuffer>();
        let atomic = ptr::dangling_mut::<DrmAtomicCommit>();
        let mut slot = Some(PrecommittedDisplay {
            atomic,
            ready: true,
            completion: Completion::Pending(core::num::NonZeroU64::new(7).unwrap()),
            object,
            framebuffer,
            crtc: ptr::dangling_mut::<DrmCrtc>(),
            event,
        });

        let staged = take_ready_precommitted(&mut slot, atomic)
            .expect("post-swap pipe callback consumes the committed stage");
        assert!(matches!(staged.completion, Completion::Pending(_)));
        assert_eq!(staged.object, object);
        assert_eq!(staged.framebuffer, framebuffer);
        assert_eq!(staged.event, event);
        assert!(take_ready_precommitted(&mut slot, atomic).is_none());
    }

    #[test]
    fn pre_swap_skips_unbound_plane_changes_but_tracks_pipe_detach() {
        let crtc = ptr::dangling_mut::<DrmCrtc>();
        // SAFETY: Initializing zeroed C-layout storage with null crtc pointer for old plane state.
        let mut old_plane = unsafe { core::mem::zeroed::<DrmPlaneState>() };
        // SAFETY: Initializing zeroed C-layout storage with null crtc pointer for new plane state.
        let mut new_plane = unsafe { core::mem::zeroed::<DrmPlaneState>() };

        assert!(!atomic_change_affects_pipe(
            ptr::null_mut(),
            &mut old_plane,
            &mut new_plane,
            crtc,
        ));

        old_plane.crtc = crtc;
        assert!(atomic_change_affects_pipe(
            ptr::null_mut(),
            &mut old_plane,
            &mut new_plane,
            crtc,
        ));
        old_plane.crtc = ptr::null_mut();
        new_plane.crtc = crtc;
        assert!(atomic_change_affects_pipe(
            ptr::null_mut(),
            &mut old_plane,
            &mut new_plane,
            crtc,
        ));

        assert!(atomic_change_affects_pipe(
            ptr::dangling_mut::<DrmCrtcState>(),
            ptr::null_mut(),
            ptr::null_mut(),
            crtc,
        ));
    }
}

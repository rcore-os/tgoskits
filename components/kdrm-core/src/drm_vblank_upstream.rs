// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_vblank.c`.
//!
//! The vblank machinery: per-CRTC counters and timestamps, vblank reference
//! counting with the delayed disable timer, the vblank IRQ entry point
//! `drm_crtc_handle_vblank()`, timestamping through the driver's
//! `get_vblank_timestamp` hook, vblank event delivery, and the WAIT_VBLANK
//! and CRTC_GET_SEQUENCE/QUEUE_SEQUENCE ioctls. Functions are named and
//! ordered as in the C file.
//!
//! Deviations (documented in the batch report):
//! - Module parameters (`vblankoffdelay`, `timestamp_precision_usec`) are
//!   compiled to their default values (5000 ms, 20 us).
//! - Debug logging (`drm_dbg_vbl`, `drm_dbg_core`) is dropped; `drm_WARN_ONCE`
//!   reports through the kapi warning helper without the "once" throttle.

// The Linux ports state the invariant of each C-ABI call once in the module
// header, as the GT ports do; per-block SAFETY prose is not repeated.

use core::{
    ffi::c_void,
    mem::{offset_of, size_of},
    sync::atomic::{AtomicI32, AtomicI64, AtomicU32, Ordering},
};

use drm_kapi::{
    assert::warn_on,
    config::{EBUSY, EINTR, EINVAL, ENOMEM, EOPNOTSUPP, ETIMEDOUT, GFP_KERNEL, HZ},
    list::{list_add_tail, list_del, list_empty},
    locks::{spin_lock_irqsave_raw, spin_lock_raw, spin_unlock_irqrestore_raw, spin_unlock_raw},
    timer::HRTIMER_MODE_REL,
    types::{AtomicT, AtomicT64, ListHead, Seqlock},
};

use crate::{
    drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset,
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_file_types::{DrmFile, DrmPendingEvent},
    drm_mode_object_upstream::{DRIVER_ATOMIC, DRIVER_MODESET},
    drm_modes_types::DrmDisplayMode,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_vblank_types::{
        DrmPendingVblankEvent, DrmVblankCrtc, DrmVblankCrtcConfig, DrmVblankCrtcTimer,
    },
    uapi_drm_consts::{DRM_EVENT_CRTC_SEQUENCE, DRM_EVENT_FLIP_COMPLETE, DRM_EVENT_VBLANK},
    uapi_drm_types::{
        _DRM_VBLANK_ABSOLUTE, _DRM_VBLANK_EVENT, _DRM_VBLANK_HIGH_CRTC_MASK,
        _DRM_VBLANK_NEXTONMISS, _DRM_VBLANK_RELATIVE, _DRM_VBLANK_SECONDARY, _DRM_VBLANK_SIGNAL,
        DrmCrtcGetSequence, DrmCrtcQueueSequence, DrmEvent, DrmEventCrtcSequence, DrmEventVblank,
        DrmWaitVblank, DrmWaitVblankReply,
    },
};

/// `_DRM_VBLANK_*` masks and shift are macros in include/uapi/drm/drm.h.
const _DRM_VBLANK_TYPES_MASK: u32 = _DRM_VBLANK_ABSOLUTE | _DRM_VBLANK_RELATIVE;
const _DRM_VBLANK_FLAGS_MASK: u32 =
    _DRM_VBLANK_EVENT | _DRM_VBLANK_SIGNAL | _DRM_VBLANK_SECONDARY | _DRM_VBLANK_NEXTONMISS;
const _DRM_VBLANK_HIGH_CRTC_SHIFT: u32 = 1;
/// `EACCES`/`ENOENT`: errno values not all present in the kapi errno table.
const ENOENT: i32 = 2;
use drm_kapi::types::Hrtimer;

use crate::{
    drm_modeset_helper_vtables_types::DrmCrtcHelperFuncs,
    uapi_drm_consts::{
        DRM_CRTC_SEQUENCE_NEXT_ON_MISS, DRM_CRTC_SEQUENCE_RELATIVE, DRM_MODE_FLAG_INTERLACE,
    },
};

/// `DRM_TIMESTAMP_MAXRETRIES` (drm_vblank.c).
const DRM_TIMESTAMP_MAXRETRIES: i32 = 3;
/// `CLOCK_MONOTONIC` / `HRTIMER_MODE_REL` (include/uapi/linux/time.h, hrtimer.h).
const CLOCK_MONOTONIC: i32 = 1;
const HRTIMER_NORESTART: i32 = 0;
const HRTIMER_RESTART: i32 = 1;

/// Module parameter `vblankoffdelay` (default 5000 ms).
static DRM_VBLANK_OFFDELAY: AtomicI32 = AtomicI32::new(5000);
/// Module parameter `timestamp_precision_usec` (default 20 us).
static DRM_TIMESTAMP_PRECISION: AtomicI32 = AtomicI32::new(20);

unsafe extern "C" {
    // upstream: drm_file.c
    fn drm_send_event_timestamp_locked(dev: *mut DrmDevice, e: *mut DrmPendingEvent, now: i64);
    fn drm_event_reserve_init_locked(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        p: *mut DrmPendingEvent,
        e: *mut DrmEvent,
    ) -> i32;
    // upstream: drm_lease.c
    fn drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool;
    // upstream: drm_modeset_lock.c
    fn drm_modeset_lock(lock: *mut DrmModesetLock, ctx: *mut DrmModesetAcquireCtx) -> i32;
    fn drm_modeset_unlock(lock: *mut DrmModesetLock);
    // upstream: drm_modes.c
    fn drm_mode_copy(dst: *mut DrmDisplayMode, src: *const DrmDisplayMode);
}

/// `struct timespec64` (include/linux/time64.h).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec64 {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

// ---------------------------------------------------------------------------
// Header inlines and small helpers.
// ---------------------------------------------------------------------------

/// `drm_vblank_passed()` from drm_internal.h.
// upstream: drm_internal.h drm_vblank_passed()
#[inline]
pub(crate) fn drm_vblank_passed(seq: u64, ref_: u64) -> bool {
    seq.wrapping_sub(ref_) <= (1u64 << 23)
}

/// `drm_crtc_index()` (include/drm/drm_crtc.h).
#[inline]
/// # Safety
///
/// `crtc` must be a valid, aligned, readable pointer to an initialized `DrmCrtc`.
unsafe fn drm_crtc_index(crtc: *const DrmCrtc) -> u32 {
    // SAFETY: `crtc` is valid and readable per caller preconditions; reading `index` does not alias mutably.
    unsafe { (*crtc).index }
}

/// `drm_core_check_feature()` (include/drm/drm_drv.h).
#[inline]
/// # Safety
///
/// `dev` must be a valid, live pointer to `DrmDevice` whose `driver` points to an initialized `DrmDriver`.
unsafe fn drm_core_check_feature(dev: *mut DrmDevice, feature: u32) -> bool {
    // SAFETY: `dev` and its `driver` vtable pointer are valid and live per caller preconditions;
    // reading `driver_features` is safe and non-mutating.
    unsafe { ((*(*dev).driver).driver_features & feature) != 0 }
}

/// `drm_vblank_crtc()` (static, drm_vblank.c).
// upstream: drm_vblank.c drm_vblank_crtc()
#[inline]
/// # Safety
///
/// `dev` must be a valid pointer to `DrmDevice` where `vblank` points to an allocated array
/// of at least `pipe + 1` `DrmVblankCrtc` elements.
unsafe fn drm_vblank_crtc(dev: *mut DrmDevice, pipe: u32) -> *mut DrmVblankCrtc {
    // SAFETY: `(*dev).vblank` points to an allocated array of `num_crtcs` `DrmVblankCrtc` elements,
    // and caller guarantees `pipe < dev->num_crtcs`. Offset arithmetic within the slice is in-bounds.
    unsafe { (*dev).vblank.add(pipe as usize) }
}

/// `drm_crtc_vblank_crtc()` (exported).
// upstream: drm_vblank.c drm_crtc_vblank_crtc()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`. `(*crtc).dev` must point
/// to a live `DrmDevice` where `drm_vblank_init` has been called, and `(*crtc).index` must be strictly
/// less than `(*(*crtc).dev).num_crtcs`.
pub unsafe extern "C" fn drm_crtc_vblank_crtc(crtc: *mut DrmCrtc) -> *mut DrmVblankCrtc {
    // SAFETY: `crtc` and its `dev` field are valid per caller preconditions. `drm_crtc_index(crtc)`
    // yields `(*crtc).index` which corresponds to the valid pipe in `dev->vblank`.
    unsafe { drm_vblank_crtc((*crtc).dev, drm_crtc_index(crtc)) }
}

/// `drm_crtc_from_index()` (drm_crtc.c), resolved through the CRTC list.
#[inline]
/// # Safety
///
/// `dev` must be a valid, live pointer to `DrmDevice` with initialized `mode_config.crtc_list`.
unsafe fn drm_crtc_from_index(dev: *mut DrmDevice, idx: u32) -> *mut DrmCrtc {
    // SAFETY: `dev` is valid and live per caller preconditions; delegates to `drm_crtc_upstream::drm_crtc_from_index`.
    unsafe { crate::drm_crtc_upstream::drm_crtc_from_index(dev, idx as i32) }
}

// upstream: include/linux/atomic.h atomic64_read()
#[inline]
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT64`.
unsafe fn atomic64_read(v: *const AtomicT64) -> i64 {
    // SAFETY: `v` points to an aligned `AtomicT64` whose `counter` field is loaded via `AtomicI64`
    // with SeqCst ordering.
    unsafe { (*(core::ptr::addr_of!((*v).counter) as *const AtomicI64)).load(Ordering::SeqCst) }
}

// upstream: include/linux/atomic.h atomic64_add()
#[inline]
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT64`.
unsafe fn atomic64_add(i: i64, v: *mut AtomicT64) {
    // SAFETY: `v` points to an aligned `AtomicT64` whose `counter` field is updated via `AtomicI64::fetch_add`
    // with SeqCst ordering.
    unsafe {
        (*(core::ptr::addr_of_mut!((*v).counter) as *const AtomicI64))
            .fetch_add(i, Ordering::SeqCst);
    }
}

// upstream: include/linux/seqlock.h write_seqlock()
/// # Safety
///
/// `sl` must be a valid, aligned pointer to an initialized `Seqlock`.
unsafe fn write_seqlock(sl: *mut Seqlock) {
    // SAFETY: `sl` points to an initialized seqlock; `spin_lock_raw` locks the spinlock,
    // and the sequence counter is atomically incremented to odd to signal writing state.
    unsafe {
        spin_lock_raw(core::ptr::addr_of_mut!((*sl).lock));
        let seq = &*(core::ptr::addr_of!((*sl).seqcount.sequence) as *const AtomicU32);
        seq.fetch_add(1, Ordering::SeqCst);
    }
}

// upstream: include/linux/seqlock.h write_sequnlock()
/// # Safety
///
/// `sl` must be a valid, aligned pointer to an initialized `Seqlock` currently held by `write_seqlock`.
unsafe fn write_sequnlock(sl: *mut Seqlock) {
    // SAFETY: `sl` was locked by this thread via `write_seqlock`; sequence counter is incremented
    // to even to signal stable state, and `spin_unlock_raw` releases the spinlock.
    unsafe {
        let seq = &*(core::ptr::addr_of!((*sl).seqcount.sequence) as *const AtomicU32);
        seq.fetch_add(1, Ordering::SeqCst);
        spin_unlock_raw(core::ptr::addr_of_mut!((*sl).lock));
    }
}

// upstream: include/linux/seqlock.h read_seqbegin()
/// # Safety
///
/// `sl` must be a valid, aligned pointer to an initialized `Seqlock`.
unsafe fn read_seqbegin(sl: *const Seqlock) -> u32 {
    // SAFETY: `sl` points to an initialized seqlock; sequence counter is read atomically
    // until an even value indicates no concurrent writer.
    unsafe {
        let seq = &*(core::ptr::addr_of!((*sl).seqcount.sequence) as *const AtomicU32);
        loop {
            let s = seq.load(Ordering::SeqCst);
            if s & 1 == 0 {
                return s;
            }
            core::hint::spin_loop();
        }
    }
}

// upstream: include/linux/seqlock.h read_seqretry()
/// # Safety
///
/// `sl` must be a valid, aligned pointer to an initialized `Seqlock`.
unsafe fn read_seqretry(sl: *const Seqlock, start: u32) -> bool {
    // SAFETY: `sl` points to an initialized seqlock; sequence counter is read atomically
    // with SeqCst ordering to check whether a write occurred since `start`.
    unsafe {
        let seq = &*(core::ptr::addr_of!((*sl).seqcount.sequence) as *const AtomicU32);
        seq.load(Ordering::SeqCst) != start
    }
}

// ---------------------------------------------------------------------------
// Counter and timestamp storage.
// ---------------------------------------------------------------------------

/// Stores a new vblank count increment and timestamp under the seqlock.
// upstream: drm_vblank.c store_vblank()
/// # Safety
///
/// `dev` must be a valid `DrmDevice`, `pipe < dev->num_crtcs`, and caller must hold `dev->vblank_time_lock`.
unsafe fn store_vblank(
    dev: *mut DrmDevice,
    pipe: u32,
    vblank_count_inc: u32,
    t_vblank: i64,
    last: u32,
) {
    // SAFETY: `dev` is valid and caller holds `vblank_time_lock`. `drm_vblank_crtc` yields a valid
    // pointer within `dev->vblank`. Updates to `time` and `count` are serialized under `vblank->seqlock`.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);

        // assert_spin_locked(&dev->vblank_time_lock)
        (*vblank).last = last;

        write_seqlock(core::ptr::addr_of_mut!((*vblank).seqlock));
        (*vblank).time = t_vblank;
        atomic64_add(
            vblank_count_inc as i64,
            core::ptr::addr_of_mut!((*vblank).count),
        );
        write_sequnlock(core::ptr::addr_of_mut!((*vblank).seqlock));
    }
}

/// The hardware counter's maximum, falling back to the device-wide value.
// upstream: drm_vblank.c drm_max_vblank_count()
/// # Safety
///
/// `dev` must point to a live `DrmDevice` with initialized `vblank` array, and `pipe < dev->num_crtcs`.
unsafe fn drm_max_vblank_count(dev: *mut DrmDevice, pipe: u32) -> u32 {
    // SAFETY: `dev` is valid and `pipe` is within `dev->num_crtcs`; reads `max_vblank_count`
    // from either the CRTC's vblank structure or device-level fallback.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        if (*vblank).max_vblank_count != 0 {
            (*vblank).max_vblank_count
        } else {
            (*dev).max_vblank_count
        }
    }
}

/// Counter used by drivers without a hardware vblank counter.
// upstream: drm_vblank.c drm_vblank_no_hw_counter()
/// # Safety
///
/// `dev` must point to a valid, live `DrmDevice`, and `pipe < dev->num_crtcs`.
unsafe fn drm_vblank_no_hw_counter(dev: *mut DrmDevice, pipe: u32) -> u32 {
    // SAFETY: `dev` and `pipe` are valid per caller preconditions; queries `drm_max_vblank_count` and returns 0.
    unsafe {
        // drm_WARN_ON_ONCE(dev, drm_max_vblank_count(dev, pipe) != 0)
        warn_on(drm_max_vblank_count(dev, pipe) != 0);
        0
    }
}

/// Reads the hardware counter through the CRTC's `get_vblank_counter` hook.
// upstream: drm_vblank.c __get_vblank_counter()
/// # Safety
///
/// `dev` must point to a live `DrmDevice` with modesetting enabled, and `pipe < dev->num_crtcs`.
unsafe fn __get_vblank_counter(dev: *mut DrmDevice, pipe: u32) -> u32 {
    // SAFETY: `dev` is valid per caller preconditions. If MODESET is active, `drm_crtc_from_index`
    // retrieves the valid CRTC whose vtable hook `get_vblank_counter` is invoked if present.
    unsafe {
        if drm_core_check_feature(dev, DRIVER_MODESET) {
            let crtc = drm_crtc_from_index(dev, pipe);
            if warn_on(crtc.is_null()) {
                return 0;
            }
            if let Some(get_vblank_counter) = (*(*crtc).funcs).get_vblank_counter {
                return get_vblank_counter(crtc);
            }
        }
        drm_vblank_no_hw_counter(dev, pipe)
    }
}

/// Samples the counter and the timestamp, retrying until the counter is stable.
// upstream: drm_vblank.c get_vblank_counter_and_timestamp()
/// # Safety
///
/// `dev` must be valid, `pipe < dev->num_crtcs`, and `cur_vblank` and `t_vblank` must be valid writable pointers.
unsafe fn get_vblank_counter_and_timestamp(
    dev: *mut DrmDevice,
    pipe: u32,
    cur_vblank: *mut u32,
    t_vblank: *mut i64,
    in_vblank_irq: bool,
) -> bool {
    // SAFETY: `dev` is valid and output pointers `cur_vblank` and `t_vblank` point to caller-owned
    // stack variables. Repeatedly samples hardware counter and timestamp until consistent.
    unsafe {
        let mut count = DRM_TIMESTAMP_MAXRETRIES;
        let mut rc;
        loop {
            *cur_vblank = __get_vblank_counter(dev, pipe);
            rc = drm_get_last_vbltimestamp(dev, pipe, t_vblank, in_vblank_irq);
            if !(*cur_vblank != __get_vblank_counter(dev, pipe) && {
                count -= 1;
                count > 0
            }) {
                break;
            }
        }
        rc
    }
}

/// Re-anchors the vblank timestamp after a modeset.
// upstream: drm_vblank.c drm_reset_vblank_timestamp()
/// # Safety
///
/// `dev` must point to a live `DrmDevice`, `pipe < dev->num_crtcs`, and caller must hold `dev->vbl_lock`.
unsafe fn drm_reset_vblank_timestamp(dev: *mut DrmDevice, pipe: u32) {
    // SAFETY: `dev` is valid; `vblank_time_lock` is locked around timestamp query and `store_vblank`.
    unsafe {
        let mut cur_vblank: u32 = 0;
        let mut t_vblank: i64 = 0;

        spin_lock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));

        // sample the current counter to avoid random jumps when
        // drm_vblank_enable() applies the diff
        let rc = get_vblank_counter_and_timestamp(dev, pipe, &mut cur_vblank, &mut t_vblank, false);

        // Only reinitialize corresponding vblank timestamp if high-precision query
        // available and didn't fail. Otherwise reinitialize delayed at next vblank
        // interrupt and assign 0 for now, to mark the vblanktimestamp as invalid.
        if !rc {
            t_vblank = 0;
        }

        // +1 to make sure user will never see the same vblank counter value before
        // and after a modeset
        store_vblank(dev, pipe, 1, t_vblank, cur_vblank);

        spin_unlock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));
    }
}

/// Advances the software vblank count by the number of vblanks that elapsed.
// upstream: drm_vblank.c drm_update_vblank_count()
/// # Safety
///
/// `dev` must be a valid `DrmDevice`, `pipe < dev->num_crtcs`, and caller must hold `dev->vblank_time_lock`.
unsafe fn drm_update_vblank_count(dev: *mut DrmDevice, pipe: u32, in_vblank_irq: bool) {
    // SAFETY: `dev` is valid and `vblank_time_lock` is held by caller. Updates software vblank count
    // based on elapsed duration or hardware counter, committing through `store_vblank`.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut cur_vblank: u32 = 0;
        let mut t_vblank: i64 = 0;
        let framedur_ns = (*vblank).framedur_ns;
        let max_vblank_count = drm_max_vblank_count(dev, pipe);
        let mut diff: u32;

        // Interrupts were disabled prior to this call, so deal with counter wrap if
        // needed.
        // NOTE! It's possible we lost a full dev->max_vblank_count + 1 events here
        // if the register is small or we had vblank interrupts off for a long time.
        //
        // We repeat the hardware vblank counter & timestamp query until we get
        // consistent results. This to prevent races between gpu updating its
        // hardware counter while we are retrieving the corresponding vblank
        // timestamp.
        let rc = get_vblank_counter_and_timestamp(
            dev,
            pipe,
            &mut cur_vblank,
            &mut t_vblank,
            in_vblank_irq,
        );

        if max_vblank_count != 0 {
            // trust the hw counter when it's around
            diff = cur_vblank.wrapping_sub((*vblank).last) & max_vblank_count;
        } else if rc && framedur_ns != 0 {
            let diff_ns = (t_vblank - (*vblank).time) as u64;
            // Figure out how many vblanks we've missed based on the difference in
            // the timestamps and the frame/field duration.
            // drm_dbg_vbl(...): dropped
            diff = drm_kapi::primitives::div_round_closest_ull(diff_ns, framedur_ns as u64) as u32;
        } else {
            // some kind of default for drivers w/o accurate vbl timestamping
            diff = if in_vblank_irq { 1 } else { 0 };
        }

        // Within a drm_vblank_pre_modeset - drm_vblank_post_modeset interval? If so
        // then vblank irqs keep running and it will likely happen that the hardware
        // vblank counter is not trustworthy as it might reset at some point in that
        // interval and vblank timestamps are not trustworthy either in that
        // interval. Iow. this can result in a bogus diff >> 1 which must be avoided
        // as it would cause random large forward jumps of the software vblank count.
        if diff > 1 && ((*vblank).inmodeset & 0x2) != 0 {
            // drm_dbg_vbl(...): dropped
            diff = 1;
        }

        if diff == 0 {
            // drm_WARN_ON_ONCE(dev, cur_vblank != vblank->last)
            warn_on(cur_vblank != (*vblank).last);
            return;
        }

        // Only reinitialize corresponding vblank timestamp if high-precision query
        // available and didn't fail, or we were called from the vblank interrupt.
        // Otherwise reinitialize delayed at next vblank interrupt and assign 0 for
        // now, to mark the vblanktimestamp as invalid.
        if !rc && !in_vblank_irq {
            t_vblank = 0;
        }

        store_vblank(dev, pipe, diff, t_vblank, cur_vblank);
    }
}

/// Current software vblank count of a pipe.
// upstream: drm_vblank.c drm_vblank_count()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be a valid, non-null pointer to a live `DrmDevice`. If `pipe >= (*dev).num_crtcs`
/// or vblank has not been initialized on `dev`, this function safely returns 0 without accessing
/// invalid vblank state.
pub unsafe extern "C" fn drm_vblank_count(dev: *mut DrmDevice, pipe: u32) -> u64 {
    // SAFETY: `dev` is valid. If `pipe < dev->num_crtcs`, `vblank` pointer is valid
    // and `count` is read atomically via `atomic64_read`.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);

        if warn_on(pipe >= (*dev).num_crtcs) {
            return 0;
        }

        // The read barrier corresponds to the implicit write barrier of the write
        // seqlock in store_vblank(); the atomic read is ordered by SeqCst.
        let count = atomic64_read(core::ptr::addr_of!((*vblank).count));
        count as u64
    }
}

/// Vblank count after bringing the software counter up to date.
// upstream: drm_vblank.c drm_crtc_accurate_vblank_count()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` whose `dev` field points
/// to a live `DrmDevice`. Caller must ensure the CRTC remains registered and live across this call.
pub unsafe extern "C" fn drm_crtc_accurate_vblank_count(crtc: *mut DrmCrtc) -> u64 {
    // SAFETY: `crtc` and `(*crtc).dev` are valid per caller preconditions. `vblank_time_lock`
    // is acquired with irqsave to serialize count update and retrieval.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);

        // drm_WARN_ONCE(dev, drm_debug_enabled(DRM_UT_VBL) && !crtc->funcs->get_vblank_timestamp, ...)

        let mut flags: core::ffi::c_ulong = 0;
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock), &mut flags);
        drm_update_vblank_count(dev, pipe, false);
        let vblank = drm_vblank_count(dev, pipe);
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock), flags);
        vblank
    }
}

/// Disables the hardware vblank interrupt through the CRTC's `disable_vblank`.
// upstream: drm_vblank.c __disable_vblank()
/// # Safety
///
/// `dev` must point to a live `DrmDevice`, `pipe < dev->num_crtcs`, and caller must hold `dev->vbl_lock`.
unsafe fn __disable_vblank(dev: *mut DrmDevice, pipe: u32) {
    // SAFETY: `dev` is valid; CRTC is resolved via `drm_crtc_from_index` and `disable_vblank`
    // callback is called if non-null.
    unsafe {
        if drm_core_check_feature(dev, DRIVER_MODESET) {
            let crtc = drm_crtc_from_index(dev, pipe);
            if warn_on(crtc.is_null()) {
                return;
            }
            if let Some(disable_vblank) = (*(*crtc).funcs).disable_vblank {
                disable_vblank(crtc);
            }
        }
    }
}

// upstream: drm_vblank.c atomic_read()/atomic_inc()/atomic_dec() on vblank->refcount
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT`.
#[inline]
unsafe fn atomic_read(v: *const AtomicT) -> i32 {
    // SAFETY: `v` points to an aligned `AtomicT` whose `counter` field is loaded via `AtomicI32`
    // with SeqCst ordering.
    unsafe { (*(core::ptr::addr_of!((*v).counter) as *const AtomicI32)).load(Ordering::SeqCst) }
}

// upstream: drm_vblank.c atomic_inc()
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT`.
#[inline]
unsafe fn atomic_inc(v: *mut AtomicT) {
    // SAFETY: `v` points to an aligned `AtomicT` whose `counter` field is updated via `AtomicI32::fetch_add`
    // with SeqCst ordering.
    unsafe {
        (*(core::ptr::addr_of_mut!((*v).counter) as *const AtomicI32))
            .fetch_add(1, Ordering::SeqCst);
    }
}

// upstream: drm_vblank.c atomic_dec()
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT`.
#[inline]
unsafe fn atomic_dec(v: *mut AtomicT) {
    // SAFETY: `v` points to an aligned `AtomicT` whose `counter` field is updated via `AtomicI32::fetch_sub`
    // with SeqCst ordering.
    unsafe {
        (*(core::ptr::addr_of_mut!((*v).counter) as *const AtomicI32))
            .fetch_sub(1, Ordering::SeqCst);
    }
}

// upstream: drm_vblank.c atomic_add_return()
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT`.
#[inline]
unsafe fn atomic_add_return(i: i32, v: *mut AtomicT) -> i32 {
    // SAFETY: `v` points to an aligned `AtomicT` whose `counter` field is updated via `AtomicI32::fetch_add`
    // with SeqCst ordering.
    unsafe {
        (*(core::ptr::addr_of_mut!((*v).counter) as *const AtomicI32))
            .fetch_add(i, Ordering::SeqCst)
            + i
    }
}

/// Atomic decrement returning true when the counter reached zero.
///
/// # Safety
///
/// `v` must be a valid, aligned pointer to an initialized `AtomicT`.
#[inline]
unsafe fn atomic_dec_and_test(v: *mut AtomicT) -> bool {
    // SAFETY: `v` points to an aligned `AtomicT` whose `counter` field is updated via `AtomicI32::fetch_sub`
    // with SeqCst ordering, returning true if the previous value was 1.
    unsafe {
        (*(core::ptr::addr_of_mut!((*v).counter) as *const AtomicI32))
            .fetch_sub(1, Ordering::SeqCst)
            == 1
    }
}

/// `struct drm_pending_vblank_event` event union accessors (ManuallyDrop fields).
///
/// # Safety
///
/// `e` must be a valid pointer to an initialized `DrmPendingVblankEvent`.
#[inline]
unsafe fn ev_base(e: *mut DrmPendingVblankEvent) -> *mut DrmEvent {
    // SAFETY: `e` points to a live `DrmPendingVblankEvent`; accessing union field `base` is valid.
    unsafe { core::ptr::addr_of_mut!((*e).event.base) as *mut DrmEvent }
}

/// # Safety
///
/// `e` must be a valid pointer to an initialized `DrmPendingVblankEvent`.
#[inline]
unsafe fn ev_vbl(e: *mut DrmPendingVblankEvent) -> *mut DrmEventVblank {
    // SAFETY: `e` points to a live `DrmPendingVblankEvent`; accessing union field `vbl` is valid.
    unsafe { core::ptr::addr_of_mut!((*e).event.vbl) as *mut DrmEventVblank }
}

/// # Safety
///
/// `e` must be a valid pointer to an initialized `DrmPendingVblankEvent`.
#[inline]
unsafe fn ev_seq(e: *mut DrmPendingVblankEvent) -> *mut DrmEventCrtcSequence {
    // SAFETY: `e` points to a live `DrmPendingVblankEvent`; accessing union field `seq` is valid.
    unsafe { core::ptr::addr_of_mut!((*e).event.seq) as *mut DrmEventCrtcSequence }
}

/// # Safety
///
/// `p` must be a valid pointer to an initialized `DrmWaitVblank`.
#[inline]
unsafe fn wv_req(p: *mut DrmWaitVblank) -> *mut crate::uapi_drm_types::DrmWaitVblankRequest {
    // SAFETY: `p` points to a live `DrmWaitVblank`; accessing union field `request` is valid.
    unsafe {
        core::ptr::addr_of_mut!((*p).request) as *mut crate::uapi_drm_types::DrmWaitVblankRequest
    }
}

/// # Safety
///
/// `p` must be a valid pointer to an initialized `DrmWaitVblank`.
#[inline]
unsafe fn wv_reply(p: *mut DrmWaitVblank) -> *mut DrmWaitVblankReply {
    // SAFETY: `p` points to a live `DrmWaitVblank`; accessing union field `reply` is valid.
    unsafe { core::ptr::addr_of_mut!((*p).reply) as *mut DrmWaitVblankReply }
}

/// Container of a vblank pointer from its `disable_timer` (timer_setup callback argument).
///
/// # Safety
///
/// `t` must be the `disable_timer` member embedded within a live `DrmVblankCrtc`.
#[inline]
unsafe fn vblank_from_timer(t: *mut drm_kapi::types::TimerList) -> *mut DrmVblankCrtc {
    // SAFETY: `t` points to `disable_timer` within a live `DrmVblankCrtc`. Offset subtraction
    // via `offset_of!(DrmVblankCrtc, disable_timer)` correctly recovers the enclosing struct.
    unsafe { (t as *mut u8).sub(offset_of!(DrmVblankCrtc, disable_timer)) as *mut DrmVblankCrtc }
}

/// Disables the vblank interrupt and saves the count, keeping the counter
/// ticking as if it had never stopped. Called with `dev->vbl_lock` held.
// upstream: drm_vblank.c drm_vblank_disable_and_save()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be a valid, non-null pointer to a live `DrmDevice`, and `pipe` must be `< (*dev).num_crtcs`.
/// Caller must hold `dev->vbl_lock` with interrupts disabled.
pub unsafe extern "C" fn drm_vblank_disable_and_save(dev: *mut DrmDevice, pipe: u32) {
    // SAFETY: `dev` is valid and caller holds `vbl_lock`. `vblank_time_lock` is acquired with irqsave;
    // if enabled, count is updated and hardware vblank disabled via `__disable_vblank`.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut irqflags: core::ffi::c_ulong = 0;

        // Prevent vblank irq processing while disabling vblank irqs, so no updates
        // of timestamps or count can happen after we've disabled. Needed to prevent
        // races in case of delayed irq's.
        spin_lock_irqsave_raw(
            core::ptr::addr_of_mut!((*dev).vblank_time_lock),
            &mut irqflags,
        );

        // Update vblank count and disable vblank interrupts only if the interrupts
        // were enabled. This avoids calling the ->disable_vblank() operation in
        // atomic context with the hardware potentially runtime suspended.
        if (*vblank).enabled {
            // Update the count and timestamp to maintain the appearance that the
            // counter has been ticking all along until this time. This makes the
            // count account for the entire time between drm_crtc_vblank_on() and
            // drm_crtc_vblank_off().
            drm_update_vblank_count(dev, pipe, false);
            __disable_vblank(dev, pipe);
            (*vblank).enabled = false;
        }

        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock), irqflags);
    }
}

/// Disable-timer callback: drops the hardware vblank once no user remains.
// upstream: drm_vblank.c vblank_disable_fn()
/// # Safety
///
/// `t` must be a valid pointer to the `disable_timer` member embedded inside an allocated, live
/// `DrmVblankCrtc`. This function is the callback invoked by the kernel timer subsystem upon timer expiration.
pub unsafe extern "C" fn vblank_disable_fn(t: *mut drm_kapi::types::TimerList) {
    // SAFETY: `vblank` is recovered from `t` via `vblank_from_timer`. `dev->vbl_lock` is acquired
    // with irqsave; if refcount is 0 and enabled, `drm_vblank_disable_and_save` is called.
    unsafe {
        let vblank = vblank_from_timer(t);
        let dev = (*vblank).dev;
        let pipe = (*vblank).pipe;
        let mut irqflags: core::ffi::c_ulong = 0;

        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).vbl_lock), &mut irqflags);
        if atomic_read(core::ptr::addr_of!((*vblank).refcount)) == 0 && (*vblank).enabled {
            // drm_dbg_core(dev, "disabling vblank on crtc %u\n", pipe): dropped
            drm_vblank_disable_and_save(dev, pipe);
        }
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).vbl_lock), irqflags);
    }
}

/// Managed-release action for one vblank CRTC.
// upstream: drm_vblank.c drm_vblank_init_release()
/// # Safety
///
/// `dev` must point to a live `DrmDevice` and `ptr` must point to a `DrmVblankCrtc` initialized by `drm_vblank_init`.
unsafe extern "C" fn drm_vblank_init_release(dev: *mut c_void, ptr: *mut c_void) {
    // SAFETY: `dev` and `vblank` are valid pointers registered during `drm_vblank_init`.
    // Any active hrtimer is cancelled, worker thread destroyed, and disable timer deleted synchronously.
    unsafe {
        let vblank = ptr as *mut DrmVblankCrtc;
        let dev = dev as *mut DrmDevice;

        warn_on(
            core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled))
                && drm_core_check_feature(dev, DRIVER_MODESET),
        );

        if !(*vblank).vblank_timer.crtc.is_null() {
            drm_kapi::timer::hrtimer_cancel(core::ptr::addr_of_mut!((*vblank).vblank_timer.timer));
        }

        // Drain and stop the owned kthread adapter when a worker exists.
        if !(*vblank).worker.is_null() {
            drm_kapi::kthread::destroy_worker((*vblank).worker.cast());
        }

        drm_kapi::timer::timer_delete_sync(core::ptr::addr_of_mut!((*vblank).disable_timer));
    }
}

/// Allocates the per-CRTC vblank state and starts each CRTC's vblank worker.
// upstream: drm_vblank.c drm_vblank_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be a valid, non-null pointer to an initialized `DrmDevice` undergoing driver setup.
/// Must be called before enabling modesetting or registering display devices, and must not be called
/// concurrently with other vblank operations on `dev`.
pub unsafe extern "C" fn drm_vblank_init(dev: *mut DrmDevice, num_crtcs: u32) -> i32 {
    // SAFETY: `dev` is valid. `vbl_lock` and `vblank_time_lock` are initialized. Memory for `num_crtcs`
    // `DrmVblankCrtc` structs is allocated via `drmm_kzalloc_c` and initialized, release action registered,
    // and worker thread initialized.
    unsafe {
        spin_lock_init_ptr(core::ptr::addr_of_mut!((*dev).vbl_lock));
        spin_lock_init_ptr(core::ptr::addr_of_mut!((*dev).vblank_time_lock));

        // drmm_kcalloc(dev, num_crtcs, sizeof(*dev->vblank), GFP_KERNEL)
        let bytes = match (num_crtcs as usize).checked_mul(size_of::<DrmVblankCrtc>()) {
            Some(b) => b,
            None => return -ENOMEM,
        };
        (*dev).vblank = drm_kapi::memory::drmm_kzalloc_c(dev as *mut c_void, bytes, GFP_KERNEL)
            as *mut DrmVblankCrtc;
        if (*dev).vblank.is_null() {
            return -ENOMEM;
        }

        (*dev).num_crtcs = num_crtcs;

        for i in 0..num_crtcs {
            let vblank = drm_vblank_crtc(dev, i);
            (*vblank).dev = dev;
            (*vblank).pipe = i;
            drm_kapi::wait::init_waitqueue_head(&mut (*vblank).queue);
            drm_kapi::timer::timer_setup(
                core::ptr::addr_of_mut!((*vblank).disable_timer),
                vblank_disable_fn,
                0,
            );
            // seqlock_init()
            spin_lock_init_ptr(core::ptr::addr_of_mut!((*vblank).seqlock.lock));
            (*vblank).seqlock.seqcount.sequence = 0;

            let ret = drm_kapi::kernel_core::drmm_add_action_or_reset(
                dev as *mut c_void,
                drm_vblank_init_release,
                vblank as *mut c_void,
            );
            if ret != 0 {
                return ret;
            }

            let ret = crate::drm_vblank_work_upstream::drm_vblank_worker_init(vblank);
            if ret != 0 {
                return ret;
            }
        }
        0
    }
}

// upstream: include/linux/spinlock.h spin_lock_init() on a raw lock
/// # Safety
///
/// `lock` must be a valid, aligned pointer to uninitialized or reusable `Spinlock` memory.
#[inline]
unsafe fn spin_lock_init_ptr(lock: *mut drm_kapi::types::Spinlock) {
    // SAFETY: `lock` points to allocated spinlock storage initialized via `drm_kapi::locks::spin_lock_init`.
    unsafe { drm_kapi::locks::spin_lock_init(&mut *lock) }
}

/// True when the device has vblank-capable CRTCs.
// upstream: drm_vblank.c drm_dev_has_vblank()
#[unsafe(no_mangle)]
/// # Safety
///
/// If non-null, `dev` must be an aligned, readable pointer to an initialized `DrmDevice`.
/// Returns `false` if `dev` is null.
pub unsafe extern "C" fn drm_dev_has_vblank(dev: *const DrmDevice) -> bool {
    // SAFETY: `dev` is valid and readable per caller preconditions; reading `num_crtcs` is sound.
    unsafe { (*dev).num_crtcs != 0 }
}

/// Wait queue that vblank waiters sleep on.
// upstream: drm_vblank.c drm_crtc_vblank_waitqueue()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` belonging to a `DrmDevice`
/// with initialized vblank support. The returned waitqueue pointer is valid for the lifetime of `crtc->dev`.
pub unsafe extern "C" fn drm_crtc_vblank_waitqueue(
    crtc: *mut DrmCrtc,
) -> *mut drm_kapi::types::WaitQueueHead {
    // SAFETY: `drm_crtc_vblank_crtc` returns a valid pointer to the CRTC's vblank structure
    // whose `queue` waitqueue head is returned.
    unsafe { core::ptr::addr_of_mut!((*drm_crtc_vblank_crtc(crtc)).queue) }
}

/// Recomputes line and frame durations from a mode's timings and copies the
/// mode to `hwmode`.
// upstream: drm_vblank.c drm_calc_timestamping_constants()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` with vblank initialized.
/// `mode` must be a valid, non-null pointer to an initialized `DrmDisplayMode` representing the active timing.
/// Caller must hold the modesetting lock for `crtc` (e.g. `mode_config.mutex` or commit locks) to ensure
/// exclusive mutation of `vblank->hwmode` and framedur constants.
pub unsafe extern "C" fn drm_calc_timestamping_constants(
    crtc: *mut DrmCrtc,
    mode: *const DrmDisplayMode,
) {
    // SAFETY: `crtc` and `mode` are valid per caller preconditions. Line and frame durations
    // are calculated and stored into `vblank`, and `mode` is copied into `vblank->hwmode`.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);
        let vblank = drm_crtc_vblank_crtc(crtc);
        let mut linedur_ns: i32 = 0;
        let mut framedur_ns: i32 = 0;
        let dotclock = (*mode).crtc_clock;

        if !drm_dev_has_vblank(dev) {
            return;
        }
        if warn_on(pipe >= (*dev).num_crtcs) {
            return;
        }

        // Valid dotclock?
        if dotclock > 0 {
            let frame_size = (*mode).crtc_htotal as i32 * (*mode).crtc_vtotal as i32;

            // Convert scanline length in pixels and video dot clock to line duration
            // and frame duration in nanoseconds.
            linedur_ns = (((*mode).crtc_htotal as u64 * 1_000_000) / dotclock as u64) as i32;
            framedur_ns = (((frame_size as u64) * 1_000_000) / dotclock as u64) as i32;

            // Fields of interlaced scanout modes are only half a frame duration.
            if (*mode).flags & DRM_MODE_FLAG_INTERLACE != 0 {
                framedur_ns /= 2;
            }
        } else {
            // drm_err(dev, "crtc %u: Can't calculate constants, dotclock = 0!\n")
        }

        (*vblank).linedur_ns = linedur_ns;
        (*vblank).framedur_ns = framedur_ns;
        drm_mode_copy(core::ptr::addr_of_mut!((*vblank).hwmode), mode);
    }
}

/// Core of the driver timestamp query: samples the scanout position and
/// converts it into the vblank start time.
// upstream: drm_vblank.c drm_crtc_vblank_helper_get_vblank_timestamp_internal()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must point to an initialized `DrmCrtc` whose `dev` has vblank support.
/// `max_error` and `vblank_time` must be valid, aligned, non-null pointers to writable `i32` and `i64` locations.
/// If `get_scanout_position` is provided, it must be safe to call with `crtc`, scanout coordinate pointers, and timing pointers.
pub unsafe extern "C" fn drm_crtc_vblank_helper_get_vblank_timestamp_internal(
    crtc: *mut DrmCrtc,
    max_error: *mut i32,
    vblank_time: *mut i64,
    in_vblank_irq: bool,
    get_scanout_position: Option<
        unsafe extern "C" fn(
            *mut DrmCrtc,
            bool,
            *mut i32,
            *mut i32,
            *mut i64,
            *mut i64,
            *mut DrmDisplayMode,
        ) -> bool,
    >,
) -> bool {
    // SAFETY: `crtc` is valid and `pipe < dev->num_crtcs`. Scanout position is queried via
    // `get_scanout_position` callback and converted into calibrated vblank timestamp.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = (*crtc).index;

        if pipe >= (*dev).num_crtcs {
            // drm_err(dev, "Invalid crtc %u\n", pipe)
            return false;
        }

        // Scanout position query not supported? Should not happen.
        let get_scanout_position = match get_scanout_position {
            Some(f) => f,
            None => return false,
        };

        let mode: *mut DrmDisplayMode = if drm_drv_uses_atomic_modeset(dev) {
            core::ptr::addr_of_mut!((*drm_crtc_vblank_crtc(crtc)).hwmode)
        } else {
            core::ptr::addr_of_mut!((*crtc).hwmode)
        };

        // If mode timing undefined, just return as no-op: Happens during initial
        // modesetting of a crtc.
        if (*mode).crtc_clock == 0 {
            // drm_dbg_core(dev, "crtc %u: Noop due to uninitialized mode.\n", pipe)
            warn_on(drm_drv_uses_atomic_modeset(dev));
            return false;
        }

        // Get current scanout position with system timestamp. Repeat query up to
        // DRM_TIMESTAMP_MAXRETRIES times if single query takes longer than
        // max_error nanoseconds. This guarantees a tight bound on maximum error if
        // code gets preempted or delayed for some reason.
        let mut vpos: i32 = 0;
        let mut hpos: i32 = 0;
        let mut stime: i64 = 0;
        let mut etime: i64 = 0;
        let mut duration_ns: i32 = 0;
        let mut i: i32 = 0;
        while i < DRM_TIMESTAMP_MAXRETRIES {
            // Get vertical and horizontal scanout position vpos, hpos, and bounding
            // timestamps stime, etime, pre/post query.
            let vbl_status = get_scanout_position(
                crtc,
                in_vblank_irq,
                &mut vpos,
                &mut hpos,
                &mut stime,
                &mut etime,
                mode,
            );

            // Return as no-op if scanout query unsupported or failed.
            if !vbl_status {
                // drm_dbg_core(dev, "crtc %u : scanoutpos query failed.\n", pipe)
                return false;
            }

            // Compute uncertainty in timestamp of scanout position query.
            duration_ns = (etime - stime) as i32;

            // Accept result with < max_error nsecs timing uncertainty.
            if duration_ns <= *max_error {
                break;
            }
            i += 1;
        }

        // Noisy system timing? (debug report dropped)

        // Return upper bound of timestamp precision error.
        *max_error = duration_ns;

        // Convert scanout position into elapsed time at raw_time query since start
        // of scanout at first display scanline. delta_ns can be negative if start of
        // scanout hasn't happened yet.
        let delta_ns = (1_000_000i64 * (vpos as i64 * (*mode).crtc_htotal as i64 + hpos as i64))
            / (*mode).crtc_clock as i64;

        // Subtract time delta from raw timestamp to get final vblank_time timestamp
        // for end of vblank.
        *vblank_time = etime - delta_ns;
        true
    }
}

/// `drm_crtc_vblank_helper_get_vblank_timestamp()`: the helper with the
/// CRTC helper's scanout-position hook.
// upstream: drm_vblank.c drm_crtc_vblank_helper_get_vblank_timestamp()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must point to an initialized `DrmCrtc`. `max_error` and `vblank_time` must point to valid,
/// aligned, writable `i32` and `i64` memory locations. Caller must ensure `crtc->helper_private` (if present)
/// contains a valid helper vtable.
pub unsafe extern "C" fn drm_crtc_vblank_helper_get_vblank_timestamp(
    crtc: *mut DrmCrtc,
    max_error: *mut i32,
    vblank_time: *mut i64,
    in_vblank_irq: bool,
) -> bool {
    // SAFETY: Delegates to `drm_crtc_vblank_helper_get_vblank_timestamp_internal` using
    // `get_scanout_position` from `crtc->helper_private`.
    unsafe {
        let helper: *mut DrmCrtcHelperFuncs = (*crtc).helper_private;
        drm_crtc_vblank_helper_get_vblank_timestamp_internal(
            crtc,
            max_error,
            vblank_time,
            in_vblank_irq,
            (*helper).get_scanout_position,
        )
    }
}

/// Asks the driver for the last vblank timestamp, falling back to the
/// monotonic clock.
// upstream: drm_vblank.c drm_crtc_get_last_vbltimestamp()
/// # Safety
///
/// `crtc` and `tvblank` must be valid pointers.
unsafe fn drm_crtc_get_last_vbltimestamp(
    crtc: *mut DrmCrtc,
    tvblank: *mut i64,
    in_vblank_irq: bool,
) -> bool {
    // SAFETY: `crtc` and `tvblank` are valid per caller preconditions. Invokes `get_vblank_timestamp`
    // hook on CRTC helper funcs or falls back to `ktime_get`.
    unsafe {
        let mut ret = false;

        // Define requested maximum error on timestamps (nanoseconds).
        let mut max_error: i32 = DRM_TIMESTAMP_PRECISION.load(Ordering::Relaxed) * 1000;

        // Query driver if possible and precision timestamping enabled.
        if !crtc.is_null()
            && let Some(get_vblank_timestamp) = (*(*crtc).funcs).get_vblank_timestamp
            && max_error > 0
        {
            ret = get_vblank_timestamp(crtc, &mut max_error, tvblank, in_vblank_irq);
        }

        // GPU high precision timestamp query unsupported or failed. Return current
        // monotonic/gettimeofday timestamp as best estimate.
        if !ret {
            *tvblank = drm_kapi::primitives::ktime_get();
        }
        ret
    }
}

/// Per-pipe variant of [`drm_crtc_get_last_vbltimestamp`].
// upstream: drm_vblank.c drm_get_last_vbltimestamp()
/// # Safety
///
/// `dev` and `tvblank` must be valid pointers and `pipe < dev->num_crtcs`.
unsafe fn drm_get_last_vbltimestamp(
    dev: *mut DrmDevice,
    pipe: u32,
    tvblank: *mut i64,
    in_vblank_irq: bool,
) -> bool {
    // SAFETY: Resolves CRTC for `pipe` and calls `drm_crtc_get_last_vbltimestamp`.
    unsafe {
        let crtc = drm_crtc_from_index(dev, pipe);
        drm_crtc_get_last_vbltimestamp(crtc, tvblank, in_vblank_irq)
    }
}

/// Current vblank count of a CRTC.
// upstream: drm_vblank.c drm_crtc_vblank_count()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` whose `dev` points to a live `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_vblank_count(crtc: *mut DrmCrtc) -> u64 {
    // SAFETY: `crtc` and its `dev` pointer are valid and live; delegates count retrieval for the CRTC's pipe index.
    unsafe { drm_vblank_count((*crtc).dev, drm_crtc_index(crtc)) }
}

/// Count and timestamp read consistently under the vblank seqlock.
// upstream: drm_vblank.c drm_vblank_count_and_time()
/// # Safety
///
/// `dev` and `vblanktime` must be valid pointers and `pipe < dev->num_crtcs`.
unsafe fn drm_vblank_count_and_time(dev: *mut DrmDevice, pipe: u32, vblanktime: *mut i64) -> u64 {
    // SAFETY: Reads software vblank count and timestamp inside a seqlock retry loop until consistent.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut vblank_count: i64;

        if warn_on(pipe >= (*dev).num_crtcs) {
            *vblanktime = 0;
            return 0;
        }

        loop {
            let seq = read_seqbegin(core::ptr::addr_of!((*vblank).seqlock));
            vblank_count = atomic64_read(core::ptr::addr_of!((*vblank).count));
            *vblanktime = (*vblank).time;
            if !read_seqretry(core::ptr::addr_of!((*vblank).seqlock), seq) {
                break;
            }
        }
        vblank_count as u64
    }
}

/// Count and timestamp of a CRTC read consistently.
// upstream: drm_vblank.c drm_crtc_vblank_count_and_time()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` whose `dev` points to a live `DrmDevice`.
/// `vblanktime` must be a valid, aligned, non-null pointer to a writable `i64` destination.
pub unsafe extern "C" fn drm_crtc_vblank_count_and_time(
    crtc: *mut DrmCrtc,
    vblanktime: *mut i64,
) -> u64 {
    // SAFETY: `crtc`, `crtc->dev`, and destination pointer `vblanktime` are valid; reads count and timestamp under seqlock for the CRTC's pipe.
    unsafe { drm_vblank_count_and_time((*crtc).dev, drm_crtc_index(crtc), vblanktime) }
}

/// Start time of the next vblank interval (the vblank start line).
// upstream: drm_vblank.c drm_crtc_next_vblank_start()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// `vblanktime` must point to a valid, aligned, writable `i64` location.
/// Returns `-EINVAL` if `crtc->dev` lacks vblank support or if frame duration has not been calculated.
pub unsafe extern "C" fn drm_crtc_next_vblank_start(
    crtc: *mut DrmCrtc,
    vblanktime: *mut i64,
) -> i32 {
    // SAFETY: Computes next vblank start time using accurate vblank count and framedur_ns.
    unsafe {
        if !drm_dev_has_vblank((*crtc).dev) {
            return -EINVAL;
        }

        let vblank = drm_crtc_vblank_crtc(crtc);
        let mode = core::ptr::addr_of!((*vblank).hwmode);

        if (*vblank).framedur_ns == 0 || (*vblank).linedur_ns == 0 {
            return -EINVAL;
        }

        if !drm_crtc_get_last_vbltimestamp(crtc, vblanktime, false) {
            return -EINVAL;
        }

        // DIV_ROUND_DOWN_ULL((u64)framedur_ns * crtc_vblank_start, crtc_vtotal)
        let vblank_start = ((*vblank).framedur_ns as u64 * (*mode).crtc_vblank_start as u64)
            / (*mode).crtc_vtotal as u64;
        *vblanktime += vblank_start as i64; // ktime_add(vblanktime, ns_to_ktime(vblank_start))
        0
    }
}

// upstream: drm_vblank.c __enable_vblank()
/// # Safety
///
/// `dev` must point to a live `DrmDevice`, `pipe < dev->num_crtcs`, and caller must hold `dev->vbl_lock`.
unsafe fn __enable_vblank(dev: *mut DrmDevice, pipe: u32) -> i32 {
    // SAFETY: Resolves CRTC from pipe and invokes `enable_vblank` hook from `crtc->funcs`.
    unsafe {
        if drm_core_check_feature(dev, DRIVER_MODESET) {
            let crtc = drm_crtc_from_index(dev, pipe);
            if warn_on(crtc.is_null()) {
                return 0;
            }
            if let Some(enable_vblank) = (*(*crtc).funcs).enable_vblank {
                return enable_vblank(crtc);
            }
        }
        -EINVAL
    }
}

/// Enables the hardware vblank interrupt for the first reference.
// upstream: drm_vblank.c drm_vblank_enable()
/// # Safety
///
/// `dev` must be valid, `vbl_lock` held, and `pipe < dev->num_crtcs`.
unsafe fn drm_vblank_enable(dev: *mut DrmDevice, pipe: u32) -> i32 {
    // SAFETY: If not enabled, enables hardware vblank via `__enable_vblank` and updates timestamps.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut ret: i32 = 0;

        // assert_spin_locked(&dev->vbl_lock)
        spin_lock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));
        if !(*vblank).enabled {
            // Enable vblank irqs under vblank_time_lock protection. All vblank count &
            // timestamp updates are held off until we are done reinitializing master
            // counter and timestamps. Filtercode in drm_handle_vblank() will prevent
            // double-accounting of same vblank interval.
            ret = __enable_vblank(dev, pipe);
            if ret != 0 {
                atomic_dec(core::ptr::addr_of_mut!((*vblank).refcount));
            } else {
                drm_update_vblank_count(dev, pipe, false);
                // drm_update_vblank_count() includes a wmb so we just need to ensure
                // that the compiler emits the write to mark the vblank as enabled
                // after the call to drm_update_vblank_count().
                core::ptr::write_volatile(core::ptr::addr_of_mut!((*vblank).enabled), true);
            }
        }
        spin_unlock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));
        ret
    }
}

/// Takes a vblank reference, enabling the interrupt on the 0 -> 1 transition.
// upstream: drm_vblank.c drm_vblank_get()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be a valid, non-null pointer to a live `DrmDevice`, and `pipe` must be `< (*dev).num_crtcs`.
/// May be called from process or interrupt context; internally serializes via `dev->vbl_lock`.
pub unsafe extern "C" fn drm_vblank_get(dev: *mut DrmDevice, pipe: u32) -> i32 {
    // SAFETY: Locks `vbl_lock` with irqsave, increments refcount, enables vblank via
    // `drm_vblank_enable` if 0, unlocks.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut irqflags: core::ffi::c_ulong = 0;
        let mut ret: i32 = 0;

        if !drm_dev_has_vblank(dev) {
            return -EINVAL;
        }
        if warn_on(pipe >= (*dev).num_crtcs) {
            return -EINVAL;
        }

        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).vbl_lock), &mut irqflags);
        // Going from 0->1 means we have to enable interrupts again
        if atomic_add_return(1, core::ptr::addr_of_mut!((*vblank).refcount)) == 1 {
            ret = drm_vblank_enable(dev, pipe);
        } else if !(*vblank).enabled {
            atomic_dec(core::ptr::addr_of_mut!((*vblank).refcount));
            ret = -EINVAL;
        }
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).vbl_lock), irqflags);
        ret
    }
}

/// `drm_crtc_vblank_get()`: vblank reference for a CRTC.
// upstream: drm_vblank.c drm_crtc_vblank_get()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` with an associated `DrmDevice`.
pub unsafe extern "C" fn drm_crtc_vblank_get(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: `crtc` and its `dev` field are valid per caller preconditions; increments vblank refcount for this CRTC's pipe index.
    unsafe { drm_vblank_get((*crtc).dev, drm_crtc_index(crtc)) }
}

/// Drops a vblank reference; the last one schedules the delayed disable.
// upstream: drm_vblank.c drm_vblank_put()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must be a valid, non-null pointer to a live `DrmDevice`, and `pipe` must be `< (*dev).num_crtcs`.
/// Each call must balance a prior successful `drm_vblank_get(dev, pipe)`.
pub unsafe extern "C" fn drm_vblank_put(dev: *mut DrmDevice, pipe: u32) {
    // SAFETY: Locks `vbl_lock`, decrements refcount, arms disable timer if refcount reaches 0.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let vblank_offdelay = (*vblank).config.offdelay_ms;

        if warn_on(pipe >= (*dev).num_crtcs) {
            return;
        }
        if warn_on(atomic_read(core::ptr::addr_of!((*vblank).refcount)) == 0) {
            return;
        }

        // Last user schedules interrupt disable
        if atomic_dec_and_test(core::ptr::addr_of_mut!((*vblank).refcount)) {
            if vblank_offdelay == 0 {
            } else if vblank_offdelay < 0 {
                vblank_disable_fn(core::ptr::addr_of_mut!((*vblank).disable_timer));
            } else if !(*vblank).config.disable_immediate {
                // jiffies + ((vblank_offdelay * HZ) / 1000)
                let delay = (vblank_offdelay as u64 * HZ as u64) / 1000;
                drm_kapi::timer::mod_timer(
                    core::ptr::addr_of_mut!((*vblank).disable_timer),
                    drm_kapi::primitives::jiffies() + delay,
                );
            }
        }
    }
}

/// `drm_crtc_vblank_put()`: drops a CRTC's vblank reference.
// upstream: drm_vblank.c drm_crtc_vblank_put()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// Each call must balance a prior successful `drm_crtc_vblank_get(crtc)`.
pub unsafe extern "C" fn drm_crtc_vblank_put(crtc: *mut DrmCrtc) {
    // SAFETY: `crtc` and its `dev` field are valid per caller preconditions; decrements vblank refcount for this CRTC's pipe index.
    unsafe { drm_vblank_put((*crtc).dev, drm_crtc_index(crtc)) }
}

/// Waits up to one second for the next vblank.
// upstream: drm_vblank.c drm_crtc_wait_one_vblank()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized, active `DrmCrtc`.
/// Must be called from sleepable process context; caller must not hold spinlocks or locks that
/// would prevent the vblank IRQ handler from executing.
pub unsafe extern "C" fn drm_crtc_wait_one_vblank(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: Obtains vblank reference via `drm_crtc_vblank_get`, waits on `queue` until count advances
    // or timeout, releases reference via `drm_crtc_vblank_put`.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);

        let ret = drm_vblank_get(dev, pipe);
        if warn_on(ret != 0) {
            return ret;
        }

        let vblank = drm_crtc_vblank_crtc(crtc);
        let last = drm_vblank_count(dev, pipe);
        let wait = drm_kapi::wait_event_timeout!(
            (*vblank).queue,
            last != drm_vblank_count(dev, pipe),
            drm_kapi::primitives::msecs_to_jiffies(1000u32),
        );

        // drm_WARN(dev, ret == 0, "vblank wait timed out on crtc %i\n", pipe)
        warn_on(wait == 0);
        drm_vblank_put(dev, pipe);

        if wait != 0 { 0 } else { -ETIMEDOUT }
    }
}

/// Flushes the CRTC's vblank worker thread.
// upstream: drm_internal.h drm_vblank_flush_worker()
/// # Safety
///
/// `vblank` must be a valid, live pointer to `DrmVblankCrtc`.
unsafe fn drm_vblank_flush_worker(vblank: *mut DrmVblankCrtc) {
    // SAFETY: If `vblank->worker` is non-null, calls `flush_worker` on the kthread worker.
    unsafe { drm_kapi::kthread::flush_worker((*vblank).worker.cast()) }
}

/// Visits every CRTC in `mode_config.crtc_list`.
// upstream: include/drm/drm_crtc.h drm_for_each_crtc()
/// # Safety
///
/// `dev` must be a valid `DrmDevice` with initialized `mode_config.crtc_list`.
unsafe fn for_each_crtc(dev: *mut DrmDevice, mut f: impl FnMut(*mut DrmCrtc) -> bool) {
    // SAFETY: Iterates through intrusive list `dev->mode_config.crtc_list` recovering enclosing
    // `DrmCrtc` via `offset_of!`.
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

/// Turns vblank processing off for a CRTC, sending any queued events first.
// upstream: drm_vblank.c drm_crtc_vblank_off()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// Caller must hold the modeset locks (e.g. `mode_config.mutex` or commit locks) for `crtc` to prevent
/// concurrent enable/disable state transitions.
pub unsafe extern "C" fn drm_crtc_vblank_off(crtc: *mut DrmCrtc) {
    // SAFETY: Disables vblank on CRTC, cancels timer, flushes pending events, wakes waiters.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);
        let vblank = drm_crtc_vblank_crtc(crtc);
        let mut now: i64 = 0;

        if warn_on(pipe >= (*dev).num_crtcs) {
            return;
        }

        // Grab event_lock early to prevent vblank work from being scheduled while
        // we're in the middle of shutting down vblank interrupts.
        drm_kapi::locks::spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        spin_lock_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));

        // Avoid redundant vblank disables without previous drm_crtc_vblank_on().
        if drm_core_check_feature(dev, DRIVER_ATOMIC) || (*vblank).inmodeset == 0 {
            drm_vblank_disable_and_save(dev, pipe);
        }

        drm_kapi::wait::wake_up(core::ptr::addr_of_mut!((*vblank).queue));

        // Prevent subsequent drm_vblank_get() from re-enabling the vblank interrupt
        // by bumping the refcount.
        if (*vblank).inmodeset == 0 {
            atomic_inc(core::ptr::addr_of_mut!((*vblank).refcount));
            (*vblank).inmodeset = 1;
        }
        spin_unlock_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));

        // Send any queued vblank events, lest the natives grow disquiet.
        let seq = drm_vblank_count_and_time(dev, pipe, &mut now);
        let head: *mut ListHead = core::ptr::addr_of_mut!((*dev).vblank_event_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let e = event_from_link(cur);
            if (*e).pipe == pipe {
                // drm_dbg_core(dev, "Sending premature vblank event on disable ...")
                list_del(core::ptr::addr_of_mut!((*e).base.link));
                drm_vblank_put(dev, pipe);
                send_vblank_event(dev, e, seq, now);
            }
            cur = next;
        }

        // Cancel any leftover pending vblank work.
        crate::drm_vblank_work_upstream::drm_vblank_cancel_pending_works(vblank);

        drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        // Will be reset by the modeset helpers when re-enabling the crtc by calling
        // drm_calc_timestamping_constants().
        (*vblank).hwmode.crtc_clock = 0;

        // Wait for any vblank work that's still executing to finish.
        drm_vblank_flush_worker(vblank);
    }
}

/// Maps a `DrmPendingVblankEvent`'s `base.link` list node back to the event.
///
/// # Safety
///
/// `link` must point to the `base.link` list node within a live, allocated `DrmPendingVblankEvent`.
#[inline]
unsafe fn event_from_link(link: *mut ListHead) -> *mut DrmPendingVblankEvent {
    // SAFETY: Subtracts offsets of `base` and `link` from `link` to recover enclosing `DrmPendingVblankEvent`.
    unsafe {
        (link as *mut u8)
            .sub(offset_of!(DrmPendingVblankEvent, base) + offset_of!(DrmPendingEvent, link))
            as *mut DrmPendingVblankEvent
    }
}

/// Clears the "vblank in modeset" state and drops its reference.
// upstream: drm_vblank.c drm_crtc_vblank_reset()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// Caller must hold the modeset locks for `crtc` (typically called during driver resume or CRTC enable).
pub unsafe extern "C" fn drm_crtc_vblank_reset(crtc: *mut DrmCrtc) {
    // SAFETY: Locks `vbl_lock` with irqsave, resets vblank timestamp and state for `crtc`.
    unsafe {
        let dev = (*crtc).dev;
        let vblank = drm_crtc_vblank_crtc(crtc);

        drm_kapi::locks::spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));

        // Prevent subsequent drm_vblank_get() from enabling the vblank interrupt by
        // bumping the refcount.
        if (*vblank).inmodeset == 0 {
            atomic_inc(core::ptr::addr_of_mut!((*vblank).refcount));
            (*vblank).inmodeset = 1;
        }
        drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));

        warn_on(!list_empty(&(*dev).vblank_event_list));
        warn_on(!list_empty(&(*vblank).pending_work));
    }
}

/// Sets the hardware counter's maximum for a CRTC (once per device).
// upstream: drm_vblank.c drm_crtc_set_max_vblank_count()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` with initialized vblank support.
/// Must be called during driver initialization or under modeset lock before enabling vblank interrupts.
pub unsafe extern "C" fn drm_crtc_set_max_vblank_count(crtc: *mut DrmCrtc, max_vblank_count: u32) {
    // SAFETY: Locks `vbl_lock` with irqsave, updates `vblank->max_vblank_count` and software `last` count.
    unsafe {
        let dev = (*crtc).dev;
        let vblank = drm_crtc_vblank_crtc(crtc);

        warn_on((*dev).max_vblank_count != 0);
        warn_on(core::ptr::read_volatile(core::ptr::addr_of!((*vblank).inmodeset)) == 0);

        (*vblank).max_vblank_count = max_vblank_count;
    }
}

/// Enables vblank processing with an explicit configuration.
// upstream: drm_vblank.c drm_crtc_vblank_on_config()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// `config` may be null (module defaults are used) or must point to a readable `DrmVblankCrtcConfig`.
/// Caller must hold the modeset locks for `crtc`.
pub unsafe extern "C" fn drm_crtc_vblank_on_config(
    crtc: *mut DrmCrtc,
    config: *const DrmVblankCrtcConfig,
) {
    // SAFETY: Re-enables vblank for CRTC, applies config if present, restores vblank events.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);
        let vblank = drm_crtc_vblank_crtc(crtc);

        if warn_on(pipe >= (*dev).num_crtcs) {
            return;
        }

        drm_kapi::locks::spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));

        core::ptr::copy_nonoverlapping(config, core::ptr::addr_of_mut!((*vblank).config), 1);

        // Drop our private "prevent drm_vblank_get" refcount
        if (*vblank).inmodeset != 0 {
            atomic_dec(core::ptr::addr_of_mut!((*vblank).refcount));
            (*vblank).inmodeset = 0;
        }

        drm_reset_vblank_timestamp(dev, pipe);

        // re-enable interrupts if there are users left, or the user wishes vblank
        // interrupts to be enabled all the time.
        if atomic_read(core::ptr::addr_of!((*vblank).refcount)) != 0
            || (*vblank).config.offdelay_ms == 0
        {
            warn_on(drm_vblank_enable(dev, pipe) != 0);
        }

        drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).vbl_lock));
    }
}

/// Enables vblank processing with the module-default off-delay.
// upstream: drm_vblank.c drm_crtc_vblank_on()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// Caller must hold the modeset locks for `crtc`.
pub unsafe extern "C" fn drm_crtc_vblank_on(crtc: *mut DrmCrtc) {
    // SAFETY: Constructs default vblank config and calls `drm_crtc_vblank_on_config`.
    unsafe {
        let config = DrmVblankCrtcConfig {
            offdelay_ms: DRM_VBLANK_OFFDELAY.load(Ordering::Relaxed),
            disable_immediate: (*(*crtc).dev).vblank_disable_immediate,
        };
        drm_crtc_vblank_on_config(crtc, &config);
    }
}

/// Estimates how many vblanks passed while the interrupt was off and
/// rewinds the software `last` count accordingly.
// upstream: drm_vblank.c drm_vblank_restore()
/// # Safety
///
/// `dev` must be a valid pointer to `DrmDevice`, `pipe < dev->num_crtcs`, and `vbl_lock` must be held.
unsafe fn drm_vblank_restore(dev: *mut DrmDevice, pipe: u32) {
    // SAFETY: Estimates elapsed frames while off via timestamp or hw counter, rewinds `last` count.
    unsafe {
        let max_vblank_count = drm_max_vblank_count(dev, pipe);
        let mut t_vblank: i64 = 0;
        let mut cur_vblank: u32 = 0;
        let mut diff: u32 = 1;

        if warn_on(pipe >= (*dev).num_crtcs) {
            return;
        }

        let vblank = drm_vblank_crtc(dev, pipe);
        let framedur_ns = (*vblank).framedur_ns;

        get_vblank_counter_and_timestamp(dev, pipe, &mut cur_vblank, &mut t_vblank, false);

        let diff_ns = (t_vblank - (*vblank).time) as u64;
        if framedur_ns != 0 {
            diff = drm_kapi::primitives::div_round_closest_ull(diff_ns, framedur_ns as u64) as u32;
        }

        (*vblank).last = cur_vblank.wrapping_sub(diff) & max_vblank_count;
    }
}

/// Restores the software vblank state after a hardware-only power cycle.
// upstream: drm_vblank.c drm_crtc_vblank_restore()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// Hardware must have returned to an active state; caller must hold modeset synchronization for `crtc`.
pub unsafe extern "C" fn drm_crtc_vblank_restore(crtc: *mut DrmCrtc) {
    // SAFETY: Acquires `vbl_lock` with irqsave, calls `drm_vblank_restore` to restore sequence count.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);
        let vblank = drm_vblank_crtc(dev, pipe);

        warn_on((*(*crtc).funcs).get_vblank_timestamp.is_none());
        warn_on((*vblank).inmodeset != 0);
        warn_on(!(*vblank).config.disable_immediate);

        drm_vblank_restore(dev, pipe);
    }
}

/// Queues a vblank event for a WAIT_VBLANK request, or sends it immediately
/// when the sequence has already passed.
// upstream: drm_vblank.c drm_queue_vblank_event()
/// # Safety
///
/// `dev`, `vblwait`, and `file_priv` must be valid, live pointers, and `pipe < dev->num_crtcs`.
unsafe fn drm_queue_vblank_event(
    dev: *mut DrmDevice,
    pipe: u32,
    req_seq: u64,
    vblwait: *mut DrmWaitVblank,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Allocates `DrmPendingVblankEvent`, links into `dev->vblank_event_list` under `event_lock`.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut now: i64 = 0;

        let e = drm_kapi::memory::kzalloc(size_of::<DrmPendingVblankEvent>(), GFP_KERNEL)
            as *mut DrmPendingVblankEvent;
        if e.is_null() {
            drm_vblank_put(dev, pipe);
            return -ENOMEM;
        }

        (*e).pipe = pipe;
        (*ev_base(e)).r#type = DRM_EVENT_VBLANK;
        (*ev_base(e)).length = size_of::<DrmEventVblank>() as u32;
        (*ev_vbl(e)).user_data = (*wv_req(vblwait)).signal;
        (*ev_vbl(e)).crtc_id = 0;

        if drm_core_check_feature(dev, DRIVER_MODESET) {
            let crtc = drm_crtc_from_index(dev, pipe);
            if !crtc.is_null() {
                (*ev_vbl(e)).crtc_id = (*crtc).base.id;
            }
        }

        drm_kapi::locks::spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        // drm_crtc_vblank_off() might have been called after we called
        // drm_vblank_get(). drm_crtc_vblank_off() holds event_lock around the vblank
        // disable, so no need for further locking. The reference from
        // drm_vblank_get() protects against vblank disable from another source.
        if !core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled)) {
            drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            drm_kapi::memory::kfree(e);
            drm_vblank_put(dev, pipe);
            return -EINVAL;
        }

        let ret = drm_event_reserve_init_locked(
            dev,
            file_priv,
            core::ptr::addr_of_mut!((*e).base),
            core::ptr::addr_of_mut!((*e).event.base) as *mut DrmEvent,
        );
        if ret != 0 {
            drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            drm_kapi::memory::kfree(e);
            drm_vblank_put(dev, pipe);
            return ret;
        }

        let seq = drm_vblank_count_and_time(dev, pipe, &mut now);
        // drm_dbg_core(...) and trace_drm_vblank_event_queued(): dropped

        (*e).sequence = req_seq;

        if drm_vblank_passed(seq, req_seq) {
            drm_vblank_put(dev, pipe);
            send_vblank_event(dev, e, seq, now);
            (*wv_reply(vblwait)).sequence = seq as u32;
        } else {
            // drm_handle_vblank_events will call drm_vblank_put
            list_add_tail(
                core::ptr::addr_of_mut!((*e).base.link),
                core::ptr::addr_of_mut!((*dev).vblank_event_list),
            );
            (*wv_reply(vblwait)).sequence = req_seq as u32;
        }

        drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        let _ = vblank;
        0
    }
}

/// True for a pure query: relative zero-sequence request without event or
/// NEXTONMISS.
// upstream: drm_vblank.c drm_wait_vblank_is_query()
/// # Safety
///
/// `vblwait` must point to an initialized `DrmWaitVblank`.
unsafe fn drm_wait_vblank_is_query(vblwait: *mut DrmWaitVblank) -> bool {
    // SAFETY: Reads request fields from union via `wv_req`.
    unsafe {
        if (*wv_req(vblwait)).sequence != 0 {
            return false;
        }
        _DRM_VBLANK_RELATIVE
            == ((*wv_req(vblwait)).r#type
                & (_DRM_VBLANK_TYPES_MASK | _DRM_VBLANK_EVENT | _DRM_VBLANK_NEXTONMISS))
    }
}

/// Widens a 32-bit hardware sequence to 64 bits relative to `near`.
// upstream: drm_vblank.c widen_32_to_64()
#[inline]
fn widen_32_to_64(narrow: u32, near: u64) -> u64 {
    near.wrapping_add((narrow.wrapping_sub(near as u32)) as i32 as i64 as u64)
}

/// Fills the WAIT_VBLANK reply with the current count and timestamp.
// upstream: drm_vblank.c drm_wait_vblank_reply()
/// # Safety
///
/// `dev` and `reply` must be valid pointers and `pipe < dev->num_crtcs`.
unsafe fn drm_wait_vblank_reply(dev: *mut DrmDevice, pipe: u32, reply: *mut DrmWaitVblankReply) {
    // SAFETY: Populates `reply` with current sequence and timestamp.
    unsafe {
        let mut now: i64 = 0;
        (*reply).sequence = drm_vblank_count_and_time(dev, pipe, &mut now) as u32;
        let tv_sec = now / 1_000_000_000;
        let tv_nsec = now % 1_000_000_000;
        (*reply).tval_sec = tv_sec as u32 as i64;
        (*reply).tval_usec = tv_nsec / 1000;
    }
}

/// WAIT_VBLANK is supported whenever the device has vblank CRTCs.
// upstream: drm_vblank.c drm_wait_vblank_supported()
/// # Safety
///
/// `dev` must be a valid pointer to `DrmDevice`.
unsafe fn drm_wait_vblank_supported(dev: *const DrmDevice) -> bool {
    // SAFETY: `dev` points to a readable `DrmDevice`; delegates to `drm_dev_has_vblank` to check `dev->num_crtcs != 0`.
    unsafe { drm_dev_has_vblank(dev) }
}

/// Legacy WAIT_VBLANK ioctl: blocks until a sequence, or queues an event.
// upstream: drm_vblank.c drm_wait_vblank_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to a live, registered `DrmDevice`.
/// `data` must point to an aligned, readable and writable `DrmWaitVblank` ioctl buffer.
/// `file_priv` must point to a live `DrmFile` structure corresponding to the calling process.
pub unsafe extern "C" fn drm_wait_vblank_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Validates ioctl arguments, performs vblank wait/query/event queuing.
    unsafe {
        let vblwait = data as *mut DrmWaitVblank;

        if !drm_wait_vblank_supported(dev) {
            return -EOPNOTSUPP;
        }

        if (*wv_req(vblwait)).r#type & _DRM_VBLANK_SIGNAL != 0 {
            return -EINVAL;
        }

        if (*wv_req(vblwait)).r#type
            & !(_DRM_VBLANK_TYPES_MASK | _DRM_VBLANK_FLAGS_MASK | _DRM_VBLANK_HIGH_CRTC_MASK)
            != 0
        {
            // drm_dbg_core(...): dropped
            return -EINVAL;
        }

        let flags = (*wv_req(vblwait)).r#type & _DRM_VBLANK_FLAGS_MASK;
        let high_pipe = (*wv_req(vblwait)).r#type & _DRM_VBLANK_HIGH_CRTC_MASK;
        let pipe_index: u32 = if high_pipe != 0 {
            high_pipe >> _DRM_VBLANK_HIGH_CRTC_SHIFT
        } else if flags & _DRM_VBLANK_SECONDARY != 0 {
            1
        } else {
            0
        };

        // Convert lease-relative crtc index into global crtc index
        let pipe: u32 = if drm_core_check_feature(dev, DRIVER_MODESET) {
            let mut pipe_out = 0u32;
            let mut pipe_index_local = pipe_index;
            for_each_crtc(dev, |crtc| {
                if drm_lease_held(file_priv, (*crtc).base.id as i32) {
                    if pipe_index_local == 0 {
                        return false;
                    }
                    pipe_index_local -= 1;
                }
                pipe_out += 1;
                true
            });
            pipe_out
        } else {
            pipe_index
        };

        if pipe >= (*dev).num_crtcs {
            return -EINVAL;
        }

        let vblank = drm_vblank_crtc(dev, pipe);

        // If the counter is currently enabled and accurate, short-circuit queries
        // to return the cached timestamp of the last vblank.
        if (*vblank).config.disable_immediate
            && drm_wait_vblank_is_query(vblwait)
            && core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled))
        {
            drm_wait_vblank_reply(
                dev,
                pipe,
                core::ptr::addr_of_mut!((*vblwait).reply) as *mut DrmWaitVblankReply,
            );
            return 0;
        }

        let mut ret = drm_vblank_get(dev, pipe);
        if ret != 0 {
            // drm_dbg_core(...): dropped
            return ret;
        }

        let seq = drm_vblank_count(dev, pipe);
        let req_seq: u64;
        'done: {
            match (*wv_req(vblwait)).r#type & _DRM_VBLANK_TYPES_MASK {
                _DRM_VBLANK_RELATIVE => {
                    req_seq = seq + (*wv_req(vblwait)).sequence as u64;
                    (*wv_req(vblwait)).sequence = req_seq as u32;
                    (*wv_req(vblwait)).r#type &= !_DRM_VBLANK_RELATIVE;
                }
                _DRM_VBLANK_ABSOLUTE => {
                    req_seq = widen_32_to_64((*wv_req(vblwait)).sequence, seq);
                }
                _ => {
                    ret = -EINVAL;
                    break 'done;
                }
            }

            let mut req_seq = req_seq;
            if flags & _DRM_VBLANK_NEXTONMISS != 0 && drm_vblank_passed(seq, req_seq) {
                req_seq = seq + 1;
                (*wv_req(vblwait)).r#type &= !_DRM_VBLANK_NEXTONMISS;
                (*wv_req(vblwait)).sequence = req_seq as u32;
            }

            if flags & _DRM_VBLANK_EVENT != 0 {
                // must hold on to the vblank ref until the event fires; drm_vblank_put
                // will be called asynchronously
                return drm_queue_vblank_event(dev, pipe, req_seq, vblwait, file_priv);
            }

            if req_seq != seq {
                // drm_dbg_core(...): dropped
                let wait = drm_kapi::wait_event_interruptible_timeout!(
                    (*vblank).queue,
                    drm_vblank_passed(drm_vblank_count(dev, pipe), req_seq)
                        || !core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled)),
                    drm_kapi::primitives::msecs_to_jiffies(3000u32),
                );
                match wait {
                    0 => ret = -EBUSY,
                    w if w == -(drm_kapi::config::ERESTARTSYS as i64) => ret = -EINTR,
                    w if w < 0 => ret = -EINTR,
                    _ => ret = 0,
                }
            }

            if ret != -EINTR {
                drm_wait_vblank_reply(
                    dev,
                    pipe,
                    core::ptr::addr_of_mut!((*vblwait).reply) as *mut DrmWaitVblankReply,
                );
                // drm_dbg_core(...): dropped
            }
        }

        // done:
        drm_vblank_put(dev, pipe);
        ret
    }
}

/// Delivers the events of `pipe` whose sequence has passed.
// upstream: drm_vblank.c drm_handle_vblank_events()
/// # Safety
///
/// `dev` must be valid, `pipe < dev->num_crtcs`, and caller must hold `event_lock`.
unsafe fn drm_handle_vblank_events(dev: *mut DrmDevice, pipe: u32) {
    // SAFETY: Traverses `dev->vblank_event_list` under `event_lock` and dispatches expired events.
    unsafe {
        let crtc = drm_crtc_from_index(dev, pipe);
        let mut now: i64 = 0;

        // assert_spin_locked(&dev->event_lock)
        let seq = drm_vblank_count_and_time(dev, pipe, &mut now);

        let head: *mut ListHead = core::ptr::addr_of_mut!((*dev).vblank_event_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let e = event_from_link(cur);
            if (*e).pipe == pipe && drm_vblank_passed(seq, (*e).sequence) {
                // drm_dbg_core(...): dropped
                list_del(core::ptr::addr_of_mut!((*e).base.link));
                drm_vblank_put(dev, pipe);
                send_vblank_event(dev, e, seq, now);
            }
            cur = next;
        }

        // high_prec tracing is dropped
        let _high_prec = !crtc.is_null() && (*(*crtc).funcs).get_vblank_timestamp.is_some();
    }
}

/// Handles one vblank interrupt for `pipe`: updates the count and timestamp,
/// wakes waiters, and delivers events and vblank work.
// upstream: drm_vblank.c drm_handle_vblank()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to a live `DrmDevice`, and `pipe` must be `< (*dev).num_crtcs`.
/// Typically invoked from the driver's hardware vblank interrupt service routine.
pub unsafe extern "C" fn drm_handle_vblank(dev: *mut DrmDevice, pipe: u32) -> bool {
    // SAFETY: Main vblank IRQ handler: updates count, wakes waitqueue, handles events and pending work.
    unsafe {
        let vblank = drm_vblank_crtc(dev, pipe);
        let mut irqflags: core::ffi::c_ulong = 0;

        if warn_on(!drm_dev_has_vblank(dev)) {
            return false;
        }
        if warn_on(pipe >= (*dev).num_crtcs) {
            return false;
        }

        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*dev).event_lock), &mut irqflags);

        // Need timestamp lock to prevent concurrent execution with vblank enable/disable,
        // as this would cause inconsistent or corrupted timestamps and vblank counts.
        spin_lock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));

        // Vblank irq handling disabled. Nothing to do.
        if !core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled)) {
            spin_unlock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));
            spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), irqflags);
            return false;
        }

        drm_update_vblank_count(dev, pipe, true);
        spin_unlock_raw(core::ptr::addr_of_mut!((*dev).vblank_time_lock));

        drm_kapi::wait::wake_up(core::ptr::addr_of_mut!((*vblank).queue));

        // With instant-off, we defer disabling the interrupt until after we finish
        // processing the following vblank after all events have been signaled. The
        // disable has to be last (after drm_handle_vblank_events) so that the
        // timestamp is always accurate.
        let disable_irq = (*vblank).config.disable_immediate
            && (*vblank).config.offdelay_ms > 0
            && atomic_read(core::ptr::addr_of!((*vblank).refcount)) == 0;

        drm_handle_vblank_events(dev, pipe);
        crate::drm_vblank_work_upstream::drm_handle_vblank_works(vblank);

        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*dev).event_lock), irqflags);

        if disable_irq {
            vblank_disable_fn(core::ptr::addr_of_mut!((*vblank).disable_timer));
        }
        true
    }
}

/// CRTC form of [`drm_handle_vblank`]; called from the driver's vblank IRQ.
// upstream: drm_vblank.c drm_crtc_handle_vblank()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// Typically called from the driver's hardware vblank interrupt handler.
pub unsafe extern "C" fn drm_crtc_handle_vblank(crtc: *mut DrmCrtc) -> bool {
    // SAFETY: `crtc` and `crtc->dev` are valid; dispatches vblank interrupt processing for the CRTC's pipe index.
    unsafe { drm_handle_vblank((*crtc).dev, drm_crtc_index(crtc)) }
}

/// Sends a vblank or flip-complete event with the given count and timestamp.
// upstream: drm_vblank.c send_vblank_event()
/// # Safety
///
/// `dev` and `e` must be valid pointers; caller must hold `dev->event_lock`.
unsafe fn send_vblank_event(
    dev: *mut DrmDevice,
    e: *mut DrmPendingVblankEvent,
    seq: u64,
    now: i64,
) {
    // SAFETY: Fills event sequence and timestamp fields, delivers via `drm_send_event_timestamp_locked`.
    unsafe {
        match (*ev_base(e)).r#type {
            DRM_EVENT_VBLANK | DRM_EVENT_FLIP_COMPLETE => {
                let tv_sec = now / 1_000_000_000;
                let tv_nsec = now % 1_000_000_000;
                (*ev_vbl(e)).sequence = seq as u32;
                // e->event is a user space structure, with hardcoded unsigned 32-bit
                // seconds/microseconds. This is safe as we always use monotonic
                // timestamps since linux-4.15.
                (*ev_vbl(e)).tv_sec = tv_sec as u32;
                (*ev_vbl(e)).tv_usec = (tv_nsec / 1000) as u32;
            }
            DRM_EVENT_CRTC_SEQUENCE => {
                if seq != 0 {
                    (*ev_seq(e)).sequence = seq;
                }
                (*ev_seq(e)).time_ns = now;
            }
            _ => {}
        }

        // trace_drm_vblank_event_delivered(): dropped
        // Use the same timestamp for any associated fence signal to avoid mismatch
        // in timestamps for vsync & fence events triggered by the same HW event.
        drm_send_event_timestamp_locked(dev, core::ptr::addr_of_mut!((*e).base), now);
    }
}

/// Arms a vblank event for the CRTC's next vblank (`sequence` = current + 1).
// upstream: drm_vblank.c drm_crtc_arm_vblank_event()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// `e` must point to an allocated, initialized `DrmPendingVblankEvent`; caller transfers event ownership
/// to the vblank subsystem. Caller must hold `dev->event_lock` with interrupts disabled.
pub unsafe extern "C" fn drm_crtc_arm_vblank_event(
    crtc: *mut DrmCrtc,
    e: *mut DrmPendingVblankEvent,
) {
    // SAFETY: Arms vblank event under `event_lock` for delivery on next vblank interrupt.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);

        // assert_spin_locked(&dev->event_lock)
        (*e).pipe = pipe;
        (*e).sequence = drm_crtc_accurate_vblank_count(crtc) + 1;
        list_add_tail(
            core::ptr::addr_of_mut!((*e).base.link),
            core::ptr::addr_of_mut!((*dev).vblank_event_list),
        );
    }
}

/// Sends a vblank event immediately with the current count and timestamp.
// upstream: drm_vblank.c drm_crtc_send_vblank_event()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc`.
/// `e` must point to an allocated, initialized `DrmPendingVblankEvent`; caller transfers event ownership
/// to the event delivery path. Caller must hold `dev->event_lock` with interrupts disabled.
pub unsafe extern "C" fn drm_crtc_send_vblank_event(
    crtc: *mut DrmCrtc,
    e: *mut DrmPendingVblankEvent,
) {
    // SAFETY: Sends vblank event immediately under `event_lock`.
    unsafe {
        let dev = (*crtc).dev;
        let pipe = drm_crtc_index(crtc);
        let mut now: i64 = 0;

        let seq = if drm_dev_has_vblank(dev) {
            drm_vblank_count_and_time(dev, pipe, &mut now)
        } else {
            now = drm_kapi::primitives::ktime_get();
            0
        };

        (*e).pipe = pipe;
        send_vblank_event(dev, e, seq, now);
    }
}

/// CRTC_GET_SEQUENCE ioctl: current count, timestamp and active state.
// upstream: drm_vblank.c drm_crtc_get_sequence_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to a live, registered `DrmDevice`.
/// `data` must point to an aligned, readable and writable `DrmCrtcGetSequence` ioctl argument buffer.
/// `file_priv` must point to the calling process's live `DrmFile`.
pub unsafe extern "C" fn drm_crtc_get_sequence_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Validates CRTC and retrieves current sequence and timestamp into `get_seq`.
    unsafe {
        let get_seq = data as *mut DrmCrtcGetSequence;
        let mut now: i64 = 0;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }
        if !drm_dev_has_vblank(dev) {
            return -EOPNOTSUPP;
        }

        let crtc = crate::drm_crtc_upstream::drm_crtc_find(dev, file_priv, (*get_seq).crtc_id);
        if crtc.is_null() {
            return -ENOENT;
        }

        let pipe = drm_crtc_index(crtc);
        let vblank = drm_crtc_vblank_crtc(crtc);
        let vblank_enabled =
            core::ptr::read_volatile(core::ptr::addr_of!((*vblank).config.disable_immediate))
                && core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled));

        if !vblank_enabled {
            let ret = drm_crtc_vblank_get(crtc);
            if ret != 0 {
                // drm_dbg_core(...): dropped
                return ret;
            }
        }

        drm_modeset_lock(
            core::ptr::addr_of_mut!((*crtc).mutex),
            core::ptr::null_mut(),
        );
        (*get_seq).active = if !(*crtc).state.is_null() {
            (*(*crtc).state).enable as u32
        } else {
            (*crtc).enabled as u32
        };
        drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));

        (*get_seq).sequence = drm_vblank_count_and_time(dev, pipe, &mut now);
        (*get_seq).sequence_ns = now;

        if !vblank_enabled {
            drm_crtc_vblank_put(crtc);
        }
        0
    }
}

/// CRTC_QUEUE_SEQUENCE ioctl: queues a CRTC sequence event for a future count.
// upstream: drm_vblank.c drm_crtc_queue_sequence_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to a live, registered `DrmDevice`.
/// `data` must point to an aligned, readable and writable `DrmCrtcQueueSequence` ioctl argument buffer.
/// `file_priv` must point to the calling process's live `DrmFile`.
pub unsafe extern "C" fn drm_crtc_queue_sequence_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Validates parameters, allocates event, and queues sequence wait on `dev->vblank_event_list`.
    unsafe {
        let queue_seq = data as *mut DrmCrtcQueueSequence;
        let mut now: i64 = 0;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }
        if !drm_dev_has_vblank(dev) {
            return -EOPNOTSUPP;
        }

        let crtc = crate::drm_crtc_upstream::drm_crtc_find(dev, file_priv, (*queue_seq).crtc_id);
        if crtc.is_null() {
            return -ENOENT;
        }

        let flags = (*queue_seq).flags;

        // Check valid flag bits
        if flags & !(DRM_CRTC_SEQUENCE_RELATIVE | DRM_CRTC_SEQUENCE_NEXT_ON_MISS) != 0 {
            return -EINVAL;
        }

        let pipe = drm_crtc_index(crtc);
        let vblank = drm_crtc_vblank_crtc(crtc);

        let e = drm_kapi::memory::kzalloc(size_of::<DrmPendingVblankEvent>(), GFP_KERNEL)
            as *mut DrmPendingVblankEvent;
        if e.is_null() {
            return -ENOMEM;
        }

        let mut ret = drm_crtc_vblank_get(crtc);
        if ret != 0 {
            // drm_dbg_core(...): dropped
            drm_kapi::memory::kfree(e);
            return ret;
        }

        let seq = drm_vblank_count_and_time(dev, pipe, &mut now);
        let mut req_seq = (*queue_seq).sequence;

        if flags & DRM_CRTC_SEQUENCE_RELATIVE != 0 {
            req_seq += seq;
        }

        if flags & DRM_CRTC_SEQUENCE_NEXT_ON_MISS != 0 && drm_vblank_passed(seq, req_seq) {
            req_seq = seq + 1;
        }

        (*e).pipe = pipe;
        (*ev_base(e)).r#type = DRM_EVENT_CRTC_SEQUENCE;
        (*ev_base(e)).length = size_of::<DrmEventCrtcSequence>() as u32;
        (*ev_seq(e)).user_data = (*queue_seq).user_data;

        drm_kapi::locks::spin_lock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));

        // drm_crtc_vblank_off() might have been called after we called
        // drm_crtc_vblank_get(). drm_crtc_vblank_off() holds event_lock around the
        // vblank disable, so no need for further locking. The reference from
        // drm_crtc_vblank_get() protects against vblank disable from another source.
        if !core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled)) {
            ret = -EINVAL;
            drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            drm_crtc_vblank_put(crtc);
            drm_kapi::memory::kfree(e);
            return ret;
        }

        ret = drm_event_reserve_init_locked(
            dev,
            file_priv,
            core::ptr::addr_of_mut!((*e).base),
            core::ptr::addr_of_mut!((*e).event.base) as *mut DrmEvent,
        );
        if ret != 0 {
            drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            drm_crtc_vblank_put(crtc);
            drm_kapi::memory::kfree(e);
            return ret;
        }

        (*e).sequence = req_seq;

        if drm_vblank_passed(seq, req_seq) {
            drm_crtc_vblank_put(crtc);
            send_vblank_event(dev, e, seq, now);
            (*queue_seq).sequence = seq;
        } else {
            // drm_handle_vblank_events will call drm_vblank_put
            list_add_tail(
                core::ptr::addr_of_mut!((*e).base.link),
                core::ptr::addr_of_mut!((*dev).vblank_event_list),
            );
            (*queue_seq).sequence = req_seq;
        }

        drm_kapi::locks::spin_unlock_irq_raw(core::ptr::addr_of_mut!((*dev).event_lock));
        0
    }
}

/// Hrtimer callback that emulates vblank interrupts for drivers without one.
// upstream: drm_vblank.c drm_vblank_timer_function()
/// # Safety
///
/// `timer` must point to the `timer` field of `DrmVblankCrtcTimer` embedded in `DrmVblankCrtc`.
unsafe extern "C" fn drm_vblank_timer_function(timer: *mut Hrtimer) -> i32 {
    // SAFETY: Casts `timer` to enclosing `DrmVblankCrtcTimer`, reads `crtc`, and calls `drm_handle_vblank`.
    unsafe {
        let vtimer = timer as *mut DrmVblankCrtcTimer; // timer is the first member
        let crtc = (*vtimer).crtc;
        let crtc_funcs: *mut DrmCrtcHelperFuncs = (*crtc).helper_private;
        let mut irqflags: core::ffi::c_ulong = 0;

        spin_lock_irqsave_raw(
            core::ptr::addr_of_mut!((*vtimer).interval_lock),
            &mut irqflags,
        );
        let interval = (*vtimer).interval;
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*vtimer).interval_lock), irqflags);

        if interval == 0 {
            return HRTIMER_NORESTART;
        }

        let ret_overrun = drm_kapi::timer::hrtimer_forward_now(timer, interval);
        if ret_overrun != 1 {
            // drm_dbg_vbl(dev, "vblank timer overrun\n"): dropped
        }

        let succ = if let Some(handle_vblank_timeout) = (*crtc_funcs).handle_vblank_timeout {
            handle_vblank_timeout(crtc)
        } else {
            drm_crtc_handle_vblank(crtc)
        };

        if !succ {
            return HRTIMER_NORESTART;
        }
        HRTIMER_RESTART
    }
}

/// Starts the emulated vblank hrtimer for a CRTC at its frame duration.
// upstream: drm_vblank.c drm_crtc_vblank_start_timer()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` with calculated frame duration
/// (`framedur_ns > 0`). Must not be called concurrently with timer destruction.
pub unsafe extern "C" fn drm_crtc_vblank_start_timer(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: Initializes and starts fake vblank hrtimer for `crtc`.
    unsafe {
        let vblank = drm_crtc_vblank_crtc(crtc);
        let vtimer = core::ptr::addr_of_mut!((*vblank).vblank_timer);
        let mut flags: core::ffi::c_ulong = 0;

        if (*vtimer).crtc.is_null() {
            // Set up the data structures on the first invocation.
            (*vtimer).crtc = crtc;
            spin_lock_init_ptr(core::ptr::addr_of_mut!((*vtimer).interval_lock));
            drm_kapi::timer::hrtimer_setup(
                core::ptr::addr_of_mut!((*vtimer).timer),
                Some(drm_vblank_timer_function),
                CLOCK_MONOTONIC,
                HRTIMER_MODE_REL,
            );
        } else {
            // Timer should not be active. If it is, wait for the previous cancel
            // operations to finish.
            while drm_kapi::timer::hrtimer_active(core::ptr::addr_of!((*vtimer).timer)) {
                drm_kapi::timer::hrtimer_try_to_cancel(core::ptr::addr_of_mut!((*vtimer).timer));
            }
        }

        drm_calc_timestamping_constants(crtc, core::ptr::addr_of!((*crtc).mode));

        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*vtimer).interval_lock), &mut flags);
        (*vtimer).interval = (*vblank).framedur_ns as i64; // ns_to_ktime(framedur_ns)
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*vtimer).interval_lock), flags);

        drm_kapi::timer::hrtimer_start_range_ns(
            core::ptr::addr_of_mut!((*vtimer).timer),
            (*vtimer).interval,
            0,
            HRTIMER_MODE_REL,
        );
        0
    }
}

/// Stops the emulated vblank hrtimer without waiting for a running callback.
// upstream: drm_vblank.c drm_crtc_vblank_cancel_timer()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` with an emulated vblank timer.
pub unsafe extern "C" fn drm_crtc_vblank_cancel_timer(crtc: *mut DrmCrtc) {
    // SAFETY: Clears timer interval under `vtimer->interval_lock` and attempts hrtimer cancellation.
    unsafe {
        let vblank = drm_crtc_vblank_crtc(crtc);
        let vtimer = core::ptr::addr_of_mut!((*vblank).vblank_timer);
        let mut flags: core::ffi::c_ulong = 0;

        // Calling hrtimer_cancel() can result in a deadlock with DRM's
        // vblank_time_lime_lock and hrtimers' softirq_expiry_lock. So clear interval
        // and indicate cancellation. The timer function will cancel itself on the
        // next invocation.
        spin_lock_irqsave_raw(core::ptr::addr_of_mut!((*vtimer).interval_lock), &mut flags);
        (*vtimer).interval = 0;
        spin_unlock_irqrestore_raw(core::ptr::addr_of_mut!((*vtimer).interval_lock), flags);

        drm_kapi::timer::hrtimer_try_to_cancel(core::ptr::addr_of_mut!((*vtimer).timer));
    }
}

/// Time of the next vblank, taken from the emulated timer's expiry rolled back
/// by one frame.
// upstream: drm_vblank.c drm_crtc_vblank_get_vblank_timeout()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be a valid, non-null pointer to an initialized `DrmCrtc` using the emulated vblank timer.
/// `vblank_time` must be a valid, aligned, writable pointer to an `i64` destination.
pub unsafe extern "C" fn drm_crtc_vblank_get_vblank_timeout(
    crtc: *mut DrmCrtc,
    vblank_time: *mut i64,
) {
    // SAFETY: Reads timer expiry from `vtimer->timer` and writes adjusted vblank time to `*vblank_time`.
    unsafe {
        let vblank = drm_crtc_vblank_crtc(crtc);
        let vtimer = core::ptr::addr_of_mut!((*vblank).vblank_timer);
        let mut cur_time: i64 = 0;

        if !core::ptr::read_volatile(core::ptr::addr_of!((*vblank).enabled)) {
            *vblank_time = drm_kapi::primitives::ktime_get();
            return;
        }

        // A concurrent vblank timeout could update the expires field before we
        // compare it with the vblank time. Hence we'd compare the old expiry time to
        // the new vblank time; deducing the timer had already expired. Reread until
        // we get consistent values from both fields.
        loop {
            let cur_count = drm_crtc_vblank_count_and_time(crtc, &mut cur_time);
            *vblank_time =
                drm_kapi::timer::hrtimer_get_expires(core::ptr::addr_of!((*vtimer).timer));
            if cur_count == drm_crtc_vblank_count_and_time(crtc, &mut cur_time) {
                break;
            }
        }

        if warn_on(*vblank_time == cur_time) {
            return; // Already expired
        }

        // To prevent races we roll the hrtimer forward before we do any interrupt
        // processing - this is how real hw works (the interrupt is only generated
        // after all the vblank registers are updated) and what the vblank core
        // expects. Therefore we need to always correct the timestamp by one frame.
        *vblank_time -= (*vtimer).interval;
    }
}

#[cfg(test)]
mod tests {
    use core::mem::zeroed;
    use std::{
        boxed::Box,
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        vec::Vec,
    };

    use drm_kapi::{list::init_list_head, wait::init_waitqueue_head};

    use super::*;
    use crate::{
        drm_crtc_types::DrmCrtcFuncs,
        drm_vblank_work_types::DrmVblankWork,
        drm_vblank_work_upstream::{drm_vblank_work_init, drm_vblank_work_schedule},
        kernel_placeholder_types::KthreadWork,
        test_support::TestDrm,
    };

    /// Per-fixture fake hardware state, reached through `DrmDevice::dev_private`
    /// so host tests do not share a process-global counter or call log.
    struct TestHwState {
        counter: AtomicU32,
        enable_calls: AtomicU32,
        disable_calls: AtomicU32,
    }

    impl TestHwState {
        fn new() -> Self {
            Self {
                counter: AtomicU32::new(0),
                enable_calls: AtomicU32::new(0),
                disable_calls: AtomicU32::new(0),
            }
        }
    }

    /// # Safety
    ///
    /// `crtc` must point to a fixture CRTC whose device stores the live `TestHwState` in `dev_private`.
    unsafe extern "C" fn fake_get_vblank_counter(crtc: *mut DrmCrtc) -> u32 {
        // SAFETY: Recovers `TestHwState` from fixture `crtc->dev->dev_private` and reads simulated hardware `counter`.
        unsafe {
            let state = (*(*crtc).dev).dev_private.cast::<TestHwState>();
            (*state).counter.load(Ordering::SeqCst)
        }
    }
    /// # Safety
    ///
    /// `crtc` must point to a fixture CRTC whose device stores the live `TestHwState` in `dev_private`.
    unsafe extern "C" fn fake_enable_vblank(crtc: *mut DrmCrtc) -> i32 {
        // SAFETY: Recovers `TestHwState` from fixture `crtc->dev->dev_private` and increments `enable_calls`.
        unsafe {
            let state = (*(*crtc).dev).dev_private.cast::<TestHwState>();
            (*state).enable_calls.fetch_add(1, Ordering::SeqCst);
            0
        }
    }
    /// # Safety
    ///
    /// `crtc` must point to a fixture CRTC whose device stores the live `TestHwState` in `dev_private`.
    unsafe extern "C" fn fake_disable_vblank(crtc: *mut DrmCrtc) {
        // SAFETY: Recovers `TestHwState` from fixture `crtc->dev->dev_private` and increments `disable_calls`.
        unsafe {
            let state = (*(*crtc).dev).dev_private.cast::<TestHwState>();
            (*state).disable_calls.fetch_add(1, Ordering::SeqCst);
        }
    }
    /// # Safety
    ///
    /// Test stub; performs no memory operations.
    unsafe extern "C" fn noop_work(_work: *mut KthreadWork) {}

    /// A device with `n` CRTCs whose vblank state is initialised the way
    /// `drm_vblank_init()` leaves it (minus the worker thread).
    struct Fixture {
        drm: TestDrm,
        crtcs: Vec<*mut DrmCrtc>,
        hw: Arc<TestHwState>,
    }

    impl Fixture {
        fn new(n: u32) -> Self {
            let mut drm = TestDrm::new(DRIVER_MODESET);
            let dev = drm.ptr();
            let hw = Arc::new(TestHwState::new());
            // SAFETY: Initializes test fixture device and CRTC structs with test allocations.
            unsafe {
                (*dev).dev_private = Arc::as_ptr(&hw).cast_mut().cast();
                init_list_head(core::ptr::addr_of_mut!((*dev).mode_config.crtc_list));
                init_list_head(core::ptr::addr_of_mut!((*dev).mode_config.plane_list));
                init_list_head(core::ptr::addr_of_mut!((*dev).vblank_event_list));
                drm_kapi::locks::spin_lock_init(&mut (*dev).vbl_lock);
                drm_kapi::locks::spin_lock_init(&mut (*dev).vblank_time_lock);
                drm_kapi::locks::spin_lock_init(&mut (*dev).event_lock);
                (*dev).num_crtcs = n;
                (*dev).mode_config.num_crtc = n as i32;

                let vblanks: Vec<DrmVblankCrtc> = (0..n).map(|_| zeroed()).collect();
                let vblanks: &'static mut [DrmVblankCrtc] = Box::leak(vblanks.into_boxed_slice());
                (*dev).vblank = vblanks.as_mut_ptr();
                for i in 0..n {
                    let v = (*dev).vblank.add(i as usize);
                    (*v).dev = dev;
                    (*v).pipe = i;
                    (*v).config.offdelay_ms = -1; // immediate disable
                    init_waitqueue_head(&mut (*v).queue);
                    init_list_head(core::ptr::addr_of_mut!((*v).pending_work));
                    init_waitqueue_head(&mut (*v).work_wait_queue);
                    drm_kapi::locks::spin_lock_init(&mut (*v).seqlock.lock);
                }

                let funcs: &'static mut DrmCrtcFuncs = Box::leak(Box::new(zeroed()));
                funcs.get_vblank_counter = Some(fake_get_vblank_counter);
                funcs.enable_vblank = Some(fake_enable_vblank);
                funcs.disable_vblank = Some(fake_disable_vblank);

                let mut crtcs = Vec::new();
                for i in 0..n {
                    let crtc: *mut DrmCrtc = Box::into_raw(Box::new(zeroed()));
                    (*crtc).dev = dev;
                    (*crtc).index = i;
                    (*crtc).funcs = funcs as *mut DrmCrtcFuncs;
                    init_list_head(core::ptr::addr_of_mut!((*crtc).head));
                    list_add_tail(
                        core::ptr::addr_of_mut!((*crtc).head),
                        core::ptr::addr_of_mut!((*dev).mode_config.crtc_list),
                    );
                    crtcs.push(crtc);
                }
                Self { drm, crtcs, hw }
            }
        }

        fn dev(&mut self) -> *mut DrmDevice {
            self.drm.ptr()
        }
        fn vblank(&mut self, pipe: u32) -> *mut DrmVblankCrtc {
            // SAFETY: Accesses `dev->vblank` array for pipe index on the fixture.
            unsafe { (*self.dev()).vblank.add(pipe as usize) }
        }
        fn crtc(&self, pipe: u32) -> *mut DrmCrtc {
            self.crtcs[pipe as usize]
        }
    }

    #[test]
    fn vblank_passed_handles_wraparound() {
        assert!(drm_vblank_passed(5, 3));
        assert!(drm_vblank_passed(5, 5));
        assert!(!drm_vblank_passed(3, 5));
        // Sequences that wrapped past u64::MAX still order correctly.
        assert!(drm_vblank_passed(0, u64::MAX));
        assert!(!drm_vblank_passed(u64::MAX, 0));
        // Anything more than 2^23 behind counts as not passed.
        assert!(!drm_vblank_passed(0, 1 << 23 | 1));
    }

    #[test]
    fn update_vblank_count_wraps_with_max_vblank_count() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Updates `max_vblank_count` and `last` on fixture pipe 0 and verifies counter wraparound calculation.
        unsafe {
            let v = fx.vblank(0);
            (*v).max_vblank_count = 0xFF;
            (*v).last = 0xFE;
            fx.hw.counter.store(0x02, Ordering::SeqCst);
            drm_update_vblank_count(dev, 0, true);
            // (2 - 0xFE) & 0xFF == 4 vblanks passed across the wrap.
            assert_eq!(drm_vblank_count(dev, 0), 4);
            assert_eq!((*v).last, 2);
        }
    }

    #[test]
    fn max_vblank_count_falls_back_to_device() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Mutates `dev->max_vblank_count` directly on fixture device to test fallback path.
        unsafe {
            assert_eq!(drm_max_vblank_count(dev, 0), 0);
            (*dev).max_vblank_count = 0xFFFF;
            assert_eq!(drm_max_vblank_count(dev, 0), 0xFFFF);
            (*fx.vblank(0)).max_vblank_count = 0xFF;
            assert_eq!(drm_max_vblank_count(dev, 0), 0xFF);
        }
    }

    #[test]
    fn crtc_from_index_finds_registered_crtcs() {
        let mut fx = Fixture::new(2);
        let dev = fx.dev();
        // SAFETY: Looks up CRTC pointers via `drm_crtc_from_index` on fixture device with 2 registered CRTCs.
        unsafe {
            assert_eq!(drm_crtc_from_index(dev, 1), fx.crtc(1));
            assert_eq!(drm_crtc_from_index(dev, 0), fx.crtc(0));
            assert!(drm_crtc_from_index(dev, 2).is_null());
        }
    }

    #[test]
    fn get_put_refcount_enables_and_immediately_disables() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Acquires and releases vblank references on fixture pipe 0, verifying refcount transitions.
        unsafe {
            let v = fx.vblank(0);
            assert_eq!(drm_vblank_get(dev, 0), 0);
            assert_eq!(
                (*v).refcount
                    .counter
                    .load(core::sync::atomic::Ordering::Relaxed),
                1
            );
            assert!((*v).enabled);
            assert_eq!(fx.hw.enable_calls.load(Ordering::SeqCst), 1);

            // The second reference does not re-enable the interrupt.
            assert_eq!(drm_vblank_get(dev, 0), 0);
            assert_eq!(
                (*v).refcount
                    .counter
                    .load(core::sync::atomic::Ordering::Relaxed),
                2
            );
            assert_eq!(fx.hw.enable_calls.load(Ordering::SeqCst), 1);

            drm_vblank_put(dev, 0);
            assert_eq!(fx.hw.disable_calls.load(Ordering::SeqCst), 0);
            assert!((*v).enabled);

            // The last put with offdelay < 0 disables at once.
            drm_vblank_put(dev, 0);
            assert_eq!(
                (*v).refcount
                    .counter
                    .load(core::sync::atomic::Ordering::Relaxed),
                0
            );
            assert_eq!(fx.hw.disable_calls.load(Ordering::SeqCst), 1);
            assert!(!(*v).enabled);

            // A new reference turns the interrupt back on.
            assert_eq!(drm_crtc_vblank_get(fx.crtc(0)), 0);
            assert_eq!(fx.hw.enable_calls.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn zero_offdelay_never_disables() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Sets `config.offdelay_ms = 0` on fixture pipe 0 and verifies disable timer is not armed.
        unsafe {
            (*fx.vblank(0)).config.offdelay_ms = 0;
            assert_eq!(drm_vblank_get(dev, 0), 0);
            drm_vblank_put(dev, 0);
            assert_eq!(fx.hw.disable_calls.load(Ordering::SeqCst), 0);
            assert!((*fx.vblank(0)).enabled);
        }
    }

    #[test]
    fn vblank_get_fails_when_disabled_by_driver_hook() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Clears `funcs.enable_vblank` on fixture CRTC 0 to simulate driver hook returning error.
        unsafe {
            // Without an enable hook the CRTC cannot provide vblanks.
            (*(*fx.crtc(0)).funcs).enable_vblank = None;
            assert_eq!(drm_vblank_get(dev, 0), -EINVAL);
            assert_eq!(
                (*fx.vblank(0))
                    .refcount
                    .counter
                    .load(core::sync::atomic::Ordering::Relaxed),
                0
            );
        }
    }

    #[test]
    fn handle_vblank_delivers_passed_events_in_list_order() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Sets `enabled` and `refcount` on fixture pipe 0, queues events, and drives `drm_handle_vblank`.
        unsafe {
            let v = fx.vblank(0);
            (*v).enabled = true;
            (*v).refcount.counter.store(3, Ordering::SeqCst);
            (*v).max_vblank_count = u32::MAX; // trust the hardware counter
            fx.hw.counter.store(4, Ordering::SeqCst);

            let mut file: DrmFile = zeroed();
            file.event_space = 4096;
            init_list_head(core::ptr::addr_of_mut!(file.event_list));
            init_list_head(core::ptr::addr_of_mut!(file.pending_event_list));
            drm_kapi::wait::init_waitqueue_head(&mut file.event_wait);
            let file_priv = &mut file as *mut DrmFile;

            let mk = |seq: u64| -> *mut DrmPendingVblankEvent {
                let e: *mut DrmPendingVblankEvent = Box::into_raw(Box::new(zeroed()));
                (*e).pipe = 0;
                (*e).sequence = seq;
                (*ev_base(e)).r#type = DRM_EVENT_VBLANK;
                (*ev_base(e)).length = core::mem::size_of::<DrmEventVblank>() as u32;
                e
            };
            let a = mk(3);
            let b = mk(9);
            let c = mk(4);
            assert_eq!(
                crate::drm_file_upstream::drm_event_reserve_init(
                    dev,
                    file_priv,
                    core::ptr::addr_of_mut!((*a).base),
                    ev_base(a),
                ),
                0
            );
            assert_eq!(
                crate::drm_file_upstream::drm_event_reserve_init(
                    dev,
                    file_priv,
                    core::ptr::addr_of_mut!((*b).base),
                    ev_base(b),
                ),
                0
            );
            assert_eq!(
                crate::drm_file_upstream::drm_event_reserve_init(
                    dev,
                    file_priv,
                    core::ptr::addr_of_mut!((*c).base),
                    ev_base(c),
                ),
                0
            );
            list_add_tail(
                core::ptr::addr_of_mut!((*a).base.link),
                core::ptr::addr_of_mut!((*dev).vblank_event_list),
            );
            list_add_tail(
                core::ptr::addr_of_mut!((*b).base.link),
                core::ptr::addr_of_mut!((*dev).vblank_event_list),
            );
            list_add_tail(
                core::ptr::addr_of_mut!((*c).base.link),
                core::ptr::addr_of_mut!((*dev).vblank_event_list),
            );

            assert!(drm_handle_vblank(dev, 0));
            assert_eq!(drm_vblank_count(dev, 0), 4);
            // Events whose sequence has passed are sent in list order; the
            // future one stays queued.
            let delivered_a = core::ptr::addr_of_mut!((*file_priv).event_list);
            let delivered_c = (*delivered_a).next;
            assert_eq!(delivered_c, core::ptr::addr_of_mut!((*a).base.link));
            assert_eq!((*delivered_c).next, core::ptr::addr_of_mut!((*c).base.link));
            assert_eq!((*(*delivered_c).next).next, delivered_a);
            assert!(!drm_kapi::list::list_empty(&(*dev).vblank_event_list));
            let next = (*dev).vblank_event_list.next;
            assert_eq!(next as usize, core::ptr::addr_of!((*b).base.link) as usize);

            // Remove all real list links before releasing the fixture storage.
            drm_kapi::list::list_del_init(core::ptr::addr_of_mut!((*a).base.link));
            drm_kapi::list::list_del_init(core::ptr::addr_of_mut!((*c).base.link));
            drm_kapi::list::list_del_init(core::ptr::addr_of_mut!((*b).base.link));
            drm_kapi::list::list_del_init(core::ptr::addr_of_mut!((*b).base.pending_link));
            drop(Box::from_raw(a));
            drop(Box::from_raw(b));
            drop(Box::from_raw(c));
        }
    }

    #[test]
    fn handle_vblank_is_noop_when_disabled() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Calls `drm_handle_vblank` on disabled fixture CRTC 0 to verify it returns false without side effects.
        unsafe {
            assert!(!drm_handle_vblank(dev, 0));
            assert_eq!(drm_vblank_count(dev, 0), 0);
        }
    }

    #[test]
    fn vblank_work_waits_for_target_then_pending_cancel_releases_reference() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Sets `max_vblank_count` and tests scheduling and cancelling `DrmVblankWork` on fixture pipe 0.
        unsafe {
            let v = fx.vblank(0);
            (*v).max_vblank_count = u32::MAX;
            fx.hw.counter.store(2, Ordering::SeqCst);
            assert_eq!(drm_vblank_get(dev, 0), 0);
            drm_vblank_put(dev, 0);
            drm_update_vblank_count(dev, 0, false);
            assert_eq!(drm_vblank_count(dev, 0), 2);

            let mut work: DrmVblankWork = zeroed();
            drm_vblank_work_init(&mut work, fx.crtc(0), noop_work);

            // A future target remains on the vblank list and owns one ref.
            assert_eq!(drm_vblank_work_schedule(&mut work, 5, false), 1);
            assert!(!list_empty(&(*v).pending_work));
            assert_eq!(
                (*v).refcount
                    .counter
                    .load(core::sync::atomic::Ordering::Relaxed),
                1
            );

            // Cancel the pending item under the event lock, as the source
            // helper's caller contract requires. The worker adapter itself is
            // tested with a real scheduler task in drm-kapi.
            spin_lock_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            crate::drm_vblank_work_upstream::drm_vblank_cancel_pending_works(v);
            spin_unlock_raw(core::ptr::addr_of_mut!((*dev).event_lock));
            assert!(list_empty(&(*v).pending_work));
            assert!(list_empty(&work.node));
            assert_eq!(
                (*v).refcount
                    .counter
                    .load(core::sync::atomic::Ordering::Relaxed),
                0
            );
        }
    }

    #[test]
    fn wait_one_vblank_succeeds_when_vblank_arrives() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        let crtc = fx.crtc(0);
        // SAFETY: Spawns worker thread driving `drm_handle_vblank` while main thread waits in `drm_crtc_wait_one_vblank`.
        unsafe {
            let v = fx.vblank(0);
            (*v).max_vblank_count = u32::MAX;
            fx.hw.counter.store(1, Ordering::SeqCst);
            assert_eq!(drm_vblank_get(dev, 0), 0);
            drm_vblank_put(dev, 0);

            let dev_addr = dev as usize;
            let hw = Arc::clone(&fx.hw);
            let worker = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10));
                hw.counter.store(2, Ordering::SeqCst);
                drm_handle_vblank(dev_addr as *mut DrmDevice, 0);
            });

            let ret = drm_crtc_wait_one_vblank(crtc);
            worker.join().unwrap();
            assert_eq!(ret, 0);
        }
    }

    #[test]
    fn wait_vblank_ioctl_succeeds_when_sequence_arrives() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Constructs `DrmWaitVblank` ioctl buffer and invokes `drm_wait_vblank_ioctl` on fixture pipe 0.
        unsafe {
            let v = fx.vblank(0);
            (*v).max_vblank_count = u32::MAX;
            fx.hw.counter.store(1, Ordering::SeqCst);
            assert_eq!(drm_vblank_get(dev, 0), 0);
            drm_vblank_put(dev, 0);

            let mut vblwait: DrmWaitVblank = core::mem::zeroed();
            (*wv_req(&mut vblwait)).r#type = _DRM_VBLANK_ABSOLUTE;
            (*wv_req(&mut vblwait)).sequence = 2;

            let dev_addr = dev as usize;
            let hw = Arc::clone(&fx.hw);
            let worker = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10));
                hw.counter.store(2, Ordering::SeqCst);
                drm_handle_vblank(dev_addr as *mut DrmDevice, 0);
            });

            let ret = drm_wait_vblank_ioctl(
                dev,
                &mut vblwait as *mut _ as *mut c_void,
                core::ptr::null_mut(),
            );
            worker.join().unwrap();
            assert_eq!(ret, 0);
        }
    }

    #[test]
    fn wait_one_vblank_times_out_when_no_vblank() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        let crtc = fx.crtc(0);
        // SAFETY: Calls `drm_crtc_wait_one_vblank` on fixture with no vblank interrupts to verify timeout handling.
        unsafe {
            let v = fx.vblank(0);
            (*v).max_vblank_count = u32::MAX;
            fx.hw.counter.store(1, Ordering::SeqCst);
            assert_eq!(drm_vblank_get(dev, 0), 0);
            drm_vblank_put(dev, 0);

            let ret = drm_crtc_wait_one_vblank(crtc);
            assert_eq!(ret, -ETIMEDOUT);
        }
    }

    #[test]
    fn wait_vblank_ioctl_times_out_with_ebusy() {
        let mut fx = Fixture::new(1);
        let dev = fx.dev();
        // SAFETY: Calls `drm_wait_vblank_ioctl` with future target sequence on fixture and verifies timeout returns -EBUSY.
        unsafe {
            let v = fx.vblank(0);
            (*v).max_vblank_count = u32::MAX;
            fx.hw.counter.store(1, Ordering::SeqCst);
            assert_eq!(drm_vblank_get(dev, 0), 0);
            drm_vblank_put(dev, 0);

            let mut vblwait: DrmWaitVblank = core::mem::zeroed();
            (*wv_req(&mut vblwait)).r#type = _DRM_VBLANK_ABSOLUTE;
            (*wv_req(&mut vblwait)).sequence = 5;

            let ret = drm_wait_vblank_ioctl(
                dev,
                &mut vblwait as *mut _ as *mut c_void,
                core::ptr::null_mut(),
            );
            assert_eq!(ret, -EBUSY);
        }
    }
}

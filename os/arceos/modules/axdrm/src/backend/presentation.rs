//! Display event ownership moves from command completion to presentation.

use drm_kapi::locks::{spin_lock_irq, spin_unlock_irq};
use kdrm_core::{
    drm_crtc_types::DrmCrtc,
    drm_vblank_types::DrmPendingVblankEvent,
    drm_vblank_upstream::{
        drm_crtc_arm_vblank_event, drm_crtc_send_vblank_event, drm_crtc_vblank_get,
        drm_dev_has_vblank,
    },
};

/// # Safety
/// If non-null, `crtc` and `event` must point to live, properly initialized `DrmCrtc` and `DrmPendingVblankEvent`.
pub(super) unsafe fn send_event(crtc: *mut DrmCrtc, event: *mut DrmPendingVblankEvent) {
    if crtc.is_null() || event.is_null() {
        return;
    }
    // Transfer the event to the vblank owner only after RDIF command completion,
    // as in Linux virtgpu's atomic_flush. This runs after wait_completion has left
    // the GPU lock, so event/fence callbacks cannot re-enter ax-gpu under it.
    // SAFETY: 'crtc' and 'event' are non-null and valid. (*crtc).dev is checked against null before accessing event_lock.
    unsafe {
        let dev = (*crtc).dev;
        if dev.is_null() {
            return;
        }
        spin_lock_irq(&mut (*dev).event_lock);
        if drm_dev_has_vblank(dev) && drm_crtc_vblank_get(crtc) == 0 {
            // The queued event consumes the vblank reference on delivery and
            // signals its out-fence with the same sequence/timestamp boundary.
            drm_crtc_arm_vblank_event(crtc, event);
        } else {
            // Disabled CRTCs or devices without vblank have no future edge.
            drm_crtc_send_vblank_event(crtc, event);
        }
        spin_unlock_irq(&mut (*dev).event_lock);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use core::{
        mem::size_of,
        ptr,
        sync::atomic::{AtomicU32, Ordering},
    };
    use std::{sync::OnceLock, time::Instant};

    use drm_kapi::{
        config::GFP_KERNEL,
        dma_fence::{dma_fence_get, dma_fence_put},
        memory::kfree,
    };
    use kdrm_core::{
        drm_crtc_types::DrmCrtcFuncs, drm_device_types::DrmDevice, drm_drv_types::DrmDriver,
        drm_file_types::DrmFile, drm_file_upstream::drm_event_reserve_init,
        drm_syncobj_upstream::drm_syncobj_fence_create, drm_vblank_types::DrmVblankCrtc,
        drm_vblank_upstream::drm_handle_vblank, uapi_drm_types::DrmEventVblank,
    };

    use super::*;

    fn host_now() -> u64 {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
    }

    pub(crate) fn init_host_services() {
        // Only the real core's clock/lock/wake boundaries are hosted here;
        // vblank advancement is explicit, not a claim about native IRQ timing.
        static CLOCK: drm_kapi::clock::ClockOps = drm_kapi::clock::ClockOps {
            now_ns: host_now,
            delay_us: |_| {},
            sleep_us: |_| {},
            relax: std::thread::yield_now,
        };
        static LOCKS: drm_kapi::locks::LockContextOps = drm_kapi::locks::LockContextOps {
            preempt_disable: || {},
            preempt_enable: || {},
            irq_save_disable: || 0,
            irq_restore: |_| {},
            irq_disable: || {},
            irq_enable: || {},
            irqs_disabled: || false,
        };
        static WAITS: drm_kapi::wait::WaitOps = drm_kapi::wait::WaitOps {
            wait_timeout_until: |_, ready| {
                assert!(ready());
                false
            },
            wake_all: || {},
        };
        drm_kapi::clock::install_clock_ops(&CLOCK).unwrap();
        drm_kapi::locks::install_lock_context_ops(&LOCKS).unwrap();
        drm_kapi::wait::install_wait_ops(&WAITS).unwrap();
    }

    unsafe extern "C" fn counter(crtc: *mut DrmCrtc) -> u32 {
        // SAFETY: Each fixture device points to its live atomic hardware counter.
        unsafe { &*(*(*crtc).dev).dev_private.cast::<AtomicU32>() }.load(Ordering::Acquire)
    }

    #[test]
    fn display_out_fence_and_event_share_the_next_vblank_boundary() {
        init_host_services();
        // SAFETY: All C-layout records are zero-valid and initialized below.
        // Every pointer remains live through synchronous core event delivery.
        unsafe {
            let mut driver: DrmDriver = core::mem::zeroed();
            driver.driver_features = kdrm_core::drm_mode_object_upstream::DRIVER_MODESET;
            let mut dev: DrmDevice = core::mem::zeroed();
            let mut vblank: DrmVblankCrtc = core::mem::zeroed();
            let mut crtc: DrmCrtc = core::mem::zeroed();
            let mut funcs: DrmCrtcFuncs = core::mem::zeroed();
            let mut file: DrmFile = core::mem::zeroed();
            let hw = AtomicU32::new(0);
            dev.driver = ptr::addr_of!(driver).cast_mut();
            dev.dev_private = ptr::addr_of!(hw).cast_mut().cast();
            dev.num_crtcs = 1;
            dev.mode_config.num_crtc = 1;
            dev.vblank = &mut vblank;
            drm_kapi::list::init_list_head(ptr::addr_of_mut!(dev.mode_config.crtc_list));
            drm_kapi::list::init_list_head(ptr::addr_of_mut!(dev.vblank_event_list));
            crtc.dev = &mut dev;
            funcs.get_vblank_counter = Some(counter);
            crtc.funcs = &mut funcs;
            drm_kapi::list::list_add_tail(
                ptr::addr_of_mut!(crtc.head),
                ptr::addr_of_mut!(dev.mode_config.crtc_list),
            );
            vblank.dev = &mut dev;
            vblank.enabled = true;
            vblank
                .refcount
                .counter
                .store(1, core::sync::atomic::Ordering::Relaxed);
            vblank.max_vblank_count = u32::MAX;
            drm_kapi::wait::init_waitqueue_head(&mut vblank.queue);
            drm_kapi::list::init_list_head(ptr::addr_of_mut!(vblank.pending_work));
            drm_kapi::wait::init_waitqueue_head(&mut vblank.work_wait_queue);
            file.event_space = 4096;
            drm_kapi::list::init_list_head(ptr::addr_of_mut!(file.pending_event_list));
            drm_kapi::list::init_list_head(ptr::addr_of_mut!(file.event_list));
            drm_kapi::wait::init_waitqueue_head(&mut file.event_wait);

            for active in [true, false] {
                vblank.enabled = active;
                let event =
                    drm_kapi::memory::kzalloc(size_of::<DrmPendingVblankEvent>(), GFP_KERNEL)
                        .cast::<DrmPendingVblankEvent>();
                assert!(!event.is_null());
                let payload = ptr::addr_of_mut!((*event).event.vbl).cast::<DrmEventVblank>();
                (*payload).base.r#type = 2; // DRM_EVENT_FLIP_COMPLETE
                (*payload).base.length = size_of::<DrmEventVblank>() as u32;
                assert_eq!(
                    drm_event_reserve_init(
                        &mut dev,
                        &mut file,
                        ptr::addr_of_mut!((*event).base),
                        ptr::addr_of_mut!((*payload).base)
                    ),
                    0
                );
                let out_fence = drm_syncobj_fence_create();
                assert!(!out_fence.is_null());
                (*event).base.fence = dma_fence_get(out_fence);
                send_event(&mut crtc, event);
                if active {
                    assert!(
                        !drm_kapi::dma_fence::dma_fence_is_signaled(out_fence),
                        "RDIF completion is not presentation completion"
                    );
                    assert!(drm_kapi::list::list_empty(&file.event_list));
                    hw.store(1, Ordering::Release);
                    assert!(drm_handle_vblank(&mut dev, 0));
                    assert_eq!((*payload).sequence, 1);
                    assert_eq!(
                        drm_kapi::dma_fence::dma_fence_timestamp(out_fence),
                        vblank.time
                    );
                }
                assert!(drm_kapi::dma_fence::dma_fence_is_signaled(out_fence));
                assert!(!drm_kapi::list::list_empty(&file.event_list));
                drm_kapi::list::list_del_init(ptr::addr_of_mut!((*event).base.link));
                kfree(event);
                dma_fence_put(out_fence);
            }
        }
    }
}

// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! Registered DRM device and its lifetime-owned RDIF adapter state.

use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_void},
    ptr,
    sync::atomic::AtomicBool,
};

use ax_gpu::rdif_display::OutputId;
use ax_lazyinit::LazyInit;
use drm_kapi::dma::{DMA_BIDIRECTIONAL, DMA_FROM_DEVICE, DMA_TO_DEVICE, DmaOps, install_dma_ops};
use kdrm_core::{
    drm_atomic_helper_upstream::drm_atomic_helper_commit,
    drm_atomic_state_helper_upstream::{
        drm_atomic_helper_connector_destroy_state, drm_atomic_helper_connector_duplicate_state,
        drm_atomic_helper_connector_reset,
    },
    drm_connector_types::{DrmConnector, DrmConnectorFuncs},
    drm_connector_upstream::{drm_connector_cleanup, drm_connector_init},
    drm_device_types::DrmDevice,
    drm_drv_types::DrmDriver,
    drm_drv_upstream::{drm_dev_alloc, drm_dev_put, drm_dev_register, drm_dev_unregister},
    drm_framebuffer_types::DrmFramebuffer,
    drm_gem_types::DrmGemObject,
    drm_mode_config_types::DrmModeConfigFuncs,
    drm_mode_config_upstream::drmm_mode_config_init,
    drm_modeset_helper_vtables_types::{
        DrmConnectorHelperFuncs, DrmCrtcHelperFuncs, DrmModeConfigHelperFuncs,
    },
    drm_simple_kms_helper_types_upstream::{DrmSimpleDisplayPipe, DrmSimpleDisplayPipeFuncs},
    drm_simple_kms_helper_upstream::drm_simple_display_pipe_init,
    drm_vblank_upstream::drm_vblank_init,
    uapi_drm_consts::{DRM_FORMAT_XRGB8888, DRM_MODE_CONNECTOR_VIRTUAL},
};

use crate::backend::{fence, gem, kms, virtgpu};

const DRIVER_NAME: &[u8] = b"virtio_gpu\0";
const DRIVER_DESC: &[u8] = b"VirtIO GPU\0";
const MIN_MODE: u32 = 32;
const MAX_MODE: u32 = 8192;
const DRIVER_GEM: u32 = 1 << 0;
const DRIVER_MODESET: u32 = 1 << 1;
const DRIVER_RENDER: u32 = 1 << 3;
const DRIVER_ATOMIC: u32 = 1 << 4;

#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectMapping {
    address: u64,
    size: usize,
    direction: u32,
    attrs: u64,
}

// VirtIO GPU DMA uses the RDIF-registered device's direct guest-physical
// domain. Retain admitted spans so an unmap must match a previously mapped
// address, length, direction, and attributes exactly.
static DIRECT_MAPPINGS: spin::Mutex<Vec<DirectMapping>> = spin::Mutex::new(Vec::new());
static DMA_DEVICE_TOKEN: u8 = 0;
static DMA_OPS: DmaOps = DmaOps {
    map: map_direct_dma,
    unmap: unmap_direct_dma,
    iommu_mapped: direct_dma_iommu_mapped,
};

fn dma_device_token() -> *mut c_void {
    ptr::addr_of!(DMA_DEVICE_TOKEN).cast_mut().cast()
}

fn direct_device_registered(dev: *mut c_void) -> bool {
    !dev.is_null()
        && dev == dma_device_token()
        && ax_gpu::identity().is_some_and(|identity| identity.driver_name == "virtio_gpu")
        && ax_gpu::capabilities()
            .is_some_and(|caps| caps.dma_domain == ax_gpu::rdif_gpu::DmaDomainId::Direct)
}

fn map_direct_dma(
    dev: *mut c_void,
    physical: u64,
    size: usize,
    direction: u32,
    attrs: u64,
) -> Result<u64, i32> {
    if dev != dma_device_token() {
        return Err(-drm_kapi::config::ENODEV);
    }
    if !direct_device_registered(dev) {
        return Err(-drm_kapi::config::EOPNOTSUPP);
    }
    if size == 0
        || size > u32::MAX as usize
        || !matches!(
            direction,
            DMA_BIDIRECTIONAL | DMA_TO_DEVICE | DMA_FROM_DEVICE
        )
        || physical.checked_add(size as u64).is_none()
    {
        return Err(-drm_kapi::config::EINVAL);
    }
    let mut mappings = DIRECT_MAPPINGS.lock();
    mappings
        .try_reserve(1)
        .map_err(|_| -drm_kapi::config::ENOMEM)?;
    mappings.push(DirectMapping {
        address: physical,
        size,
        direction,
        attrs,
    });
    Ok(physical)
}

fn unmap_direct_dma(
    dev: *mut c_void,
    dma_address: u64,
    size: usize,
    direction: u32,
    attrs: u64,
) -> Result<(), i32> {
    if dev != dma_device_token() {
        return Err(-drm_kapi::config::ENODEV);
    }
    if !direct_device_registered(dev) {
        return Err(-drm_kapi::config::EOPNOTSUPP);
    }
    let mapping = DirectMapping {
        address: dma_address,
        size,
        direction,
        attrs,
    };
    let mut mappings = DIRECT_MAPPINGS.lock();
    let Some(index) = mappings.iter().position(|candidate| *candidate == mapping) else {
        return Err(-drm_kapi::config::EINVAL);
    };
    mappings.swap_remove(index);
    Ok(())
}

fn direct_dma_iommu_mapped(dev: *mut c_void) -> bool {
    !dev.is_null()
        && dev == dma_device_token()
        && ax_gpu::identity().is_some_and(|identity| identity.driver_name == "virtio_gpu")
        && ax_gpu::capabilities()
            .is_some_and(|caps| caps.dma_domain != ax_gpu::rdif_gpu::DmaDomainId::Direct)
}

/// The DRM-side KMS objects are initialized before registration and remain at
/// stable addresses until the final DRM device reference is released.
pub(crate) struct DriverPrivate {
    pub(crate) output: OutputId,
    pub(crate) connected: AtomicBool,
    pub(crate) last_scanout: spin::Mutex<*mut DrmGemObject>,
    pub(crate) last_scanout_fb: spin::Mutex<*mut DrmFramebuffer>,
    pub(crate) precommitted: spin::Mutex<Option<kms::PrecommittedDisplay>>,
    pub(crate) pipe: UnsafeCell<DrmSimpleDisplayPipe>,
    pub(crate) crtc_helper: UnsafeCell<DrmCrtcHelperFuncs>,
    pub(crate) connector: UnsafeCell<DrmConnector>,
}

// SAFETY: `pipe` and `connector` are initialized before the device is
// published; the per-CRTC helper table is initialized before registration and
// remains immutable afterward. KMS callbacks are serialized by DRM.
unsafe impl Send for DriverPrivate {}
// SAFETY: See the `Send` proof; shared callbacks do not mutate the KMS objects
// or helper table outside DRM modeset serialization.
unsafe impl Sync for DriverPrivate {}

struct SyncDriver(DrmDriver);

// SAFETY: The driver table contains immutable function pointers and static
// strings/tables. The DRM core reads it concurrently but never mutates it.
unsafe impl Sync for SyncDriver {}

static DRM_DRIVER: SyncDriver = SyncDriver(DrmDriver {
    load: None,
    open: Some(virtgpu::open),
    postclose: Some(virtgpu::postclose),
    unload: None,
    release: Some(drm_release),
    master_set: None,
    master_drop: None,
    debugfs_init: None,
    gem_create_object: Some(gem::create_object),
    prime_handle_to_fd: None,
    prime_fd_to_handle: None,
    gem_prime_import: None,
    gem_prime_import_sg_table: Some(gem::prime_import_sg_table),
    dumb_create: Some(gem::dumb_create),
    dumb_map_offset: Some(gem::dumb_map_offset),
    fbdev_probe: None,
    show_fdinfo: None,
    major: 0,
    minor: 1,
    patchlevel: 0,
    name: DRIVER_NAME.as_ptr() as *mut c_char,
    desc: DRIVER_DESC.as_ptr() as *mut c_char,
    driver_features: DRIVER_GEM
        | DRIVER_MODESET
        | DRIVER_RENDER
        | DRIVER_ATOMIC
        | kdrm_core::drm_ioctl_upstream::DRIVER_SYNCOBJ
        | kdrm_core::drm_ioctl_upstream::DRIVER_SYNCOBJ_TIMELINE,
    ioctls: virtgpu::IOCTLS.as_ptr() as *mut _,
    num_ioctls: virtgpu::IOCTLS.len() as i32,
    fops: ptr::null_mut(),
});

static MODE_FUNCS: DrmModeConfigFuncs = DrmModeConfigFuncs {
    fb_create: Some(kms::fb_create),
    get_format_info: None,
    mode_valid: Some(kms::mode_valid),
    atomic_check: Some(kms::atomic_check),
    atomic_commit: Some(drm_atomic_helper_commit),
    atomic_state_alloc: None,
    atomic_state_clear: None,
    atomic_state_free: None,
};

static MODE_HELPER_FUNCS: DrmModeConfigHelperFuncs = DrmModeConfigHelperFuncs {
    atomic_commit_tail: Some(kms::atomic_commit_tail),
    atomic_commit_pre_swap: Some(kms::atomic_commit_pre_swap),
};

static CONNECTOR_FUNCS: DrmConnectorFuncs = DrmConnectorFuncs {
    dpms: None,
    reset: Some(drm_atomic_helper_connector_reset),
    detect: Some(kms::connector_detect),
    force: None,
    fill_modes: Some(kdrm_core::drm_probe_helper_upstream::drm_helper_probe_single_connector_modes),
    set_property: None,
    late_register: None,
    early_unregister: None,
    destroy: Some(kms::connector_destroy),
    atomic_duplicate_state: Some(drm_atomic_helper_connector_duplicate_state),
    atomic_destroy_state: Some(drm_atomic_helper_connector_destroy_state),
    atomic_set_property: None,
    atomic_get_property: None,
    atomic_print_state: None,
    oob_hotplug_event: None,
    debugfs_init: None,
};

static CONNECTOR_HELPER_FUNCS: DrmConnectorHelperFuncs = DrmConnectorHelperFuncs {
    get_modes: Some(kms::connector_get_modes),
    detect_ctx: None,
    mode_valid: Some(kms::connector_mode_valid),
    mode_valid_ctx: None,
    best_encoder: Some(kms::best_encoder),
    atomic_best_encoder: None,
    atomic_check: None,
    atomic_commit: None,
    prepare_writeback_job: None,
    cleanup_writeback_job: None,
    enable_hpd: None,
    disable_hpd: None,
};

static PIPE_FUNCS: DrmSimpleDisplayPipeFuncs = DrmSimpleDisplayPipeFuncs {
    mode_valid: Some(kms::pipe_mode_valid),
    enable: Some(kms::pipe_enable),
    disable: Some(kms::pipe_disable),
    check: None,
    update: Some(kms::pipe_update),
    prepare_fb: None,
    cleanup_fb: None,
    begin_fb_access: None,
    end_fb_access: None,
    enable_vblank: Some(kms::pipe_enable_vblank),
    disable_vblank: Some(kms::pipe_disable_vblank),
    reset_crtc: None,
    duplicate_crtc_state: None,
    destroy_crtc_state: None,
    reset_plane: None,
    duplicate_plane_state: None,
    destroy_plane_state: None,
};

/// Retained owner of the common DRM device registered for the active RDIF GPU.
///
/// The DRM core keeps an additional owner reference in `dev_private`; this
/// wrapper keeps the public device handle and RDIF adapter state alive for
/// Starry file and mapping objects.
pub struct DeviceOwner {
    drm: *mut DrmDevice,
    _private: Arc<DriverPrivate>,
}

// SAFETY: The core device is internally synchronized by kdrm-core; the private
// state follows the KMS serialization documented above. The GPU transport is
// not stored here and remains owned by ax-gpu.
unsafe impl Send for DeviceOwner {}
// SAFETY: Shared access only returns the stable core pointer or immutable
// metadata; mutation is performed through kdrm-core callbacks.
unsafe impl Sync for DeviceOwner {}

impl DeviceOwner {
    /// Raw core device pointer. It remains valid while this owner is retained.
    pub fn raw(&self) -> *mut DrmDevice {
        self.drm
    }

    /// DRM minor selected by the core for the primary node.
    pub fn primary_minor_index(&self) -> Option<u32> {
        if self.drm.is_null() {
            return None;
        }
        // SAFETY: `self.drm` is kept alive by this owner and core release is
        // prevented until this owner drops.
        let minor = unsafe { (*self.drm).primary };
        // SAFETY: A registered primary node, when present, points to a core-
        // owned DrmMinor that lives at least as long as the device reference.
        unsafe { minor.as_ref() }.map(|minor| minor.index as u32)
    }

    /// DRM minor selected by the core for the render node.
    pub fn render_minor_index(&self) -> Option<u32> {
        if self.drm.is_null() {
            return None;
        }
        // SAFETY: `self.drm` is kept alive by this owner and core release is
        // prevented until this owner drops.
        let minor = unsafe { (*self.drm).render };
        // SAFETY: A registered render node, when present, points to a core-
        // owned DrmMinor that lives at least as long as the device reference.
        unsafe { minor.as_ref() }.map(|minor| minor.index as u32)
    }
}

impl Drop for DeviceOwner {
    fn drop(&mut self) {
        if self.drm.is_null() {
            return;
        }
        // SAFETY: `self.drm` is the registered reference owned by this
        // wrapper. Unregister prevents new node opens before the final put.
        unsafe {
            drm_dev_unregister(self.drm);
            drm_dev_put(self.drm);
        }
        self.drm = ptr::null_mut();
    }
}

static DEVICE: LazyInit<Arc<DeviceOwner>> = LazyInit::new();

/// Return a retained handle to the registered DRM device, if any.
pub fn device() -> Option<Arc<DeviceOwner>> {
    DEVICE.get().cloned()
}

/// Initialize and register the common DRM device around the existing RDIF GPU.
pub(crate) fn init() -> Result<(), i32> {
    if DEVICE.is_inited() || !ax_gpu::has_gpu() || !ax_gpu::has_display_controller() {
        return Ok(());
    }
    let identity = ax_gpu::identity().ok_or(-drm_kapi::config::ENODEV)?;
    if identity.driver_name != "virtio_gpu" {
        return Ok(());
    }
    let output = first_connected_output().ok_or(-drm_kapi::config::ENODEV)?;
    // The KAPI map/unmap provider is device-scoped by this opaque token; it
    // never infers a DMA device from a null parent pointer.
    install_dma_ops(&DMA_OPS).map_err(|_| -drm_kapi::config::EINVAL)?;
    let private = Arc::new(DriverPrivate {
        output,
        connected: AtomicBool::new(true),
        last_scanout: spin::Mutex::new(ptr::null_mut()),
        last_scanout_fb: spin::Mutex::new(ptr::null_mut()),
        precommitted: spin::Mutex::new(None),
        // SAFETY: The simple-KMS C records are initialized to zero before
        // their individual DRM constructors fill every field they expose. All-zeroes is a valid bit pattern for this uninit buffer.
        pipe: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        // SAFETY: The table is copied from simple-KMS after pipe initialization
        // and before DRM registration publishes any CRTC callback; all-zeroes represents nullable function pointers.
        crtc_helper: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        // SAFETY: The connector record follows the same C zero-init contract before `drm_connector_init`.
        connector: UnsafeCell::new(unsafe { core::mem::zeroed() }),
    });

    // SAFETY: `DRM_DRIVER.0` is a valid static driver table, and `dma_device_token()` provides a stable non-null device token.
    let drm = unsafe { drm_dev_alloc(&DRM_DRIVER.0, dma_device_token()) };
    if drm.is_null() {
        return Err(-drm_kapi::config::ENOMEM);
    }
    // Transfer a strong Arc to the core release callback. The local Arc is
    // also retained by DeviceOwner after registration succeeds.
    // SAFETY: `drm` is non-null from `drm_dev_alloc` above. `Arc::into_raw` creates a raw pointer transferring ownership of one strong reference to `dev_private`.
    unsafe {
        (*drm).dev_private = Arc::into_raw(private.clone()) as *mut DriverPrivate as *mut c_void;
    }

    // SAFETY: `drm` was returned by `drm_dev_alloc` and is non-null; `private` has stable addresses.
    let init_result = unsafe { init_drm_device(drm, &private) };
    if init_result != 0 {
        // SAFETY: `drm` was allocated above and must be freed via `drm_dev_put` on initialization error.
        unsafe { drm_dev_put(drm) };
        return Err(init_result);
    }
    // SAFETY: `drm` is fully initialized and ready to be registered with the system.
    let register_result = unsafe { drm_dev_register(drm, 0) };
    if register_result != 0 {
        // SAFETY: `drm` was allocated above and must be freed via `drm_dev_put` on registration error.
        unsafe { drm_dev_put(drm) };
        return Err(register_result);
    }
    // SAFETY: `drm` is registered and `init_drm_device` initialized the connector.
    unsafe { probe_initial_modes(drm, private.connector.get()) };

    // Install the only GPU completion notifier. It is strictly wake/enqueue
    // only; the work item inspects RDIF tokens after ax-gpu releases its lock.
    fence::init();
    ax_gpu::set_completion_notifier(fence::on_gpu_completion);
    DEVICE.init_once(Arc::new(DeviceOwner {
        drm,
        _private: private,
    }));
    Ok(())
}

/// Linux fills each connector's mode list once at load, from the in-kernel
/// fbdev client's initial configuration, so a first `GETCONNECTOR` that passes
/// a nonzero `count_modes` already sees the modes. There is no in-kernel
/// client here, so run that probe directly.
///
/// # Safety
///
/// `drm` must be a registered device and `connector` an initialized connector
/// of it whose `funcs` table outlives the call.
unsafe fn probe_initial_modes(drm: *mut DrmDevice, connector: *mut DrmConnector) {
    // SAFETY: the caller guarantees both objects are live; `mode_config.mutex`
    // serializes the probe with GETCONNECTOR, as `fill_modes` requires.
    unsafe {
        let config = ptr::addr_of_mut!((*drm).mode_config);
        let Some(fill_modes) = (*(*connector).funcs).fill_modes else {
            return;
        };
        drm_kapi::mutex::mutex_lock(ptr::addr_of_mut!((*config).mutex));
        fill_modes(connector, (*config).max_width, (*config).max_height);
        drm_kapi::mutex::mutex_unlock(ptr::addr_of_mut!((*config).mutex));
    }
}

fn first_connected_output() -> Option<OutputId> {
    ax_gpu::with_display(|display| {
        (0..display.output_count())
            .map(OutputId::new)
            .find(|id| display.output(*id).is_ok_and(|output| output.connected))
    })
    .ok()
    .flatten()
}

/// # Safety
///
/// - `drm` must point to a live, allocated `DrmDevice` that has not yet been registered.
/// - `private` must have stable memory addresses for its embedded KMS objects.
unsafe fn init_drm_device(drm: *mut DrmDevice, private: &DriverPrivate) -> i32 {
    if drm.is_null() {
        return -drm_kapi::config::EINVAL;
    }
    // SAFETY: `drm` is verified non-null and points to a newly allocated `DrmDevice`.
    // KMS records inside `private` have stable addresses for the device's lifetime.
    unsafe {
        let ret = drmm_mode_config_init(drm);
        if ret != 0 {
            return ret;
        }
        (*drm).mode_config.min_width = MIN_MODE;
        (*drm).mode_config.min_height = MIN_MODE;
        (*drm).mode_config.max_width = MAX_MODE;
        (*drm).mode_config.max_height = MAX_MODE;
        (*drm).mode_config.preferred_depth = 24;
        (*drm).mode_config.fb_modifiers_not_supported = true;
        (*drm).mode_config.funcs = ptr::addr_of!(MODE_FUNCS) as *mut _;
        (*drm).mode_config.helper_private = ptr::addr_of!(MODE_HELPER_FUNCS) as *mut _;

        let ret = drm_vblank_init(drm, 1);
        if ret != 0 {
            return ret;
        }
        let connector = private.connector.get();
        let ret = drm_connector_init(
            drm,
            connector,
            ptr::addr_of!(CONNECTOR_FUNCS),
            DRM_MODE_CONNECTOR_VIRTUAL as i32,
        );
        if ret != 0 {
            return ret;
        }
        (*connector).helper_private = ptr::addr_of!(CONNECTOR_HELPER_FUNCS) as *mut _;

        let formats = [DRM_FORMAT_XRGB8888];
        let ret = drm_simple_display_pipe_init(
            drm,
            private.pipe.get(),
            ptr::addr_of!(PIPE_FUNCS),
            formats.as_ptr(),
            formats.len() as u32,
            ptr::null(),
            connector,
        );
        if ret != 0 {
            drm_connector_cleanup(connector);
            return ret;
        }
        // Keep the upstream simple-KMS CRTC helper behavior except for its
        // driver-specific primary-plane-required check. The common atomic
        // helper still validates modesets and each attached plane; a CRTC may
        // remain active with no primary framebuffer, as VirtGPU permits.
        let pipe = private.pipe.get();
        let helpers = (*pipe).crtc.helper_private.cast::<DrmCrtcHelperFuncs>();
        if helpers.is_null() {
            return -drm_kapi::config::EINVAL;
        }
        let driver_helpers = private.crtc_helper.get();
        ptr::copy_nonoverlapping(helpers, driver_helpers, 1);
        (*driver_helpers).atomic_check = Some(kms::crtc_atomic_check);
        (*pipe).crtc.helper_private = driver_helpers.cast();

        kdrm_core::drm_mode_config_upstream::drm_mode_config_reset(drm);
        0
    }
}

/// # Safety
///
/// `drm` must be null or point to a live `DrmDevice` whose `dev_private` contains a valid `DriverPrivate` pointer.
pub(crate) unsafe fn private_from_device<'a>(drm: *mut DrmDevice) -> Option<&'a DriverPrivate> {
    if drm.is_null() {
        return None;
    }
    // SAFETY: Every callback receives the live device allocated above. Its
    // dev_private pointer owns a strong Arc until `drm_release` runs.
    unsafe { (*drm).dev_private.cast::<DriverPrivate>().as_ref() }
}

/// # Safety
///
/// `drm` must be null or point to a live `DrmDevice` being released at final teardown by kdrm-core.
unsafe extern "C" fn drm_release(drm: *mut DrmDevice) {
    if drm.is_null() {
        return;
    }
    // SAFETY: kdrm-core invokes release once at final device teardown; the raw
    // pointer is the unique strong Arc reference transferred during init.
    unsafe {
        let raw = (*drm).dev_private.cast::<DriverPrivate>();
        (*drm).dev_private = ptr::null_mut();
        if !raw.is_null() {
            drop(Arc::from_raw(raw));
        }
    }
}

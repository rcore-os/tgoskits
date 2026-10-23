// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! GEM objects whose actual storage is owned by the registered RDIF GPU.

use alloc::{sync::Arc, vec::Vec};
use core::{ffi::c_void, mem::size_of, num::NonZeroUsize, ptr};

use ax_gpu::{
    MappableBacking,
    rdif_gpu::{
        Backing, BufferDescriptor, BufferHandle, Completion, DmaAddr, DmaDomainId, DmaSegment,
        GpuError, PixelFormat, Resource3d,
    },
};
use drm_kapi::{
    config::{GFP_KERNEL, PAGE_SIZE, err_ptr, is_err},
    memory::{kfree, kmalloc, kzalloc},
    scatterlist::{
        sg_alloc_table, sg_dma_address, sg_dma_len, sg_free_table, sg_mark_end, sg_next,
    },
    types::SgTable,
};
use kdrm_core::{
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_fourcc_types::DrmFormatInfo,
    drm_fourcc_upstream::drm_format_info_min_pitch,
    drm_framebuffer_types::{DrmFramebuffer, DrmFramebufferFuncs},
    drm_framebuffer_upstream::{drm_framebuffer_init, drm_framebuffer_put},
    drm_gem_framebuffer_helper_upstream::{
        drm_format_info_plane_height, drm_format_info_plane_width, drm_gem_fb_create_handle,
        drm_gem_fb_destroy,
    },
    drm_gem_shmem_helper_types_upstream::DrmGemShmemObject,
    drm_gem_shmem_helper_upstream::drm_gem_shmem_release,
    drm_gem_types::{DrmGemObject, DrmGemObjectFuncs},
    drm_gem_upstream::{
        drm_gem_dumb_map_offset, drm_gem_handle_create, drm_gem_object_init, drm_gem_object_lookup,
        drm_gem_object_put, drm_gem_object_release,
    },
    drm_modeset_helper_upstream::drm_helper_mode_fill_fb_struct,
    drm_plane_upstream::drm_any_plane_has_format,
    drm_prime_upstream::{DmaBuf, DmaBufAttachment, drm_gem_prime_export},
    uapi_drm_mode_types::{DrmModeCreateDumb, DrmModeFbCmd2},
};

const NEG_EINVAL: i32 = -drm_kapi::config::EINVAL;
const NEG_ENOENT: i32 = -drm_kapi::config::ENOENT;
const NEG_ENODEV: i32 = -drm_kapi::config::ENODEV;
const NEG_ENOMEM: i32 = -drm_kapi::config::ENOMEM;
const NEG_EIO: i32 = -drm_kapi::config::EIO;
const NEG_EOPNOTSUPP: i32 = -drm_kapi::config::EOPNOTSUPP;
const MAX_DIMENSION: u32 = 8192;
const MAX_BUFFER_SIZE: usize = 256 * 1024 * 1024;

struct SyncGemFuncs(DrmGemObjectFuncs);

// SAFETY: The callback table and its function pointers are immutable static
// data. DRM invokes callbacks according to each object's refcount/lifecycle.
unsafe impl Sync for SyncGemFuncs {}

static GEM_FUNCS: SyncGemFuncs = SyncGemFuncs(DrmGemObjectFuncs {
    free: Some(gem_free),
    open: Some(crate::backend::virtgpu::gem_open),
    close: Some(crate::backend::virtgpu::gem_close),
    print_info: None,
    export: Some(gem_export),
    pin: None,
    unpin: None,
    get_sg_table: Some(gem_get_sg_table),
    vmap: None,
    vunmap: None,
    mmap: None,
    evict: None,
    status: None,
    rss: None,
    vm_ops: ptr::null_mut(),
});

#[repr(C)]
struct AxFramebuffer {
    base: DrmFramebuffer,
    scanout_resource: Option<BufferHandle>,
}

const _: () = assert!(core::mem::offset_of!(AxFramebuffer, base) == 0);

struct SyncFramebufferFuncs(DrmFramebufferFuncs);

// SAFETY: The framebuffer callbacks and their function pointers are immutable
// static data. DRM invokes them according to each framebuffer's refcount.
unsafe impl Sync for SyncFramebufferFuncs {}

static AX_FRAMEBUFFER_FUNCS: SyncFramebufferFuncs = SyncFramebufferFuncs(DrmFramebufferFuncs {
    destroy: Some(ax_framebuffer_destroy),
    create_handle: Some(drm_gem_fb_create_handle),
    dirty: Some(ax_framebuffer_dirtyfb),
});

struct ObjectPrivate {
    shmem_object: bool,
    state: spin::Mutex<ObjectState>,
}

struct ObjectState {
    size: usize,
    export_pages: Option<PinnedPages>,
    width: u32,
    height: u32,
    stride: u32,
    format: Option<PixelFormat>,
    mapping: Option<Arc<MappableBacking>>,
    host_mapping: Option<super::host_blob::HostMapping>,
    backing: Option<Arc<dyn Backing>>,
    resource: Option<BufferHandle>,
    resource_3d: Option<Resource3d>,
    context: Option<ax_gpu::rdif_gpu::ContextHandle>,
    last_completion: Option<Completion>,
    resource_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    is_blob: bool,
}

impl ObjectState {
    fn new(size: usize) -> Self {
        Self {
            size,
            export_pages: None,
            width: 0,
            height: 0,
            stride: 0,
            format: None,
            mapping: None,
            host_mapping: None,
            backing: None,
            resource: None,
            resource_3d: None,
            context: None,
            last_completion: None,
            resource_id: 0,
            blob_mem: 0,
            blob_flags: 0,
            is_blob: false,
        }
    }
}

impl Drop for ObjectState {
    fn drop(&mut self) {
        if let Some(resource) = self.resource.take() {
            if self.host_mapping.is_some() {
                super::host_blob::unmap_before_release(resource);
            }
            // Keep every backing owner and BAR slot live until completion.
            release_resource(resource);
        }
    }
}

/// LinuxKPI page identities retained while the exporter GEM object owns the
/// corresponding DMA backing. SG tables can borrow these identities without
/// manufacturing `struct page` values from DMA addresses.
struct PinnedPages {
    pages: Vec<*mut drm_kapi::types::Page>,
}

// SAFETY: Each pointer is an owned opaque Page-record reference. The vector is
// stored behind ObjectState's mutex and only read immutably after installation.
unsafe impl Send for PinnedPages {}

impl Drop for PinnedPages {
    fn drop(&mut self) {
        for page in self.pages.drain(..) {
            // SAFETY: `pin_physical_frame` returned one owned metadata ref for
            // every entry and the backing GEM object is reaching final release.
            unsafe { drm_kapi::shmem::put_page(page) };
        }
    }
}

fn allocate_private(shmem_object: bool, size: usize) -> *mut ObjectPrivate {
    let storage = kmalloc(size_of::<ObjectPrivate>(), GFP_KERNEL).cast::<ObjectPrivate>();
    if storage.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `storage` is a fresh allocation large enough for ObjectPrivate;
    // ptr::write initializes the mutex and all owned fields exactly once.
    unsafe {
        ptr::write(
            storage,
            ObjectPrivate {
                shmem_object,
                state: spin::Mutex::new(ObjectState::new(size)),
            },
        );
    }
    storage
}

/// # Safety
///
/// `obj` must be null or point to a live, properly aligned `DrmGemObject` whose reference is held by the caller.
unsafe fn private_from_object<'a>(obj: *mut DrmGemObject) -> Option<&'a ObjectPrivate> {
    if obj.is_null() {
        return None;
    }
    // SAFETY: Every caller must hold a live GEM object reference. The private
    // pointer is assigned before publication and cleared at final release.
    unsafe {
        (*obj)
            .vma_node
            .driver_private
            .cast::<ObjectPrivate>()
            .as_ref()
    }
}

/// Driver hook for Linux shmem helper allocations. Those allocations are
/// shells owned by kdrm-core; the public dumb path below uses the RDIF backing
/// directly and does not allocate an unrelated shmem page set.
///
/// # Safety
///
/// `_dev` must be a live `DrmDevice`. The returned object will be initialized by the core shmem helper.
pub(crate) unsafe extern "C" fn create_object(
    _dev: *mut DrmDevice,
    _size: usize,
) -> *mut DrmGemObject {
    let shmem = kzalloc(size_of::<DrmGemShmemObject>(), GFP_KERNEL).cast::<DrmGemShmemObject>();
    if shmem.is_null() {
        return err_ptr::<DrmGemObject>(NEG_ENOMEM);
    }
    // The shmem initializer fills the base fields and uses our free callback.
    // SAFETY: `shmem` is non-null and points to a newly allocated `DrmGemShmemObject`.
    unsafe { (*shmem).base.funcs = ptr::addr_of!(GEM_FUNCS.0) as *mut _ };
    shmem.cast()
}

/// # Safety
///
/// `dev` must point to a live, valid `DrmDevice`.
unsafe fn allocate_gem_object(
    dev: *mut DrmDevice,
    size: usize,
    shmem_object: bool,
    state: ObjectState,
) -> Result<*mut DrmGemObject, i32> {
    let allocation_size = if shmem_object {
        size_of::<DrmGemShmemObject>()
    } else {
        size_of::<DrmGemObject>()
    };
    let obj = kzalloc(allocation_size, GFP_KERNEL).cast::<DrmGemObject>();
    if obj.is_null() {
        return Err(NEG_ENOMEM);
    }
    // SAFETY: The zeroed GEM allocation has the correct C layout; the core
    // initializer constructs locks/refcounts, and this driver then installs
    // its immutable callback table and private state before publication.
    let ret = unsafe { drm_gem_object_init(dev, obj, size) };
    if ret != 0 {
        // SAFETY: `obj` was allocated by kzalloc and is freed on init failure.
        unsafe { kfree(obj.cast::<c_void>()) };
        return Err(ret);
    }
    let private = allocate_private(shmem_object, size);
    if private.is_null() {
        // SAFETY: Releases GEM object fields and frees the allocation on private creation failure.
        unsafe {
            drm_gem_object_release(obj);
            kfree(obj.cast::<c_void>());
        }
        return Err(NEG_ENOMEM);
    }
    // SAFETY: private was freshly allocated; the caller supplied a valid
    // ObjectState value that is moved into the mutex before publication.
    unsafe {
        (*private).state = spin::Mutex::new(state);
        (*obj).funcs = ptr::addr_of!(GEM_FUNCS.0) as *mut _;
        (*obj).vma_node.driver_private = private.cast();
    }
    Ok(obj)
}

/// # Safety
///
/// - `file` and `dev` must be live DRM core objects.
/// - `args` must point to a live, writable `DrmModeCreateDumb` struct provided by DRM ioctl handler.
pub(crate) unsafe extern "C" fn dumb_create(
    file: *mut DrmFile,
    dev: *mut DrmDevice,
    args: *mut DrmModeCreateDumb,
) -> i32 {
    if file.is_null() || dev.is_null() || args.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: The DRM ioctl core passes a kernel copy of the request struct.
    let request = unsafe { &mut *args };
    if request.flags != 0
        || request.width == 0
        || request.height == 0
        || request.width > MAX_DIMENSION
        || request.height > MAX_DIMENSION
    {
        return NEG_EINVAL;
    }
    // Match virtio_gpu_mode_dumb_create: the virtio DRM dumb-buffer ABI only
    // accepts 32 bpp, even though this backend supports other scanout formats
    // through ADDFB2 and PRIME.
    if request.bpp != 32 {
        return NEG_EINVAL;
    }
    let format = PixelFormat::Xrgb8888;
    let bytes_per_pixel = format.bytes_per_pixel() as u32;
    let Some(stride) = request.width.checked_mul(bytes_per_pixel) else {
        return NEG_EINVAL;
    };
    let Some(raw_size) = (stride as usize).checked_mul(request.height as usize) else {
        return NEG_EINVAL;
    };
    let Some(size) = raw_size
        .checked_add(PAGE_SIZE - 1)
        .map(|size| size & !(PAGE_SIZE - 1))
        .filter(|size| *size != 0 && *size <= MAX_BUFFER_SIZE)
    else {
        return NEG_EINVAL;
    };
    let Some(nonzero_size) = NonZeroUsize::new(size) else {
        return NEG_EINVAL;
    };
    let mapping = match ax_gpu::allocate_mappable_backing(nonzero_size) {
        Ok(mapping) => mapping,
        Err(error) => return gpu_error_to_errno(error),
    };
    let backing = mapping.backing();
    let resource = match ax_gpu::capabilities() {
        Some(capabilities) if capabilities.supports_image_2d => {
            match ax_gpu::with_gpu(|device| {
                device.create_buffer(
                    BufferDescriptor::Image2d {
                        width: request.width,
                        height: request.height,
                        stride,
                        format,
                    },
                    backing.clone(),
                )
            }) {
                Ok(Ok(handle)) => Some(handle),
                Ok(Err(GpuError::Unsupported)) => None,
                Ok(Err(error)) | Err(error) => return gpu_error_to_errno(error),
            }
        }
        _ => None,
    };
    let mut state = ObjectState::new(size);
    state.width = request.width;
    state.height = request.height;
    state.stride = stride;
    state.format = Some(format);
    state.mapping = Some(mapping);
    state.backing = Some(backing);
    state.resource = resource;
    // SAFETY: `dev` is a live `DrmDevice`; `state` contains initialized fields and Arc references.
    let obj = match unsafe { allocate_gem_object(dev, size, false, state) } {
        Ok(obj) => obj,
        Err(error) => return error,
    };
    let mut handle = 0;
    // SAFETY: `file` and `obj` are live DRM objects; `handle` is a valid stack pointer.
    let ret = unsafe { drm_gem_handle_create(file, obj, &mut handle) };
    if ret != 0 {
        // SAFETY: Drop the reference created during allocation on handle creation failure.
        unsafe { drm_gem_object_put(obj) };
        return ret;
    }
    // SAFETY: Drop our reference; the newly created handle holds the remaining active reference.
    unsafe { drm_gem_object_put(obj) };
    request.handle = handle;
    request.pitch = stride;
    request.size = size as u64;
    0
}

/// Build the GEM shell for a RDIF-backed VirtGPU resource. Device ownership
/// remains in ax-gpu; this object retains only its RDIF handle and backing.
/// Only RDIF-owned mappable allocations have a corresponding Starry VMA.
/// Imported DMA-BUFs and host-only blobs must not receive a synthetic dumb
/// offset that the mmap adapter cannot back.
///
/// # Safety
///
/// - `file` and `dev` must be live DRM core objects.
/// - `offset` must point to a writable `u64` storage.
pub(crate) unsafe extern "C" fn dumb_map_offset(
    file: *mut DrmFile,
    dev: *mut DrmDevice,
    handle: u32,
    offset: *mut u64,
) -> i32 {
    if file.is_null() || dev.is_null() || offset.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `file` is valid; looking up `handle` returns a new reference to `DrmGemObject` if found.
    let object = unsafe { drm_gem_object_lookup(file, handle) };
    if object.is_null() {
        return NEG_ENOENT;
    }
    let mappable = !is_imported(object) && cpu_mapping(object).is_some();
    // SAFETY: Drops the reference obtained from `drm_gem_object_lookup`.
    unsafe { drm_gem_object_put(object) };
    if !mappable {
        return NEG_EINVAL;
    }
    // SAFETY: `file`, `dev`, and `offset` are valid per preconditions; forwards to DRM core helper.
    unsafe { drm_gem_dumb_map_offset(file, dev, handle, offset) }
}

/// Create a framebuffer whose allocation is embedded with its RDIF scanout
/// resource. Imported DMA-BUFs have no resource id until their framebuffer
/// supplies the image dimensions and format required by RESOURCE_CREATE_2D.
///
/// # Safety
///
/// `dev`, `file`, `format`, and `command` must point to live, initialized DRM core structures.
pub(crate) unsafe fn create_framebuffer(
    dev: *mut DrmDevice,
    file: *mut DrmFile,
    format: *const DrmFormatInfo,
    command: *const DrmModeFbCmd2,
    pixel_format: PixelFormat,
) -> *mut DrmFramebuffer {
    if dev.is_null() || file.is_null() || format.is_null() || command.is_null() {
        return err_ptr::<DrmFramebuffer>(NEG_EINVAL);
    }
    let framebuffer = kzalloc(size_of::<AxFramebuffer>(), GFP_KERNEL).cast::<AxFramebuffer>();
    if framebuffer.is_null() {
        return err_ptr::<DrmFramebuffer>(NEG_ENOMEM);
    }
    // SAFETY: `framebuffer` points to newly allocated zeroed memory of size `AxFramebuffer`.
    unsafe { ptr::addr_of_mut!((*framebuffer).scanout_resource).write(None) };
    // SAFETY: `framebuffer` is non-null and valid for writes.
    let base = unsafe { ptr::addr_of_mut!((*framebuffer).base) };
    // SAFETY: Initializes embedded DRM framebuffer struct with validated parameters.
    let ret = unsafe { ax_gem_fb_init(dev, base, file, format, command) };
    if ret != 0 {
        // SAFETY: Free the allocated AxFramebuffer on initialization failure.
        unsafe { kfree(framebuffer.cast::<c_void>()) };
        return err_ptr::<DrmFramebuffer>(ret);
    }

    // Same-device PRIME imports returned by kdrm-core's cache already have
    // this driver's GPU resource and bypass import_sg_table. Only foreign
    // DMA-BUF objects need a framebuffer-scoped 2D VirtGPU resource.
    // SAFETY: `base` is initialized and `obj[0]` holds the primary plane GEM object.
    let obj = unsafe { (*base).obj[0] };
    if is_imported(obj) {
        let resource = match create_imported_scanout_resource(base, pixel_format) {
            Ok(resource) => resource,
            Err(error) => {
                // SAFETY: Release framebuffer references on error.
                unsafe { drm_framebuffer_put(base) };
                return err_ptr::<DrmFramebuffer>(error);
            }
        };
        // SAFETY: `framebuffer` is exclusively owned during initialization.
        unsafe { (*framebuffer).scanout_resource = Some(resource) };
    }
    base
}

/// The upstream generic FB helper rejects host3D GEMs whose userspace shadow
/// allocation is smaller than the image. Match its lookup/format/FB init path
/// while applying a resource-metadata check for host3D and strict byte bounds
/// for all guest-backed/imported allocations.
///
/// # Safety
///
/// `dev`, `framebuffer`, `file`, `format`, and `command` must point to live, allocated DRM structures.
unsafe fn ax_gem_fb_init(
    dev: *mut DrmDevice,
    framebuffer: *mut DrmFramebuffer,
    file: *mut DrmFile,
    format: *const DrmFormatInfo,
    command: *const DrmModeFbCmd2,
) -> i32 {
    if dev.is_null()
        || framebuffer.is_null()
        || file.is_null()
        || format.is_null()
        || command.is_null()
    {
        return NEG_EINVAL;
    }
    // SAFETY: `format` is non-null and points to a valid `DrmFormatInfo` per caller preconditions.
    let info = unsafe { &*format };
    // SAFETY: `command` is non-null and points to a valid `DrmModeFbCmd2` per caller preconditions.
    let request = unsafe { &*command };
    // SAFETY: `dev` is valid; checks if any plane on the device supports this format.
    if info.num_planes == 0
        || info.num_planes as usize > kdrm_core::drm_framebuffer_types::DRM_FORMAT_MAX_PLANES
        || !unsafe { drm_any_plane_has_format(dev, request.pixel_format, request.modifier[0]) }
    {
        return NEG_EINVAL;
    }
    let num_planes = info.num_planes as usize;
    let mut objects = [ptr::null_mut::<DrmGemObject>(); 4];
    let mut acquired = 0usize;
    let mut error = 0;
    for (plane, object_slot) in objects.iter_mut().enumerate().take(num_planes) {
        // SAFETY: `format` is valid and plane index is checked < num_planes <= 4.
        let width = unsafe { drm_format_info_plane_width(format, request.width, plane as u32) };
        // SAFETY: `format` is valid and plane index is checked < num_planes <= 4.
        let height = unsafe { drm_format_info_plane_height(format, request.height, plane as u32) };
        // SAFETY: `file` is valid; looks up GEM object handle, returning a new reference if found.
        let object = unsafe { drm_gem_object_lookup(file, request.handles[plane]) };
        if object.is_null() {
            error = -drm_kapi::config::ENOENT;
            break;
        }
        *object_slot = object;
        acquired += 1;
        // SAFETY: `format` is valid and plane index is in bounds.
        let Ok(min_pitch) =
            usize::try_from(unsafe { drm_format_info_min_pitch(format, plane as i32, width) })
        else {
            error = -drm_kapi::config::EOVERFLOW;
            break;
        };
        let Some(required) = (height.saturating_sub(1) as usize)
            .checked_mul(request.pitches[plane] as usize)
            .and_then(|bytes| bytes.checked_add(min_pitch))
            .and_then(|bytes| bytes.checked_add(request.offsets[plane] as usize))
        else {
            error = -drm_kapi::config::EOVERFLOW;
            break;
        };
        if !framebuffer_storage_valid(object, required, width, height) {
            error = NEG_EINVAL;
            break;
        }
    }
    if error != 0 {
        for object in objects.into_iter().take(acquired) {
            // SAFETY: Drops the GEM references acquired during the loop on error.
            unsafe { drm_gem_object_put(object) };
        }
        return error;
    }
    // SAFETY: Fills `framebuffer` struct from format and request parameters.
    unsafe { drm_helper_mode_fill_fb_struct(dev, framebuffer, format, command) };
    for (plane, object) in objects.iter().enumerate().take(num_planes) {
        // SAFETY: `framebuffer` is exclusively owned during initialization; assigns acquired GEM object pointers.
        unsafe { (*framebuffer).obj[plane] = *object };
    }
    // SAFETY: Initializes `framebuffer` with the static vtable `AX_FRAMEBUFFER_FUNCS`.
    let ret =
        unsafe { drm_framebuffer_init(dev, framebuffer, ptr::addr_of!(AX_FRAMEBUFFER_FUNCS.0)) };
    if ret != 0 {
        for object in objects.into_iter().take(num_planes) {
            // SAFETY: Drops the GEM references on init failure.
            unsafe { drm_gem_object_put(object) };
        }
    }
    ret
}

fn create_imported_scanout_resource(
    framebuffer: *mut DrmFramebuffer,
    pixel_format: PixelFormat,
) -> Result<BufferHandle, i32> {
    if framebuffer.is_null()
        || !matches!(pixel_format, PixelFormat::Xrgb8888 | PixelFormat::Argb8888)
        || !ax_gpu::capabilities().is_some_and(|caps| caps.supports_image_2d)
    {
        return Err(NEG_EOPNOTSUPP);
    }
    // SAFETY: `framebuffer` is verified non-null above and points to a live `DrmFramebuffer`.
    let fb = unsafe { &*framebuffer };
    if fb.offsets[0] != 0 {
        return Err(NEG_EOPNOTSUPP);
    }
    let object = fb.obj[0];
    // SAFETY: `object` is the GEM object of plane 0 held by the live framebuffer.
    let private = unsafe { private_from_object(object) }.ok_or(NEG_EINVAL)?;
    let backing = private.state.lock().backing.clone().ok_or(NEG_EOPNOTSUPP)?;
    if backing.domain_id() != DmaDomainId::Direct {
        return Err(NEG_EOPNOTSUPP);
    }
    let descriptor = BufferDescriptor::Image2d {
        width: fb.width,
        height: fb.height,
        stride: fb.pitches[0],
        format: pixel_format,
    };
    let required = descriptor.required_len().ok_or(NEG_EINVAL)?;
    if required > private.state.lock().size {
        return Err(NEG_EINVAL);
    }
    match ax_gpu::with_gpu(|gpu| gpu.create_buffer(descriptor, backing)) {
        Ok(Ok(resource)) => Ok(resource),
        Ok(Err(error)) | Err(error) => Err(gpu_error_to_errno(error)),
    }
}

pub(crate) fn framebuffer_scanout_resource(
    framebuffer: *const DrmFramebuffer,
) -> Option<BufferHandle> {
    if framebuffer.is_null() {
        return None;
    }
    // SAFETY: `framebuffer` is non-null and points to an `AxFramebuffer` created by `create_framebuffer`.
    unsafe { (*framebuffer.cast::<AxFramebuffer>()).scanout_resource }
}

/// # Safety
///
/// `framebuffer` must be null or point to a live `AxFramebuffer` reaching final destruction.
unsafe extern "C" fn ax_framebuffer_destroy(framebuffer: *mut DrmFramebuffer) {
    if framebuffer.is_null() {
        return;
    }
    let extended = framebuffer.cast::<AxFramebuffer>();
    // SAFETY: `extended` is verified non-null and uniquely owned during destruction.
    let resource = unsafe { (*extended).scanout_resource.take() };
    if let Some(resource) = resource {
        release_resource(resource);
    }
    // SAFETY: Destroys the base GEM framebuffer via kdrm-core helper.
    unsafe { drm_gem_fb_destroy(framebuffer) };
}

/// # Safety
///
/// Pointers passed must be valid for the duration of the dirtyfb atomic helper call.
unsafe extern "C" fn ax_framebuffer_dirtyfb(
    framebuffer: *mut DrmFramebuffer,
    file_priv: *mut DrmFile,
    flags: u32,
    color: u32,
    clips: *mut kdrm_core::uapi_drm_types::DrmClipRect,
    num_clips: u32,
) -> i32 {
    // SAFETY: DIRTYFB supplies a live framebuffer/file and copied clip array;
    // the shared atomic helper owns conversion and damage-state lifetime.
    unsafe {
        kdrm_core::drm_damage_helper_upstream::drm_atomic_helper_dirtyfb(
            framebuffer,
            file_priv,
            flags,
            color,
            clips,
            num_clips,
        )
    }
}

pub(crate) struct VirglObjectInfo {
    pub(crate) size: usize,
    pub(crate) mapping: Option<Arc<MappableBacking>>,
    pub(super) host_mapping: Option<super::host_blob::HostMapping>,
    pub(crate) backing: Option<Arc<dyn Backing>>,
    pub(crate) resource: BufferHandle,
    pub(crate) resource_3d: Option<Resource3d>,
    pub(crate) context: Option<ax_gpu::rdif_gpu::ContextHandle>,
    pub(crate) resource_id: u32,
    pub(crate) blob_mem: u32,
    pub(crate) blob_flags: u32,
    pub(crate) is_blob: bool,
}

pub(crate) fn create_virgl_object(
    dev: *mut DrmDevice,
    info: VirglObjectInfo,
) -> Result<*mut DrmGemObject, i32> {
    let mut state = ObjectState::new(info.size);
    state.mapping = info.mapping;
    state.host_mapping = info.host_mapping;
    state.backing = info.backing;
    state.resource = Some(info.resource);
    state.resource_3d = info.resource_3d;
    state.context = info.context;
    state.resource_id = info.resource_id;
    state.blob_mem = info.blob_mem;
    state.blob_flags = info.blob_flags;
    state.is_blob = info.is_blob;
    if info.size == 0 {
        return Err(NEG_EINVAL);
    }
    // SAFETY: `dev` is a live `DrmDevice` pointer.
    unsafe { allocate_gem_object(dev, info.size, false, state) }
}

/// Metadata needed by the VirtGPU ioctl adapter while the caller owns a GEM
/// reference obtained from the DRM handle table.
pub(crate) fn virgl_resource_info(
    obj: *mut DrmGemObject,
) -> Option<(
    BufferHandle,
    Option<ax_gpu::rdif_gpu::ContextHandle>,
    u32,
    usize,
    u32,
    u32,
)> {
    // SAFETY: Caller holds an active reference to `obj` from handle lookup.
    let private = unsafe { private_from_object(obj) }?;
    let state = private.state.lock();
    Some((
        state.resource?,
        state.context,
        state.resource_id,
        state.size,
        state.blob_mem,
        state.blob_flags,
    ))
}

pub(crate) fn record_completion(obj: *mut DrmGemObject, completion: Completion) {
    // SAFETY: Callers retain the GEM reference returned by drm_gem_object_lookup.
    if let Some(private) = unsafe { private_from_object(obj) } {
        private.state.lock().last_completion = Some(completion);
    }
}

pub(crate) fn last_completion(obj: *mut DrmGemObject) -> Option<Completion> {
    // SAFETY: Callers retain the GEM reference returned by drm_gem_object_lookup.
    let private = unsafe { private_from_object(obj) }?;
    private.state.lock().last_completion
}

/// # Safety
///
/// `dev`, `attach`, and `sgt` must point to live, properly initialized DRM and scatterlist structures.
pub(crate) unsafe extern "C" fn prime_import_sg_table(
    dev: *mut DrmDevice,
    attach: *mut c_void,
    sgt: *mut SgTable,
) -> *mut DrmGemObject {
    if dev.is_null() || attach.is_null() || sgt.is_null() {
        return err_ptr::<DrmGemObject>(NEG_EINVAL);
    }
    let attachment = attach.cast::<DmaBufAttachment>();
    // SAFETY: The core passes a live DMA-BUF attachment to this callback; checks `dmabuf` pointer.
    if unsafe { (*attachment).dmabuf.is_null() } {
        return err_ptr::<DrmGemObject>(NEG_EINVAL);
    }
    // The kdrm-core PRIME self-export cache returns the original GEM object
    // before invoking this callback. Foreign Direct-domain DMA-BUFs retain
    // their SG backing here; the missing VirtGPU resource is created with the
    // framebuffer geometry later in `create_framebuffer`.
    // SAFETY: `attachment` is a valid `DmaBufAttachment` pointer.
    let dmabuf = unsafe { (*attachment).dmabuf };
    // SAFETY: Checks whether `dmabuf` is error pointer or `sgt->sgl` is null.
    if dmabuf.is_null() || is_err(dmabuf) || unsafe { (*sgt).sgl.is_null() } {
        return err_ptr::<DrmGemObject>(NEG_EINVAL);
    }
    let Some(capabilities) = ax_gpu::capabilities() else {
        return err_ptr::<DrmGemObject>(NEG_ENODEV);
    };
    if capabilities.dma_domain != DmaDomainId::Direct {
        return err_ptr::<DrmGemObject>(NEG_EOPNOTSUPP);
    }
    // SAFETY: `dmabuf` is verified non-null and not error; reads `size` field.
    let backing = match imported_backing(sgt, unsafe { (*dmabuf).size }, dmabuf) {
        Ok(backing) => Arc::new(backing),
        Err(error) => return err_ptr::<DrmGemObject>(error),
    };
    // SAFETY: Core helper allocates a `DrmGemShmemObject` shell for the imported attachment.
    let obj = unsafe {
        kdrm_core::drm_gem_shmem_helper_upstream::drm_gem_shmem_prime_import_sg_table(
            dev, attachment, sgt,
        )
    };
    if obj.is_null() || is_err(obj) {
        return obj;
    }
    let private = allocate_private(true, backing.len());
    if private.is_null() {
        // Leave attachment unmapping to drm_gem_prime_import_dev's error path.
        // SAFETY: `obj` was returned by `drm_gem_shmem_prime_import_sg_table` above.
        unsafe { (*obj.cast::<DrmGemShmemObject>()).sgt = ptr::null_mut() };
        // SAFETY: Release the newly allocated GEM object on private allocation failure.
        unsafe { drm_gem_object_put(obj) };
        return err_ptr::<DrmGemObject>(NEG_ENOMEM);
    }
    // SAFETY: `private` and `obj` are freshly allocated and exclusively owned here.
    unsafe {
        (*private).state = spin::Mutex::new(ObjectState {
            size: backing.len(),
            export_pages: None,
            width: 0,
            height: 0,
            stride: 0,
            format: None,
            mapping: None,
            backing: Some(backing),
            host_mapping: None,
            resource: None,
            resource_3d: None,
            context: None,
            last_completion: None,
            resource_id: 0,
            blob_mem: 0,
            blob_flags: 0,
            is_blob: false,
        });
        (*obj).funcs = ptr::addr_of!(GEM_FUNCS.0) as *mut _;
        (*obj).vma_node.driver_private = private.cast();
    }
    obj
}

fn imported_backing(
    sgt: *mut SgTable,
    size: usize,
    dmabuf: *mut DmaBuf,
) -> Result<ImportedBacking, i32> {
    if size == 0 || sgt.is_null() || dmabuf.is_null() || is_err(dmabuf) {
        return Err(NEG_EINVAL);
    }
    // SAFETY: `sgt` is verified non-null and points to a valid `SgTable`.
    let count = unsafe { (*sgt).nents };
    if count == 0 {
        return Err(NEG_EINVAL);
    }
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(count as usize)
        .map_err(|_| NEG_ENOMEM)?;
    // SAFETY: `sgt` is non-null and valid.
    let mut sg = unsafe { (*sgt).sgl };
    let mut total_len = 0usize;
    for _ in 0..count {
        if sg.is_null() {
            return Err(NEG_EINVAL);
        }
        // SAFETY: `sg` is checked non-null and points to a valid scatterlist entry.
        let address = unsafe { sg_dma_address(sg) };
        // SAFETY: `sg` is checked non-null and points to a valid scatterlist entry.
        let len = unsafe { sg_dma_len(sg) } as usize;
        let nonzero_len = NonZeroUsize::new(len).ok_or(NEG_EINVAL)?;
        total_len = total_len.checked_add(len).ok_or(NEG_EINVAL)?;
        segments.push(DmaSegment::new(DmaAddr::from(address), nonzero_len));
        // SAFETY: `sg` is valid; advances to next scatterlist node.
        sg = unsafe { sg_next(sg) };
    }
    if total_len < size {
        return Err(NEG_EINVAL);
    }
    // RDIF resources may outlive the GEM attachment, so keep the source
    // DMA-BUF allocation alive for as long as the immutable Direct backing.
    // SAFETY: `dmabuf` is non-null and valid; increments reference count.
    unsafe { kdrm_core::drm_prime_upstream::get_dma_buf(dmabuf) };
    Ok(ImportedBacking {
        size,
        segments,
        dmabuf,
    })
}

struct ImportedBacking {
    size: usize,
    segments: Vec<DmaSegment>,
    dmabuf: *mut DmaBuf,
}

impl Drop for ImportedBacking {
    fn drop(&mut self) {
        // SAFETY: `self.dmabuf` is a valid `DmaBuf` pointer whose reference was acquired via `get_dma_buf`.
        unsafe { kdrm_core::drm_dma_buf_upstream::dma_buf_put(self.dmabuf) };
    }
}

// SAFETY: `ImportedBacking` contains immutable DmaSegment vectors and an owned raw DmaBuf pointer.
unsafe impl Send for ImportedBacking {}
// SAFETY: Thread safety is guaranteed because the DMA segments and underlying buffer are read-only during usage.
unsafe impl Sync for ImportedBacking {}
// SAFETY: Implementation upholds the `Backing` contract for direct-mapped memory regions.
unsafe impl Backing for ImportedBacking {
    fn len(&self) -> usize {
        self.size
    }

    fn domain_id(&self) -> DmaDomainId {
        DmaDomainId::Direct
    }

    fn segments(&self) -> &[DmaSegment] {
        &self.segments
    }

    fn sync_for_device(&self, range: core::ops::Range<usize>) -> Result<(), GpuError> {
        if range.start <= range.end && range.end <= self.size {
            Ok(())
        } else {
            Err(GpuError::InvalidArgument)
        }
    }

    fn sync_for_cpu(&self, range: core::ops::Range<usize>) -> Result<(), GpuError> {
        self.sync_for_device(range)
    }
}

/// # Safety
///
/// `obj` must be null or point to a live `DrmGemObject` with an active reference.
unsafe extern "C" fn gem_export(obj: *mut DrmGemObject, flags: i32) -> *mut c_void {
    if obj.is_null() {
        return err_ptr::<c_void>(NEG_EINVAL);
    }
    // SAFETY: Called by the DRM core with a live GEM reference.
    unsafe { drm_gem_prime_export(obj, flags).cast() }
}

/// # Safety
///
/// `obj` must point to a live `DrmGemObject` with an active reference.
unsafe extern "C" fn gem_get_sg_table(obj: *mut DrmGemObject) -> *mut SgTable {
    // A Direct domain preserves the physical identity needed to build SG
    // pages. Translated-domain PRIME export requires a device-specific mapping.
    if !ax_gpu::capabilities().is_some_and(|caps| caps.dma_domain == DmaDomainId::Direct) {
        return err_ptr::<SgTable>(NEG_EOPNOTSUPP);
    }
    // SAFETY: DRM invokes get_sg_table with a live GEM object reference.
    let Some(private) = (unsafe { private_from_object(obj) }) else {
        return err_ptr::<SgTable>(NEG_EINVAL);
    };
    let backing = private.state.lock().backing.clone();
    let Some(backing) = backing else {
        return err_ptr::<SgTable>(NEG_EOPNOTSUPP);
    };
    if backing.domain_id() != DmaDomainId::Direct {
        return err_ptr::<SgTable>(NEG_EOPNOTSUPP);
    }
    let source = backing.segments();
    let mut segments = Vec::new();
    if segments.try_reserve_exact(source.len()).is_err() {
        return err_ptr::<SgTable>(NEG_ENOMEM);
    }
    segments.extend_from_slice(source);
    if segments.is_empty() || segments.len() > u32::MAX as usize {
        return err_ptr::<SgTable>(NEG_EINVAL);
    }
    let page_count = match sg_page_count(&segments) {
        Ok(count) if count > 0 && count <= u32::MAX as usize => count,
        Ok(_) => return err_ptr::<SgTable>(NEG_EINVAL),
        Err(error) => return err_ptr::<SgTable>(error),
    };

    // The Direct RDIF contract makes each segment address a physical address.
    // Pin actual Page metadata for these frames and retain it on the GEM
    // object; if the frame is not in the host direct map, fail PRIME export
    // rather than fabricating an SG page identity from its bus address.
    let candidate = match pin_sg_pages(&segments, page_count) {
        Ok(pages) => pages,
        Err(error) => return err_ptr::<SgTable>(error),
    };
    let mut candidate = Some(candidate);
    let (page_array, retained_page_count) = {
        let mut state = private.state.lock();
        if state.export_pages.is_none() {
            state.export_pages = candidate.take();
        }
        let Some(pages) = state.export_pages.as_ref() else {
            return err_ptr::<SgTable>(NEG_ENOMEM);
        };
        (pages.pages.as_ptr(), pages.pages.len())
    };
    // If another export won the initialization race, discard this duplicate
    // set of Page references; the installed set remains object-owned.
    drop(candidate);
    if retained_page_count != page_count {
        return err_ptr::<SgTable>(NEG_EOPNOTSUPP);
    }

    let table = kmalloc(size_of::<SgTable>(), GFP_KERNEL).cast::<SgTable>();
    if table.is_null() {
        return err_ptr::<SgTable>(NEG_ENOMEM);
    }
    // SAFETY: `table` is a newly allocated block of `size_of::<SgTable>()` bytes.
    unsafe { ptr::write_bytes(table, 0, 1) };
    // SAFETY: `table` is non-null and zeroed; allocates scatterlist table with `page_count` entries.
    let ret = unsafe { sg_alloc_table(table, page_count as u32, GFP_KERNEL) };
    if ret != 0 {
        // SAFETY: Free the allocated table storage on allocation failure.
        unsafe { kfree(table.cast::<c_void>()) };
        return err_ptr::<SgTable>(ret);
    }
    // SAFETY: `table` was successfully allocated by `sg_alloc_table`.
    let mut sg = unsafe { (*table).sgl };
    let mut page_index = 0usize;
    for segment in &segments {
        let (mut current, end) = match sg_segment_span(segment) {
            Ok(span) => span,
            Err(error) => {
                // SAFETY: Free table entries and container on failure.
                unsafe {
                    sg_free_table(table);
                    kfree(table.cast::<c_void>());
                }
                return err_ptr::<SgTable>(error);
            }
        };
        while current < end {
            if sg.is_null() || page_index >= page_count {
                // SAFETY: Free table entries and container on failure.
                unsafe {
                    sg_free_table(table);
                    kfree(table.cast::<c_void>());
                }
                return err_ptr::<SgTable>(NEG_EINVAL);
            }
            let Some((_page_base, offset, length)) = sg_page_chunk(current, end) else {
                // SAFETY: Free table entries and container on failure.
                unsafe {
                    sg_free_table(table);
                    kfree(table.cast::<c_void>());
                }
                return err_ptr::<SgTable>(NEG_EINVAL);
            };
            let (Ok(offset), Ok(length)) = (u32::try_from(offset), u32::try_from(length)) else {
                // SAFETY: Free table entries and container on failure.
                unsafe {
                    sg_free_table(table);
                    kfree(table.cast::<c_void>());
                }
                return err_ptr::<SgTable>(-drm_kapi::config::EOVERFLOW);
            };
            // SAFETY: page_array points into immutable ObjectState-owned pins;
            // the GEM reference provided to get_sg_table keeps them alive.
            let page = unsafe { *page_array.add(page_index) };
            if page.is_null() {
                // SAFETY: Free table entries and container on failure.
                unsafe {
                    sg_free_table(table);
                    kfree(table.cast::<c_void>());
                }
                return err_ptr::<SgTable>(NEG_EINVAL);
            }
            // SAFETY: `sg` is a valid pointer within `table`'s scatterlist.
            unsafe {
                (*sg).page_link = page as usize;
                (*sg).offset = offset;
                (*sg).length = length;
                (*sg).dma_address = 0;
                (*sg).dma_length = 0;
            }
            page_index += 1;
            current += length as usize;
            if page_index < page_count {
                // SAFETY: `sg` is valid; moves to next scatterlist node.
                sg = unsafe { sg_next(sg) };
            }
        }
    }
    if page_index != page_count || sg.is_null() {
        // SAFETY: Free table entries and container on failure.
        unsafe {
            sg_free_table(table);
            kfree(table.cast::<c_void>());
        }
        return err_ptr::<SgTable>(NEG_EINVAL);
    }
    // SAFETY: `sg` points to the final valid entry in the scatterlist.
    unsafe { sg_mark_end(sg) };
    table
}

fn sg_segment_span(segment: &DmaSegment) -> Result<(usize, usize), i32> {
    let start = usize::try_from(segment.addr.as_u64()).map_err(|_| -drm_kapi::config::EOVERFLOW)?;
    let end = start
        .checked_add(segment.len.get())
        .ok_or(-drm_kapi::config::EOVERFLOW)?;
    Ok((start, end))
}

fn sg_page_chunk(current: usize, end: usize) -> Option<(usize, usize, usize)> {
    if current >= end {
        return None;
    }
    let page_base = current & !(PAGE_SIZE - 1);
    let offset = current - page_base;
    let length = (end - current).min(PAGE_SIZE - offset);
    Some((page_base, offset, length))
}

fn sg_page_count(segments: &[DmaSegment]) -> Result<usize, i32> {
    let mut count = 0usize;
    for segment in segments {
        let (start, end) = sg_segment_span(segment)?;
        let last = end - 1;
        let pages = (last / PAGE_SIZE)
            .checked_sub(start / PAGE_SIZE)
            .and_then(|pages| pages.checked_add(1))
            .ok_or(-drm_kapi::config::EOVERFLOW)?;
        count = count
            .checked_add(pages)
            .ok_or(-drm_kapi::config::EOVERFLOW)?;
    }
    Ok(count)
}

fn pin_sg_pages(segments: &[DmaSegment], page_count: usize) -> Result<PinnedPages, i32> {
    let mut pins = PinnedPages { pages: Vec::new() };
    pins.pages
        .try_reserve_exact(page_count)
        .map_err(|_| NEG_ENOMEM)?;
    for segment in segments {
        let (mut current, end) = sg_segment_span(segment)?;
        while current < end {
            let Some((page_base, _offset, length)) = sg_page_chunk(current, end) else {
                return Err(NEG_EINVAL);
            };
            // SAFETY: The caller established Direct DMA identity, while the
            // live GEM backing owns this RAM range for the returned pin's life.
            let page = unsafe { drm_kapi::shmem::pin_physical_frame(page_base) };
            if page.is_null() {
                return Err(NEG_EOPNOTSUPP);
            }
            pins.pages.push(page);
            current = current
                .checked_add(length)
                .ok_or(-drm_kapi::config::EOVERFLOW)?;
        }
    }
    if pins.pages.len() != page_count {
        return Err(NEG_EINVAL);
    }
    Ok(pins)
}

/// # Safety
///
/// `obj` must be null or point to a live `DrmGemObject` whose refcount has dropped to 0 and is being freed.
unsafe extern "C" fn gem_free(obj: *mut DrmGemObject) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is non-null and being freed; reads `driver_private` pointer.
    let private_ptr = unsafe { (*obj).vma_node.driver_private.cast::<ObjectPrivate>() };
    // SAFETY: Clear `driver_private` before teardown to avoid dangling references.
    unsafe { (*obj).vma_node.driver_private = ptr::null_mut() };
    if !private_ptr.is_null() {
        // SAFETY: Metadata has a unique owner in `driver_private` and final
        // GEM release guarantees no callback still holds it.
        let private = unsafe { BoxedPrivate::from_raw(private_ptr) };
        let state = core::mem::replace(&mut *private.state.lock(), ObjectState::new(0));
        drop(state);
        if private.shmem_object {
            // SAFETY: Imported and helper-created objects have a full
            // DrmGemShmemObject allocation with the common GEM object at 0.
            unsafe { drm_gem_shmem_release(obj.cast::<DrmGemShmemObject>()) };
        } else {
            // SAFETY: Dumb objects use only the common GEM allocation and were
            // initialized by drm_gem_object_init above.
            unsafe { drm_gem_object_release(obj) };
        }
        drop(private);
        // SAFETY: Free the allocated GEM object memory block.
        unsafe { kfree(obj.cast::<c_void>()) };
        return;
    }
    // The generic shmem helper may allocate a shell before attaching our
    // private metadata. Such objects are still core-owned shmem GEM objects.
    // SAFETY: Releases shmem GEM object shell and frees the allocation.
    unsafe {
        drm_gem_shmem_release(obj.cast::<DrmGemShmemObject>());
        kfree(obj.cast::<c_void>());
    }
}

struct BoxedPrivate(*mut ObjectPrivate);

impl BoxedPrivate {
    /// # Safety
    ///
    /// `ptr` must be non-null and point to an exclusively owned, live `ObjectPrivate` allocation.
    unsafe fn from_raw(ptr: *mut ObjectPrivate) -> Self {
        Self(ptr)
    }
}

impl core::ops::Deref for BoxedPrivate {
    type Target = ObjectPrivate;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `BoxedPrivate` uniquely owns the initialized pointer.
        unsafe { &*self.0 }
    }
}

impl core::ops::DerefMut for BoxedPrivate {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: `BoxedPrivate` uniquely owns the initialized pointer.
        unsafe { &mut *self.0 }
    }
}

impl Drop for BoxedPrivate {
    fn drop(&mut self) {
        // SAFETY: This wrapper is the unique owner; drop fields before freeing
        // the KAPI allocation so Arc and mutex resources are released once.
        unsafe {
            ptr::drop_in_place(self.0);
            kfree(self.0.cast::<c_void>());
        }
    }
}

pub(crate) fn release_resource(resource: BufferHandle) {
    let mut last_retry_error = None;
    loop {
        let result = ax_gpu::with_gpu_for_cleanup(|gpu| gpu.release_buffer(resource));
        // Cleanup may drain completions without leaving another device IRQ.
        super::fence::on_gpu_completion();
        match result {
            Ok(Ok(Completion::Complete)) => return,
            Ok(Ok(completion @ Completion::Pending(_))) => loop {
                let result = ax_gpu::wait_completion(completion, ax_gpu::GPU_WAIT_TIMEOUT);
                super::fence::on_gpu_completion();
                match result {
                    Ok(()) | Err(GpuError::DeviceLost) => return,
                    Err(GpuError::TimedOut) => continue,
                    Err(error) => {
                        if last_retry_error != Some(error) {
                            log::debug!("retrying GPU GEM release wait after {error}");
                            last_retry_error = Some(error);
                        }
                        if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
                            drm_kapi::task_runtime::yield_now();
                        }
                    }
                }
            },
            Ok(Err(GpuError::Busy)) => {
                // Each attached file context owns a GEM ref; final GEM release
                // therefore occurs only after its contexts detached the
                // resource. Busy here is a transient queue/backpressure
                // condition, so service RDIF progress and retry instead of
                // dropping the last backing owner.
            }
            Ok(Err(GpuError::DeviceLost)) | Err(GpuError::DeviceLost) => return,
            Ok(Err(error)) | Err(error) => {
                if last_retry_error != Some(error) {
                    log::debug!("retrying GPU GEM release after {error}");
                    last_retry_error = Some(error);
                }
            }
        }
        if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
            drm_kapi::task_runtime::yield_now();
        }
    }
}

/// Validate the storage contract for one DRM framebuffer plane. Ordinary GEM,
/// guest-backed blobs and imported DMA-BUFs must cover every referenced byte.
/// Virgl 3D resources are host-rendered and may intentionally have only a
/// page-sized shadow GEM allocation; their accepted image geometry comes
/// from the exact RESOURCE_CREATE_3D metadata instead.
pub(crate) fn framebuffer_storage_valid(
    obj: *mut DrmGemObject,
    required: usize,
    width: u32,
    height: u32,
) -> bool {
    // SAFETY: Caller holds a live GEM reference.
    let Some(private) = (unsafe { private_from_object(obj) }) else {
        return false;
    };
    let state = private.state.lock();
    framebuffer_storage_valid_for(state.size, state.resource_3d, required, width, height)
}

fn framebuffer_storage_valid_for(
    size: usize,
    resource_3d: Option<Resource3d>,
    required: usize,
    width: u32,
    height: u32,
) -> bool {
    if let Some(resource) = resource_3d {
        resource.width == width
            && resource.height == height
            && resource.target == 2 // PIPE_TEXTURE_2D / VIRTGPU_TEXTURE_2D
            && resource.depth <= 1
            && resource.array_size <= 1
            && resource.last_level == 0
            && resource.samples <= 1
    } else {
        required <= size
    }
}

pub(crate) fn scanout_buffer(
    obj: *mut DrmGemObject,
) -> Option<ax_gpu::rdif_display::ScanoutBuffer> {
    // SAFETY: Caller holds a live GEM reference through the framebuffer.
    let private = unsafe { private_from_object(obj) }?;
    let state = private.state.lock();
    if let Some(resource) = state.resource {
        Some(ax_gpu::rdif_display::ScanoutBuffer::Gpu(resource))
    } else {
        state
            .backing
            .as_ref()
            .map(|backing| ax_gpu::rdif_display::ScanoutBuffer::Backing(backing.clone()))
    }
}

pub(crate) fn is_imported(obj: *mut DrmGemObject) -> bool {
    if obj.is_null() {
        return true;
    }
    // SAFETY: Called under an active GEM reference from the DRM mmap helper.
    unsafe { !(*obj).import_attach.is_null() }
}

pub(crate) fn gpu_error_to_errno(error: GpuError) -> i32 {
    match error {
        GpuError::Unsupported => NEG_EOPNOTSUPP,
        GpuError::NotAvailable | GpuError::DeviceLost => NEG_ENODEV,
        GpuError::InvalidArgument | GpuError::InvalidHandle => NEG_EINVAL,
        GpuError::OutOfMemory => NEG_ENOMEM,
        GpuError::Busy | GpuError::NotReady => -drm_kapi::config::EBUSY,
        _ => NEG_EIO,
    }
}

/// CPU-visible storage retained by the object's GEM reference. Host-visible
/// BAR pages are not DMA backing and must never be exported as normal SG RAM.
pub(crate) struct CpuMapping {
    pub(crate) physical: ax_memory_addr::PhysAddrRange,
    pub(crate) cached: bool,
}

pub(crate) fn cpu_mapping(obj: *mut DrmGemObject) -> Option<CpuMapping> {
    // SAFETY: The caller holds a live GEM reference throughout mapping lookup.
    let private = unsafe { private_from_object(obj) }?;
    let state = private.state.lock();
    if let Some(host) = &state.host_mapping {
        Some(CpuMapping {
            physical: host.physical,
            cached: host.cached,
        })
    } else {
        state.mapping.as_ref().map(|mapping| CpuMapping {
            physical: mapping.physical(),
            cached: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sg_page_ranges_split_at_physical_page_boundaries() {
        let start = PAGE_SIZE - 16;
        let end = start + PAGE_SIZE + 32;
        assert_eq!(sg_page_chunk(start, end), Some((0, PAGE_SIZE - 16, 16)));
        assert_eq!(
            sg_page_chunk(start + 16, end),
            Some((PAGE_SIZE, 0, PAGE_SIZE))
        );
        assert_eq!(
            sg_page_chunk(start + 16 + PAGE_SIZE, end),
            Some((PAGE_SIZE * 2, 0, 16))
        );
        assert_eq!(sg_page_chunk(end, end), None);

        let segment = DmaSegment::new(
            DmaAddr::from(start as u64),
            NonZeroUsize::new(end - start).unwrap(),
        );
        assert_eq!(sg_page_count(&[segment]), Ok(3));
    }

    #[test]
    fn host3d_framebuffer_uses_resource_geometry_not_shadow_gem_size() {
        let host3d = Resource3d {
            target: 2,
            format: 2,
            bind: 0,
            width: 1920,
            height: 1080,
            depth: 1,
            array_size: 1,
            last_level: 0,
            samples: 1,
            flags: 0,
        };
        let framebuffer_bytes = 1920usize * 4 * 1080;
        assert!(framebuffer_storage_valid_for(
            PAGE_SIZE,
            Some(host3d),
            framebuffer_bytes,
            1920,
            1080,
        ));
        assert!(!framebuffer_storage_valid_for(
            PAGE_SIZE,
            Some(host3d),
            framebuffer_bytes,
            1280,
            720,
        ));
        assert!(!framebuffer_storage_valid_for(
            PAGE_SIZE,
            None,
            framebuffer_bytes,
            1920,
            1080,
        ));
    }
}

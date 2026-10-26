// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! VirtIO GPU private DRM ioctls translated to the active RDIF device.

use alloc::{format, vec::Vec};
use core::{
    ffi::c_void,
    mem::size_of,
    num::NonZeroUsize,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_gpu::rdif_gpu::{
    BlobDescriptor, BufferHandle, Completion, ContextHandle, GpuCapabilities, GpuError, Resource3d,
    Transfer3d, TransferBox, VirglOps,
};
use drm_kapi::{
    config::{
        EBUSY, EFAULT, EINVAL, EIO, ENODEV, ENOENT, ENOMEM, EOPNOTSUPP, ETIME, GFP_KERNEL,
        PAGE_SIZE,
    },
    dma_fence::dma_fence_put,
    kernel_services::{fd_install, get_unused_fd_flags, put_unused_fd},
    memory::{kfree, kmalloc},
    mm_native::{copy_from_user, copy_to_user},
    types::DmaFence,
};
use kdrm_core::{
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_gem_types::DrmGemObject,
    drm_gem_upstream::{
        drm_gem_create_mmap_offset, drm_gem_handle_create, drm_gem_handle_delete,
        drm_gem_object_get, drm_gem_object_lookup, drm_gem_object_put,
    },
    drm_ioctl_types::{DRM_RENDER_ALLOW, DrmIoctlDesc},
    drm_syncobj_upstream::{
        DrmSyncobj, drm_syncobj_find, drm_syncobj_find_fence, drm_syncobj_put,
        drm_syncobj_replace_fence, drm_syncobj_sync_file_create, drm_syncobj_sync_file_file,
        drm_syncobj_sync_file_get_fence, drm_syncobj_wait_fence,
    },
};

use super::{fence, gem, virtgpu_uapi::*};

const MAX_COMMAND: usize = 16 * 1024 * 1024;
const MAX_HANDLES: usize = 4096;
const MAX_RESOURCE_SIZE: usize = 256 * 1024 * 1024;
const MAX_CAPSET_ENUM: u32 = 64;
const NEG_EFAULT: i32 = -EFAULT;
const NEG_EINVAL: i32 = -EINVAL;
const NEG_EBUSY: i32 = -EBUSY;
const NEG_EIO: i32 = -EIO;
const NEG_ENODEV: i32 = -ENODEV;
const NEG_ENOENT: i32 = -ENOENT;
const NEG_ENOMEM: i32 = -ENOMEM;
const NEG_EOPNOTSUPP: i32 = -EOPNOTSUPP;
const NEG_ETIME: i32 = -ETIME;
const DMA_RESV_USAGE_READ: u32 = 2;
static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);

#[repr(C)]
struct FilePrivate {
    state: spin::Mutex<FileState>,
}

struct FileState {
    context: Option<ContextHandle>,
    context_init: u32,
    num_rings: u32,
    creating_context: bool,
    attached: *mut AttachedResource,
}

#[repr(C)]
struct AttachedResource {
    handle: Option<BufferHandle>,
    handles: usize,
    attaching: bool,
    object: *mut DrmGemObject,
    next: *mut AttachedResource,
}

struct ExecOutputSyncobj {
    object: *mut DrmSyncobj,
    point: u64,
}

// `drm_ioctl()` indexes driver ioctls by `nr - DRM_COMMAND_BASE`. Keep the
// unused slot 0 so the Linux command number is also the descriptor index.
pub(crate) static IOCTLS: [DrmIoctlDesc; 12] = [
    DrmIoctlDesc {
        cmd: 0,
        flags: 0,
        func: ptr::null_mut(),
        name: ptr::null_mut(),
    },
    desc(DRM_IOCTL_VIRTGPU_MAP, ioctl_map),
    desc(DRM_IOCTL_VIRTGPU_EXECBUFFER, ioctl_execbuffer),
    desc(DRM_IOCTL_VIRTGPU_GETPARAM, ioctl_getparam),
    desc(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE, ioctl_resource_create),
    desc(DRM_IOCTL_VIRTGPU_RESOURCE_INFO, ioctl_resource_info),
    desc(
        DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST,
        ioctl_transfer_from_host,
    ),
    desc(DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST, ioctl_transfer_to_host),
    desc(DRM_IOCTL_VIRTGPU_WAIT, ioctl_wait),
    desc(DRM_IOCTL_VIRTGPU_GET_CAPS, ioctl_get_caps),
    desc(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, ioctl_blob_create),
    desc(DRM_IOCTL_VIRTGPU_CONTEXT_INIT, ioctl_context_init),
];

const fn desc(
    cmd: u32,
    function: unsafe extern "C" fn(*mut DrmDevice, *mut c_void, *mut DrmFile) -> i32,
) -> DrmIoctlDesc {
    DrmIoctlDesc {
        cmd,
        flags: DRM_RENDER_ALLOW,
        func: function as *const () as *mut c_void,
        name: ptr::null_mut(),
    }
}

/// # Safety
///
/// If non-null, `file` must point to an active, valid `DrmFile` whose memory is
/// accessible and whose `driver_priv` field is managed exclusively by this driver.
pub(crate) unsafe extern "C" fn open(_dev: *mut DrmDevice, file: *mut DrmFile) -> i32 {
    if file.is_null() {
        return NEG_EINVAL;
    }
    let private = kmalloc(size_of::<FilePrivate>(), GFP_KERNEL).cast::<FilePrivate>();
    if private.is_null() {
        return NEG_ENOMEM;
    }
    // SAFETY: `private` points to freshly allocated heap memory sized and aligned
    // for `FilePrivate`. `ptr::write` initializes it without dropping uninitialized
    // memory. `file` was validated non-null and points to an open `DrmFile` where
    // `driver_priv` can be initialized.
    unsafe {
        ptr::write(
            private,
            FilePrivate {
                state: spin::Mutex::new(FileState {
                    context: None,
                    context_init: 0,
                    num_rings: 1,
                    creating_context: false,
                    attached: ptr::null_mut(),
                }),
            },
        );
        (*file).driver_priv = private.cast();
    }
    0
}

/// # Safety
///
/// If non-null, `file` must point to a valid `DrmFile` undergoing teardown by kdrm-core,
/// with no concurrent calls using `file->driver_priv`.
pub(crate) unsafe extern "C" fn postclose(_dev: *mut DrmDevice, file: *mut DrmFile) {
    if file.is_null() {
        return;
    }
    // SAFETY: `file` was checked non-null and points to a valid `DrmFile`.
    // Reading `driver_priv` accesses driver state allocated in `open`.
    let private = unsafe { (*file).driver_priv.cast::<FilePrivate>() };
    if private.is_null() {
        return;
    }
    // SAFETY: `file` is valid and non-null; resetting `driver_priv` detaches private
    // state before destruction to prevent any subsequent use.
    unsafe { (*file).driver_priv = ptr::null_mut() };
    let (context, mut attached) = {
        // SAFETY: `private` was allocated in `open` and remains valid until dropped and freed below.
        let mut state = unsafe { &*private }.state.lock();
        let attached = state.attached;
        state.attached = ptr::null_mut();
        (state.context, attached)
    };
    if let Some(context) = context {
        destroy_context_before_drop(context);
    }
    // The attachment references keep each GEM object (and its RDIF resource)
    // alive until this file's context has released all host-side attachments.
    while !attached.is_null() {
        let node = attached;
        // SAFETY: `node` is non-null and points to a valid `AttachedResource` allocated
        // by `attach_resource`. Teardown is serialized, so reading `node` is safe.
        let entry = unsafe { node.read() };
        attached = entry.next;
        // SAFETY: `entry.object` holds a reference acquired during `attach_resource`.
        // Releasing it via `drm_gem_object_put` balances the reference count.
        // `node` was allocated via `kmalloc` and is freed with `kfree`.
        unsafe {
            drm_gem_object_put(entry.object);
            kfree(node.cast::<c_void>());
        }
    }
    // SAFETY: `private` was allocated with `kmalloc` and holds a valid `FilePrivate`.
    // It is no longer referenced anywhere; running drop_in_place destroys the mutex
    // and `kfree` deallocates the backing storage.
    unsafe {
        ptr::drop_in_place(private);
        kfree(private.cast::<c_void>());
    }
}

/// Register each handle before it becomes visible in the core's IDR. The
/// file-local count is independent of global handles, PRIME and VMA owners.
///
/// # Safety
/// The core retains both the object and the open file throughout this hook.
pub(crate) unsafe extern "C" fn gem_open(obj: *mut DrmGemObject, file: *mut DrmFile) -> i32 {
    if obj.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: The caller retains the live file, initialized by `open`.
    let Ok(private) = (unsafe { file_private(file) }) else {
        return NEG_EINVAL;
    };
    let node = kmalloc(size_of::<AttachedResource>(), GFP_KERNEL).cast::<AttachedResource>();
    if node.is_null() {
        return NEG_ENOMEM;
    }
    let mut state = private.state.lock();
    // SAFETY: The state lock protects the list and every node until removal.
    unsafe {
        let found = find_attachment(&state, obj);
        if !found.is_null() {
            let Some(count) = (*found).handles.checked_add(1) else {
                drop(state);
                kfree(node);
                return NEG_ENOMEM;
            };
            (*found).handles = count;
            drop(state);
            kfree(node);
            return 0;
        }
        drm_gem_object_get(obj);
        node.write(AttachedResource {
            handle: None,
            handles: 1,
            attaching: false,
            object: obj,
            next: state.attached,
        });
        state.attached = node;
    }
    0
}

/// # Safety
/// The state lock is held; `object` is compared by identity, not dereferenced.
unsafe fn find_attachment(state: &FileState, object: *mut DrmGemObject) -> *mut AttachedResource {
    let mut node = state.attached;
    while !node.is_null() {
        // SAFETY: All nodes remain allocated while the state lock is held.
        unsafe {
            if (*node).object == object {
                return node;
            }
            node = (*node).next;
        }
    }
    ptr::null_mut()
}

/// # Safety
/// The state lock is held and `node` is a linked member with no handles or
/// in-flight attachment. The caller consumes the removed allocation.
unsafe fn unlink_attachment(state: &mut FileState, node: *mut AttachedResource) {
    let mut link = ptr::addr_of_mut!(state.attached);
    // SAFETY: Traversal and mutation are serialized by the state lock.
    unsafe {
        while !(*link).is_null() {
            if *link == node {
                *link = (*node).next;
                return;
            }
            link = ptr::addr_of_mut!((**link).next);
        }
    }
}

/// Consume one file-local handle and release an unattached last keepalive.
/// Returns true only when device serialization is required for a host attachment.
///
/// # Safety
/// `private` belongs to the live file and `obj` is retained by the DRM core.
unsafe fn close_handle(private: &FilePrivate, obj: *mut DrmGemObject) -> bool {
    let mut state = private.state.lock();
    // SAFETY: The state lock protects traversal and every node field.
    unsafe {
        let node = find_attachment(&state, obj);
        if node.is_null() || (*node).handles == 0 {
            return false;
        }
        (*node).handles -= 1;
        if (*node).handles != 0 {
            return false;
        }
        if (*node).handle.is_some() || (*node).attaching {
            return true;
        }
        unlink_attachment(&mut state, node);
        drop(state);
        // Final GEM release can reenter the device owner; never hold the
        // short file-state lock while dropping this detached reference.
        drm_gem_object_put((*node).object);
        kfree(node);
    }
    false
}

/// Last file-local handle close detaches before releasing the attachment's
/// keepalive. Active submissions, VMAs and other files retain their own refs.
///
/// # Safety
/// The core retains the object through this hook, and the file is still open.
pub(crate) unsafe extern "C" fn gem_close(obj: *mut DrmGemObject, file: *mut DrmFile) {
    if obj.is_null() || file.is_null() {
        return;
    }
    // SAFETY: The caller retains the initialized file throughout this callback.
    let Ok(private) = (unsafe { file_private(file) }) else {
        return;
    };
    // SAFETY: Both the file state and object remain live through this hook.
    if !unsafe { close_handle(private, obj) } {
        return;
    }
    let retired = {
        // Use the same device lock as attach_resource. Never take a sleeping
        // GPU lock while holding the short file-state spinlock.
        ax_gpu::with_gpu_for_cleanup(|gpu| {
            let (node, resource, context) = {
                let state = private.state.lock();
                // SAFETY: The file state protects all node fields.
                let node = unsafe { find_attachment(&state, obj) };
                if node.is_null() || unsafe { (*node).handles != 0 } {
                    return Ok(ptr::null_mut());
                }
                // SAFETY: Attach completed before the GPU lock was acquired.
                // SAFETY: Keep even an unattached node pinned while this
                // cleanup temporarily releases the state lock.
                unsafe {
                    (*node).attaching = true;
                }
                (node, unsafe { (*node).handle }, state.context)
            };
            if let Some(resource) = resource {
                let virgl = gpu.virgl().ok_or(GpuError::Unsupported)?;
                let result =
                    virgl.detach_resource(context.ok_or(GpuError::InvalidHandle)?, resource);
                virgl.ctrl_notify();
                if let Err(error) = result {
                    let _state = private.state.lock();
                    // SAFETY: The pending-operation flag pins this node.
                    unsafe {
                        (*node).attaching = false;
                    }
                    return Err(error);
                }
            }
            let mut state = private.state.lock();
            // SAFETY: No other last-close owns this node, and attach/detach are
            // serialized by the GPU lock. A new open can only increase handles.
            unsafe {
                (*node).handle = None;
                (*node).attaching = false;
                if (*node).handles != 0 {
                    return Ok(ptr::null_mut());
                }
                unlink_attachment(&mut state, node);
            }
            Ok(node)
        })
        .and_then(|result| result)
    };
    // Publish progress pumped by cleanup, after releasing the GPU lock.
    fence::on_gpu_completion();
    match retired {
        Ok(node) if !node.is_null() => {
            // SAFETY: The removed node owns one GEM reference. Drop outside
            // both locks because final GEM release can reenter the GPU owner.
            unsafe {
                drm_gem_object_put((*node).object);
                kfree(node);
            }
        }
        Ok(_) => {}
        Err(error) => {
            // A failed detach cannot authorize releasing device-owned storage.
            // Keep the zero-handle entry until context teardown confirms safety.
            log::warn!("retaining VirtGPU attachment after detach failure: {error}");
        }
    }
}

/// # Safety
/// `file` remains live throughout the returned borrow; its driver-private
/// pointer is immutable between open and final postclose. Other core fields
/// may change concurrently, so do not borrow the whole `DrmFile`.
unsafe fn file_private<'a>(file: *const DrmFile) -> Result<&'a FilePrivate, i32> {
    if file.is_null() {
        return Err(NEG_EINVAL);
    }
    // SAFETY: The caller retains the file through every private-state access.
    let private = unsafe { (*file).driver_priv.cast::<FilePrivate>() };
    // SAFETY: Open installed this allocation before publishing the file;
    // postclose cannot run while the caller retains the file.
    unsafe { private.as_ref() }.ok_or(NEG_EINVAL)
}

pub(super) fn with_virgl<T>(
    operation: impl FnOnce(&mut dyn VirglOps) -> Result<T, GpuError>,
) -> Result<T, i32> {
    ax_gpu::with_gpu(|device| {
        device
            .virgl()
            .ok_or(GpuError::Unsupported)
            .and_then(operation)
    })
    .map_err(gem::gpu_error_to_errno)?
    .map_err(gem::gpu_error_to_errno)
}

fn capabilities() -> Result<GpuCapabilities, i32> {
    let identity = ax_gpu::identity().ok_or(NEG_ENODEV)?;
    if identity.driver_name != "virtio_gpu" {
        return Err(NEG_EOPNOTSUPP);
    }
    ax_gpu::capabilities().ok_or(NEG_ENODEV)
}

fn context(file: *mut DrmFile) -> Result<Option<ContextHandle>, i32> {
    // SAFETY: If non-null, `file` points to an active `DrmFile` structure provided by DRM core.
    let context = unsafe { file_private(file) }?.state.lock().context;
    Ok(context)
}

#[derive(Clone, Copy)]
struct ContextConfig {
    capset_id: u32,
    num_rings: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            capset_id: 0,
            num_rings: 1,
        }
    }
}

impl ContextConfig {
    fn parse(params: &[DrmVirtgpuContextSetParam]) -> Result<Self, i32> {
        let mut config = Self::default();
        let mut poll_mask = 0u64;
        let mut seen = 0u32;
        for param in params {
            if !(1..=3).contains(&param.param) || seen & (1 << param.param) != 0 {
                return Err(NEG_EINVAL);
            }
            seen |= 1 << param.param;
            match param.param {
                VIRTGPU_CONTEXT_PARAM_CAPSET_ID => {
                    config.capset_id = u32::try_from(param.value).map_err(|_| NEG_EINVAL)?;
                }
                VIRTGPU_CONTEXT_PARAM_NUM_RINGS if (1..=64).contains(&param.value) => {
                    config.num_rings = param.value as u32;
                }
                VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK => poll_mask = param.value,
                _ => return Err(NEG_EINVAL),
            }
        }
        if config.capset_id == 0 || (config.num_rings < 64 && poll_mask >> config.num_rings != 0) {
            return Err(NEG_EINVAL);
        }
        // The adapter already tracks completions for every submitted ring;
        // a poll mask changes notification preference, not fence ownership.
        Ok(config)
    }
}

fn create_context(
    file: *mut DrmFile,
    config: ContextConfig,
    explicit: bool,
) -> Result<ContextHandle, i32> {
    // SAFETY: If non-null, `file` points to an active `DrmFile` structure provided by DRM core.
    let private = unsafe { file_private(file) }?;
    if !capabilities()?.supports_3d {
        return Err(NEG_EOPNOTSUPP);
    }
    {
        let mut state = private.state.lock();
        if let Some(context) = state.context {
            return if explicit {
                Err(-drm_kapi::config::EEXIST)
            } else {
                Ok(context)
            };
        }
        if state.creating_context {
            return Err(NEG_EBUSY);
        }
        state.creating_context = true;
    }
    let name = format!(
        "starry-ctx-{}",
        NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed)
    );
    let created = with_virgl(|virgl| {
        let result = virgl.create_context(&name, config.capset_id);
        virgl.ctrl_notify();
        result
    });
    let mut state = private.state.lock();
    state.creating_context = false;
    match created {
        Ok(context) => {
            state.context = Some(context);
            state.context_init = config.capset_id;
            state.num_rings = config.num_rings;
            Ok(context)
        }
        Err(error) => Err(error),
    }
}

fn ensure_context(file: *mut DrmFile) -> Result<ContextHandle, i32> {
    if let Some(context) = context(file)? {
        Ok(context)
    } else {
        create_context(file, ContextConfig::default(), false)
    }
}

fn attach_resource(
    file: *mut DrmFile,
    context: ContextHandle,
    resource: BufferHandle,
    object: *mut DrmGemObject,
) -> Result<(), i32> {
    // SAFETY: The ioctl owns the live file and an independent GEM lookup ref.
    let private = unsafe { file_private(file) }?;
    with_virgl(|virgl| {
        let node = {
            let state = private.state.lock();
            // SAFETY: The state lock pins the node during lookup and mutation.
            let node = unsafe { find_attachment(&state, object) };
            if node.is_null() || unsafe { (*node).handles == 0 } {
                return Err(GpuError::InvalidHandle);
            }
            // SAFETY: Mark the node before releasing the lock. Last close must
            // then acquire the GPU lock and cannot free it across this request.
            unsafe {
                if (*node).handle.is_some() {
                    return Ok(());
                }
                (*node).attaching = true;
            }
            node
        };
        let result = virgl.attach_resource(context, resource);
        virgl.ctrl_notify();
        let _state = private.state.lock();
        // SAFETY: The GPU lock and attaching flag keep the node allocated.
        unsafe {
            (*node).attaching = false;
            if result.is_ok() {
                (*node).handle = Some(resource);
            }
        }
        result
    })
}

/// Destroy a closing file's context before dropping the GEM references that
/// keep its attached resources/backing alive. VirtIO RDIF removes each
/// accepted detach from its table before returning Busy, so retry is safe.
fn destroy_context_before_drop(context: ContextHandle) {
    let mut logged = None;
    loop {
        let result = ax_gpu::with_gpu_for_cleanup(|device| {
            let virgl = device.virgl().ok_or(GpuError::Unsupported)?;
            let result = virgl.destroy_context(context);
            virgl.ctrl_notify();
            result
        });
        // Cleanup may drain completions without leaving another device IRQ.
        fence::on_gpu_completion();
        match result {
            Ok(Ok(())) => return,
            Ok(Err(GpuError::Busy)) => {
                if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
                    drm_kapi::task_runtime::yield_now();
                }
            }
            // RDIF guarantees DeviceLost means the device has stopped all DMA.
            Ok(Err(GpuError::DeviceLost)) | Err(GpuError::DeviceLost) => return,
            // The context was created by this adapter and cannot normally
            // disappear except after successful destruction or device loss.
            // Preserve bounded diagnostics without spinning on an invariant
            // violation; the driver must not release the attached GEM refs.
            Ok(Err(error)) => {
                if logged != Some(error) {
                    log::error!("VirtGPU context cleanup stalled: {error}");
                    logged = Some(error);
                }
                if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
                    drm_kapi::task_runtime::yield_now();
                }
            }
            Err(error) => {
                if error != GpuError::NotAvailable && logged != Some(GpuError::NotAvailable) {
                    log::error!("VirtGPU context cleanup unavailable: errno {error}");
                    logged = Some(GpuError::NotAvailable);
                }
                if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
                    drm_kapi::task_runtime::yield_now();
                }
            }
        }
    }
}

fn copy_from_user_array<T: Copy + Default>(
    pointer: u64,
    count: u32,
    limit: usize,
) -> Result<Vec<T>, i32> {
    if count as usize > limit {
        return Err(NEG_EINVAL);
    }
    if count != 0 && pointer == 0 {
        return Err(NEG_EFAULT);
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(count as usize)
        .map_err(|_| NEG_ENOMEM)?;
    for index in 0..count as usize {
        let address = pointer
            .checked_add((index * size_of::<T>()) as u64)
            .ok_or(NEG_EINVAL)?;
        let mut value = T::default();
        // SAFETY: `&mut value` points to a live local stack variable with size `size_of::<T>()`.
        // `copy_from_user` safely handles userspace pointer validation and returns non-zero on fault.
        if unsafe {
            copy_from_user(
                (&mut value as *mut T).cast(),
                address as usize as *const c_void,
                size_of::<T>(),
            )
        } != 0
        {
            return Err(NEG_EFAULT);
        }
        values.push(value);
    }
    Ok(values)
}

fn supported_capset(id: u32, version: u32) -> Result<ax_gpu::rdif_gpu::CapsetInfo, i32> {
    if !capabilities()?.supports_3d {
        return Err(NEG_EOPNOTSUPP);
    }
    for index in 0..MAX_CAPSET_ENUM {
        match with_virgl(|virgl| virgl.capset_info(index)) {
            Ok(info) if info.id == id && info.max_version >= version => return Ok(info),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Err(NEG_EINVAL)
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpuGetparam` ioctl buffer.
unsafe extern "C" fn ioctl_getparam(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    _file: *mut DrmFile,
) -> i32 {
    if data.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: drm ioctl core guarantees `data` points to a valid kernel-space
    // `DrmVirtgpuGetparam` structure.
    let request = unsafe { *data.cast::<DrmVirtgpuGetparam>() };
    let caps = match capabilities() {
        Ok(caps) => caps,
        Err(error) => return error,
    };
    // Linux `virtgpu_getparam_ioctl()` writes an `int` through this user
    // pointer, even though the pointer itself is carried in a `u64` field.
    // Writing eight bytes here corrupts the adjacent userspace object for
    // callers that provide the upstream `int *` buffer.
    let value: u32 = match request.param {
        VIRTGPU_PARAM_3D_FEATURES => u32::from(caps.supports_3d),
        VIRTGPU_PARAM_CAPSET_QUERY_FIX => 1,
        VIRTGPU_PARAM_RESOURCE_BLOB => u32::from(caps.supports_blob),
        VIRTGPU_PARAM_HOST_VISIBLE => u32::from(
            caps.supports_blob
                && caps.supports_context_init
                && with_virgl(|virgl| Ok(virgl.host_memory_region().is_some())).unwrap_or(false),
        ),
        VIRTGPU_PARAM_CROSS_DEVICE => 0,
        VIRTGPU_PARAM_CONTEXT_INIT => u32::from(caps.supports_context_init),
        VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS => {
            if !caps.supports_3d {
                0
            } else {
                let mut mask = 0u64;
                for index in 0..MAX_CAPSET_ENUM {
                    let Ok(info) = with_virgl(|virgl| virgl.capset_info(index)) else {
                        break;
                    };
                    if info.id < u64::BITS {
                        mask |= 1u64 << info.id;
                    }
                }
                // Match the upstream `int value` temporary/copy size.
                mask as u32
            }
        }
        _ => return NEG_EINVAL,
    };
    if request.value == 0
        // SAFETY: `&value` points to a valid stack `u32` of size `size_of::<u32>()`.
        // `copy_to_user` validates the userspace address `request.value` and returns non-zero on fault.
        || unsafe {
            copy_to_user(
                request.value as usize as *mut c_void,
                (&value as *const u32).cast(),
                size_of::<u32>(),
            )
        } != 0
    {
        return NEG_EFAULT;
    }
    0
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpuGetCaps` ioctl buffer.
unsafe extern "C" fn ioctl_get_caps(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    _file: *mut DrmFile,
) -> i32 {
    if data.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: drm ioctl core guarantees `data` points to a valid kernel-space
    // `DrmVirtgpuGetCaps` structure.
    let request = unsafe { *data.cast::<DrmVirtgpuGetCaps>() };
    if request.size == 0 {
        return NEG_EINVAL;
    }
    if request.addr == 0 {
        return NEG_EFAULT;
    }
    let info = match supported_capset(request.cap_set_id, request.cap_set_ver) {
        Ok(info) => info,
        Err(error) => return error,
    };
    let contents = match with_virgl(|virgl| {
        virgl.capset(request.cap_set_id, request.cap_set_ver, info.max_size)
    }) {
        Ok(contents) => contents,
        Err(error) => return error,
    };
    let copy_len = (request.size as usize).min(contents.len());
    if copy_len == 0
        // SAFETY: `contents` is a valid Vec buffer with at least `copy_len` bytes.
        // `copy_to_user` validates userspace address `request.addr` and returns non-zero on fault.
        || unsafe {
            copy_to_user(
                request.addr as usize as *mut c_void,
                contents.as_ptr().cast(),
                copy_len,
            )
        } != 0
    {
        return NEG_EFAULT;
    }
    0
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpuContextInit` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_context_init(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a valid `DrmVirtgpuContextInit`
    // buffer managed by drm ioctl core.
    let request = unsafe { *data.cast::<DrmVirtgpuContextInit>() };
    let caps = match capabilities() {
        Ok(caps) => caps,
        Err(error) => return error,
    };
    if !caps.supports_context_init || !caps.supports_3d || request.num_params > 3 {
        return NEG_EINVAL;
    }
    let params = match copy_from_user_array::<DrmVirtgpuContextSetParam>(
        request.ctx_set_params,
        request.num_params,
        3,
    ) {
        Ok(params) => params,
        Err(error) => return error,
    };
    let config = match ContextConfig::parse(&params) {
        Ok(config) => config,
        Err(error) => return error,
    };
    // Venus advertises capset version 0; max_version is not a requirement to
    // support version 1. Probe the version-independent capset ID here.
    if supported_capset(config.capset_id, 0).is_err() {
        return NEG_EINVAL;
    }
    match create_context(file, config, true) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// # Safety
///
/// If non-null, `dev` must point to an active `DrmDevice`, `data` must point to a
/// valid kernel-space `DrmVirtgpuResourceCreate` ioctl buffer, and `file` must point
/// to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_resource_create(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if dev.is_null() || data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel `DrmVirtgpuResourceCreate` buffer.
    let mut request = unsafe { *data.cast::<DrmVirtgpuResourceCreate>() };
    if !capabilities().is_ok_and(|caps| caps.supports_3d) {
        return NEG_EOPNOTSUPP;
    }
    let context = match ensure_context(file) {
        Ok(context) => context,
        Err(error) => return error,
    };
    let requested = (request.size as usize).max(PAGE_SIZE);
    let Some(size) = requested
        .checked_add(PAGE_SIZE - 1)
        .map(|size| size & !(PAGE_SIZE - 1))
        .filter(|size| *size <= MAX_RESOURCE_SIZE)
    else {
        return NEG_EINVAL;
    };
    let Some(nonzero) = NonZeroUsize::new(size) else {
        return NEG_EINVAL;
    };
    let mapping = match ax_gpu::allocate_mappable_backing(nonzero) {
        Ok(mapping) => mapping,
        Err(error) => return gem::gpu_error_to_errno(error),
    };
    let backing = mapping.backing();
    let desc = Resource3d {
        target: request.target,
        format: request.format,
        bind: request.bind,
        width: request.width,
        height: request.height,
        depth: request.depth,
        array_size: request.array_size,
        last_level: request.last_level,
        samples: request.nr_samples,
        flags: request.flags,
    };
    let created = with_virgl(|virgl| {
        let resource = virgl.create_resource_3d(desc, Some(backing.clone()))?;
        virgl.ctrl_notify();
        let id = virgl.command_resource_id(resource)?;
        Ok((resource, id))
    });
    let (resource, id) = match created {
        Ok(value) => value,
        Err(error) => return error,
    };
    let obj = match gem::create_virgl_object(
        dev,
        gem::VirglObjectInfo {
            size,
            mapping: Some(mapping),
            host_mapping: None,
            backing: Some(backing),
            resource,
            resource_3d: Some(desc),
            context: Some(context),
            resource_id: id,
            blob_mem: 0,
            blob_flags: 0,
            is_blob: false,
        },
    ) {
        Ok(obj) => obj,
        Err(error) => return error,
    };
    let mut handle = 0;
    // SAFETY: `file` and `obj` are valid pointers; `&mut handle` is a valid out-pointer.
    let ret = unsafe { drm_gem_handle_create(file, obj, &mut handle) };
    if ret != 0 {
        // SAFETY: `obj` holds an initial refcount from creation that must be dropped on failure.
        unsafe { drm_gem_object_put(obj) };
        return ret;
    }
    if let Err(error) = attach_resource(file, context, resource, obj) {
        // SAFETY: `file` is valid; deleting the handle cleans up the newly registered handle.
        unsafe { drm_gem_handle_delete(file, handle) };
        // SAFETY: Releasing initial refcount from creation.
        unsafe { drm_gem_object_put(obj) };
        return error;
    }
    // SAFETY: Releasing the initial reference from `create_virgl_object`, leaving
    // the reference held by the handle table and attachment.
    unsafe { drm_gem_object_put(obj) };
    request.bo_handle = handle;
    request.res_handle = id;
    request.size = size.min(u32::MAX as usize) as u32;
    // SAFETY: `data` was validated non-null and points to a writable `DrmVirtgpuResourceCreate` buffer.
    unsafe { *data.cast::<DrmVirtgpuResourceCreate>() = request };
    0
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpuResourceInfo` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_resource_info(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel `DrmVirtgpuResourceInfo` buffer.
    let mut request = unsafe { *data.cast::<DrmVirtgpuResourceInfo>() };
    // SAFETY: `file` is valid and non-null; `drm_gem_object_lookup` returns a referenced
    // GEM object pointer if found, or null.
    let obj = unsafe { drm_gem_object_lookup(file, request.bo_handle) };
    if obj.is_null() {
        return NEG_ENOENT;
    }
    let info = gem::virgl_resource_info(obj);
    // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
    unsafe { drm_gem_object_put(obj) };
    let Some((_, _, resource_id, size, blob_mem, _)) = info else {
        return NEG_EINVAL;
    };
    request.res_handle = resource_id;
    request.size = size.min(u32::MAX as usize) as u32;
    request.blob_mem = blob_mem;
    // SAFETY: `data` was validated non-null and points to a writable `DrmVirtgpuResourceInfo` buffer.
    unsafe { *data.cast::<DrmVirtgpuResourceInfo>() = request };
    0
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpuMap` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_map(_dev: *mut DrmDevice, data: *mut c_void, file: *mut DrmFile) -> i32 {
    if data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel `DrmVirtgpuMap` buffer.
    let mut request = unsafe { *data.cast::<DrmVirtgpuMap>() };
    // SAFETY: `file` is valid and non-null; `drm_gem_object_lookup` returns a referenced
    // GEM object pointer if found, or null.
    let obj = unsafe { drm_gem_object_lookup(file, request.handle) };
    if obj.is_null() {
        return NEG_ENOENT;
    }
    if gem::cpu_mapping(obj).is_none() || gem::is_imported(obj) {
        // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
        unsafe { drm_gem_object_put(obj) };
        return NEG_EOPNOTSUPP;
    }
    // SAFETY: `obj` was validated non-null and points to a valid GEM object.
    let ret = unsafe { drm_gem_create_mmap_offset(obj) };
    if ret == 0 {
        // SAFETY: On success, `obj->vma_node.vm_node` contains initialized offset fields.
        request.offset = unsafe { (*obj).vma_node.vm_node.start as u64 }
            .checked_shl(PAGE_SIZE.trailing_zeros())
            .unwrap_or(0);
    }
    // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
    unsafe { drm_gem_object_put(obj) };
    if ret != 0 {
        return ret;
    }
    // SAFETY: `data` was validated non-null and points to a writable `DrmVirtgpuMap` buffer.
    unsafe { *data.cast::<DrmVirtgpuMap>() = request };
    0
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpu3dTransferToHost` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_transfer_to_host(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: Forwarding valid ioctl pointers to `ioctl_transfer`.
    unsafe { ioctl_transfer(data, file, true) }
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpu3dTransferToHost` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_transfer_from_host(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: Forwarding valid ioctl pointers to `ioctl_transfer`.
    unsafe { ioctl_transfer(data, file, false) }
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpu3dTransferToHost` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe fn ioctl_transfer(data: *mut c_void, file: *mut DrmFile, to_host: bool) -> i32 {
    if data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel-space transfer request buffer.
    let request = unsafe { *data.cast::<DrmVirtgpu3dTransferToHost>() };
    // SAFETY: `file` is valid; looking up GEM handle returns object with reference count incremented or null.
    let obj = unsafe { drm_gem_object_lookup(file, request.bo_handle) };
    if obj.is_null() {
        return NEG_ENOENT;
    }
    let Some((resource, ..)) = gem::virgl_resource_info(obj) else {
        // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
        unsafe { drm_gem_object_put(obj) };
        return NEG_EINVAL;
    };
    let context = match ensure_context(file) {
        Ok(context) => context,
        Err(error) => {
            // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
            unsafe { drm_gem_object_put(obj) };
            return error;
        }
    };
    if let Err(error) = attach_resource(file, context, resource, obj) {
        // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
        unsafe { drm_gem_object_put(obj) };
        return error;
    }
    let transfer = Transfer3d {
        resource,
        context,
        box_: TransferBox {
            x: request.box_.x,
            y: request.box_.y,
            z: request.box_.z,
            width: request.box_.w,
            height: request.box_.h,
            depth: request.box_.d,
        },
        offset: request.offset as u64,
        level: request.level,
        stride: request.stride,
        layer_stride: request.layer_stride,
    };
    let completion = with_virgl(|virgl| {
        let completion = if to_host {
            virgl.transfer_to_host(transfer)?
        } else {
            virgl.transfer_from_host(transfer)?
        };
        virgl.ctrl_notify();
        Ok(completion)
    });
    let ret = match completion {
        Ok(completion) => {
            gem::record_completion(obj, completion);
            let usage = if to_host {
                DMA_RESV_USAGE_READ
            } else {
                kdrm_core::drm_gem_atomic_helper_upstream::DMA_RESV_USAGE_WRITE as u32
            };
            track_gem_completion(obj, completion, usage)
        }
        Err(error) => error,
    };
    // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
    unsafe { drm_gem_object_put(obj) };
    ret
}

fn track_gem_completion(obj: *mut DrmGemObject, completion: Completion, usage: u32) -> i32 {
    if completion == Completion::Complete {
        return 0;
    }
    let fence = fence::new_completion(completion);
    if fence.is_null() {
        return NEG_ENOMEM;
    }
    // SAFETY: `obj` is a valid GEM object held live by caller, and `fence` is a newly created
    // fence with reference count 1.
    let ret = unsafe { fence::add_to_object_resv(obj, fence, usage) };
    // SAFETY: `add_to_object_resv` acquires its own reference if stored; releasing the caller's initial reference.
    unsafe { dma_fence_put(fence) };
    ret
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpu3dWait` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_wait(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel `DrmVirtgpu3dWait` buffer.
    let request = unsafe { *data.cast::<DrmVirtgpu3dWait>() };
    if request.flags & !VIRTGPU_WAIT_NOWAIT != 0 {
        return NEG_EINVAL;
    }
    // SAFETY: `file` is valid; looking up GEM handle returns object with reference count incremented or null.
    let obj = unsafe { drm_gem_object_lookup(file, request.handle) };
    if obj.is_null() {
        return NEG_ENOENT;
    }
    let completion = gem::last_completion(obj);
    let result = if let Some(completion) = completion {
        if request.flags & VIRTGPU_WAIT_NOWAIT != 0 {
            match ax_gpu::with_gpu(|device| device.completion_status(completion)) {
                Ok(Ok(ax_gpu::rdif_gpu::CompletionStatus::Complete)) => 0,
                Ok(Ok(ax_gpu::rdif_gpu::CompletionStatus::Pending)) => NEG_EBUSY,
                Ok(Err(error)) | Err(error) => gem::gpu_error_to_errno(error),
            }
        } else {
            match ax_gpu::wait_completion(completion, ax_gpu::GPU_WAIT_TIMEOUT) {
                Ok(()) => 0,
                Err(GpuError::TimedOut) => NEG_ETIME,
                Err(error) => gem::gpu_error_to_errno(error),
            }
        }
    } else {
        0
    };
    // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
    unsafe { drm_gem_object_put(obj) };
    result
}

// Guest-backed blobs consume kernel DMA memory. Host-only blobs consume no
// guest backing; mappable ones are separately bounded by the negotiated BAR.
fn blob_gem_size(memory: u32, size: u64) -> Result<usize, i32> {
    usize::try_from(size)
        .ok()
        .and_then(|size| size.checked_add(PAGE_SIZE - 1))
        .map(|size| size & !(PAGE_SIZE - 1))
        .filter(|size| {
            *size != 0 && (memory == VIRTGPU_BLOB_MEM_HOST3D || *size <= MAX_RESOURCE_SIZE)
        })
        .ok_or(NEG_EINVAL)
}

/// # Safety
///
/// If non-null, `dev` must point to an active `DrmDevice`, `data` must point to a valid
/// kernel-space `DrmVirtgpuResourceCreateBlob` buffer, and `file` must point to an active `DrmFile`.
unsafe extern "C" fn ioctl_blob_create(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if dev.is_null() || data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel `DrmVirtgpuResourceCreateBlob` buffer.
    let mut request = unsafe { *data.cast::<DrmVirtgpuResourceCreateBlob>() };
    if !capabilities().is_ok_and(|caps| caps.supports_blob) {
        return NEG_EOPNOTSUPP;
    }
    let size = match blob_gem_size(request.blob_mem, request.size) {
        Ok(size) => size,
        Err(error) => return error,
    };
    let known_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE
        | VIRTGPU_BLOB_FLAG_USE_SHAREABLE
        | VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE;
    if request.blob_flags & !known_flags != 0
        || request.blob_flags & VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE != 0
        || request.blob_hints & !DRM_VIRTGPU_BLOB_FLAG_HINT_DEFER_MAPPING != 0
    {
        return NEG_EINVAL;
    }
    if request.cmd_size as usize > MAX_COMMAND || (request.cmd_size != 0 && request.cmd == 0) {
        return NEG_EINVAL;
    }
    let commands = match copy_bytes(request.cmd, request.cmd_size as usize, MAX_COMMAND) {
        Ok(commands) => commands,
        Err(error) => return error,
    };
    let needs_context = matches!(
        request.blob_mem,
        VIRTGPU_BLOB_MEM_HOST3D | VIRTGPU_BLOB_MEM_HOST3D_GUEST
    );
    if !matches!(
        request.blob_mem,
        VIRTGPU_BLOB_MEM_GUEST | VIRTGPU_BLOB_MEM_HOST3D | VIRTGPU_BLOB_MEM_HOST3D_GUEST
    ) {
        return NEG_EINVAL;
    }
    let context = if needs_context {
        match ensure_context(file) {
            Ok(context) => Some(context),
            Err(error) => return error,
        }
    } else {
        context(file).ok().flatten()
    };
    let host_layout = if request.blob_mem == VIRTGPU_BLOB_MEM_HOST3D
        && request.blob_flags & VIRTGPU_BLOB_FLAG_USE_MAPPABLE != 0
    {
        match super::host_blob::Layout::new(request.size) {
            Ok(layout) => Some(layout),
            Err(error) => return error,
        }
    } else {
        None
    };
    let (mapping, backing) = if request.blob_mem == VIRTGPU_BLOB_MEM_GUEST
        || request.blob_mem == VIRTGPU_BLOB_MEM_HOST3D_GUEST
    {
        let Some(nonzero) = NonZeroUsize::new(size) else {
            return NEG_EINVAL;
        };
        match ax_gpu::allocate_mappable_backing(nonzero) {
            Ok(mapping) => {
                let backing = mapping.backing();
                (Some(mapping), Some(backing))
            }
            Err(error) => return gem::gpu_error_to_errno(error),
        }
    } else {
        (None, None)
    };
    let descriptor = BlobDescriptor {
        memory: request.blob_mem,
        flags: request.blob_flags,
        // The host allocation belongs to Vulkan and may be only 4 KiB.
        // BAR slot padding must not enlarge the RESOURCE_CREATE_BLOB size.
        size: request.size,
        id: request.blob_id,
    };
    let created = with_virgl(|virgl| {
        let resource = virgl.create_blob(context, descriptor, backing.clone(), &commands)?;
        virgl.ctrl_notify();
        let resource_id = virgl.command_resource_id(resource)?;
        Ok((resource, resource_id))
    });
    let (resource, resource_id) = match created {
        Ok(value) => value,
        Err(error) => return error,
    };
    let host_mapping = if request.blob_flags & VIRTGPU_BLOB_FLAG_USE_MAPPABLE != 0 {
        if let Some(layout) = &host_layout {
            match layout.map(resource) {
                Ok(mapping) => Some(mapping),
                // Mapping failure consumes the newly created device resource.
                Err(error) => return error,
            }
        } else {
            None
        }
    } else {
        None
    };
    let obj = match gem::create_virgl_object(
        dev,
        gem::VirglObjectInfo {
            size,
            mapping,
            host_mapping,
            backing,
            resource,
            resource_3d: None,
            context,
            resource_id,
            blob_mem: request.blob_mem,
            blob_flags: request.blob_flags,
            is_blob: true,
        },
    ) {
        Ok(obj) => obj,
        Err(error) => return error,
    };
    let mut handle = 0;
    // SAFETY: `file` and `obj` are valid; `&mut handle` is a valid out-pointer.
    let ret = unsafe { drm_gem_handle_create(file, obj, &mut handle) };
    if ret != 0 {
        // SAFETY: Initial refcount from `create_virgl_object` dropped on error.
        unsafe { drm_gem_object_put(obj) };
        return ret;
    }
    if let Some(context) = context
        && let Err(error) = attach_resource(file, context, resource, obj)
    {
        // SAFETY: Handle was registered above and must be removed on failure.
        unsafe { drm_gem_handle_delete(file, handle) };
        // SAFETY: Initial refcount from `create_virgl_object` dropped on error.
        unsafe { drm_gem_object_put(obj) };
        return error;
    }
    // SAFETY: Releasing initial reference from `create_virgl_object`, leaving
    // reference held by handle table (and attachment if any).
    unsafe { drm_gem_object_put(obj) };
    request.bo_handle = handle;
    request.res_handle = resource_id;
    // SAFETY: `data` was validated non-null and points to a writable `DrmVirtgpuResourceCreateBlob` buffer.
    unsafe { *data.cast::<DrmVirtgpuResourceCreateBlob>() = request };
    0
}

fn copy_bytes(pointer: u64, size: usize, limit: usize) -> Result<Vec<u8>, i32> {
    if size > limit || (size != 0 && pointer == 0) {
        return Err(NEG_EINVAL);
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| NEG_ENOMEM)?;
    bytes.resize(size, 0);
    // SAFETY: `bytes` was resized to `size` bytes, so `bytes.as_mut_ptr()` points to
    // valid, initialized kernel memory of length `size`. `copy_from_user` validates
    // the userspace pointer `pointer` and returns non-zero on fault.
    if size != 0
        && unsafe {
            copy_from_user(
                bytes.as_mut_ptr().cast(),
                pointer as usize as *const c_void,
                size,
            )
        } != 0
    {
        return Err(NEG_EFAULT);
    }
    Ok(bytes)
}

/// # Safety
///
/// If non-null, `data` must point to a valid kernel-space `DrmVirtgpuExecbuffer` ioctl buffer,
/// and `file` must point to an active, valid `DrmFile`.
unsafe extern "C" fn ioctl_execbuffer(
    _dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    if data.is_null() || file.is_null() {
        return NEG_EINVAL;
    }
    // SAFETY: `data` was validated non-null and points to a kernel `DrmVirtgpuExecbuffer` buffer.
    let mut request = unsafe { *data.cast::<DrmVirtgpuExecbuffer>() };
    if request.flags & !VIRTGPU_EXECBUF_FLAGS != 0
        || request.size as usize > MAX_COMMAND
        || request.num_bo_handles as usize > MAX_HANDLES
        || (request.size != 0 && request.command == 0)
        || (request.num_bo_handles != 0 && request.bo_handles == 0)
        || (request.flags & VIRTGPU_EXECBUF_FENCE_FD_IN != 0 && request.fence_fd < 0)
    {
        return NEG_EINVAL;
    }
    if !capabilities().is_ok_and(|caps| caps.supports_3d) {
        return NEG_EOPNOTSUPP;
    }
    if request.num_in_syncobjs != 0
        && (request.syncobj_stride as usize) < size_of::<DrmVirtgpuExecbufferSyncobj>()
        || request.num_out_syncobjs != 0
            && (request.syncobj_stride as usize) < size_of::<DrmVirtgpuExecbufferSyncobj>()
    {
        return NEG_EINVAL;
    }
    let context = match ensure_context(file) {
        Ok(context) => context,
        Err(error) => return error,
    };
    let (capset_id, num_rings) = {
        // SAFETY: The ioctl retains the live file and private allocation.
        let private = match unsafe { file_private(file) } {
            Ok(private) => private,
            Err(error) => return error,
        };
        let state = private.state.lock();
        (state.context_init, state.num_rings)
    };
    let ring = if request.flags & VIRTGPU_EXECBUF_RING_IDX != 0 {
        if request.ring_idx >= num_rings {
            return NEG_EINVAL;
        }
        Some(request.ring_idx as u8)
    } else if capset_id == 4 {
        Some(0) // Venus can kick commands resident in its shared ring.
    } else {
        None
    };
    if request.size == 0 && capset_id != 4 {
        return NEG_EINVAL;
    }
    if request.flags & VIRTGPU_EXECBUF_FENCE_FD_IN != 0 {
        // SAFETY: `request.fence_fd` is checked >= 0; `drm_syncobj_sync_file_get_fence`
        // returns a referenced fence or null.
        let fence = unsafe { drm_syncobj_sync_file_get_fence(request.fence_fd) };
        if fence.is_null() {
            return NEG_EINVAL;
        }
        // SAFETY: `fence` was verified non-null.
        let wait = unsafe { drm_syncobj_wait_fence(fence, true) };
        // SAFETY: Releasing reference acquired from `drm_syncobj_sync_file_get_fence`.
        unsafe { dma_fence_put(fence) };
        if wait != 0 {
            return wait;
        }
    }

    let command = match copy_bytes(request.command, request.size as usize, MAX_COMMAND) {
        Ok(command) => command,
        Err(error) => return error,
    };
    let handles = match copy_from_user_array::<u32>(
        request.bo_handles,
        request.num_bo_handles,
        MAX_HANDLES,
    ) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    if let Err(error) = wait_input_syncobjs(file, &request) {
        return error;
    }
    let output_syncobjs = match collect_output_syncobjs(file, &request) {
        Ok(output) => output,
        Err(error) => return error,
    };
    let mut objects = Vec::new();
    if objects.try_reserve_exact(handles.len()).is_err() {
        release_output_syncobjs(output_syncobjs);
        return NEG_ENOMEM;
    }
    for handle in handles {
        // SAFETY: `file` is valid; looking up GEM handle returns object with reference count incremented or null.
        let object = unsafe { drm_gem_object_lookup(file, handle) };
        if object.is_null() {
            release_objects(objects);
            release_output_syncobjs(output_syncobjs);
            return NEG_ENOENT;
        }
        if gem::virgl_resource_info(object).is_none() {
            // SAFETY: Balance reference acquired via `drm_gem_object_lookup`.
            unsafe { drm_gem_object_put(object) };
            release_objects(objects);
            release_output_syncobjs(output_syncobjs);
            return NEG_EINVAL;
        }
        objects.push(object);
    }

    let mut attach_error = 0;
    for object in &objects {
        let Some((resource, ..)) = gem::virgl_resource_info(*object) else {
            attach_error = NEG_EINVAL;
            break;
        };
        if let Err(error) = attach_resource(file, context, resource, *object) {
            attach_error = error;
            break;
        }
    }
    if attach_error != 0 {
        release_objects(objects);
        release_output_syncobjs(output_syncobjs);
        return attach_error;
    }
    let completion = match with_virgl(|virgl| {
        let completion = if let Some(ring) = ring {
            virgl.submit_ring(context, &command, ring)?
        } else {
            virgl.submit(context, &command)?
        };
        virgl.ctrl_notify();
        Ok(completion)
    }) {
        Ok(completion) => completion,
        Err(error) => {
            release_objects(objects);
            release_output_syncobjs(output_syncobjs);
            return error;
        }
    };
    let fence = fence::new_completion(completion);
    if fence.is_null() {
        release_objects(objects);
        release_output_syncobjs(output_syncobjs);
        return NEG_ENOMEM;
    }
    let mut reservation_error = 0;
    for object in &objects {
        gem::record_completion(*object, completion);
        // SAFETY: `*object` is a valid GEM object from `objects`, and `fence` is a valid fence.
        let ret = unsafe {
            fence::add_to_object_resv(
                *object,
                fence,
                kdrm_core::drm_gem_atomic_helper_upstream::DMA_RESV_USAGE_WRITE as u32,
            )
        };
        if ret != 0 && reservation_error == 0 {
            reservation_error = ret;
        }
    }

    let mut output_error = 0;
    for output in &output_syncobjs {
        let ret = if output.point == 0 {
            // SAFETY: `output.object` is a valid `DrmSyncobj` and `fence` is a valid fence.
            unsafe { drm_syncobj_replace_fence(output.object, fence) };
            0
        } else {
            // SAFETY: `output.object` is a valid `DrmSyncobj` and `fence` is a valid fence.
            unsafe {
                kdrm_core::drm_syncobj_upstream::drm_syncobj_add_point_auto(
                    output.object,
                    fence,
                    output.point,
                )
            }
        };
        if ret != 0 && output_error == 0 {
            output_error = ret;
        }
    }
    let fd_error = if request.flags & VIRTGPU_EXECBUF_FENCE_FD_OUT != 0 {
        export_fence_fd(fence, &mut request.fence_fd)
    } else {
        0
    };
    // SAFETY: Releasing initial reference of `fence` created by `fence::new_completion`.
    unsafe { dma_fence_put(fence) };
    release_objects(objects);
    release_output_syncobjs(output_syncobjs);
    if fd_error != 0 {
        return fd_error;
    }
    if output_error != 0 {
        return output_error;
    }
    if reservation_error != 0 {
        return reservation_error;
    }
    // SAFETY: `data` was validated non-null and points to a writable `DrmVirtgpuExecbuffer` buffer.
    unsafe { *data.cast::<DrmVirtgpuExecbuffer>() = request };
    0
}

fn wait_input_syncobjs(file: *mut DrmFile, request: &DrmVirtgpuExecbuffer) -> Result<(), i32> {
    let descriptors = copy_strided_syncobjs(
        request.in_syncobjs,
        request.syncobj_stride,
        request.num_in_syncobjs,
    )?;
    for descriptor in descriptors {
        if descriptor.flags & !VIRTGPU_EXECBUF_SYNCOBJ_FLAGS != 0 {
            return Err(NEG_EINVAL);
        }
        let mut fence = ptr::null_mut();
        // SAFETY: `file` is valid; looking up syncobj fence writes an acquired reference to `fence` or null.
        let ret = unsafe {
            drm_syncobj_find_fence(file, descriptor.handle, descriptor.point, 0, &mut fence)
        };
        if ret != 0 {
            return Err(ret);
        }
        // SAFETY: `fence` was returned non-null on success.
        let wait = unsafe { drm_syncobj_wait_fence(fence, true) };
        // SAFETY: Releasing reference acquired from `drm_syncobj_find_fence`.
        unsafe { dma_fence_put(fence) };
        if wait != 0 {
            return Err(wait);
        }
        if descriptor.flags & VIRTGPU_EXECBUF_SYNCOBJ_RESET != 0 {
            // SAFETY: `file` is valid; `drm_syncobj_find` returns a referenced syncobj or null.
            let object = unsafe { drm_syncobj_find(file, descriptor.handle) };
            if object.is_null() {
                return Err(NEG_ENOENT);
            }
            // SAFETY: `object` was verified non-null. Replacing fence with null resets it;
            // `drm_syncobj_put` balances the reference acquired by `drm_syncobj_find`.
            unsafe {
                drm_syncobj_replace_fence(object, ptr::null_mut());
                drm_syncobj_put(object);
            }
        }
    }
    Ok(())
}

fn collect_output_syncobjs(
    file: *mut DrmFile,
    request: &DrmVirtgpuExecbuffer,
) -> Result<Vec<ExecOutputSyncobj>, i32> {
    let descriptors = copy_strided_syncobjs(
        request.out_syncobjs,
        request.syncobj_stride,
        request.num_out_syncobjs,
    )?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(descriptors.len())
        .map_err(|_| NEG_ENOMEM)?;
    for descriptor in descriptors {
        if descriptor.flags != 0 {
            release_output_syncobjs(output);
            return Err(NEG_EINVAL);
        }
        // SAFETY: `file` is valid; `drm_syncobj_find` returns a referenced syncobj or null.
        let object = unsafe { drm_syncobj_find(file, descriptor.handle) };
        if object.is_null() {
            release_output_syncobjs(output);
            return Err(NEG_ENOENT);
        }
        output.push(ExecOutputSyncobj {
            object,
            point: descriptor.point,
        });
    }
    Ok(output)
}

fn copy_strided_syncobjs(
    pointer: u64,
    stride: u32,
    count: u32,
) -> Result<Vec<DrmVirtgpuExecbufferSyncobj>, i32> {
    if count as usize > MAX_HANDLES {
        return Err(NEG_EINVAL);
    }
    if count != 0 && pointer == 0 {
        return Err(NEG_EFAULT);
    }
    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(count as usize)
        .map_err(|_| NEG_ENOMEM)?;
    for index in 0..count as usize {
        let offset = index
            .checked_mul(stride as usize)
            .and_then(|offset| u64::try_from(offset).ok())
            .and_then(|offset| pointer.checked_add(offset))
            .ok_or(NEG_EINVAL)?;
        let mut descriptor = DrmVirtgpuExecbufferSyncobj::default();
        // SAFETY: `&mut descriptor` points to a live local stack variable.
        // `copy_from_user` validates userspace address `offset` and returns non-zero on fault.
        if unsafe {
            copy_from_user(
                (&mut descriptor as *mut DrmVirtgpuExecbufferSyncobj).cast(),
                offset as usize as *const c_void,
                size_of::<DrmVirtgpuExecbufferSyncobj>(),
            )
        } != 0
        {
            return Err(NEG_EFAULT);
        }
        descriptors.push(descriptor);
    }
    Ok(descriptors)
}

fn release_output_syncobjs(output: Vec<ExecOutputSyncobj>) {
    for entry in output {
        // SAFETY: `entry.object` holds a reference acquired in `collect_output_syncobjs`.
        unsafe { drm_syncobj_put(entry.object) };
    }
}

fn release_objects(objects: Vec<*mut DrmGemObject>) {
    for object in objects {
        // SAFETY: `object` holds a reference acquired via `drm_gem_object_lookup`.
        unsafe { drm_gem_object_put(object) };
    }
}

fn export_fence_fd(fence: *mut DmaFence, output: &mut i32) -> i32 {
    // SAFETY: Allocates an unused file descriptor with O_CLOEXEC flags.
    let fd = unsafe { get_unused_fd_flags(0x80000) };
    if fd < 0 {
        return fd;
    }
    // SAFETY: `fence` is a valid fence pointer; `drm_syncobj_sync_file_create`
    // creates a sync_file holding a reference to `fence`.
    let sync_file = unsafe { drm_syncobj_sync_file_create(fence) };
    if sync_file.is_null() {
        // SAFETY: `fd` is a valid unused fd allocated above; releasing it on error.
        unsafe { put_unused_fd(fd) };
        return NEG_ENOMEM;
    }
    // SAFETY: `sync_file` was returned non-null and points to a valid sync file.
    let file = unsafe { drm_syncobj_sync_file_file(sync_file) };
    if file.is_null() {
        // SAFETY: `fd` is a valid unused fd allocated above; releasing it on error.
        unsafe { put_unused_fd(fd) };
        return NEG_EIO;
    }
    // SAFETY: Installs `file` into the file descriptor table at `fd`.
    unsafe { fd_install(fd, file.cast()) };
    *output = fd;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_only_blobs_do_not_consume_the_guest_backing_budget() {
        let large_host_allocation = MAX_RESOURCE_SIZE as u64 + PAGE_SIZE as u64;
        assert_eq!(
            blob_gem_size(VIRTGPU_BLOB_MEM_HOST3D, large_host_allocation),
            Ok(large_host_allocation as usize)
        );
        for memory in [VIRTGPU_BLOB_MEM_GUEST, VIRTGPU_BLOB_MEM_HOST3D_GUEST] {
            assert_eq!(
                blob_gem_size(memory, large_host_allocation),
                Err(NEG_EINVAL)
            );
            assert_eq!(blob_gem_size(memory, PAGE_SIZE as u64 - 1), Ok(PAGE_SIZE));
        }
        assert_eq!(blob_gem_size(VIRTGPU_BLOB_MEM_HOST3D, 0), Err(NEG_EINVAL));
        assert_eq!(
            blob_gem_size(VIRTGPU_BLOB_MEM_HOST3D, u64::MAX),
            Err(NEG_EINVAL)
        );
    }

    #[test]
    fn file_local_handle_counts_release_keepalives_independently() {
        // SAFETY: These C-shaped records start zeroed; all refcount and
        // per-file private state is initialized before the hooks use it.
        unsafe {
            let mut object: DrmGemObject = core::mem::zeroed();
            drm_kapi::memory::kref_init(ptr::addr_of_mut!(object.refcount));
            let mut first: DrmFile = core::mem::zeroed();
            let mut second: DrmFile = core::mem::zeroed();
            assert_eq!(open(ptr::null_mut(), &mut first), 0);
            assert_eq!(open(ptr::null_mut(), &mut second), 0);
            for _ in 0..32 {
                assert_eq!(gem_open(&mut object, &mut first), 0);
                assert_eq!(gem_open(&mut object, &mut first), 0);
                assert_eq!(gem_open(&mut object, &mut second), 0);
                assert_eq!(
                    drm_kapi::memory::refcount_read(&object.refcount.refcount),
                    3
                );
                assert!(!close_handle(file_private(&first).unwrap(), &mut object));
                assert_eq!(
                    drm_kapi::memory::refcount_read(&object.refcount.refcount),
                    3
                );
                assert!(!close_handle(file_private(&second).unwrap(), &mut object));
                assert_eq!(
                    drm_kapi::memory::refcount_read(&object.refcount.refcount),
                    2
                );
                assert!(!close_handle(file_private(&first).unwrap(), &mut object));
                assert_eq!(
                    drm_kapi::memory::refcount_read(&object.refcount.refcount),
                    1
                );
            }
            // All local keepalives are gone; no context/device was created in
            // this hosted ownership test. Native detach is exercised by the
            // repeated-resource QEMU application, not by a fake GPU runtime.
            for file in [&mut first, &mut second] {
                let private = file.driver_priv.cast::<FilePrivate>();
                assert!((*private).state.lock().attached.is_null());
                ptr::drop_in_place(private);
                kfree(private);
                file.driver_priv = ptr::null_mut();
            }
        }
    }

    #[test]
    fn context_rings_validate_the_declared_ring_namespace() {
        let mut params = [
            DrmVirtgpuContextSetParam {
                param: VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
                value: 4,
            },
            DrmVirtgpuContextSetParam {
                param: VIRTGPU_CONTEXT_PARAM_NUM_RINGS,
                value: 64,
            },
            DrmVirtgpuContextSetParam {
                param: VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK,
                value: 1 << 63,
            },
        ];
        let config = ContextConfig::parse(&params).unwrap();
        assert_eq!((config.capset_id, config.num_rings), (4, 64));
        params[1].value = 63;
        assert!(ContextConfig::parse(&params).is_err());
        params[2].value = 1;
        assert!(ContextConfig::parse(&params).is_ok());
        params[1].value = 0;
        assert!(ContextConfig::parse(&params).is_err());
        params[1] = params[0];
        assert!(ContextConfig::parse(&params).is_err());
    }
}

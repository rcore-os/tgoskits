//! VirtIO GPU private DRM ioctl ABI.
//!
//! Layout and ioctl numbers follow Linux v7.1 `include/uapi/drm/virtgpu_drm.h`.

use bytemuck::{AnyBitPattern, NoUninit};

use super::super::drm::{DRM_TYPE, iowr};

// ======== virtgpu ioctls ========
//
// Private ioctls for virtio-gpu 3D (virgl) support. These live at
// DRM_COMMAND_BASE + N and are dispatched through the same ioctl match
// as core DRM commands.
//
// Reference: Linux v7.1 include/uapi/drm/virtgpu_drm.h
// Mesa: src/gallium/winsys/virgl/drm/virgl_drm_winsys.c

// ---- virtgpu ioctl numbers ----
// DRM ioctl encoding: DRM_IOWR(DRM_COMMAND_BASE + N, struct)
// where DRM_TYPE = b'd' (0x64)

pub const DRM_IOCTL_VIRTGPU_MAP: u32 = iowr::<DrmVirtgpuMap>(DRM_TYPE, 0x41);
pub const DRM_IOCTL_VIRTGPU_EXECBUFFER: u32 = iowr::<DrmVirtgpuExecbuffer>(DRM_TYPE, 0x42);
pub const DRM_IOCTL_VIRTGPU_GETPARAM: u32 = iowr::<DrmVirtgpuGetparam>(DRM_TYPE, 0x43);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = iowr::<DrmVirtgpuResourceCreate>(DRM_TYPE, 0x44);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 = iowr::<DrmVirtgpuResourceInfo>(DRM_TYPE, 0x45);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST: u32 =
    iowr::<DrmVirtgpu3dTransferFromHost>(DRM_TYPE, 0x46);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u32 =
    iowr::<DrmVirtgpu3dTransferToHost>(DRM_TYPE, 0x47);
pub const DRM_IOCTL_VIRTGPU_WAIT: u32 = iowr::<DrmVirtgpu3dWait>(DRM_TYPE, 0x48);
pub const DRM_IOCTL_VIRTGPU_GET_CAPS: u32 = iowr::<DrmVirtgpuGetCaps>(DRM_TYPE, 0x49);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB: u32 =
    iowr::<DrmVirtgpuResourceCreateBlob>(DRM_TYPE, 0x4a);
pub const DRM_IOCTL_VIRTGPU_CONTEXT_INIT: u32 = iowr::<DrmVirtgpuContextInit>(DRM_TYPE, 0x4b);

// ---- GETPARAM parameter IDs (VIRTGPU_PARAM_*) ----

/// Whether the device supports 3D features (virgl).
pub const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
/// Whether capset query fix is supported.
pub const VIRTGPU_PARAM_CAPSET_QUERY_FIX: u64 = 2;
/// Whether RESOURCE_CREATE_BLOB is supported.
pub const VIRTGPU_PARAM_RESOURCE_BLOB: u64 = 3;
/// Whether host-visible blob resources can be mmap'd.
pub const VIRTGPU_PARAM_HOST_VISIBLE: u64 = 4;
/// Whether cross-device resource sharing is supported.
pub const VIRTGPU_PARAM_CROSS_DEVICE: u64 = 5;
/// Whether CONTEXT_INIT is supported.
pub const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;
/// Bitmask of supported capset IDs.
pub const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;
/// User-space debug naming (v6.12+).
#[allow(dead_code)]
pub const VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME: u64 = 8;
/// Blob alignment requirement (v6.15+).
#[allow(dead_code)]
pub const VIRTGPU_PARAM_BLOB_ALIGNMENT: u64 = 9;

// ---- EXECBUFFER flags (VIRTGPU_EXECBUF_*) ----

/// fence_fd is an input dependency.
pub const VIRTGPU_EXECBUF_FENCE_FD_IN: u32 = 0x01;
/// fence_fd is an output completion signal.
pub const VIRTGPU_EXECBUF_FENCE_FD_OUT: u32 = 0x02;
/// Use ring_idx field.
#[allow(dead_code)]
pub const VIRTGPU_EXECBUF_RING_IDX: u32 = 0x04;
/// syncobj reset (v6.6+).
#[allow(dead_code)]
pub const VIRTGPU_EXECBUF_SYNCOBJ_RESET: u32 = 0x01;

// ---- WAIT flags ----

/// Non-blocking wait.
#[allow(dead_code)]
pub const VIRTGPU_WAIT_NOWAIT: u32 = 1;

// ---- CONTEXT parameters (VIRTGPU_CONTEXT_PARAM_*) ----

/// Capset ID for the context.
pub const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
/// Number of command rings.
pub const VIRTGPU_CONTEXT_PARAM_NUM_RINGS: u64 = 0x0002;
/// Poll rings mask.
pub const VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 0x0003;
/// Debug name (v6.12+).
#[allow(dead_code)]
pub const VIRTGPU_CONTEXT_PARAM_DEBUG_NAME: u64 = 0x0004;

// ---- CAPSET IDs (VIRTGPU_DRM_CAPSET_*) ----

/// Virgl capset v1.
pub const VIRTGPU_DRM_CAPSET_VIRGL: u32 = 1;
/// Virgl capset v2 (Mesa prefers this).
pub const VIRTGPU_DRM_CAPSET_VIRGL2: u32 = 2;
/// gfxstream Vulkan capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_GFXSTREAM_VULKAN: u32 = 3;
/// Venus (Vulkan) capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_VENUS: u32 = 4;
/// Cross-domain capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_CROSS_DOMAIN: u32 = 5;
/// DRM capset.
#[allow(dead_code)]
pub const VIRTGPU_DRM_CAPSET_DRM: u32 = 6;

// ---- BLOB memory types (VIRTGPU_BLOB_MEM_*) ----

/// Guest memory blob.
pub const VIRTGPU_BLOB_MEM_GUEST: u32 = 0x0001;
/// Host 3D memory blob (Mesa uses this).
pub const VIRTGPU_BLOB_MEM_HOST3D: u32 = 0x0002;
/// Host 3D + guest memory blob.
pub const VIRTGPU_BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;

// ---- BLOB flags (VIRTGPU_BLOB_FLAG_*) ----

/// Blob can be mmap'd (Mesa uses this).
#[allow(dead_code)]
pub const VIRTGPU_BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;
/// Blob can be shared.
#[allow(dead_code)]
pub const VIRTGPU_BLOB_FLAG_USE_SHAREABLE: u32 = 0x0002;
/// Blob can be used across devices.
#[allow(dead_code)]
pub const VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 0x0004;
/// Hint: defer mapping.
#[allow(dead_code)]
pub const DRM_VIRTGPU_BLOB_FLAG_HINT_DEFER_MAPPING: u32 = 0x0001;

// ---- Event codes ----

/// Fence signaled event.
#[allow(dead_code)]
pub const VIRTGPU_EVENT_FENCE_SIGNALED: u32 = 0x90000000;

// ---- virtgpu structs ----

/// Maps a GEM handle to an mmap offset.
/// Linux: `struct drm_virtgpu_map` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuMap {
    /// Output: offset for mmap system call.
    pub offset: u64,
    /// Input: GEM handle.
    pub handle: u32,
    pub pad: u32,
}

/// Submits a virgl command buffer to the host.
/// Linux: `struct drm_virtgpu_execbuffer` (v6.6+: 64 bytes)
///
/// We use the 64-byte layout with the syncobj tail added in v6.6, which is the
/// layout current Mesa virgl expects (the ioctl number encodes this size).
///
/// **Critical**: There is NO `ctx_id` field. The context is implicitly
/// bound to the file descriptor via CONTEXT_INIT.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuExecbuffer {
    /// VIRTGPU_EXECBUF_FENCE_FD_IN/OUT/RING_IDX flags.
    pub flags: u32,
    /// Command buffer size in bytes.
    pub size: u32,
    /// User-space pointer to command buffer.
    pub command: u64,
    /// User-space pointer to __u32 array of GEM handles.
    pub bo_handles: u64,
    /// Number of bo_handles.
    pub num_bo_handles: u32,
    /// Input/output fence fd (signed! default -1).
    pub fence_fd: i32,
    /// Command ring index (used when RING_IDX flag is set).
    pub ring_idx: u32,
    // ---- syncobj fields (Linux drm_virtgpu_execbuffer tail, 64B total) ----
    // 缺少这些字段会让 struct 只有 40 字节 → ioctl 号(size 位)与 mesa 的 64
    // 字节不一致 → EXECBUFFER 落不到 handler → "expect bad rendering 95"。
    /// Size of each @drm_virtgpu_execbuffer_syncobj.
    pub syncobj_stride: u32,
    /// Number of in syncobjs.
    pub num_in_syncobjs: u32,
    /// Number of out syncobjs.
    pub num_out_syncobjs: u32,
    /// Pointer to in syncobj array.
    pub in_syncobjs: u64,
    /// Pointer to out syncobj array.
    pub out_syncobjs: u64,
}

/// Queries a driver parameter.
/// Linux: `struct drm_virtgpu_getparam` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuGetparam {
    /// Input: parameter ID (VIRTGPU_PARAM_*).
    pub param: u64,
    /// Output: parameter value.
    pub value: u64,
}

/// Creates a 3D resource (texture, render target, buffer, etc.).
/// Linux: `struct drm_virtgpu_resource_create` (56 bytes)
///
/// **Critical**: `bo_handle` (input, GEM handle) and `res_handle` (output,
/// virtio-gpu resource ID) are DIFFERENT concepts. Mesa uses them
/// independently.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuResourceCreate {
    /// Input: target type (GL_TEXTURE_2D, etc.).
    pub target: u32,
    /// Input: PIPE_FORMAT_* pixel format.
    pub format: u32,
    /// Input: VIRGL_BIND_* binding flags.
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    /// Input: mipmap highest level (not "level").
    pub last_level: u32,
    /// Input: MSAA sample count (usually 0).
    pub nr_samples: u32,
    /// Input: resource flags.
    pub flags: u32,
    /// Input: associate with existing GEM BO (0 = kernel allocates new BO).
    pub bo_handle: u32,
    /// Output: virtio-gpu resource ID (NOT GEM handle!).
    pub res_handle: u32,
    /// Output: resource size (for transfer validation).
    pub size: u32,
    /// Input/Output: row stride.
    pub stride: u32,
}

/// Queries resource information.
/// Linux: `struct drm_virtgpu_resource_info` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuResourceInfo {
    /// Input: GEM handle.
    pub bo_handle: u32,
    /// Output: virtio-gpu resource ID.
    pub res_handle: u32,
    /// Output: resource size.
    pub size: u32,
    /// Output: blob memory type (0 for non-blob resources).
    pub blob_mem: u32,
}

/// 3D box region for data transfer operations.
/// Linux: `struct drm_virtgpu_3d_box` (24 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpu3dBox {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

/// Transfers data from guest to host for a 3D resource.
/// Linux: `struct drm_virtgpu_3d_transfer_to_host` (44 bytes)
///
/// **Critical**: `offset` is u32 (not u64), and the box is a NESTED
/// DrmVirtgpu3dBox struct (not 6 inline u32s).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpu3dTransferToHost {
    /// GEM handle.
    pub bo_handle: u32,
    /// Nested 3D box (24 bytes).
    pub box_: DrmVirtgpu3dBox,
    /// Mipmap level.
    pub level: u32,
    /// Buffer offset (u32, not u64!).
    pub offset: u32,
    /// Row stride.
    pub stride: u32,
    /// Layer stride.
    pub layer_stride: u32,
}

/// Transfers data from host to guest for a 3D resource.
/// Linux: `struct drm_virtgpu_3d_transfer_from_host` (44 bytes)
/// Layout identical to transfer_to_host.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpu3dTransferFromHost {
    /// GEM handle.
    pub bo_handle: u32,
    /// Nested 3D box (24 bytes).
    pub box_: DrmVirtgpu3dBox,
    /// Mipmap level.
    pub level: u32,
    /// Buffer offset (u32, not u64!).
    pub offset: u32,
    /// Row stride.
    pub stride: u32,
    /// Layer stride.
    pub layer_stride: u32,
}

/// Waits for a resource to become idle.
/// Linux: `struct drm_virtgpu_3d_wait` (8 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpu3dWait {
    /// GEM handle (0 is invalid).
    pub handle: u32,
    /// VIRTGPU_WAIT_NOWAIT etc.
    pub flags: u32,
}

/// Retrieves capability set data.
/// Linux: `struct drm_virtgpu_get_caps` (24 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuGetCaps {
    /// Input: capset ID (VIRTGPU_DRM_CAPSET_VIRGL=1, VIRGL2=2).
    pub cap_set_id: u32,
    /// Input: capset version.
    pub cap_set_ver: u32,
    /// Input: user-space buffer pointer.
    pub addr: u64,
    /// Input/Output: buffer size.
    pub size: u32,
    pub pad: u32,
}

/// Creates a blob resource for coherent memory sharing.
/// Linux: `struct drm_virtgpu_resource_create_blob` (48 bytes)
///
/// Mesa virgl calls this when `supports_coherent=true` (i.e. both
/// RESOURCE_BLOB and HOST_VISIBLE are reported as supported).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuResourceCreateBlob {
    /// VIRTGPU_BLOB_MEM_* (Mesa uses HOST3D).
    pub blob_mem: u32,
    /// VIRTGPU_BLOB_FLAG_* (Mesa uses USE_MAPPABLE).
    pub blob_flags: u32,
    /// Output: GEM handle.
    pub bo_handle: u32,
    /// Output: virtio-gpu resource ID.
    pub res_handle: u32,
    /// Blob size in bytes.
    pub size: u64,
    pub pad: u32,
    /// VIRGL command size in bytes.
    pub cmd_size: u32,
    /// User-space pointer to VIRGL_PIPE_RES_CREATE command.
    pub cmd: u64,
    /// Blob unique identifier.
    pub blob_id: u64,
}

/// Initializes a rendering context on this file descriptor.
/// Linux: `struct drm_virtgpu_context_init` (16 bytes)
///
/// **Critical**: This is a PURE INPUT struct — there is NO `ctx_id` output
/// field. The context is implicitly bound to the file descriptor. Each fd
/// can only call CONTEXT_INIT once (repeated calls return -EEXIST).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuContextInit {
    /// Input: number of parameters (kernel limits to max 3).
    pub num_params: u32,
    pub pad: u32,
    /// Input: pointer to DrmVirtgpuContextSetParam array.
    pub ctx_set_params: u64,
}

/// A single context parameter for CONTEXT_INIT.
/// Linux: `struct drm_virtgpu_context_set_param` (16 bytes)
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern)]
pub struct DrmVirtgpuContextSetParam {
    /// VIRTGPU_CONTEXT_PARAM_* constant.
    pub param: u64,
    /// Parameter value.
    pub value: u64,
}

/// Syncobj entry for execbuffer (v6.6+).
/// Linux: `struct drm_virtgpu_execbuffer_syncobj` (16 bytes)
/// Mesa virgl does not use this.
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct DrmVirtgpuExecbufferSyncobj {
    pub handle: u32,
    pub flags: u32,
    pub point: u64,
}

// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Linux v7.2.3 `include/uapi/drm/virtgpu_drm.h` ABI definitions.
//!
//! Fields deliberately use fixed-width integers (including user pointers as
//! `u64`) so the layouts are stable and testable on x86_64.

// This module preserves the complete Linux VirtGPU UAPI constant set even
// where a capability is not currently exposed by the RDIF adapter.

use core::mem::{align_of, offset_of, size_of};

pub const DRM_VIRTGPU_MAP: u32 = 0x01;
pub const DRM_VIRTGPU_EXECBUFFER: u32 = 0x02;
pub const DRM_VIRTGPU_GETPARAM: u32 = 0x03;
pub const DRM_VIRTGPU_RESOURCE_CREATE: u32 = 0x04;
pub const DRM_VIRTGPU_RESOURCE_INFO: u32 = 0x05;
pub const DRM_VIRTGPU_TRANSFER_FROM_HOST: u32 = 0x06;
pub const DRM_VIRTGPU_TRANSFER_TO_HOST: u32 = 0x07;
pub const DRM_VIRTGPU_WAIT: u32 = 0x08;
pub const DRM_VIRTGPU_GET_CAPS: u32 = 0x09;
pub const DRM_VIRTGPU_RESOURCE_CREATE_BLOB: u32 = 0x0a;
pub const DRM_VIRTGPU_CONTEXT_INIT: u32 = 0x0b;

pub const VIRTGPU_EXECBUF_FENCE_FD_IN: u32 = 0x01;
pub const VIRTGPU_EXECBUF_FENCE_FD_OUT: u32 = 0x02;
pub const VIRTGPU_EXECBUF_RING_IDX: u32 = 0x04;
pub const VIRTGPU_EXECBUF_FLAGS: u32 =
    VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_FENCE_FD_OUT | VIRTGPU_EXECBUF_RING_IDX;
pub const VIRTGPU_EXECBUF_SYNCOBJ_RESET: u32 = 0x01;
pub const VIRTGPU_EXECBUF_SYNCOBJ_FLAGS: u32 = VIRTGPU_EXECBUF_SYNCOBJ_RESET;

pub const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;
pub const VIRTGPU_PARAM_CAPSET_QUERY_FIX: u64 = 2;
pub const VIRTGPU_PARAM_RESOURCE_BLOB: u64 = 3;
pub const VIRTGPU_PARAM_HOST_VISIBLE: u64 = 4;
pub const VIRTGPU_PARAM_CROSS_DEVICE: u64 = 5;
pub const VIRTGPU_PARAM_CONTEXT_INIT: u64 = 6;
pub const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;

pub const VIRTGPU_WAIT_NOWAIT: u32 = 1;

pub const VIRTGPU_BLOB_MEM_GUEST: u32 = 0x0001;
pub const VIRTGPU_BLOB_MEM_HOST3D: u32 = 0x0002;
pub const VIRTGPU_BLOB_MEM_HOST3D_GUEST: u32 = 0x0003;
pub const VIRTGPU_BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;
pub const VIRTGPU_BLOB_FLAG_USE_SHAREABLE: u32 = 0x0002;
pub const VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 0x0004;
pub const DRM_VIRTGPU_BLOB_FLAG_HINT_DEFER_MAPPING: u32 = 0x0001;

pub const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
pub const VIRTGPU_CONTEXT_PARAM_NUM_RINGS: u64 = 0x0002;
pub const VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 0x0003;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuMap {
    pub offset: u64,
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuExecbufferSyncobj {
    pub handle: u32,
    pub flags: u32,
    pub point: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuExecbuffer {
    pub flags: u32,
    pub size: u32,
    pub command: u64,
    pub bo_handles: u64,
    pub num_bo_handles: u32,
    pub fence_fd: i32,
    pub ring_idx: u32,
    pub syncobj_stride: u32,
    pub num_in_syncobjs: u32,
    pub num_out_syncobjs: u32,
    pub in_syncobjs: u64,
    pub out_syncobjs: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuGetparam {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuResourceCreate {
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u32,
    pub stride: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuResourceInfo {
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u32,
    pub blob_mem: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpu3dBox {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpu3dTransferToHost {
    pub bo_handle: u32,
    pub box_: DrmVirtgpu3dBox,
    pub level: u32,
    pub offset: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

pub type DrmVirtgpu3dTransferFromHost = DrmVirtgpu3dTransferToHost;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpu3dWait {
    pub handle: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuGetCaps {
    pub cap_set_id: u32,
    pub cap_set_ver: u32,
    pub addr: u64,
    pub size: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuResourceCreateBlob {
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u64,
    pub pad: u32,
    pub cmd_size: u32,
    pub cmd: u64,
    pub blob_id: u64,
    pub blob_hints: u32,
    pub pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuContextSetParam {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrmVirtgpuContextInit {
    pub num_params: u32,
    pub pad: u32,
    pub ctx_set_params: u64,
}

/// Linux DRM `_IOC` encoding used by `DRM_IOWR` on x86_64.
pub const fn drm_iowr(command: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | (0x64 << 8) | (0x40 + command)
}

pub const DRM_IOCTL_VIRTGPU_MAP: u32 = drm_iowr(DRM_VIRTGPU_MAP, size_of::<DrmVirtgpuMap>());
pub const DRM_IOCTL_VIRTGPU_EXECBUFFER: u32 =
    drm_iowr(DRM_VIRTGPU_EXECBUFFER, size_of::<DrmVirtgpuExecbuffer>());
pub const DRM_IOCTL_VIRTGPU_GETPARAM: u32 =
    drm_iowr(DRM_VIRTGPU_GETPARAM, size_of::<DrmVirtgpuGetparam>());
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE: u32 = drm_iowr(
    DRM_VIRTGPU_RESOURCE_CREATE,
    size_of::<DrmVirtgpuResourceCreate>(),
);
pub const DRM_IOCTL_VIRTGPU_RESOURCE_INFO: u32 = drm_iowr(
    DRM_VIRTGPU_RESOURCE_INFO,
    size_of::<DrmVirtgpuResourceInfo>(),
);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST: u32 = drm_iowr(
    DRM_VIRTGPU_TRANSFER_FROM_HOST,
    size_of::<DrmVirtgpu3dTransferFromHost>(),
);
pub const DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST: u32 = drm_iowr(
    DRM_VIRTGPU_TRANSFER_TO_HOST,
    size_of::<DrmVirtgpu3dTransferToHost>(),
);
pub const DRM_IOCTL_VIRTGPU_WAIT: u32 = drm_iowr(DRM_VIRTGPU_WAIT, size_of::<DrmVirtgpu3dWait>());
pub const DRM_IOCTL_VIRTGPU_GET_CAPS: u32 =
    drm_iowr(DRM_VIRTGPU_GET_CAPS, size_of::<DrmVirtgpuGetCaps>());
pub const DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB: u32 = drm_iowr(
    DRM_VIRTGPU_RESOURCE_CREATE_BLOB,
    size_of::<DrmVirtgpuResourceCreateBlob>(),
);
pub const DRM_IOCTL_VIRTGPU_CONTEXT_INIT: u32 =
    drm_iowr(DRM_VIRTGPU_CONTEXT_INIT, size_of::<DrmVirtgpuContextInit>());

const _: () = {
    assert!(size_of::<DrmVirtgpuMap>() == 16);
    assert!(size_of::<DrmVirtgpuExecbufferSyncobj>() == 16);
    assert!(size_of::<DrmVirtgpuExecbuffer>() == 64);
    assert!(offset_of!(DrmVirtgpuExecbuffer, command) == 8);
    assert!(offset_of!(DrmVirtgpuExecbuffer, bo_handles) == 16);
    assert!(offset_of!(DrmVirtgpuExecbuffer, in_syncobjs) == 48);
    assert!(size_of::<DrmVirtgpuGetparam>() == 16);
    assert!(size_of::<DrmVirtgpuResourceCreate>() == 56);
    assert!(size_of::<DrmVirtgpuResourceInfo>() == 16);
    assert!(size_of::<DrmVirtgpu3dBox>() == 24);
    assert!(size_of::<DrmVirtgpu3dTransferToHost>() == 44);
    assert!(size_of::<DrmVirtgpu3dWait>() == 8);
    assert!(size_of::<DrmVirtgpuGetCaps>() == 24);
    assert!(align_of::<DrmVirtgpuResourceCreateBlob>() == 8);
    assert!(size_of::<DrmVirtgpuResourceCreateBlob>() == 56);
    assert!(offset_of!(DrmVirtgpuResourceCreateBlob, size) == 16);
    assert!(offset_of!(DrmVirtgpuResourceCreateBlob, cmd) == 32);
    assert!(offset_of!(DrmVirtgpuResourceCreateBlob, blob_hints) == 48);
    assert!(size_of::<DrmVirtgpuContextSetParam>() == 16);
    assert!(size_of::<DrmVirtgpuContextInit>() == 16);
};

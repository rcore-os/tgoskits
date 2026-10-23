// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux DRM core components for TGOSKits.
//!
//! The shared core follows Linux 7.2.3 DRM object, ioctl, GEM, PRIME, atomic,
//! synchronization, and display-helper semantics. Kernel-facing operations
//! are supplied by drm_kapi; hardware register access and device ownership
//! stay in RDIF-backed drivers.
//!
//! The component is no_std and targets the repository's 64-bit little-endian
//! architectures. It intentionally does not include Intel-specific driver
//! code, firmware framebuffer devices, fbdev console integration, or DP MST
//! and bridge implementations. Helpers for those excluded paths are not
//! advertised as supported by this crate.
//!
//! Most modules retain the Linux source file names and C-shaped data model.
//! This keeps the correspondence reviewable and lets hardware adapters use
//! established DRM callback tables without introducing a second object model.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod dma_resv_types;
pub mod drm_atomic_types;
pub mod drm_auth_types;
pub mod drm_client_types;
pub mod drm_color_mgmt_types;
pub mod drm_connector_types;
pub mod drm_crtc_types;
pub mod drm_damage_helper_types;
pub mod drm_debugfs_crc_types;
pub mod drm_device_types;
pub mod drm_displayid_internal_types;
pub mod drm_dp_helper_types;
pub mod drm_dp_types;
pub mod drm_drv_types;
pub mod drm_edid_types;
pub mod drm_encoder_types;
pub mod drm_fb_helper_types;
pub mod drm_file_types;
pub mod drm_format_helper_types;
pub mod drm_fourcc_types;
pub mod drm_framebuffer_types;
pub mod drm_gem_atomic_helper_types;
pub mod drm_gem_shmem_helper_types_upstream;
pub mod drm_gem_types;
pub mod drm_ioctl_types;
pub mod drm_mm_types;
pub mod drm_mode_config_types;
pub mod drm_mode_object_types;
pub mod drm_modes_types;
pub mod drm_modeset_helper_vtables_types;
pub mod drm_modeset_lock_types;
pub mod drm_plane_types;
pub mod drm_prime_types;
pub mod drm_print_types;
pub mod drm_property_types;
pub mod drm_rect_types;
pub mod drm_vblank_types;
pub mod drm_vblank_work_types;
pub mod drm_vma_manager_types;
pub mod fb_types;
pub mod hdmi_types;
pub mod iosys_map_types;
pub mod kernel_placeholder_types;
pub mod kmsg_dump_types;
pub mod uapi_drm_mode_types;
pub mod uapi_drm_types;

pub mod uapi_drm_consts;
pub mod uapi_drm_macros;

pub mod drm_atomic_helper_upstream;
pub mod drm_atomic_state_helper_upstream;
pub mod drm_atomic_uapi_upstream;
pub mod drm_atomic_upstream;
pub mod drm_auth_upstream;
pub mod drm_blend_upstream;
pub mod drm_client_event_upstream;
pub mod drm_client_modeset_upstream;
pub mod drm_client_upstream;
pub mod drm_color_mgmt_upstream;
pub mod drm_connector_upstream;
pub mod drm_crtc_upstream;
pub mod drm_damage_helper_upstream;
pub mod drm_displayid_upstream;
pub mod drm_dma_buf_upstream;
pub mod drm_dp_dual_mode_helper_upstream;
pub mod drm_dp_helper_upstream;
pub mod drm_drv_upstream;
pub mod drm_dumb_buffers_upstream;
pub mod drm_edid_upstream;
pub mod drm_eld_upstream;
pub mod drm_encoder_upstream;
pub mod drm_file_upstream;
pub mod drm_format_helper_upstream;
pub mod drm_fourcc_upstream;
pub mod drm_framebuffer_upstream;
pub mod drm_gem_atomic_helper_upstream;
pub mod drm_gem_framebuffer_helper_upstream;
pub mod drm_gem_shmem_helper_upstream;
pub mod drm_gem_upstream;
pub mod drm_hdmi_helper_upstream;
pub mod drm_hdmi_state_helper_upstream;
pub mod drm_ioctl_upstream;
pub mod drm_lease_upstream;
pub mod drm_managed_upstream;
pub mod drm_mode_config_upstream;
pub mod drm_mode_object_upstream;
pub mod drm_modes_upstream;
pub mod drm_modeset_helper_upstream;
pub mod drm_modeset_lock_upstream;
pub mod drm_plane_helper_upstream;
pub mod drm_plane_upstream;
pub mod drm_prime_upstream;
pub mod drm_print_upstream;
pub mod drm_probe_helper_upstream;
pub mod drm_property_upstream;
pub mod drm_rect_upstream;
pub mod drm_scdc_helper_upstream;
pub mod drm_simple_kms_helper_types_upstream;
pub mod drm_simple_kms_helper_upstream;
pub mod drm_syncobj_upstream;
pub mod drm_vblank_helper_upstream;
pub mod drm_vblank_upstream;
pub mod drm_vblank_work_upstream;
pub mod drm_vma_manager_upstream;
pub mod hdmi_upstream;

/// Linux 7.2.3 renamed drm_atomic_state to drm_atomic_commit.
pub use drm_atomic_types::DrmAtomicCommit as DrmAtomicState;
pub use drm_device_types::DrmDevice;
pub use drm_drv_types::DrmDriver;
pub use drm_file_types::DrmFile;
pub use drm_simple_kms_helper_types_upstream::{DrmSimpleDisplayPipe, DrmSimpleDisplayPipeFuncs};

#[cfg(test)]
mod test_support;

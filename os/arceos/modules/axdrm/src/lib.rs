// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! DRM device integration for the RDIF-owned GPU runtime.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod backend;
mod device;
mod mapping;
pub mod runtime;

pub use device::{DeviceOwner, device};
pub use mapping::{MmapLease, mmap, mmap_gem_object};

/// Initialize the shared DRM services and register the RDIF-backed device.
///
/// Call this after `ax_gpu::init_gpu()` and before publishing the Starry DRM
/// nodes. A machine without a registered VirtIO GPU remains usable and does
/// not publish a DRM device.
pub fn init() -> Result<(), i32> {
    // Do not initialize the DRM runtime service layer when no RDIF device can
    // publish a DRM node. ArceOS GPU discovery precedes Starry startup.
    if !ax_gpu::has_gpu()
        || !ax_gpu::has_display_controller()
        || ax_gpu::identity().is_none_or(|identity| identity.driver_name != "virtio_gpu")
    {
        return Ok(());
    }
    runtime::init()?;
    device::init()
}

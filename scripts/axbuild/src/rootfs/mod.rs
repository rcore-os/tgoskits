//! Shared rootfs helpers used across ArceOS, StarryOS, and Axvisor host-side
//! build flows.
//!
//! Main responsibilities:
//! - Extract rootfs contents and inject overlay trees back into images in
//!   [`inject`]
//! - Patch QEMU arguments so a selected rootfs image is attached correctly in
//!   [`qemu`]

/// Rootfs image content extraction and overlay injection helpers.
pub(crate) mod inject;
/// QEMU argument patch helpers for wiring a rootfs image into runner configs.
pub(crate) mod qemu;
/// Runtime dependency synchronization helpers for rootfs overlay trees.
pub(crate) mod runtime;

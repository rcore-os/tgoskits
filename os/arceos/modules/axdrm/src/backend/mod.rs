// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.

pub(crate) mod fence;
pub(crate) mod gem;
mod host_blob;
pub(crate) mod kms;
mod presentation;
pub(crate) mod virtgpu;
pub(crate) mod virtgpu_uapi;

/// Total nanosecond budget one close-path cleanup (`release_resource`,
/// host-blob unmap, context destroy) may spend waiting on the host before it
/// escalates to a terminal device reset: three rounds of the stack's
/// per-wait host-trust bound (`ax_gpu::GPU_WAIT_TIMEOUT`). A healthy host
/// tears down in milliseconds; a host that wedges a submission or stops
/// popping completion tokens gets bounded patience instead of a permanent
/// waiter on the closer's thread.
pub(crate) const CLEANUP_BUDGET_NS: u64 = 3 * ax_gpu::GPU_WAIT_TIMEOUT.as_secs() * 1_000_000_000;

/// Monotonic deadline after [`CLEANUP_BUDGET_NS`] for a close-path cleanup
/// loop. Callers compare `drm_kapi::clock::now_ns()` against it between
/// retries; on expiry they warn once and escalate via
/// `ax_gpu::mark_lost_for_cleanup()`: the device reset stops all DMA, which
/// is the quiescence proof that authorizes releasing backing and recycling
/// host BAR slots the wedged host may still access. Every later operation
/// then fails fast with `DeviceLost`, so the remaining teardown walks
/// fast-fail exits instead of paying the budget again.
pub(crate) fn cleanup_deadline() -> u64 {
    drm_kapi::clock::now_ns().saturating_add(CLEANUP_BUDGET_NS)
}

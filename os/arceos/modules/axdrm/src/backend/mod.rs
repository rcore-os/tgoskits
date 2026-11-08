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
/// degrades to release-without-proof: three rounds of the stack's per-wait
/// host-trust bound (`ax_gpu::GPU_WAIT_TIMEOUT`). A healthy host tears down
/// in milliseconds; a host that wedges a submission or never retires a
/// completion (a venus-only host parking a plain-fenced control command on
/// its fence queue, for example) gets bounded patience instead of a permanent
/// waiter on the closer's thread.
pub(crate) const CLEANUP_BUDGET_NS: u64 = 3 * ax_gpu::GPU_WAIT_TIMEOUT.as_secs() * 1_000_000_000;

/// Monotonic deadline after [`CLEANUP_BUDGET_NS`] for a close-path cleanup
/// loop. Callers compare `drm_kapi::clock::now_ns()` against it between
/// retries; on expiry they warn once, give up the observation and let the
/// object go — the host may then leak its side of the resource until reset,
/// the bounded price of unwedging the guest (the same guest-recovery-beats-
/// waiting trade the driver's own bounded waits make).
pub(crate) fn cleanup_deadline() -> u64 {
    drm_kapi::clock::now_ns().saturating_add(CLEANUP_BUDGET_NS)
}

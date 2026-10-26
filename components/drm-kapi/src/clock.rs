// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Monotonic clock and delay hooks used by the timed Linux shims.
//!
//! The kernel installs one platform clock provider during runtime setup. The
//! shared DRM code depends only on these operations, not on a particular HAL or
//! scheduler. Using a time-dependent operation before installing the provider
//! is a configuration error and panics with an explicit message.

use crate::kernel_services::ProviderSlot;

/// Number of nanoseconds in one second.
pub const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Platform clock operations needed by the DRM kernel shims.
///
/// The table must have static lifetime and be installed once during kernel
/// initialization. `sleep_us` must provide the platform's normal blocking
/// sleep semantics; callers that cannot sleep should use `delay_us` or
/// `relax` instead.
pub struct ClockOps {
    /// Read a monotonic clock in nanoseconds.
    pub now_ns: fn() -> u64,
    /// Busy-wait for at least the requested number of microseconds.
    pub delay_us: fn(u32),
    /// Sleep for at least the requested number of microseconds.
    pub sleep_us: fn(u64),
    /// Perform one scheduler or processor relax step.
    pub relax: fn(),
}

/// Install-once platform clock provider.
pub static CLOCK: ProviderSlot<ClockOps> = ProviderSlot::new();

/// Install the platform clock operations.
///
/// Installing the same static table again is idempotent. Replacing an already
/// installed provider is refused by [`ProviderSlot::install`].
pub fn install_clock_ops(ops: &'static ClockOps) -> Result<(), &'static str> {
    CLOCK.install(ops)
}

#[cfg(test)]
mod test_clock {
    use super::ClockOps;

    // Host tests use one real monotonic clock per test thread. This keeps
    // timeout behavior aligned with the host WaitOps provider instead of
    // advancing time only when a test happens to poll the clock.
    std::thread_local! {
        static START: std::time::Instant = std::time::Instant::now();
    }

    fn now_ns() -> u64 {
        START.with(|start| start.elapsed().as_nanos().min(u64::MAX as u128) as u64)
    }

    fn delay_us(micros: u32) {
        std::thread::sleep(std::time::Duration::from_micros(u64::from(micros)));
    }

    fn sleep_us(micros: u64) {
        std::thread::sleep(std::time::Duration::from_micros(micros));
    }

    fn relax() {
        std::thread::yield_now();
    }

    pub(super) static OPS: ClockOps = ClockOps {
        now_ns,
        delay_us,
        sleep_us,
        relax,
    };
}

#[cfg(test)]
fn operations() -> &'static ClockOps {
    // Unit tests do not have an OS runtime to install the normal platform
    // provider. Reuse the same deterministic provider for every test thread.
    if CLOCK.get().is_none() {
        let _ = install_clock_ops(&test_clock::OPS);
    }
    CLOCK.require("clock")
}

#[cfg(not(test))]
fn operations() -> &'static ClockOps {
    CLOCK.require("clock")
}

/// Monotonic time in nanoseconds (`ktime_get()`).
#[inline]
pub fn now_ns() -> u64 {
    (operations().now_ns)()
}

/// Calibrated busy-wait (`udelay()`).
#[inline]
pub fn delay_us(micros: u32) {
    (operations().delay_us)(micros);
}

/// Sleep for at least `micros` (`usleep_range()` minimum).
#[inline]
pub fn sleep_us(micros: u64) {
    (operations().sleep_us)(micros);
}

/// One cooperative relax step inside a polling loop.
#[inline]
pub fn relax() {
    (operations().relax)();
}

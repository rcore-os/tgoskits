//! Host-side unit tests for `ax-cgroup`.
//!
//! These run under `cargo test -p ax-cgroup` on the host target. They use a
//! [`mock::MockProvider`] in place of the kernel provider. All test code is
//! gated behind `#[cfg(test)]` so it never affects the `no_std` build.

mod mock;
mod parse;

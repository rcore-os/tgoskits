//! Facts about the machine this hypervisor is running on.
//!
//! The host panel reads them through `GET /api/host` and the shell reads them
//! out of `GET /api/manifest`, which carries the same object: a panel may not
//! name another panel's operations, so the guest panel cannot ask the host
//! resource for the physical CPU count it draws an overcommit ratio against.
//! The shell handing the manifest's copy down is what keeps it from having to.
//!
//! This module answers "what machine is this", which is a property of the build
//! and of the instant the kernel started — not of the guest registry, and not of
//! a URL, so nothing here names either.
//!
//! Host **memory** is deliberately absent. `axvm::host` publishes only its
//! `cpu` module, so a total or free figure would need a new API on the AxVM
//! runtime; the host panel says the control plane exports none rather than
//! showing a number this build cannot know.

use alloc::vec::Vec;
use std::{sync::OnceLock, time::Instant};

use serde_json::{Value, json};

/// The instant the kernel entry point ran, recorded once by [`mark_boot`].
static BOOT: OnceLock<Instant> = OnceLock::new();

/// Records the boot instant. Called once from `crate::main`.
///
/// A second call is not an error: only the first instant is kept, so a caller
/// that repeats it cannot make the reported uptime jump backwards.
pub fn mark_boot() {
    let _ = BOOT.set(Instant::now());
}

/// Seconds since [`mark_boot`]; `0` when it has not run.
pub fn uptime_secs() -> u64 {
    BOOT.get().map_or(0, |boot| boot.elapsed().as_secs())
}

/// What one host response is built from.
pub struct HostFacts {
    /// `CARGO_PKG_VERSION` of this binary.
    pub version: &'static str,
    /// Target architecture (`AX_ARCH`), empty when the build did not set it.
    pub arch: &'static str,
    /// Board the build targets (`AX_PLATFORM`), empty when unset.
    pub platform: &'static str,
    /// Host CPUs the build was configured for (`AX_SMP`), `1` when unset.
    pub smp: usize,
    /// Physical CPUs the runtime reports: `axvm::host::cpu::count()`.
    pub phys_cpu_count: usize,
    /// Seconds since [`mark_boot`].
    pub uptime_secs: u64,
    /// Control-plane features this binary was built with, in a fixed order.
    pub features: Vec<&'static str>,
}

/// Reads every fact now; nothing is cached.
///
/// The machine does not change, but the uptime does, and a client that polls
/// the host panel wants the second it reads to be one the hypervisor has
/// actually been up.
pub fn facts() -> HostFacts {
    HostFacts {
        version: option_env!("CARGO_PKG_VERSION").unwrap_or("unknown"),
        arch: option_env!("AX_ARCH").unwrap_or(""),
        platform: option_env!("AX_PLATFORM").unwrap_or(""),
        // `AX_SMP` is a build-time string; a build that left it out ran on one
        // CPU, which is also what the runtime's own default is.
        smp: option_env!("AX_SMP")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
        phys_cpu_count: axvm::host::cpu::count(),
        uptime_secs: uptime_secs(),
        features: features(),
    }
}

/// The host object two routes publish, built in one place.
///
/// A JSON shape normally belongs to the handler that owns the route, but this
/// one is published twice — by the descriptor and by `GET /api/host` — and the
/// descriptor may not reach into the transport to borrow it. One function here
/// is the alternative to two copies that drift; it names no path, which is the
/// part of the layering that matters.
pub fn describe() -> Value {
    let facts = facts();
    json!({
        "version": facts.version,
        "arch": facts.arch,
        "platform": facts.platform,
        "smp": facts.smp,
        "phys_cpu_count": facts.phys_cpu_count,
        "uptime_secs": facts.uptime_secs,
        "features": facts.features,
    })
}

/// The features a reader of the host panel can act on.
///
/// Only the ones that change what this build serves are listed: a feature that
/// neither adds a route nor changes the dashboard tells an operator nothing
/// they could use, and the list is not the crate's whole feature set.
fn features() -> Vec<&'static str> {
    let mut enabled = Vec::new();
    for (on, name) in [
        (cfg!(feature = "web"), "web"),
        (cfg!(feature = "web-ui"), "web-ui"),
        (cfg!(feature = "vcpu-perf-load"), "vcpu-perf-load"),
    ] {
        if on {
            enabled.push(name)
        }
    }
    enabled
}

//! Domain execution behind the control plane.
//!
//! Modules here know the domain (VMs, configs on disk, registry changes) and
//! never know a URL. The shared [`crate::vm_pool`] module lives at the crate
//! root because the onboard shell and the HTTP handlers both use it; this
//! module re-exports that implementation only for the HTTP-side namespace.
//!
//! - [`pool`]: the guest configuration directories, scanned and written. The
//!   implementation is shared with shell commands through [`crate::vm_pool`].
//! - [`files`]: files staged on their way into the guest filesystem. The
//!   transfer routes are part of the unified `web` control-plane feature.
//! - [`events`]: registry change events consumed by `/ws/events`. The watcher
//!   runs with the same `web` feature as that route.
//! - [`host`]: the machine this hypervisor runs on, read by the host panel.

pub mod events;
pub mod files;
pub mod host;
pub(crate) use crate::vm_pool as pool;

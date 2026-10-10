//! Domain execution behind the control plane.
//!
//! This is the half of `control` that is not exclusive to it: the onboard
//! shell depends on [`pool`] for its `vm pool` subcommand just as the HTTP
//! handlers do. Modules here know the domain (VMs, configs on disk, registry
//! changes) and never know a URL, which is why the crate root keeps no copy of
//! them.
//!
//! - [`pool`]: the guest configuration directories, scanned and written.
//! - [`files`]: files staged on their way into the guest filesystem. The
//!   transfer routes are part of the unified `web` control-plane feature.
//! - [`events`]: registry change events consumed by `/ws/events`. The watcher
//!   runs with the same `web` feature as that route.
//! - [`host`]: the machine this hypervisor runs on, read by the host panel.

pub mod events;
pub mod files;
pub mod host;
pub mod pool;

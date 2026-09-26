//! Domain execution behind the control plane.
//!
//! This is the half of `control` that is not exclusive to it: the onboard
//! shell depends on [`pool`] for its `vm pool` subcommand just as the HTTP
//! handlers do. Modules here know the domain (VMs, configs on disk, registry
//! changes) and never know a URL, which is why the crate root keeps no copy of
//! them.
//!
//! - [`pool`]: the guest configuration directories, scanned and written.
//! - [`files`]: files staged on their way into the guest filesystem. Only the
//!   transfer routes use it, so it needs both `fs` and `http-axum`.
//! - [`events`]: registry change events consumed by `/ws/events`. The watcher
//!   only runs where that route exists, so the `http-axum` and
//!   `browser-console` features are both required for it.

#[cfg(all(feature = "browser-console", feature = "http-axum"))]
pub mod events;
#[cfg(all(feature = "fs", feature = "http-axum"))]
pub mod files;
#[cfg(feature = "fs")]
pub mod pool;

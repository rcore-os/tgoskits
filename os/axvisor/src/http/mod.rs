//! Management HTTP control plane.
//!
//! Served by an axum `Router` running on a tokio current-thread runtime
//! (see [`server`]). The VM list/detail and start/stop lifecycle routes live
//! in [`vm`]. JSON is built with `serde_json`.
//!
//! Security boundary: mutating routes require a build-time bearer token
//! ([`auth`]); the server binds `127.0.0.1:8080` by default and only binds
//! wider when `[env] AXVM_HTTP_BIND` opts in. See the per-module docs.
//!
//! This whole module is only compiled under the `http-axum` feature, which is
//! off by default. The hand-rolled HTTP/1.0 pilot was intentionally not
//! carried forward.

pub mod auth;
pub mod server;
pub mod vm;

/// Blocking entry point for the management HTTP server.
///
/// Spawned on its own task (see `crate::main`); builds the tokio runtime and
/// serves until the hypervisor shuts down.
pub fn serve() {
    server::serve();
}

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
#[cfg(feature = "web-ui")]
pub mod web_ui;

/// Whether the web UI is ready to be served, or why it is not.
///
/// This is the typed contract between [`web_ui::init`] (which extracts the
/// embedded UI bundle) and [`server::router`]: when `Unavailable`, the UI
/// routes return `503` while `/api/*` keeps serving. It is defined here so the
/// type is available whenever the `http-axum` feature is on, regardless of
/// `web-ui`.
#[derive(Clone, Copy, Debug)]
pub enum WebUiStatus {
    /// The bundle was extracted to `current/` and is served.
    Ready,
    /// Extraction failed; UI paths return `503`, `/api/*` stays up.
    Unavailable { reason: &'static str },
}

impl WebUiStatus {
    /// `true` when the UI should be served normally.
    pub fn is_ready(self) -> bool {
        matches!(self, WebUiStatus::Ready)
    }
}

/// Blocking entry point for the management HTTP server.
///
/// Spawned on its own task (see `crate::main`); builds the tokio runtime and
/// serves until the hypervisor shuts down.
pub fn serve() {
    server::serve();
}

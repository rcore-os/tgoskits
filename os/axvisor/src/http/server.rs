//! axum-based management HTTP server (`http-axum` feature).
//!
//! Runs an axum `Router` on a tokio current-thread runtime and serves the
//! management API. Routes and JSON fields mirror the hand-rolled pilot's API,
//! but dispatch and JSON construction are delegated to axum + serde_json.
//!
//! ```text
//! GET    /api/vms            → 200, JSON array (summary form)
//! GET    /api/vms/{id}       → 200, JSON detail (with vcpu_states) | 404
//! POST   /api/vms/create     → 200 {"id":N} | 400 | 409 | 500 (body {"toml": "..."})
//! DELETE /api/vms/{id}       → 204 | 404 | 500
//! POST   /api/vms/{id}/start  → 200 {"ok":true,"status":...} | 404 | 409 | 503
//! POST   /api/vms/{id}/stop   → 200 {"ok":true,"status":...} | 404 | 409 | 503
//! POST   /api/vms/{id}/pause  → 200 {"ok":true,"status":...} | 404 | 409 | 503
//! POST   /api/vms/{id}/resume → 200 {"ok":true,"status":...} | 404 | 409 | 503
//! ```
//!
//! Mutating routes (`create`/`delete`/`start`/`stop`/`pause`/`resume`) require
//! `Authorization: Bearer <token>` with the build-time `[env] AXVM_HTTP_TOKEN`;
//! see [`crate::http::auth`]. GET routes are open. The listener binds
//! [`bind_addr`], loopback by default.
//!
//! The tokio reactor is initialized with `enable_io()` only (no time driver),
//! which needs only epoll, so no `timerfd` syscall is required.
//!
//! # Lifecycle semantics and known limits
//!
//! Management commands use the single lifecycle owner and `VmOperation`.
//! `pause` and `stop` await acceptance; their terminal snapshots are published
//! after participant and device quiescence. `start` and `resume` await owner
//! initialization/restoration, open admission and wake; real guest progress
//! remains observable through the detail endpoint's live run counters.
//!
//! Counters aggregate execution and park progress across the current run. They
//! do not replace per-participant confirmations or prove device/DMA quiescence.
//! Host monotonic time continues during pause; saved guest timer deadlines may
//! already have expired when the vCPU resumes. A passthrough device without a
//! supported DMA quiescence contract cannot complete the affected teardown.

#[cfg(feature = "browser-console")]
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use axum::Router;

#[cfg(feature = "http-axum")]
use crate::http::vm;
#[cfg(feature = "http-axum")]
use axum::{routing::get, routing::post};

#[cfg(feature = "browser-console")]
static LISTENING: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "browser-console")]
struct ListeningGuard;

#[cfg(feature = "browser-console")]
impl Drop for ListeningGuard {
    fn drop(&mut self) {
        LISTENING.store(false, Ordering::Release);
    }
}

/// Assemble only the HTTP services selected by build features.
pub fn router() -> Router {
    let router = Router::new();

    #[cfg(feature = "http-axum")]
    let router = router.merge(management_router());

    #[cfg(feature = "browser-console")]
    let router = router.merge(crate::http::browser_console::router());

    router
}

#[cfg(feature = "http-axum")]
fn management_router() -> Router {
    Router::new()
        .route("/api/vms", get(vm::list_vms))
        .route("/api/vms/{id}", get(vm::vm_detail).delete(vm::vm_delete))
        .route("/api/vms/create", post(vm::vm_create))
        .route("/api/vms/{id}/start", post(vm::vm_start))
        .route("/api/vms/{id}/stop", post(vm::vm_stop))
        .route("/api/vms/{id}/pause", post(vm::vm_pause))
        .route("/api/vms/{id}/resume", post(vm::vm_resume))
}

/// Bind address for the management HTTP server.
///
/// Defaults to loopback (`127.0.0.1:8080`) so a stock `http-axum` build is not
/// reachable from the management network. Test/dev flows that need QEMU
/// hostfwd to reach the in-guest listener must opt in to all interfaces by
/// setting `[env] AXVM_HTTP_BIND = "0.0.0.0:8080"` in their build config; the
/// mutating routes still require the bearer token regardless of the bind.
pub(super) fn bind_addr() -> &'static str {
    option_env!("AXVM_HTTP_BIND").unwrap_or("127.0.0.1:8080")
}

/// Blocking serve: build a tokio current-thread runtime and hand it to axum.
///
/// `main` spawns this on its own task via `std::thread::spawn(|| http::serve())`;
/// the runtime is built here. Only the IO driver is enabled — the epoll
/// reactor suffices for `axum::serve`; a time driver would need `timerfd`.
pub fn serve() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("failed to build Axvisor HTTP Tokio runtime")?;
    rt.block_on(async {
        let bind = bind_addr();
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .with_context(|| format!("failed to bind Axvisor HTTP server at {bind}"))?;
        #[cfg(feature = "browser-console")]
        LISTENING.store(true, Ordering::Release);
        #[cfg(feature = "browser-console")]
        let _listening_guard = ListeningGuard;
        info!("Axvisor HTTP server (axum) listening on {bind}");
        axum::serve(listener, router())
            .await
            .context("Axvisor HTTP server stopped")
    })
}

#[cfg(feature = "browser-console")]
pub(crate) fn is_listening() -> bool {
    LISTENING.load(Ordering::Acquire)
}

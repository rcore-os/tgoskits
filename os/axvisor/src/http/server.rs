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
//! POST   /api/vms/{id}/start → 200 {"ok":true,"status":...} | 404 | 409 | 503
//! POST   /api/vms/{id}/stop  → 200 {"ok":true,"status":...} | 404 | 409 | 503
//! ```
//!
//! Mutating routes (`create`/`delete`/`start`/`stop`) require
//! `Authorization: Bearer <token>` with the build-time `[env] AXVM_HTTP_TOKEN`;
//! see [`crate::http::auth`]. GET routes are open. The listener binds
//! [`bind_addr`], loopback by default.
//!
//! The tokio reactor is initialized with `enable_io()` only (no time driver),
//! which needs only epoll, so no `timerfd` syscall is required.

use axum::{Router, routing::get, routing::post};

use crate::http::vm;

/// Assemble the management routes.
pub fn router() -> Router {
    Router::new()
        .route("/api/vms", get(vm::list_vms))
        .route("/api/vms/{id}", get(vm::vm_detail).delete(vm::vm_delete))
        .route("/api/vms/create", post(vm::vm_create))
        .route("/api/vms/{id}/start", post(vm::vm_start))
        .route("/api/vms/{id}/stop", post(vm::vm_stop))
}

/// Bind address for the management HTTP server.
///
/// Defaults to loopback (`127.0.0.1:8080`) so a stock `http-axum` build is not
/// reachable from the management network. Test/dev flows that need QEMU
/// hostfwd to reach the in-guest listener must opt in to all interfaces by
/// setting `[env] AXVM_HTTP_BIND = "0.0.0.0:8080"` in their build config; the
/// mutating routes still require the bearer token regardless of the bind.
fn bind_addr() -> &'static str {
    option_env!("AXVM_HTTP_BIND").unwrap_or("127.0.0.1:8080")
}

/// Blocking serve: build a tokio current-thread runtime and hand it to axum.
///
/// `main` spawns this on its own task via `std::thread::spawn(|| http::serve())`;
/// the runtime is built here. Only the IO driver is enabled — the epoll
/// reactor suffices for `axum::serve`; a time driver would need `timerfd`.
pub fn serve() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async {
        let bind = bind_addr();
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .expect("failed to bind management HTTP server");
        info!("management HTTP server (axum) listening on {bind}");
        axum::serve(listener, router()).await.expect("server error");
    });
}

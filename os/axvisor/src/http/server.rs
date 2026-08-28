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
//! The pause/resume routes are backed by the axvm lifecycle state machine,
//! which accepts only `Running → Paused` (pause) and `Paused → Running`
//! (resume). Callers must not assume stronger guarantees than the runtime
//! provides:
//!
//! - `pause` is fire-and-forget: the status flips to `Paused` synchronously,
//!   but running vCPUs park only at their next run-loop iteration. There is no
//!   synchronous pause-quiesce wait and **no completion-confirmation API** — a
//!   `Paused` status only means the pause request was accepted, not that the
//!   execution surface has gone quiet (see `virtualization/axvm/docs/
//!   lifecycle.md`). To *observe* a vCPU actually parking (not a full
//!   quiescence guarantee), poll the VM detail: `guest_park_count` advances
//!   only when a vCPU has genuinely parked in the suspend wait, and
//!   `guest_entry_count` advances only after the guest has actually re-entered
//!   (on first start and on every wake from suspend). Both are **VM-level
//!   monotonic aggregate** counters shared by every vCPU task of the VM — they
//!   prove that *at least one* vCPU made progress, not that every vCPU, device,
//!   or timer has quiesced (see the device/timer limits below).
//! - Pause does not save or mask guest timer state. Host time keeps flowing
//!   while the guest is suspended, so on resume the guest observes a time
//!   jump; long pauses drift time-sensitive guests.
//! - Device suspension covers only devices registered with lifecycle
//!   semantics; other devices are not quiesced while paused.

use axum::{Router, routing::get, routing::post};

use crate::http::WebUiStatus;
use crate::http::vm;
#[cfg(feature = "web-ui")]
use crate::http::web_ui;

/// Assemble the management routes.
///
/// `ui_status` is the readiness reported by [`web_ui::init`]: when the web UI
/// is unavailable the UI routes return `503` while `/api/*` keeps serving.
pub fn router(ui_status: WebUiStatus) -> Router {
    let router = Router::new()
        .route("/api/vms", get(vm::list_vms))
        .route("/api/vms/{id}", get(vm::vm_detail).delete(vm::vm_delete))
        .route("/api/vms/create", post(vm::vm_create))
        .route("/api/vms/{id}/start", post(vm::vm_start))
        .route("/api/vms/{id}/stop", post(vm::vm_stop))
        .route("/api/vms/{id}/pause", post(vm::vm_pause))
        .route("/api/vms/{id}/resume", post(vm::vm_resume));
    #[cfg(feature = "web-ui")]
    let router = router.merge(web_ui::ui_routes(ui_status));
    #[cfg(not(feature = "web-ui"))]
    let _ = ui_status;
    router
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
    // Extract the dashboard assets to `/web/axvisor-ui/current` before the async
    // runtime exists: these are blocking filesystem writes and must not contend
    // with the IO driver's event loop. `web-ui` implies `fs`, so the rootfs is
    // mounted. The returned status is passed into the router so the UI can be
    // served (Ready) or return 503 (Unavailable) while `/api/*` stays up.
    #[cfg(feature = "web-ui")]
    let ui_status = web_ui::init();
    #[cfg(not(feature = "web-ui"))]
    let ui_status = WebUiStatus::Ready;
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
        axum::serve(listener, router(ui_status))
            .await
            .expect("server error");
    });
}

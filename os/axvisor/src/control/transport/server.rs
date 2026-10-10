//! axum-based management HTTP server (`web` feature).
//!
//! Runs an axum `Router` on a tokio current-thread runtime. The router is
//! handed in already assembled (see [`crate::control::serve`]), so this module
//! owns the listener and the runtime and nothing else: it contains no path
//! literal and no domain name. Which routes exist, and what each one answers,
//! is declared in [`crate::control::capability::table`].
//!
//! The control plane runs under a local-host trust model and therefore has no
//! authentication: every route is open to any caller that can reach the
//! listener. The listener binds [`bind_addr`], loopback by default, so reaching
//! it from another host requires an explicit `[env] AXVM_HTTP_BIND` opt-in.
//!
//! The tokio reactor is initialized with `enable_io()` only (no time driver),
//! which needs only epoll, so no `timerfd` syscall is required.
//!
//! # Lifecycle semantics and known limits
//!
//! Management commands use the single lifecycle owner and `VmOperation`.
//! `pause` and `stop` await the lifecycle operation; their terminal snapshots
//! are published after participant and device quiescence. `start` and `resume` await owner
//! initialization/restoration, open admission and wake; real guest progress
//! remains observable through the detail endpoint's live run counters.
//!
//! Counters aggregate execution and park progress across the current run. They
//! do not replace per-participant confirmations or prove device/DMA quiescence.
//! Host monotonic time continues during pause; saved guest timer deadlines may
//! already have expired when the vCPU resumes. A passthrough device without a
//! supported DMA quiescence contract cannot complete the affected teardown.

use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use axum::Router;

static LISTENING: AtomicBool = AtomicBool::new(false);

struct ListeningGuard;

impl Drop for ListeningGuard {
    fn drop(&mut self) {
        LISTENING.store(false, Ordering::Release);
    }
}

/// Bind address for the management HTTP server.
///
/// Defaults to loopback (`127.0.0.1:8080`) so a stock `web` build is not
/// reachable from the management network. Test/dev flows that need QEMU
/// hostfwd to reach the in-guest listener must opt in to all interfaces by
/// setting `[env] AXVM_HTTP_BIND = "0.0.0.0:8080"` in their build config.
pub(crate) fn bind_addr() -> &'static str {
    option_env!("AXVM_HTTP_BIND").unwrap_or("127.0.0.1:8080")
}

/// Blocking serve: build a tokio current-thread runtime and hand it to axum.
///
/// `main` spawns this on its own task via `std::thread::spawn(|| control::serve())`;
/// the runtime is built here. Only the IO driver is enabled — the epoll
/// reactor suffices for `axum::serve`; a time driver would need `timerfd`.
pub fn serve(router: Router) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("failed to build Axvisor HTTP Tokio runtime")?;
    rt.block_on(async {
        let bind = bind_addr();
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .with_context(|| format!("failed to bind Axvisor HTTP server at {bind}"))?;
        LISTENING.store(true, Ordering::Release);
        let _listening_guard = ListeningGuard;
        info!("Axvisor HTTP server (axum) listening on {bind}");
        axum::serve(listener, router)
            .await
            .context("Axvisor HTTP server stopped")
    })
}

pub(crate) fn is_listening() -> bool {
    LISTENING.load(Ordering::Acquire)
}

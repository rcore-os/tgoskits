//! Dashboard assets served from the built-in package on the active root.
//!
//! `axbuild` packs the `web-ui/dist` tree into the host initramfs under
//! [`BUILTIN_WEB_UI_DIR`], and `builtin::install_builtin` republishes the same
//! package onto the disk root when one is committed. Both the initramfs memory
//! root and the installed disk root therefore expose the dashboard at one
//! absolute path, so a request is a single read from that path: nothing is
//! embedded in the kernel binary and no extraction step runs at boot.

use core::sync::atomic::{AtomicBool, Ordering};

use ax_fs_ng::{current_fs_context, vfs::FsContext};
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use axvisor::builtin::BUILTIN_WEB_UI_DIR;

const APP_SHELL_ENTRY: &str = "index.html";

/// Every dashboard response carries the same security headers.
///
/// `ws:` is reserved for the terminal stage. `style-src 'unsafe-inline'`
/// covers React inline style attributes while `script-src 'self'` stays strict
/// because vite's production output has no inline scripts.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; font-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// The app shell names the content-hashed assets, so it must be revalidated on
/// every load; the hashed assets themselves can be cached forever.
const SHELL_CACHE_CONTROL: &str = "no-cache";
const HASHED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Set once at router assembly: `false` means the booted image carried no
/// dashboard package (for example a hand-built initramfs), which turns the UI
/// routes into an explicit 503 instead of a confusing stream of 404s.
static PACKAGE_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Dashboard routes: the app shell and its content-hashed assets.
///
/// Only these two shapes are registered. There is deliberately no SPA
/// catch-all: the shell keeps its navigation in memory rather than in the URL,
/// so unknown paths (including unmatched `/api/*` or `/ws/*` ones) keep the
/// router's own 404 instead of being swallowed by the UI.
pub fn router() -> Router {
    let fs = snapshot_fs_context();
    PACKAGE_AVAILABLE.store(shell_available(&fs), Ordering::Release);
    if !PACKAGE_AVAILABLE.load(Ordering::Acquire) {
        log::warn!(
            "web-ui: no dashboard package at {BUILTIN_WEB_UI_DIR}; the built-in initramfs \
             bundle produced by `cargo axvisor build/qemu` is required to serve the UI"
        );
    }
    Router::new()
        .route("/", get(app_shell))
        .route("/assets/{*path}", get(hashed_asset))
        .with_state(fs)
}

async fn app_shell(State(fs): State<FsContext>) -> Response {
    if !PACKAGE_AVAILABLE.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    serve(&fs, APP_SHELL_ENTRY, SHELL_CACHE_CONTROL)
}

async fn hashed_asset(State(fs): State<FsContext>, Path(path): Path<String>) -> Response {
    if !PACKAGE_AVAILABLE.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    // The builtin package is a plain directory tree; still refuse anything but
    // plain relative names so a malformed request can never walk out of it.
    if path.is_empty()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve(&fs, &format!("assets/{path}"), HASHED_CACHE_CONTROL)
}

/// Reads one entry of the built-in dashboard package, or 404 when absent.
fn serve(fs: &FsContext, entry: &str, cache_control: &'static str) -> Response {
    let path = format!("{BUILTIN_WEB_UI_DIR}/{entry}");
    match fs.read(&path) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type(entry)),
                (header::CACHE_CONTROL, cache_control),
                (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Err(error) => {
            log::info!("web-ui: read {path} failed: {error:?}");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/// Clones the canonical filesystem context the router will serve from.
///
/// The HTTP thread is spawned after `builtin::prepare_root()`, so the snapshot
/// already reflects the committed disk root when one was installed.
fn snapshot_fs_context() -> FsContext {
    current_fs_context().lock().clone()
}

fn shell_available(fs: &FsContext) -> bool {
    let shell = format!("{BUILTIN_WEB_UI_DIR}/{APP_SHELL_ENTRY}");
    let Ok(resource) = fs.resolve(&shell) else {
        return false;
    };
    resource.check_is_file().is_ok() && resource.len().is_ok_and(|len| len > 0)
}

fn content_type(entry: &str) -> &'static str {
    let extension = entry.rsplit_once('.').map_or("", |(_, tail)| tail);
    match extension {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

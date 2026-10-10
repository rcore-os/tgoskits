//! Embedded dashboard assets, served straight from memory.
//!
//! The bytes come from the table `build.rs` generates out of `web-ui/dist`, so
//! a request is a lookup plus a response: no filesystem read, no extraction
//! step, and no runtime state to keep consistent with the router.

use axum::{
    Router,
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

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

const APP_SHELL_PATH: &str = "/";

/// Dashboard routes: the app shell and its content-hashed assets.
///
/// Only these two shapes are registered. There is deliberately no SPA
/// catch-all: the shell keeps its navigation in memory rather than in the URL,
/// so unknown paths (including unmatched `/api/*` or `/ws/*` ones) keep the
/// router's own 404 instead of being swallowed by the UI.
pub fn router() -> Router {
    assert_asset_table_sane();

    Router::new()
        .route(APP_SHELL_PATH, get(app_shell))
        .route("/assets/{*path}", get(hashed_asset))
}

async fn app_shell() -> Response {
    serve(APP_SHELL_PATH, SHELL_CACHE_CONTROL)
}

async fn hashed_asset(Path(path): Path<String>) -> Response {
    serve(&format!("/assets/{path}"), HASHED_CACHE_CONTROL)
}

/// Serves one entry of the embedded table, or 404 when the path is not in it.
fn serve(path: &str, cache_control: &'static str) -> Response {
    match UI_ASSETS.iter().find(|(asset, _, _)| *asset == path) {
        Some((_, mime, bytes)) => (
            [
                (header::CONTENT_TYPE, *mime),
                (header::CACHE_CONTROL, cache_control),
                (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            *bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Checks the generated table once at startup.
///
/// The table is produced by the build script, so a broken one means the build
/// emitted garbage: failing here beats serving a half-working UI whose
/// navigation silently 404s.
fn assert_asset_table_sane() {
    assert!(
        !UI_ASSETS.is_empty(),
        "web-ui: the embedded asset table is empty"
    );
    assert!(
        UI_ASSETS.iter().any(|(path, _, _)| *path == APP_SHELL_PATH),
        "web-ui: the embedded asset table has no entry for {APP_SHELL_PATH}"
    );
}

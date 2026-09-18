//! React dashboard served by the axvisor HTTP listener.
//!
//! The module owns exactly one URL namespace — `/` and its hashed assets — and
//! serves it from assets embedded in the binary (see [`assets`]). It is the
//! slot `server::ui_router()` fills when the `web-ui` feature is enabled.

pub mod assets;

use axum::Router;

/// Dashboard routes: the app shell at `/` plus `/assets/*`.
pub fn router() -> Router {
    assets::router()
}

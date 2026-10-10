//! React dashboard served by the axvisor HTTP listener.
//!
//! The module owns exactly one URL namespace — `/` and its hashed assets — and
//! serves it from assets embedded in the binary (see [`assets`]).
//! [`crate::control::serve`] merges it when the `web-ui` feature is enabled,
//! and nothing else claims `/`.

pub mod assets;

use axum::Router;

/// Dashboard routes: the app shell at `/` plus `/assets/*`.
pub fn router() -> Router {
    assets::router()
}

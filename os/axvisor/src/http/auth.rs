//! Bearer-token access control for the management HTTP control plane.
//!
//! Mutating routes (`create`/`delete`/`start`/`stop`) require an
//! `Authorization: Bearer <token>` header matching the build-time token. The
//! token is baked into the image at
//! build time from the `[env] AXVM_HTTP_TOKEN` build-config variable — the same
//! `option_env!` mechanism `crate::shell::command::base` uses for `AX_ARCH`.
//!
//! The control plane is **deny-by-default**: if `AXVM_HTTP_TOKEN` is unset,
//! every protected route returns `401` and cannot be used. There is no
//! "fall back to allowing writes without a token" path — a build that forgets
//! the token fails its tests instead of silently exposing EL2 state changes.
//! Read-only routes (`GET`) are intentionally left open; they expose no state
//! mutation, and the default loopback bind (see [`crate::http::server`]) keeps
//! them off the management network unless an operator explicitly opts in.

use axum::{
    Json,
    extract::FromRequestParts,
    http::{
        StatusCode,
        header::{AUTHORIZATION, HeaderValue},
    },
};
use serde_json::{Value, json};

/// A request that carries a matching `Authorization: Bearer <token>` header.
///
/// Attach as the first extractor on a mutating handler. Rejects the request
/// with `401 Unauthorized` when no token was baked into the image
/// (`AXVM_HTTP_TOKEN` unset) or the header is missing / does not match.
pub struct ApiToken;

/// `GET /api/auth` — reports whether the caller's token is usable.
///
/// Discovery has to stay open (a client cannot read a capability list it is not
/// yet authorized for), so a client that wants to *validate* a token before
/// relying on it needs a route that does nothing but check it. Reaching this
/// handler means the extractor already accepted the token, so the body only
/// confirms it; the manifest points clients here (see
/// [`crate::http::manifest`]) so no client hard-codes the path.
pub async fn check(_token: ApiToken) -> Json<Value> {
    Json(json!({ "ok": true }))
}

impl ApiToken {
    /// Whether the given header value carries the required bearer token.
    fn header_matches(value: &HeaderValue) -> bool {
        let Some(token) = option_env!("AXVM_HTTP_TOKEN") else {
            return false;
        };
        value.to_str().ok().is_some_and(|value| {
            value
                .strip_prefix("Bearer ")
                .is_some_and(|rest| rest == token)
        })
    }
}

impl<S: Sync> FromRequestParts<S> for ApiToken {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let authorized = parts
            .headers
            .get(AUTHORIZATION)
            .is_some_and(Self::header_matches);
        if authorized {
            Ok(ApiToken)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

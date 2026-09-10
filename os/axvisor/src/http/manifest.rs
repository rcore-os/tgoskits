//! Top-level control-plane manifest.
//!
//! The manifest is the runtime contract the dashboard shell navigates by: it
//! lists the resource families this build exposes, what each of them supports,
//! and where it hangs. The shell never hardcodes routes — it follows `href`,
//! and degrades to a JSON view for kinds it does not know.

use axum::Json;
use serde_json::{Value, json};

/// Contract version.
///
/// Bump only for shape changes an older client cannot degrade on; new fields
/// and new resource nodes are additive and keep the current version.
const PROTO: u32 = 1;

/// `GET /api/` — top-level capability manifest.
///
/// Read-only and unauthenticated like every other GET route; mutating routes
/// are the ones gated by the build-time bearer token (see [`crate::http::auth`]).
/// The `auth` node tells a client where to check a token before relying on it,
/// so the check path is a declared part of the contract rather than something
/// each client hard-codes.
pub async fn get_manifest() -> Json<Value> {
    Json(json!({
        "proto": PROTO,
        "auth": {
            "href": "/api/auth",
            "scheme": "bearer",
        },
        "resources": [{
            "kind": "vms",
            "title": "虚拟机",
            "href": "/api/vms",
            "verbs": ["read", "write"],
        }],
    }))
}

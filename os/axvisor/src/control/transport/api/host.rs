//! `GET /api/host` — the machine the hypervisor is running on.
//!
//! The dashboard shows these facts on its host panel: which build this is, what
//! it is running on, and how long it has been up. The values come from
//! [`crate::control::domain::host`], which is why this file holds no
//! `option_env!` of its own — the domain reads the machine, the handler shapes
//! the response.
//!
//! The shape is [`crate::control::domain::host::describe`] rather than one
//! written here, because the descriptor publishes the same object and a handler
//! is not a place the descriptor may reach into.

use axum::Json;
use serde_json::Value;

use crate::control::domain::host;

/// `GET /api/host` — build, machine, and uptime facts for the host panel.
///
/// Nothing here can fail: every field is known from the build or from the boot
/// instant, so the route has no error case to report and always answers 200.
/// The panel re-reads it for the uptime; everything else it could have taken
/// from the manifest it was already handed.
pub async fn host_info() -> Json<Value> {
    Json(host::describe())
}

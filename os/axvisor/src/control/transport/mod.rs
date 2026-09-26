//! Byte-level plumbing for the control plane.
//!
//! Nothing here knows what a VM is: the modules below own HTTP framing, the
//! WebSocket upgrade, the console gateway, and the management handlers. A
//! domain name only enters this layer through the URLs [`super::capability`]
//! declares, and the paths themselves stay in that layer: each module here
//! exports route builders, and [`server`] receives the assembled router as data
//! from the assembly root rather than walking the capability table itself.
//!
//! [`api`] holds the management handlers, [`events`] the registry event stream,
//! and [`browser_console`] the console gateway.

#[cfg(feature = "http-axum")]
pub mod api;
#[cfg(feature = "browser-console")]
pub mod browser_console;
#[cfg(all(feature = "browser-console", feature = "http-axum"))]
pub mod events;
pub mod server;

#[cfg(feature = "browser-console")]
use axum::http::{HeaderMap, StatusCode, header};

/// Rejects a WebSocket upgrade that a page from another origin started.
///
/// The control plane has no authentication, so the origin check is what keeps a
/// random web page from opening the hypervisor's console or event sockets from
/// the viewer's browser. It compares the `Origin` header against the `Host` the
/// request was addressed to, which accepts the listener's own page and rejects
/// anything else.
#[cfg(feature = "browser-console")]
pub(super) fn validate_browser_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;

    if origin == format!("http://{host}") || origin == format!("https://{host}") {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

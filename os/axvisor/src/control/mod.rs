//! The control plane: every entry point the dashboard can reach.
//!
//! Served by an axum `Router` running on a tokio current-thread runtime (see
//! [`transport::server`]). The browser console and VM management API are
//! independent features that may share this listener when both are enabled.
//!
//! The server binds `127.0.0.1:8080` by default and only binds wider when
//! `[env] AXVM_HTTP_BIND` opts in. The control plane runs under a local-host
//! trust model: it has no authentication, so every caller that can reach the
//! listener may create, start, and delete VMs.
//!
//! # Internal layering
//!
//! The directory holds one feature, so it also holds the boundary that used to
//! be spread over the crate root:
//!
//! - [`capability`] declares what exists and what its paths look like.
//! - [`transport`] moves bytes; it knows the declaration, not the domain.
//! - [`domain`] executes; it knows the domain, not the paths.
//! - [`web`] embeds the dashboard assets.
//!
//! The arrows run `web → capability → domain`, plus the declaration reaching
//! the transport as data rather than as an import: this module is the assembly
//! root, so it is the one place that walks [`capability::table`], builds the
//! router, and hands it to [`transport::server`]. The transport layer contains
//! no path literal as a result, and there is no module cycle between the two.
//!
//! Only `capability`, `transport`, and `web` are exclusive to this feature;
//! [`domain`] is the part the onboard shell is allowed to depend on too, which
//! is why it lives here rather than at the crate root. Everything the crate
//! exports from this module is therefore either [`serve`] (the entry point) or
//! a status query used by the startup banner.

#[cfg(any(feature = "browser-console", feature = "http-axum"))]
pub mod capability;
pub mod domain;
#[cfg(any(feature = "browser-console", feature = "http-axum"))]
pub mod transport;
#[cfg(feature = "web-ui")]
pub mod web;

/// Blocking entry point for the configured HTTP services.
///
/// Spawned on its own task (see `crate::main`); builds the tokio runtime and
/// serves until the hypervisor shuts down.
///
/// This is the assembly root: the router is built here, from the capability
/// table, and handed to the transport as data. That keeps the transport from
/// importing the capability layer and keeps the paths in one place.
#[cfg(any(feature = "browser-console", feature = "http-axum"))]
pub fn serve() -> anyhow::Result<()> {
    let router = capability::table::router();

    // The dashboard owns `/` and `/assets/*`. When the feature is off those
    // paths stay unregistered, which is how a console-gateway-only or API-only
    // build keeps its own 404 instead of serving half a UI.
    #[cfg(feature = "web-ui")]
    let router = router.merge(web::router());

    transport::server::serve(router)
}

/// Configured HTTP listener address used by the startup access banner.
#[cfg(feature = "browser-console")]
pub(crate) fn bind_addr() -> &'static str {
    transport::server::bind_addr()
}

/// Whether the HTTP listener has successfully bound its configured address.
#[cfg(feature = "browser-console")]
pub(crate) fn is_listening() -> bool {
    transport::server::is_listening()
}

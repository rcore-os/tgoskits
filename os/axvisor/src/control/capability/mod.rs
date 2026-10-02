//! What this build can serve, and under which paths.
//!
//! This is the one place that knows the response surface: the resource kinds,
//! the operations each kind offers, and the URLs those operations live at. It
//! is also the only module allowed to contain `/api/` and `/ws/` literals, so a
//! route cannot exist without being declared here and a declaration cannot
//! point at a route that was never registered.
//!
//! The declaration is [`table`], a flat record per operation. Both consumers
//! walk it instead of restating it: [`table::router`] builds the Axum router
//! from the entries, and [`descriptor`] builds `GET /api/manifest` from the
//! same entries. Neither projection keeps a second copy, so neither can drift.
//! The assembled router is handed to [`crate::control::transport`] as data by
//! the assembly root, which is why the transport never learns a path.
//!
//! What belongs in a panel, and what a panel may offer, stays with the
//! frontend; this module only says what exists.

pub mod descriptor;
pub mod table;

/// Path of the descriptor itself.
///
/// The descriptor cannot say where the descriptor lives: a client needs one
/// fixed path to ask before it has anything to read. This constant is one half
/// of that bootstrap pair; the frontend's own literal is the other half.
pub const MANIFEST_PATH: &str = "/api/manifest";

/// One resource kind: the panel the dashboard renders, and the operations that
/// panel may ask for.
pub struct Resource {
    /// Panel kind, shared with the frontend's panel registry.
    pub kind: &'static str,
    /// Panel title, in the language the dashboard is written in.
    pub title: &'static str,
    /// Base path of the resource's namespace.
    pub root: &'static str,
    /// What the panel can do, as a summary rather than a per-link list.
    ///
    /// This is coarser than [`Endpoint::verb`] and not derived from it: a
    /// console socket is one link that reads, writes, and streams, so the
    /// panel's summary names all three while the link names one. The summary is
    /// what a navigation badge counts; the links are what a panel calls.
    pub verbs: &'static [Verb],
    pub endpoints: &'static [Endpoint],
}

/// One operation on one resource.
///
/// Routing and describing read different halves of the same record: `path` and
/// `build` say where the operation lives, `name` and `verb` say what it is.
/// They are one record because they describe one operation; split apart, they
/// would become two authorities that can disagree.
pub struct Endpoint {
    /// Operation name, unique within its resource, e.g. `start`, `stream`.
    pub name: &'static str,
    pub verb: Verb,
    pub method: Method,
    /// Path template, e.g. `/api/vms/{id}/start`.
    pub path: &'static str,
    /// Route assembly for this entry's own method.
    ///
    /// Two entries that share a path are merged by Axum, which is how
    /// `GET /api/vms/{id}` and `DELETE /api/vms/{id}` end up on one route.
    pub build: fn() -> axum::routing::MethodRouter,
}

/// What an operation does to the resource.
///
/// Like [`Method`], a variant is present exactly when a build serves it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Read,
    Write,
    /// Opens a live stream and keeps it open.
    #[cfg(feature = "browser-console")]
    Stream,
}

impl Verb {
    pub const fn as_str(self) -> &'static str {
        match self {
            Verb::Read => "read",
            Verb::Write => "write",
            #[cfg(feature = "browser-console")]
            Verb::Stream => "stream",
        }
    }
}

/// HTTP method of an operation.
///
/// A variant is present exactly when a build serves it, so the type describes
/// what this hypervisor answers rather than every method HTTP has.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Method {
    Get,
    #[cfg(feature = "http-axum")]
    Post,
    #[cfg(feature = "http-axum")]
    Delete,
    /// Asks for the current offset of a transfer that stopped.
    #[cfg(feature = "http-axum")]
    Head,
    /// Carries one chunk of a transfer.
    #[cfg(feature = "http-axum")]
    Patch,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            #[cfg(feature = "http-axum")]
            Method::Post => "POST",
            #[cfg(feature = "http-axum")]
            Method::Delete => "DELETE",
            #[cfg(feature = "http-axum")]
            Method::Head => "HEAD",
            #[cfg(feature = "http-axum")]
            Method::Patch => "PATCH",
        }
    }
}

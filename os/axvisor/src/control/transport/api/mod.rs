//! Request handlers for the management API (`http-axum` feature).
//!
//! Each handler owns one URL and the shape of its JSON; the domain work it asks
//! for lives in [`crate::control::domain`]. The URLs are declared in
//! [`crate::control::capability`], which also assembles the router these
//! handlers are mounted on, so no path is named here.

#[cfg(feature = "fs")]
pub mod files;
pub mod vm;

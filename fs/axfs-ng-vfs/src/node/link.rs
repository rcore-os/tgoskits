//! Link resolution is separate from the text displayed by readlink.

use alloc::string::String;

use crate::{Location, VfsResult};

/// Snapshot acquired only after the walker decides to follow a link.
#[derive(Debug)]
pub enum LinkTarget {
    Text(String),
    Object(Location),
}

/// Optional link capability, including dynamic authorization and target lookup.
///
/// Implementations release task/fd-table locks before returning an owned target.
pub trait LinkOps: Send + Sync {
    fn target(&self) -> VfsResult<LinkTarget>;

    /// Produces one coherent display snapshot without a separate length query.
    fn read_link(&self) -> VfsResult<String>;
}

use alloc::{string::String, vec::Vec};

use axfs_ng_vfs::PathHandle;

/// The last component as supplied to the original lookup operation.
///
/// Special components are not converted into the resolved directory's
/// physical name. Namespace operations apply their own Linux error ordering.
#[derive(Debug, PartialEq, Eq)]
pub enum FinalComponent {
    Name(String),
    Root,
    Current,
    Parent,
}

/// An owned parent selection, its unresolved component and search boundary.
///
/// This result does not grant namespace mutation authority. The backend must
/// still observe and authorize the actual naming slot when committing.
pub struct ParentLookup {
    pub(super) searched: Vec<PathHandle>,
    pub(super) boundary: Option<PathHandle>,
    pub(super) directory: PathHandle,
    pub(super) component: FinalComponent,
    pub(super) directory_required: bool,
}

impl ParentLookup {
    /// Borrows the selected parent while retaining its actual mount use.
    pub fn directory(&self) -> &PathHandle {
        &self.directory
    }

    /// Borrows the unresolved component without interpreting it as a name.
    pub fn component(&self) -> &FinalComponent {
        &self.component
    }
}

//! Gate-external reference preparation for one native unlink request.

use alloc::string::String;

use crate::{Location, VfsError, VfsResult};

/// Preparation for exactly one mount, parent dentry, and original name.
///
/// This owns no target and records no lookup or authorization result. It must
/// be acquired outside every backend gate and consumed by one unlink request.
/// Backend retries borrow that same request; independent operations prepare
/// again. Preparation does not guarantee that the target has no active users.
///
/// ```compile_fail
/// fn duplicate(prepared: axfs_ng_vfs::PreparedUnlink) {
///     let _second = prepared.clone();
/// }
/// ```
#[derive(Debug)]
pub struct PreparedUnlink {
    parent: Location,
    name: String,
}

impl PreparedUnlink {
    pub(crate) fn new(parent: Location, name: String) -> Self {
        Self { parent, name }
    }

    pub(crate) fn check(&self, parent: &Location, name: &str) -> VfsResult<()> {
        if self.parent.ptr_eq(parent) && self.name.as_bytes() == name.as_bytes() {
            Ok(())
        } else {
            Err(VfsError::InvalidInput)
        }
    }
}

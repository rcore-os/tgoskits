//! Link resolution is separate from the text displayed by readlink.

use alloc::{string::String, vec::Vec};
use core::any::Any;

use crate::{MutationCredentials, PathHandle, VfsResult};

/// Immutable identity selected by the operation that resolves or displays a link.
///
/// The projection must remain unchanged during the operation. It borrows saved
/// identity data and must not allocate, inspect the current task or call a backend.
/// OS-specific links may recognize their own concrete caller through `Any`.
pub trait LinkCaller: Any + Send + Sync {
    fn credentials(&self) -> MutationCredentials<'_>;
}

/// An owned native-filesystem identity, without any operating-system task authority.
///
/// This can carry an explicitly selected initialization or mounter identity.
/// It cannot stand in for an OS-specific caller when following sensitive links.
pub struct NativeLinkCaller {
    fsuid: u32,
    fsgid: u32,
    supplementary_gids: Vec<u32>,
    cap_dac_override: bool,
    cap_dac_read_search: bool,
    cap_fowner: bool,
    cap_mknod: bool,
    cap_chown: bool,
    cap_fsetid: bool,
}

impl NativeLinkCaller {
    /// Captures a native projection. Group storage may allocate; call outside gates.
    pub fn new(credentials: MutationCredentials<'_>) -> Self {
        Self {
            fsuid: credentials.fsuid,
            fsgid: credentials.fsgid,
            supplementary_gids: credentials.supplementary_gids.to_vec(),
            cap_dac_override: credentials.cap_dac_override,
            cap_dac_read_search: credentials.cap_dac_read_search,
            cap_fowner: credentials.cap_fowner,
            cap_mknod: credentials.cap_mknod,
            cap_chown: credentials.cap_chown,
            cap_fsetid: credentials.cap_fsetid,
        }
    }

    /// Identity for explicitly trusted native initialization, never a user fallback.
    pub const fn root() -> Self {
        Self {
            fsuid: 0,
            fsgid: 0,
            supplementary_gids: Vec::new(),
            cap_dac_override: true,
            cap_dac_read_search: true,
            cap_fowner: true,
            cap_mknod: true,
            cap_chown: true,
            cap_fsetid: true,
        }
    }
}

impl LinkCaller for NativeLinkCaller {
    fn credentials(&self) -> MutationCredentials<'_> {
        MutationCredentials {
            fsuid: self.fsuid,
            fsgid: self.fsgid,
            supplementary_gids: &self.supplementary_gids,
            cap_dac_override: self.cap_dac_override,
            cap_dac_read_search: self.cap_dac_read_search,
            cap_fowner: self.cap_fowner,
            cap_mknod: self.cap_mknod,
            cap_chown: self.cap_chown,
            cap_fsetid: self.cap_fsetid,
        }
    }
}

/// Snapshot acquired only after the walker decides to follow a link.
#[derive(Debug)]
pub enum LinkTarget {
    Text(String),
    Object(PathHandle),
}

/// Optional link capability, including dynamic authorization and target lookup.
///
/// Implementations release task/fd-table locks before returning an owned target.
pub trait LinkOps: Send + Sync {
    fn target(&self, caller: &dyn LinkCaller) -> VfsResult<LinkTarget>;

    /// Produces one coherent display snapshot without a separate length query.
    fn read_link(&self, caller: &dyn LinkCaller) -> VfsResult<String>;
}

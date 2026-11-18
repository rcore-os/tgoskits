//! Path policy shared by ordinary and cache-only resolution.

mod cached;

use bitflags::bitflags;
pub use cached::{CachedLookup, CachedLookupContext, CachedLookupOptions};

/// Maximum number of followed links across one complete path operation.
pub const SYMLINKS_MAX: usize = 40;

/// Root selection for one operation, independent of its relative start path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LookupScope {
    #[default]
    ProcessRoot,
    Beneath,
    InRoot,
}

bitflags! {
    /// Restrictions enforced at every path transition.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct LookupRestrictions: u8 {
        const NO_XDEV = 1;
        const NO_MAGICLINKS = 2;
        const NO_SYMLINKS = 4;
    }
}

/// A mutually exclusive root policy and orthogonal traversal restrictions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LookupPolicy {
    pub scope: LookupScope,
    pub restrictions: LookupRestrictions,
}

impl LookupPolicy {
    pub const fn new(scope: LookupScope, restrictions: LookupRestrictions) -> Self {
        Self {
            scope,
            restrictions,
        }
    }
}

//! Typed ordinary lookup policy; cache-only lookup needs a separate capability.

use bitflags::bitflags;

/// Root selection for one operation, independent of its relative start path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LookupScope {
    #[default]
    ProcessRoot,
    Beneath,
    InRoot,
}

bitflags! {
    /// Restrictions enforced at each transition of an ordinary path session.
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

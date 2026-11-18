//! Approved initialization borrows the same real-inode commit machinery.

use super::MetadataIntent;
use crate::{Location, MetadataUpdate, TrustedInitialization, VfsResult};

impl TrustedInitialization {
    /// Restores approved archive or boot metadata on the actual target owner.
    ///
    /// This explicit initialization authority preserves participating fields,
    /// including setid and symlink modes. It preserves omitted timestamps and
    /// does not read the clock; it is not ordinary user chown/chmod.
    /// Real mount write preparation, backend support, and all completion phases
    /// still apply. Call outside every admission/backend gate. Runtime syscalls
    /// and layered copy-up must not substitute this kernel authority for their
    /// own caller/mounter authorization or unpublished-object proof.
    pub fn restore_metadata(&self, target: &Location, update: MetadataUpdate) -> VfsResult<()> {
        target.run_metadata(MetadataIntent::Initialization(update), self.credentials())
    }
}

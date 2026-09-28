//! Persistence requirements independent from OS-specific mount or inode flags.

bitflags::bitflags! {
    /// Persistence required when completing successful filesystem mutations.
    ///
    /// The empty policy allows deferred writeback. A file layer combines the
    /// inode policy with the policy shared by all mounts of its filesystem.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct WritebackPolicy: u8 {
        /// Complete data writes and metadata changes synchronously.
        const SYNCHRONOUS = 1;
        /// Complete directory-entry changes synchronously.
        const DIRECTORY_SYNC = 2;
    }
}

impl WritebackPolicy {
    /// Whether creating, linking, unlinking, or renaming an entry must persist.
    pub const fn syncs_directory(self) -> bool {
        self.intersects(Self::SYNCHRONOUS.union(Self::DIRECTORY_SYNC))
    }
}

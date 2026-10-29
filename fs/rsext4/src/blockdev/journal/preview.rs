//! Read-only recovery validation using bounded, on-demand journal locations.

use super::*;
use crate::jbd2::jbd2::ReplayIndex;

/// Final log locations and one reusable block, without a payload-data cache.
pub(super) struct ReplayPreview {
    /// Sorted, bounded index produced by the real parser and revoke rules.
    pub index: ReplayIndex,
    /// One fallibly allocated filesystem block reused by validation reads.
    pub buffer: Vec<u8>,
}

impl<B: BlockIo> Jbd2Dev<B> {
    /// Validates recovered geometry before any replay writes home blocks.
    /// Reads use a bounded location index and one payload buffer. Index/buffer
    /// allocation and parser failures propagate before invoking `validate`.
    /// The exclusive mount keeps the journal immutable until real replay.
    pub(crate) fn validate_replay<T>(
        &mut self,
        validate: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let system = self.system.as_ref().ok_or_else(Ext4Error::corrupted)?;
        let mut system = Self::make_system(system.jbd2_super_block, system.start_block);
        let mut index = ReplayIndex::default();
        if let ReplayStatus::Incomplete(failure) =
            system.replay_with_preview(&mut self.inner, &self.journal_blocks, Some(&mut index))
        {
            return Err(failure.cause());
        }
        index.finish();
        let size = self.inner.block_size() as usize;
        for home in index.home_blocks() {
            self.inner.read_window(size, home, 1)?;
        }
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(size)
            .map_err(|_| Ext4Error::no_memory().with_operation("jbd2:preview_buffer"))?;
        buffer.resize(size, 0);
        self.replay_preview = Some(ReplayPreview { index, buffer });
        // Neither old home bytes nor preview bytes may outlive this read view.
        self.inner.discard_held();
        let result = validate(self);
        self.replay_preview = None;
        self.inner.discard_held();
        result
    }
}

//! Per-slot buffer state inside a cached folio, modeled on the Linux
//! `buffer_head` (`include/linux/buffer_head.h`).
//!
//! One [`BufferHead`] describes a single device block within a
//! [`CacheFolio`](super::folio::CacheFolio). Only the `BH_Uptodate` and
//! `BH_Dirty` bits carry meaning here: the block-device cache is an
//! identity mapping (slot `i` of frame `f` is always device block
//! `f * slots_per_folio + i`), so every slot is implicitly mapped and a
//! `BH_Mapped` bit would never be cleared.

use bitflags::bitflags;

bitflags! {
    /// State bits of one device block inside a folio.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(crate) struct BufferHeadState: u8 {
        /// The slot holds data matching the last read from or write to the
        /// device (`BH_Uptodate`).
        const UPTODATE = 1 << 0;
        /// The slot differs from the on-disk copy and must be written back
        /// before the device is consistent (`BH_Dirty`).
        const DIRTY = 1 << 1;
    }
}

/// State of one device block within a folio (Linux `buffer_head`).
#[derive(Clone, Debug, Default)]
pub(crate) struct BufferHead {
    state: BufferHeadState,
}

impl BufferHead {
    pub(crate) fn is_uptodate(&self) -> bool {
        self.state.contains(BufferHeadState::UPTODATE)
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.state.contains(BufferHeadState::DIRTY)
    }

    /// Marks the slot as matching the device after a read.
    pub(crate) fn mark_uptodate(&mut self) {
        self.state.insert(BufferHeadState::UPTODATE);
    }

    /// Marks the slot as modified relative to the device copy
    /// (`mark_buffer_dirty`); modified data is always valid to read back,
    /// so the slot is uptodate as well.
    pub(crate) fn mark_dirty(&mut self) {
        self.state
            .insert(BufferHeadState::UPTODATE | BufferHeadState::DIRTY);
    }

    /// Clears the dirty bit; returns whether the slot was dirty.
    pub(crate) fn clear_dirty(&mut self) -> bool {
        let was_dirty = self.is_dirty();
        self.state.remove(BufferHeadState::DIRTY);
        was_dirty
    }
}

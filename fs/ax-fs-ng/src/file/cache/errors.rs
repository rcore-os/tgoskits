//! Mapping writeback errors and each open description's observation point.

use axfs_ng_vfs::{RawSpinLock, VfsError, VfsResult};

use super::CachedFile;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::file) struct WritebackErrorCursor(u64);

pub(super) struct WritebackErrors {
    state: RawSpinLock<ErrorState>,
}

struct ErrorState {
    sequence: u64,
    error: Option<VfsError>,
    seen: bool,
}

impl CachedFile {
    pub(in crate::file) fn sample_writeback_error(&self) -> WritebackErrorCursor {
        self.shared.writeback_errors.sample()
    }

    pub(in crate::file) fn check_and_advance_writeback_error(
        &self,
        cursor: &mut WritebackErrorCursor,
    ) -> VfsResult<()> {
        self.shared.writeback_errors.check_and_advance(cursor)
    }
}

impl WritebackErrors {
    pub(super) fn new() -> Self {
        Self {
            state: RawSpinLock::new(ErrorState {
                sequence: 0,
                error: None,
                seen: false,
            }),
        }
    }

    pub(super) fn record(&self, error: VfsError) {
        let mut state = self.state.lock();
        if state.seen || state.error.is_none() {
            // Coalesce unseen errors. Reserve zero for new observers so an
            // unseen failure is visible even after the sequence wraps.
            state.sequence = state.sequence.wrapping_add(1).max(1);
        }
        state.error = Some(error);
        state.seen = false;
    }

    fn sample(&self) -> WritebackErrorCursor {
        let state = self.state.lock();
        if state.seen {
            WritebackErrorCursor(state.sequence)
        } else {
            // A newly opened description must still observe an unseen error.
            WritebackErrorCursor::default()
        }
    }

    fn check_and_advance(&self, cursor: &mut WritebackErrorCursor) -> VfsResult<()> {
        let mut state = self.state.lock();
        if cursor.0 == state.sequence {
            return Ok(());
        }
        *cursor = WritebackErrorCursor(state.sequence);
        state.seen = true;
        state.error.map_or(Ok(()), Err)
    }
}

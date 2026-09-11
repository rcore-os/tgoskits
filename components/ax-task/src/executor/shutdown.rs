//! Shutdown under the owning scheduler transaction.

use super::*;

impl LocalExecutor {
    pub(super) fn shutdown(&mut self) {
        self.shared.close_and_wait_for_publishers();
        self.discard_ready_references();

        let mut cursor = self.active.replace(ptr::null_mut());
        while !cursor.is_null() {
            let header = cursor;
            cursor = unsafe {
                // The permanent owner reference keeps the detached active list
                // live until each future and reference are released below.
                (*header).owner_next()
            };
            unsafe {
                (*header).set_owner_next(ptr::null_mut());
            }
            let state = unsafe { &(*header).state };
            if state.fetch_or(COMPLETE, Ordering::AcqRel) & COMPLETE == 0 {
                let _owner_reference = unsafe {
                    // The detached active node transfers its permanent owner
                    // reference to this unwind-safe scope guard.
                    OwnedCoroutineReference::new(header)
                };
                unsafe {
                    // LocalExecutor is !Send, so shutdown runs on the owner Rust
                    // thread and is the final path that may destroy !Send futures.
                    CoroutineHeader::drop_future_raw(header);
                }
            }
        }
    }

    pub(super) fn discard_ready_references(&self) {
        let mut cursor = self.take_ready_snapshot();
        while !cursor.is_null() {
            let header = cursor;
            cursor = unsafe {
                // Closing waited for every pre-close publisher, so this detached
                // list is the final shared ready snapshot.
                IntrusiveInbox::take_next(header, InboxKind::Ready)
            };
            unsafe {
                (*header).state.fetch_and(!RUN_QUEUED, Ordering::AcqRel);
                release_reference(header);
            }
        }
    }
}

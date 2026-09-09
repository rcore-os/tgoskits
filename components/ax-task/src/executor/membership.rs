//! Membership under the owning scheduler transaction.

use super::*;

impl LocalExecutor {
    pub(super) fn link_active(&self, header: *mut CoroutineHeader) {
        unsafe {
            // Only the owner mutates the active list. The allocation's permanent
            // reference keeps every linked header alive.
            (*header).set_owner_next(self.active.get());
        }
        self.active.set(header);
    }

    pub(super) fn unlink_active(&self, header: *mut CoroutineHeader) {
        let mut previous = ptr::null_mut::<CoroutineHeader>();
        let mut cursor = self.active.get();
        while !cursor.is_null() {
            let next = unsafe {
                // The permanent owner reference keeps active-list nodes live.
                (*cursor).owner_next()
            };
            if cursor == header {
                if previous.is_null() {
                    self.active.set(next);
                } else {
                    unsafe {
                        // Owner-only list mutation cannot overlap a second unlink.
                        (*previous).set_owner_next(next);
                    }
                }
                unsafe {
                    // A detached node must not retain a stale owner-list link.
                    (*cursor).set_owner_next(ptr::null_mut());
                }
                return;
            }
            previous = cursor;
            cursor = next;
        }
    }
}

//! Allocation-free deferred MM ownership, using links embedded at MM creation.

use alloc::sync::Arc;

use super::MmInner;

#[derive(Default)]
pub(super) struct MmWorkLink {
    queued: bool,
    next: Option<Arc<MmInner>>,
}

/// FIFO of already allocated MM owners. The outer queue lock always precedes
/// an individual link lock; no operation holds two link locks simultaneously.
/// Retirement and repair share a link, so an MM cannot be on both queues.
pub(super) struct MmWorkQueue {
    head: Option<Arc<MmInner>>,
    tail: Option<Arc<MmInner>>,
    len: usize,
}

impl MmWorkQueue {
    pub(super) const fn new() -> Self {
        Self { head: None, tail: None, len: 0 }
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// A duplicate owner is returned for release outside the queue lock.
    pub(super) fn push(&mut self, inner: Arc<MmInner>) -> Result<(), Arc<MmInner>> {
        {
            let mut link = inner.work_link.lock();
            if link.queued {
                drop(link);
                return Err(inner);
            }
            debug_assert!(link.next.is_none());
            link.queued = true;
        }
        if let Some(tail) = &self.tail {
            tail.work_link.lock().next = Some(inner.clone());
        } else {
            self.head = Some(inner.clone());
        }
        // The old tail remains owned by the head/next chain, so replacing
        // this extra reference cannot run an MM destructor under the lock.
        self.tail = Some(inner);
        self.len += 1;
        Ok(())
    }

    pub(super) fn pop(&mut self) -> Option<Arc<MmInner>> {
        let inner = self.head.take()?;
        {
            let mut link = inner.work_link.lock();
            self.head = link.next.take();
            link.queued = false;
        }
        if self.head.is_none() {
            // `inner` retains the removed tail until the caller leaves the
            // queue lock. No last reference is released here.
            self.tail = None;
        }
        self.len -= 1;
        Some(inner)
    }
}

impl Drop for MmWorkQueue {
    fn drop(&mut self) {
        // Avoid recursive Arc-chain destruction for local/test queues.
        while let Some(inner) = self.pop() {
            drop(inner);
        }
    }
}

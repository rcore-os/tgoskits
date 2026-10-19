//! Allocation-free deferred MM ownership, using links embedded at MM creation.

use alloc::sync::Arc;

use super::{MmInner, RawSpinLock};

#[derive(Clone, Copy)]
enum WorkKind {
    Destruction,
    LazyCpuReclaim,
}

impl WorkKind {
    fn link(self, inner: &MmInner) -> &RawSpinLock<MmWorkLink> {
        match self {
            Self::Destruction => &inner.work_link,
            Self::LazyCpuReclaim => &inner.lazy_reclaim_link,
        }
    }
}

#[derive(Default)]
pub(super) struct MmWorkLink {
    pub(super) queued: bool,
    next: Option<Arc<MmInner>>,
}

/// FIFO of already allocated MM owners. The outer queue lock always precedes
/// an individual link lock; no operation holds two link locks simultaneously.
/// Retirement and repair share a link, so an MM cannot be on both queues.
/// Lazy-CPU retry owns a separate link: final activation release can enqueue
/// destruction while a retry reference is still queued.
pub(super) struct MmWorkQueue {
    head: Option<Arc<MmInner>>,
    tail: Option<Arc<MmInner>>,
    len: usize,
    kind: WorkKind,
}

impl MmWorkQueue {
    pub(super) const fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
            kind: WorkKind::Destruction,
        }
    }

    pub(super) const fn lazy_cpu_reclaim() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
            kind: WorkKind::LazyCpuReclaim,
        }
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// A duplicate owner is returned for release outside the queue lock.
    pub(super) fn push(&mut self, inner: Arc<MmInner>) -> Result<(), Arc<MmInner>> {
        {
            let mut link = self.kind.link(&inner).lock_irqsave();
            if link.queued {
                drop(link);
                return Err(inner);
            }
            debug_assert!(link.next.is_none());
            link.queued = true;
        }
        if let Some(tail) = &self.tail {
            self.kind.link(tail).lock_irqsave().next = Some(inner.clone());
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
            let mut link = self.kind.link(&inner).lock_irqsave();
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

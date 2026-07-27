//! Intrusive nodes for task-context-only deferred resource reclamation.

use core::pin::Pin;

use crate::inbox::{InboxKind, InboxNode};

pub(crate) type DeferredReclaim = unsafe fn(*mut DeferredReclaimNode, *mut ());

/// A pinned allocation's single membership in the task-system reaper inbox.
///
/// Producers only publish the embedded scheduler node. The fixed reclaim
/// function is copied and invoked by a bounded task-context drain after the
/// node has been detached from the inbox. The scheduler inbox payload is an
/// exposed containing-allocation address numerically equal to this node's
/// address. A callback that recovers that allocation must therefore place this
/// node at offset zero and keep the allocation pinned until the callback runs.
#[derive(Debug)]
#[repr(C)]
pub(crate) struct DeferredReclaimNode {
    inbox: InboxNode,
    reclaim: DeferredReclaim,
}

impl DeferredReclaimNode {
    pub(crate) const fn new(reclaim: DeferredReclaim) -> Self {
        Self {
            inbox: InboxNode::new(InboxKind::Reclaim),
            reclaim,
        }
    }

    pub(crate) fn inbox(self: Pin<&'static Self>) -> Pin<&'static InboxNode> {
        unsafe {
            // The node is pinned as part of its containing allocation, and
            // projection never moves the embedded intrusive inbox node.
            self.map_unchecked(|node| &node.inbox)
        }
    }

    pub(crate) fn address(self: Pin<&'static Self>) -> usize {
        // The containing allocation pointer was exposed by the publisher and
        // is the provenance that the reaper must later recover. This numeric
        // comparison must not replace it with provenance exposed from the
        // embedded first-field subobject.
        (self.get_ref() as *const Self).addr()
    }

    /// Runs the allocation-specific reclaimer after inbox detachment.
    ///
    /// # Safety
    ///
    /// `node` must be exclusively owned by the task-system drain, `data` must be
    /// derived from a live allocation and numerically equal to `node`'s address,
    /// and the fixed callback must not have been invoked for this publication
    /// before. If the callback casts `data` to a containing allocation, that
    /// allocation must use `node` as its first field so both addresses are equal.
    pub(crate) unsafe fn reclaim(node: *mut Self, data: *mut ()) {
        let reclaim = unsafe {
            // The detached queue membership keeps the containing allocation
            // alive until this function pointer has been copied.
            (*node).reclaim
        };
        unsafe {
            // The callback contract belongs to the allocation that embedded the
            // node and may deallocate it before returning.
            reclaim(node, data);
        }
    }
}

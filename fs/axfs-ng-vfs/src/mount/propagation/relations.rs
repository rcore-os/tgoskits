use core::ops::Range;

use super::*;

/// Every successful relation upgrade belongs to the enclosing operation's
/// retirement. Callers must retain this value beyond their protected region.
#[derive(Debug, Default)]
pub(in crate::mount) struct RelationRetirement {
    owners: Vec<Arc<Mountpoint>>,
}

impl RelationRetirement {
    pub(in crate::mount) fn collect_locked(
        &mut self,
        relations: &RawSpinLock<Vec<Weak<Mountpoint>>>,
    ) -> Range<usize> {
        let start = self.owners.len();
        let count = relations.lock().len();
        self.owners.reserve(count);
        for relation in relations.lock().iter() {
            if let Some(owner) = relation.upgrade() {
                self.owners.push(owner);
            }
        }
        start..self.owners.len()
    }

    pub(in crate::mount) fn slice(&self, range: Range<usize>) -> &[Arc<Mountpoint>] {
        &self.owners[range]
    }

    pub(in crate::mount) fn owner(&self, index: usize) -> &Arc<Mountpoint> {
        &self.owners[index]
    }

    pub(super) fn unlink_locked(
        &mut self,
        mount: &Arc<Mountpoint>,
        relations: &RawSpinLock<Vec<Weak<Mountpoint>>>,
        reverse: impl Fn(&Mountpoint) -> &RawSpinLock<Vec<Weak<Mountpoint>>>,
    ) {
        let range = self.collect_locked(relations);
        for counterpart in self.slice(range) {
            reverse(counterpart)
                .lock()
                .retain(|candidate| candidate.as_ptr() != Arc::as_ptr(mount));
        }
        relations.lock().clear();
    }
}

pub(super) fn has_relation(
    relations: &RawSpinLock<Vec<Weak<Mountpoint>>>,
    mount: &Arc<Mountpoint>,
) -> bool {
    // `mount` already owns this allocation. Comparing Weak identity needs no
    // temporary strong owner, including for unrelated candidates.
    relations
        .lock()
        .iter()
        .any(|candidate| candidate.as_ptr() == Arc::as_ptr(mount))
}

pub(super) fn add_relation(
    relations: &RawSpinLock<Vec<Weak<Mountpoint>>>,
    mount: &Arc<Mountpoint>,
) {
    // The topology gate serializes writers. Prepare vector capacity outside
    // the node lock; stale Weak entries can be pruned without upgrading them.
    let mut pending = core::mem::take(&mut *relations.lock());
    pending.retain(|candidate| candidate.strong_count() != 0);
    if !pending
        .iter()
        .any(|candidate| candidate.as_ptr() == Arc::as_ptr(mount))
    {
        pending.push(Arc::downgrade(mount));
    }
    *relations.lock() = pending;
}

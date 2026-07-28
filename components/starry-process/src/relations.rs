use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};

use crate::{Pid, Process, ProcessGroup};

#[cfg(feature = "multitask")]
pub(crate) type RelationLock<T> = ax_sync::PiMutex<T>;
#[cfg(not(feature = "multitask"))]
pub(crate) type RelationLock<T> = ax_kspin::SpinNoIrq<T>;

// Relationship writers use one order:
// process group binding -> parent child sets (ascending PID) -> child parent
// binding -> process-group member sets (ascending PGID). Session membership is
// published only when a group is created and is never nested with the process
// relationship locks. Capacity is reserved before entering this order, and
// removed storage is released only after every guard has gone away.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChildPublication {
    Open,
    ClosedForExit,
}

pub(crate) struct ChildRelations {
    publication: ChildPublication,
    entries: RelationMap<Arc<Process>>,
}

impl ChildRelations {
    pub(crate) const fn new() -> Self {
        Self {
            publication: ChildPublication::Open,
            entries: RelationMap::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.publication == ChildPublication::Open
    }

    fn close(&mut self) {
        self.publication = ChildPublication::ClosedForExit;
    }

    pub(crate) fn contains(&self, pid: Pid) -> bool {
        self.entries.contains(pid)
    }

    fn contains_process(&self, pid: Pid, process: &Arc<Process>) -> bool {
        self.entries
            .get(pid)
            .is_some_and(|registered| Arc::ptr_eq(registered, process))
    }

    fn conflicts_with(&self, destination: &Self) -> bool {
        self.entries
            .keys()
            .any(|pid| destination.entries.contains(pid))
    }

    pub(crate) fn snapshot(&self, output: &mut Vec<Arc<Process>>) {
        assert!(
            output.capacity() - output.len() >= self.entries.len(),
            "child snapshot requires reserved capacity"
        );
        for child in self.entries.values() {
            output.push(child.clone());
        }
    }

    fn has_capacity_for(&self, additional: usize) -> bool {
        self.entries.has_capacity_for(additional)
    }

    fn insert_unique_reserved(&mut self, pid: Pid, process: Arc<Process>) {
        self.entries.insert_unique_reserved(pid, process);
    }

    fn remove(&mut self, pid: Pid) -> Option<Arc<Process>> {
        self.entries.remove(pid)
    }

    fn pop_last(&mut self) -> Option<(Pid, Arc<Process>)> {
        self.entries.pop_last()
    }

    fn adopt_capacity(&mut self, spare: Vec<(Pid, Arc<Process>)>) -> Vec<(Pid, Arc<Process>)> {
        self.entries.adopt_capacity(spare)
    }
}

pub(crate) struct GroupMembers {
    entries: RelationMap<Weak<Process>>,
}

impl GroupMembers {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: RelationMap::with_capacity(capacity),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn has_capacity_for(&self, additional: usize) -> bool {
        self.entries.has_capacity_for(additional)
    }

    #[cfg(test)]
    pub(crate) fn get(&self, pid: Pid) -> Option<Arc<Process>> {
        self.entries.get(pid).and_then(Weak::upgrade)
    }

    fn contains_live(&self, pid: Pid) -> bool {
        self.entries
            .get(pid)
            .is_some_and(|registered| registered.strong_count() != 0)
    }

    fn contains_process(&self, pid: Pid, process: &Arc<Process>) -> bool {
        self.entries
            .get(pid)
            .is_some_and(|registered| registered.as_ptr() == Arc::as_ptr(process))
    }

    fn insert_reserved(&mut self, pid: Pid, process: &Arc<Process>) -> Option<Weak<Process>> {
        self.entries.insert_reserved(pid, Arc::downgrade(process))
    }

    pub(crate) fn remove(&mut self, pid: Pid) -> Option<Weak<Process>> {
        self.entries.remove(pid)
    }

    pub(crate) fn snapshot(&self, output: &mut Vec<Arc<Process>>) {
        assert!(
            output.capacity() - output.len() >= self.entries.len(),
            "process-group snapshot requires reserved capacity"
        );
        for process in self.entries.values() {
            if let Some(process) = process.upgrade() {
                output.push(process);
            }
        }
    }

    fn adopt_capacity(&mut self, spare: Vec<(Pid, Weak<Process>)>) -> Vec<(Pid, Weak<Process>)> {
        self.entries.adopt_capacity(spare)
    }
}

pub(crate) struct SessionGroups {
    entries: RelationMap<Weak<ProcessGroup>>,
}

impl SessionGroups {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: RelationMap::with_capacity(capacity),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn has_capacity_for(&self, additional: usize) -> bool {
        self.entries.has_capacity_for(additional)
    }

    pub(crate) fn insert_reserved(
        &mut self,
        pid: Pid,
        group: &Arc<ProcessGroup>,
    ) -> Option<Weak<ProcessGroup>> {
        self.entries.insert_reserved(pid, Arc::downgrade(group))
    }

    fn contains_live(&self, pid: Pid) -> bool {
        self.entries
            .get(pid)
            .is_some_and(|registered| registered.strong_count() != 0)
    }

    pub(crate) fn snapshot(&self, output: &mut Vec<Arc<ProcessGroup>>) {
        assert!(
            output.capacity() - output.len() >= self.entries.len(),
            "session snapshot requires reserved capacity"
        );
        for group in self.entries.values() {
            if let Some(group) = group.upgrade() {
                output.push(group);
            }
        }
    }

    fn adopt_capacity(
        &mut self,
        spare: Vec<(Pid, Weak<ProcessGroup>)>,
    ) -> Vec<(Pid, Weak<ProcessGroup>)> {
        self.entries.adopt_capacity(spare)
    }
}

pub(crate) fn ensure_child_capacity(lock: &RelationLock<ChildRelations>, additional: usize) {
    ensure_capacity(
        lock,
        additional,
        ChildRelations::len,
        ChildRelations::has_capacity_for,
        ChildRelations::adopt_capacity,
    );
}

pub(crate) fn ensure_member_capacity(lock: &RelationLock<GroupMembers>, additional: usize) {
    ensure_capacity(
        lock,
        additional,
        GroupMembers::len,
        GroupMembers::has_capacity_for,
        GroupMembers::adopt_capacity,
    );
}

pub(crate) fn ensure_session_capacity(lock: &RelationLock<SessionGroups>, additional: usize) {
    ensure_capacity(
        lock,
        additional,
        SessionGroups::len,
        SessionGroups::has_capacity_for,
        SessionGroups::adopt_capacity,
    );
}

fn ensure_capacity<T, V>(
    lock: &RelationLock<T>,
    additional: usize,
    len: impl Fn(&T) -> usize,
    has_capacity_for: impl Fn(&T, usize) -> bool,
    adopt_capacity: impl Fn(&mut T, Vec<(Pid, V)>) -> Vec<(Pid, V)>,
) {
    if additional == 0 {
        return;
    }

    loop {
        let required = {
            let state = lock.lock();
            if has_capacity_for(&state, additional) {
                return;
            }
            len(&state)
                .checked_add(additional)
                .expect("process relation capacity overflow")
        };
        let spare = Vec::with_capacity(required);
        let old_storage = {
            let mut state = lock.lock();
            if has_capacity_for(&state, additional) {
                drop(state);
                drop(spare);
                return;
            }
            if spare.capacity() < len(&state).saturating_add(additional) {
                drop(state);
                drop(spare);
                continue;
            }
            adopt_capacity(&mut state, spare)
        };
        // The old allocation is released only after the relation critical
        // section has ended.
        drop(old_storage);
        return;
    }
}

#[derive(Clone, Copy)]
pub(crate) enum GroupMoveScope {
    AnySession,
    SameSession,
}

pub(crate) struct ProcessRelationTxn;

impl ProcessRelationTxn {
    pub(crate) fn attach_session_group(group: &Arc<ProcessGroup>) {
        loop {
            ensure_session_capacity(&group.session.process_groups, 1);
            let mut groups = group.session.process_groups.lock();
            if !groups.has_capacity_for(1) {
                drop(groups);
                continue;
            }
            assert!(
                !groups.contains_live(group.pgid()),
                "session already contains a live process group with this PGID"
            );
            let replaced = groups.insert_reserved(group.pgid(), group);
            drop(groups);
            drop(replaced);
            return;
        }
    }

    pub(crate) fn publish(process: &Arc<Process>) -> bool {
        loop {
            let Some(parent) = process.parent() else {
                return false;
            };
            let group = process.group();
            ensure_child_capacity(&parent.children, 1);
            ensure_member_capacity(&group.processes, 1);

            let group_binding = process.group.lock();
            if !Arc::ptr_eq(&group_binding, &group) {
                drop(group_binding);
                continue;
            }
            let mut children = parent.children.lock();
            let parent_binding = process.parent.lock();
            if parent_binding.as_ptr() != Arc::as_ptr(&parent) {
                drop(parent_binding);
                drop(children);
                drop(group_binding);
                continue;
            }
            if !children.is_open() {
                return false;
            }
            let mut members = group.processes.lock();
            if !children.has_capacity_for(1) || !members.has_capacity_for(1) {
                drop(members);
                drop(parent_binding);
                drop(children);
                drop(group_binding);
                continue;
            }
            if children.contains(process.pid()) || members.contains_live(process.pid()) {
                return false;
            }

            children.insert_unique_reserved(process.pid(), process.clone());
            let replaced_member = members.insert_reserved(process.pid(), process);
            drop(members);
            drop(parent_binding);
            drop(children);
            drop(group_binding);
            // An expired weak entry for a reused PID may be replaced. Its
            // allocation is released only after the relation locks are gone.
            drop(replaced_member);
            return true;
        }
    }

    pub(crate) fn attach_group(process: &Arc<Process>) {
        loop {
            let group = process.group();
            ensure_member_capacity(&group.processes, 1);

            let group_binding = process.group.lock();
            if !Arc::ptr_eq(&group_binding, &group) {
                drop(group_binding);
                continue;
            }
            let mut members = group.processes.lock();
            if !members.has_capacity_for(1) {
                drop(members);
                drop(group_binding);
                continue;
            }
            assert!(
                !members.contains_live(process.pid())
                    || members.contains_process(process.pid(), process),
                "process group already contains a live process with this PID"
            );
            let replaced = members.insert_reserved(process.pid(), process);
            drop(members);
            drop(group_binding);
            drop(replaced);
            return;
        }
    }

    pub(crate) fn move_group(
        process: &Arc<Process>,
        target: &Arc<ProcessGroup>,
        scope: GroupMoveScope,
    ) -> bool {
        loop {
            ensure_member_capacity(&target.processes, 1);
            let mut group_binding = process.group.lock();
            let source = group_binding.clone();
            if Arc::ptr_eq(&source, target) {
                return true;
            }
            if matches!(scope, GroupMoveScope::SameSession)
                && !Arc::ptr_eq(&source.session, &target.session)
            {
                return false;
            }
            assert_ne!(
                source.pgid(),
                target.pgid(),
                "distinct process groups must not share a PGID"
            );

            let commit = if source.pgid() < target.pgid() {
                let mut source_members = source.processes.lock();
                let mut target_members = target.processes.lock();
                Self::move_group_locked(
                    process,
                    target,
                    &mut group_binding,
                    &mut source_members,
                    &mut target_members,
                )
            } else {
                let mut target_members = target.processes.lock();
                let mut source_members = source.processes.lock();
                Self::move_group_locked(
                    process,
                    target,
                    &mut group_binding,
                    &mut source_members,
                    &mut target_members,
                )
            };

            match commit {
                GroupMoveCommit::Retry => {
                    drop(group_binding);
                }
                GroupMoveCommit::Done { removed, replaced } => {
                    drop(group_binding);
                    drop(removed);
                    drop(replaced);
                    return true;
                }
            }
        }
    }

    fn move_group_locked(
        process: &Arc<Process>,
        target: &Arc<ProcessGroup>,
        group_binding: &mut Arc<ProcessGroup>,
        source_members: &mut GroupMembers,
        target_members: &mut GroupMembers,
    ) -> GroupMoveCommit {
        let published = source_members.contains_process(process.pid(), process);
        assert!(
            !target_members.contains_live(process.pid())
                || target_members.contains_process(process.pid(), process),
            "destination group already contains a different live process with this PID"
        );
        if published && !target_members.has_capacity_for(1) {
            return GroupMoveCommit::Retry;
        }

        let removed = published
            .then(|| source_members.remove(process.pid()))
            .flatten();
        let replaced = published
            .then(|| target_members.insert_reserved(process.pid(), process))
            .flatten();
        *group_binding = target.clone();
        GroupMoveCommit::Done { removed, replaced }
    }

    pub(crate) fn begin_exit(
        process: &Arc<Process>,
        reaper: &Arc<Process>,
    ) -> Option<Vec<Arc<Process>>> {
        if process.is_init() || Arc::ptr_eq(process, reaper) {
            return Some(Vec::new());
        }
        assert_ne!(
            process.pid(),
            reaper.pid(),
            "distinct process identities must not share a PID"
        );

        loop {
            let child_count = process.children.lock().len();
            ensure_child_capacity(&reaper.children, child_count);
            let mut reparented = Vec::with_capacity(child_count);

            let result = if process.pid() < reaper.pid() {
                let mut children = process.children.lock();
                let mut reaper_children = reaper.children.lock();
                Self::reparent_locked(&mut children, &mut reaper_children, reaper, &mut reparented)
            } else {
                let mut reaper_children = reaper.children.lock();
                let mut children = process.children.lock();
                Self::reparent_locked(&mut children, &mut reaper_children, reaper, &mut reparented)
            };

            match result {
                ReparentCommit::Retry => {}
                ReparentCommit::DestinationClosed => return None,
                ReparentCommit::Conflict => {
                    panic!("reparenting would replace an existing child PID")
                }
                ReparentCommit::Done => {
                    reparented.reverse();
                    return Some(reparented);
                }
            }
        }
    }

    fn reparent_locked(
        children: &mut ChildRelations,
        reaper_children: &mut ChildRelations,
        reaper: &Arc<Process>,
        reparented: &mut Vec<Arc<Process>>,
    ) -> ReparentCommit {
        if !reaper_children.is_open() {
            return ReparentCommit::DestinationClosed;
        }
        let child_count = children.len();
        if !reaper_children.has_capacity_for(child_count)
            || reparented.capacity() - reparented.len() < child_count
        {
            return ReparentCommit::Retry;
        }
        if children.conflicts_with(reaper_children) {
            return ReparentCommit::Conflict;
        }

        children.close();
        let reaper_parent = Arc::downgrade(reaper);
        while let Some((pid, child)) = children.pop_last() {
            *child.parent.lock() = reaper_parent.clone();
            reparented.push(child.clone());
            reaper_children.insert_unique_reserved(pid, child);
        }
        ReparentCommit::Done
    }

    pub(crate) fn detach(process: &Arc<Process>) {
        loop {
            let parent = process.parent();
            let group = process.group();
            let group_binding = process.group.lock();
            if !Arc::ptr_eq(&group_binding, &group) {
                drop(group_binding);
                continue;
            }

            let (removed_child, removed_member, old_parent) = if let Some(parent) = parent {
                let mut children = parent.children.lock();
                let mut parent_binding = process.parent.lock();
                if parent_binding.as_ptr() != Arc::as_ptr(&parent) {
                    drop(parent_binding);
                    drop(children);
                    drop(group_binding);
                    continue;
                }
                let mut members = group.processes.lock();
                let removed_child = children
                    .contains_process(process.pid(), process)
                    .then(|| children.remove(process.pid()))
                    .flatten();
                let removed_member = members
                    .contains_process(process.pid(), process)
                    .then(|| members.remove(process.pid()))
                    .flatten();
                let old_parent = core::mem::take(&mut *parent_binding);
                (removed_child, removed_member, old_parent)
            } else {
                let mut parent_binding = process.parent.lock();
                if parent_binding.strong_count() != 0 {
                    drop(parent_binding);
                    drop(group_binding);
                    continue;
                }
                let mut members = group.processes.lock();
                let removed_member = members
                    .contains_process(process.pid(), process)
                    .then(|| members.remove(process.pid()))
                    .flatten();
                let old_parent = core::mem::take(&mut *parent_binding);
                (None, removed_member, old_parent)
            };
            drop(group_binding);
            drop(removed_child);
            drop(removed_member);
            drop(old_parent);
            return;
        }
    }
}

enum GroupMoveCommit {
    Retry,
    Done {
        removed: Option<Weak<Process>>,
        replaced: Option<Weak<Process>>,
    },
}

enum ReparentCommit {
    Retry,
    DestinationClosed,
    Conflict,
    Done,
}

struct RelationMap<V> {
    entries: Vec<(Pid, V)>,
}

impl<V> RelationMap<V> {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn has_capacity_for(&self, additional: usize) -> bool {
        self.entries.capacity() - self.entries.len() >= additional
    }

    fn contains(&self, pid: Pid) -> bool {
        self.find(pid).is_ok()
    }

    fn get(&self, pid: Pid) -> Option<&V> {
        self.find(pid).ok().map(|index| &self.entries[index].1)
    }

    fn keys(&self) -> impl Iterator<Item = Pid> + '_ {
        self.entries.iter().map(|(pid, _)| *pid)
    }

    fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    fn insert_reserved(&mut self, pid: Pid, value: V) -> Option<V> {
        match self.find(pid) {
            Ok(index) => Some(core::mem::replace(&mut self.entries[index].1, value)),
            Err(index) => {
                assert!(
                    self.has_capacity_for(1),
                    "process relation insertion requires reserved capacity"
                );
                self.entries.insert(index, (pid, value));
                None
            }
        }
    }

    fn insert_unique_reserved(&mut self, pid: Pid, value: V) {
        let index = self
            .find(pid)
            .expect_err("process relation identity must be unique");
        assert!(
            self.has_capacity_for(1),
            "process relation insertion requires reserved capacity"
        );
        self.entries.insert(index, (pid, value));
    }

    fn remove(&mut self, pid: Pid) -> Option<V> {
        self.find(pid)
            .ok()
            .map(|index| self.entries.remove(index).1)
    }

    fn pop_last(&mut self) -> Option<(Pid, V)> {
        self.entries.pop()
    }

    fn adopt_capacity(&mut self, mut spare: Vec<(Pid, V)>) -> Vec<(Pid, V)> {
        assert!(spare.capacity() >= self.entries.len());
        core::mem::swap(&mut self.entries, &mut spare);
        self.entries.append(&mut spare);
        spare
    }

    fn find(&self, pid: Pid) -> Result<usize, usize> {
        self.entries
            .binary_search_by_key(&pid, |(entry_pid, _)| *entry_pid)
    }
}

#[cfg(test)]
mod tests {
    use super::RelationMap;

    #[test]
    fn reserved_insert_preserves_storage_capacity() {
        let mut map = RelationMap::with_capacity(2);
        let capacity = map.entries.capacity();
        assert_eq!(map.insert_reserved(7, 11), None);
        assert_eq!(map.insert_reserved(3, 13), None);
        assert_eq!(map.entries.capacity(), capacity);
        assert_eq!(
            map.values().copied().collect::<alloc::vec::Vec<_>>(),
            [13, 11]
        );
    }

    #[test]
    fn capacity_adoption_returns_old_storage_for_deferred_drop() {
        let mut map = RelationMap::with_capacity(1);
        map.insert_reserved(1, 2);
        let replacement = alloc::vec::Vec::with_capacity(4);
        let old = map.adopt_capacity(replacement);

        assert_eq!(map.entries.capacity(), 4);
        assert_eq!(map.get(1), Some(&2));
        assert!(old.is_empty());
    }
}

use super::*;

/// One owner-safe-point scan with an entry-bounded candidate budget.
pub(crate) struct BalanceScan {
    epoch: u64,
    class: Option<SchedulingClass>,
    remaining: usize,
}

impl RunQueue {
    /// Updates the configured-policy Deadline server retained inside a task.
    ///
    /// When a Deadline PI entity owns the active EDF key, its base CBS is
    /// parked inside the same rq-owned scheduling state. Updating that base
    /// server must not rebuild the donor's active key.
    pub(crate) fn update_base_deadline_entity(
        &mut self,
        id: ThreadId,
        entity: SchedulingEntity,
    ) -> bool {
        let Some(class) = self.membership_class(id) else {
            return false;
        };
        match class {
            QueueMembershipClass::Deadline(key) => {
                let Some(thread) = self.deadline.get_mut(key) else {
                    return false;
                };
                if thread.active.uses_inherited_entity() {
                    thread.active.replace_base_entity(entity);
                    return true;
                }
                let Some(new_key) = self.deadline.update_entity(key, entity) else {
                    return false;
                };
                self.replace_membership_class(id, QueueMembershipClass::Deadline(new_key));
                self.deadline.refresh_pushable(id, self.linked_current());
                true
            }
            QueueMembershipClass::DeadlineThrottled => {
                let Some(thread) = self.deadline.throttled_mut(id) else {
                    return false;
                };
                thread.active.replace_base_entity(entity);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn update_migration_capability(
        &mut self,
        id: ThreadId,
        migration_capable: bool,
    ) -> bool {
        let Some(class) = self.membership_class(id) else {
            return false;
        };
        match class {
            QueueMembershipClass::Stop => {
                self.stop
                    .as_mut()
                    .expect("stop membership must retain the stopper task")
                    .migration_capable = false;
            }
            QueueMembershipClass::Deadline(key) => {
                self.deadline
                    .get_mut(key)
                    .expect("Deadline membership must retain its queue node")
                    .migration_capable = migration_capable;
                self.deadline.refresh_pushable(id, self.linked_current());
            }
            QueueMembershipClass::DeadlineThrottled => {
                self.deadline
                    .throttled_mut(id)
                    .expect("throttled Deadline membership must retain its entity")
                    .migration_capable = migration_capable;
            }
            QueueMembershipClass::Realtime(priority) => {
                self.rt
                    .get_mut(priority, id)
                    .expect("RT membership must retain its queue node")
                    .migration_capable = migration_capable;
                self.rt
                    .refresh_pushable(id, priority, self.linked_current());
            }
            QueueMembershipClass::Fair => {
                let mut thread = self
                    .fair
                    .remove(id)
                    .expect("fair membership must retain its queue node");
                thread.migration_capable = migration_capable;
                self.fair.insert(thread);
            }
            QueueMembershipClass::IdleFair => {
                let mut thread = self
                    .idle_fair
                    .remove(id)
                    .expect("idle-fair membership must retain its queue node");
                thread.migration_capable = migration_capable;
                self.idle_fair.insert(thread);
            }
        }
        true
    }

    pub(crate) fn begin_balance_scan(&mut self, class: Option<SchedulingClass>) -> BalanceScan {
        self.balance_scan_epoch = self
            .balance_scan_epoch
            .checked_add(1)
            .expect("runqueue balance scan epoch must not wrap");
        BalanceScan {
            epoch: self.balance_scan_epoch,
            class,
            remaining: self.balance_candidate_count(class),
        }
    }

    pub(crate) fn next_balance_candidate(
        &mut self,
        scan: &mut BalanceScan,
        mut may_migrate: impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        if scan.remaining == 0 {
            return None;
        }
        let mut eligible = |thread: &QueuedThread| {
            thread.balance_scan_epoch != scan.epoch
                && thread.migration_capable
                && may_migrate(thread)
        };
        let candidate = match scan.class {
            Some(SchedulingClass::Deadline) => {
                self.deadline.find_first_pushable_matching(&mut eligible)
            }
            Some(SchedulingClass::Realtime) => self.rt.find_first_pushable_matching(&mut eligible),
            Some(SchedulingClass::Fair) => self.fair.find_first_matching(&mut eligible),
            Some(SchedulingClass::Stop | SchedulingClass::Idle) => None,
            None => self
                .deadline
                .find_first_pushable_matching(&mut eligible)
                .or_else(|| self.rt.find_first_pushable_matching(&mut eligible))
                .or_else(|| self.fair.find_first_matching(&mut eligible)),
        }?;
        scan.remaining -= 1;
        self.mark_balance_candidate(candidate.id, scan.epoch);
        Some(candidate)
    }

    fn balance_candidate_count(&self, class: Option<SchedulingClass>) -> usize {
        match class {
            Some(SchedulingClass::Deadline) => self.deadline.pushable_count(),
            Some(SchedulingClass::Realtime) => self.rt.pushable_count(),
            Some(SchedulingClass::Fair) => self.fair.migratable_count(),
            Some(SchedulingClass::Stop | SchedulingClass::Idle) => 0,
            None => self
                .deadline
                .pushable_count()
                .saturating_add(self.rt.pushable_count())
                .saturating_add(self.fair.migratable_count()),
        }
    }

    pub(crate) fn queued_thread(&self, id: ThreadId) -> Option<QueuedThreadSnapshot> {
        if self.linked_current() == Some(id) {
            return None;
        }
        match self.membership_class(id)? {
            QueueMembershipClass::Stop => self.stop.as_ref().map(QueuedThreadSnapshot::from),
            QueueMembershipClass::Deadline(key) => {
                self.deadline.get(key).map(QueuedThreadSnapshot::from)
            }
            QueueMembershipClass::DeadlineThrottled => None,
            QueueMembershipClass::Realtime(priority) => {
                self.rt.get(priority, id).map(QueuedThreadSnapshot::from)
            }
            QueueMembershipClass::Fair => {
                self.fair.find_first_matching(&mut |thread| thread.id == id)
            }
            QueueMembershipClass::IdleFair => self
                .idle_fair
                .find_first_matching(&mut |thread| thread.id == id),
        }
    }
}

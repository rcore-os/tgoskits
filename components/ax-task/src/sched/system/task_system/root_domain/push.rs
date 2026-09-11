//! Push under the owning scheduler transaction.

use super::*;

impl RootDomain {
    pub(super) fn push_iterator(&self, class: RootDomainPushClass) -> &RootDomainPushIterator {
        match class {
            RootDomainPushClass::Realtime => &self.realtime_push,
            RootDomainPushClass::Deadline => &self.deadline_push,
        }
    }

    pub(in crate::sched::system::task_system) fn request_rt_deadline_push(
        &self,
        class: RootDomainPushClass,
        requester: CpuId,
    ) {
        // Linux gates `pull_rt_task()`/`pull_dl_task()` on the root-domain
        // overload count before starting or extending the serialized push
        // iterator. A priority drop with no pushable source is not work and
        // must not contend on the iterator or create a phantom generation.
        if !self.overload.any_class(class) {
            return;
        }
        let push = self.push_iterator(class);
        let target = {
            let mut state = push.lock_state();
            state.requested_generation = state
                .requested_generation
                .checked_add(1)
                .expect("root-domain push generation exhausted");
            if state.phase != RootDomainPushPhase::Idle {
                None
            } else {
                state.scan_generation = state.requested_generation;
                state.cursor = None;
                self.publish_next_push_target(class, &mut state, requester)
            }
        };
        self.deliver_push_target(class, target);
    }

    /// Starts the serialized class push at a source proven pushable under its
    /// owner rq lock.
    ///
    /// This is Linux `rt_queue_push_tasks()` / `deadline_queue_push_tasks()`:
    /// the local rq transaction supplies the class-specific proof, so this
    /// entry must not wait for the subsequently committed overload index.
    pub(in crate::sched::system::task_system) fn start_rt_deadline_push_from(
        &self,
        class: RootDomainPushClass,
        source: CpuId,
    ) {
        let push = self.push_iterator(class);
        let target = {
            let mut state = push.lock_state();
            state.requested_generation = state
                .requested_generation
                .checked_add(1)
                .expect("root-domain push generation exhausted");
            if state.phase != RootDomainPushPhase::Idle {
                None
            } else {
                state.scan_generation = state.requested_generation;
                state.cursor = None;
                push.publish_target(&mut state, Some(source));
                Some(source)
            }
        };
        self.deliver_push_target(class, target);
    }

    pub(in crate::sched::system::task_system) fn push_target_pending(&self, source: CpuId) -> bool {
        [RootDomainPushClass::Deadline, RootDomainPushClass::Realtime]
            .into_iter()
            .any(|class| self.push_iterator(class).has_published_target(source))
    }

    pub(in crate::sched::system::task_system) fn claim_rt_deadline_push(
        &self,
        source: CpuId,
    ) -> Option<RootDomainPushClaim> {
        for class in [RootDomainPushClass::Deadline, RootDomainPushClass::Realtime] {
            let push = self.push_iterator(class);
            let mut state = push.lock_state();
            if state.phase != RootDomainPushPhase::Published(source) {
                continue;
            }
            state.phase = RootDomainPushPhase::Claimed(source);
            push.clear_published_target();
            return Some(RootDomainPushClaim {
                source,
                generation: state.scan_generation,
                class,
            });
        }
        None
    }

    pub(in crate::sched::system::task_system) fn finish_rt_deadline_push(
        &self,
        claim: RootDomainPushClaim,
        made_progress: bool,
    ) {
        let target = {
            let push = self.push_iterator(claim.class);
            let mut state = push.lock_state();
            assert_eq!(
                state.phase,
                RootDomainPushPhase::Claimed(claim.source),
                "root-domain push completion must match the claimed owner"
            );
            assert_eq!(
                state.scan_generation, claim.generation,
                "root-domain push completion must match the claimed scan generation"
            );
            if made_progress
                && self.overload.contains_class(claim.source, claim.class)
                && self
                    .runqueues
                    .get(claim.source.as_usize())
                    .is_some_and(|remote| remote.is_online())
            {
                push.publish_target(&mut state, Some(claim.source));
                Some(claim.source)
            } else {
                state.cursor = Some(claim.source);
                self.advance_push_scan(claim.class, &mut state, claim.source)
            }
        };
        self.deliver_push_target(claim.class, target);
    }

    pub(super) fn advance_push_scan(
        &self,
        class: RootDomainPushClass,
        state: &mut RootDomainPushState,
        current: CpuId,
    ) -> Option<CpuId> {
        if let Some(target) = self.publish_next_push_target(class, state, current) {
            return Some(target);
        }
        if state.scan_generation != state.requested_generation {
            state.scan_generation = state.requested_generation;
            state.cursor = None;
            return self.publish_next_push_target(class, state, current);
        }
        self.push_iterator(class).publish_target(state, None);
        None
    }

    pub(super) fn publish_next_push_target(
        &self,
        class: RootDomainPushClass,
        state: &mut RootDomainPushState,
        excluded: CpuId,
    ) -> Option<CpuId> {
        if !self.overload.any_class(class) {
            self.push_iterator(class).publish_target(state, None);
            return None;
        }
        let target = self
            .overload
            .find_next_class(class, state.cursor, excluded, |cpu| {
                self.runqueues[cpu.as_usize()].is_online()
            });
        self.push_iterator(class).publish_target(state, target);
        target
    }

    pub(super) fn deliver_push_target(
        &self,
        class: RootDomainPushClass,
        mut target: Option<CpuId>,
    ) {
        while let Some(source) = target {
            let Some(remote) = self.runqueues.get(source.as_usize()) else {
                return;
            };
            if remote.kick_scheduler_work() {
                return;
            }
            target = {
                let mut state = self.push_iterator(class).lock_state();
                if state.phase != RootDomainPushPhase::Published(source) {
                    return;
                }
                state.cursor = Some(source);
                self.advance_push_scan(class, &mut state, source)
            };
        }
    }
}

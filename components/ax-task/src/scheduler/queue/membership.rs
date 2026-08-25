use super::*;
use crate::runtime::task_runtime;

const RUNQUEUE_SEQUENCE_EXHAUSTED_INVARIANT: u32 = 0x5251_0001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SequenceAllocationError {
    Exhausted,
}

impl RunQueue {
    pub(super) fn queued_thread_including_current(
        &self,
        id: ThreadId,
    ) -> Option<QueuedThreadSnapshot> {
        match self.membership_class(id)? {
            QueueMembershipClass::Stop => self.stop.as_ref().map(QueuedThreadSnapshot::from),
            QueueMembershipClass::Deadline(key) => {
                self.deadline.get(key).map(QueuedThreadSnapshot::from)
            }
            QueueMembershipClass::DeadlineThrottled => {
                self.deadline.throttled(id).map(QueuedThreadSnapshot::from)
            }
            QueueMembershipClass::Realtime(key) => self.rt.get(key).map(QueuedThreadSnapshot::from),
            QueueMembershipClass::Fair => {
                self.fair.find_first_matching(&mut |thread| thread.id == id)
            }
        }
    }

    pub(super) fn contains(&self, id: ThreadId) -> bool {
        self.membership_class(id).is_some()
    }

    pub(super) fn membership_class(&self, id: ThreadId) -> Option<QueueMembershipClass> {
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        self.membership_lookups
            .set(self.membership_lookups.get().saturating_add(1));
        self.membership
            .get(id.slot() as usize)
            .and_then(|membership| *membership)
            .filter(|membership| membership.generation == id.generation())
            .map(|membership| membership.class)
    }

    pub(super) fn register_membership(&mut self, id: ThreadId, class: QueueMembershipClass) {
        let slot = id.slot() as usize;
        assert!(
            self.membership.len() > slot,
            "thread construction must prepare owner rq membership"
        );
        assert!(
            self.membership[slot]
                .replace(QueueMembership {
                    generation: id.generation(),
                    class,
                })
                .is_none(),
            "runqueue membership must be unique"
        );
    }

    pub(super) fn unregister_membership(&mut self, id: ThreadId) {
        let membership = self
            .membership
            .get_mut(id.slot() as usize)
            .and_then(Option::take)
            .expect("queued thread must retain owner membership until removal");
        assert_eq!(membership.generation, id.generation());
    }

    pub(super) fn replace_membership_class(&mut self, id: ThreadId, class: QueueMembershipClass) {
        let membership = self
            .membership
            .get_mut(id.slot() as usize)
            .and_then(Option::as_mut)
            .expect("queued thread must retain owner membership during rekey");
        assert_eq!(membership.generation, id.generation());
        membership.class = class;
    }

    pub(super) fn allocate_sequence(&mut self) -> u64 {
        self.try_allocate_sequence()
            .unwrap_or_else(|error| match error {
                SequenceAllocationError::Exhausted => task_runtime::fatal_invariant(
                    RUNQUEUE_SEQUENCE_EXHAUSTED_INVARIANT,
                    self.next_sequence as usize,
                ),
            })
    }

    pub(super) fn try_allocate_sequence(&mut self) -> Result<u64, SequenceAllocationError> {
        let sequence = self.next_sequence;
        let next = sequence
            .checked_add(1)
            .ok_or(SequenceAllocationError::Exhausted)?;
        self.next_sequence = next;
        Ok(sequence)
    }
}

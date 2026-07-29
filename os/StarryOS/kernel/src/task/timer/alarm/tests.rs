#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rearming_physically_replaces_the_previous_alarm_node() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let first = slot.replace(Some(Duration::from_nanos(10)));
        let second = slot.replace(Some(Duration::from_nanos(20)));

        let AlarmChange::Schedule {
            delay: first_deadline,
            token: first_token,
        } = first
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(first_deadline, first_token, ());
        let AlarmChange::Schedule {
            delay: second_deadline,
            token: second_token,
        } = second
        else {
            unreachable!("rearmed slot must produce a schedule action")
        };
        queue.schedule(second_deadline, second_token, ());

        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.earliest_deadline(), Some(Duration::from_nanos(20)));
    }

    #[test]
    fn stale_generation_cannot_replace_the_current_alarm() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let stale = slot.replace(Some(Duration::from_nanos(10)));
        let current = slot.replace(Some(Duration::from_nanos(20)));

        let AlarmChange::Schedule {
            delay: current_deadline,
            token: current_token,
        } = current
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(current_deadline, current_token, ());
        let AlarmChange::Schedule {
            delay: stale_deadline,
            token: stale_token,
        } = stale
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(stale_deadline, stale_token, ());

        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.earliest_deadline(), Some(Duration::from_nanos(20)));
    }

    #[test]
    fn disarming_physically_removes_the_alarm_node() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let schedule = slot.replace(Some(Duration::from_nanos(10)));
        let AlarmChange::Schedule {
            delay: deadline,
            token,
        } = schedule
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(deadline, token, ());

        let cancellation = slot.replace(None);
        let AlarmChange::Cancel(cancelled_slot) = cancellation else {
            unreachable!("disarmed slot must produce a cancellation")
        };
        queue.cancel(&cancelled_slot);

        assert!(queue.is_empty());
    }

    #[test]
    fn pruning_a_stale_due_node_reclassifies_the_new_future_head() {
        let stale_slot = AlarmSlot::new();
        let future_slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let stale = stale_slot.replace(Some(Duration::from_nanos(10)));
        let future = future_slot.replace(Some(Duration::from_nanos(20)));
        let AlarmChange::Schedule {
            delay: stale_deadline,
            token: stale_token,
        } = stale
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        let AlarmChange::Schedule {
            delay: future_deadline,
            token: future_token,
        } = future
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(stale_deadline, stale_token, ());
        queue.schedule(future_deadline, future_token, ());

        // Publish cancellation without applying the queue removal yet. This
        // is the exact race where the worker observes a stale due head.
        let _pending_cancellation = stale_slot.replace(None);

        assert!(matches!(
            queue.next_action(Duration::from_nanos(15)),
            AlarmQueueAction::Wait(deadline) if deadline == Duration::from_nanos(20)
        ));
        assert_eq!(queue.entries.len(), 1);
    }
}

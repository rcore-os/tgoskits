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
        let AlarmChange::Cancel(cancellation_token) = cancellation else {
            unreachable!("disarmed slot must produce a cancellation")
        };
        queue.cancel(&cancellation_token);

        assert!(queue.is_empty());
    }

    #[test]
    fn stale_cancellation_does_not_remove_a_newer_alarm_generation() {
        let slot = AlarmSlot::new();
        let mut queue = AlarmQueue::new();
        let stale_schedule = slot.replace(Some(Duration::from_nanos(10)));
        let AlarmChange::Schedule {
            delay: stale_deadline,
            token: stale_token,
        } = stale_schedule
        else {
            unreachable!("armed slot must produce a schedule action")
        };
        queue.schedule(stale_deadline, stale_token, ());

        // Delay the cancellation until a concurrent rearm has already
        // published and installed a newer generation.
        let stale_cancellation = slot.replace(None);
        let current_schedule = slot.replace(Some(Duration::from_nanos(20)));
        let AlarmChange::Schedule {
            delay: current_deadline,
            token: current_token,
        } = current_schedule
        else {
            unreachable!("rearmed slot must produce a schedule action")
        };
        queue.schedule(current_deadline, current_token, ());

        let AlarmChange::Cancel(cancellation_token) = stale_cancellation else {
            unreachable!("disarmed slot must produce a cancellation")
        };
        queue.cancel(&cancellation_token);

        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.earliest_deadline(), Some(Duration::from_nanos(20)));
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

#[cfg(axtest)]
pub(super) fn stale_alarm_cancellation_preserves_new_generation_for_test() -> bool {
    let slot = AlarmSlot::new();
    let mut queue = AlarmQueue::new();
    let stale_schedule = slot.replace(Some(Duration::from_nanos(10)));
    let AlarmChange::Schedule {
        delay: stale_deadline,
        token: stale_token,
    } = stale_schedule
    else {
        return false;
    };
    queue.schedule(stale_deadline, stale_token, ());

    let stale_cancellation = slot.replace(None);
    let current_schedule = slot.replace(Some(Duration::from_nanos(20)));
    let AlarmChange::Schedule {
        delay: current_deadline,
        token: current_token,
    } = current_schedule
    else {
        return false;
    };
    queue.schedule(current_deadline, current_token, ());

    let AlarmChange::Cancel(cancellation_token) = stale_cancellation else {
        return false;
    };
    queue.cancel(&cancellation_token);

    queue.entries.len() == 1
        && queue.earliest_deadline() == Some(Duration::from_nanos(20))
}

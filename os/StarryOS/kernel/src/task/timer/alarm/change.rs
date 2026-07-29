#[derive(Clone, Debug)]
pub(crate) enum AlarmChange {
    Cancel(AlarmSlot),
    Schedule { delay: Duration, token: AlarmToken },
}

impl AlarmChange {
    pub(crate) fn apply(self, target: AlarmTarget) {
        let mut alarms = ALARM_LIST.lock();
        let previous_earliest = alarms.earliest_deadline();
        match self {
            Self::Cancel(slot) => alarms.cancel(&slot),
            Self::Schedule { delay, token } => {
                alarms.schedule(wall_time().saturating_add(delay), token, target);
            }
        }
        let earliest_changed = alarms.earliest_deadline() != previous_earliest;
        drop(alarms);
        if earliest_changed {
            EVENT_NEW_TIMER.notify(1);
        }
    }

    pub(crate) fn apply_cancellation(self) {
        match self {
            Self::Cancel(slot) => cancel_alarm_slot(&slot),
            Self::Schedule { .. } => {
                unreachable!("disarming an alarm slot must produce a cancellation")
            }
        }
    }
}

pub(super) fn apply_alarm_changes(
    changes: impl IntoIterator<Item = AlarmChange>,
    target: AlarmTarget,
) {
    for change in changes {
        change.apply(target.clone());
    }
}

fn cancel_alarm_slot(slot: &AlarmSlot) {
    let mut alarms = ALARM_LIST.lock();
    let previous_earliest = alarms.earliest_deadline();
    alarms.cancel(slot);
    let earliest_changed = alarms.earliest_deadline() != previous_earliest;
    drop(alarms);
    if earliest_changed {
        EVENT_NEW_TIMER.notify(1);
    }
}

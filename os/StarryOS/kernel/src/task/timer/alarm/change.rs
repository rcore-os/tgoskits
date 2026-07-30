#[derive(Clone, Debug)]
pub(crate) enum AlarmChange {
    Cancel(AlarmToken),
    Schedule { delay: Duration, token: AlarmToken },
}

impl AlarmChange {
    pub(crate) fn is_current_generation(&self) -> bool {
        match self {
            Self::Cancel(token) | Self::Schedule { token, .. } => {
                token.is_current_generation()
            }
        }
    }

    pub(crate) fn apply(self, target: AlarmTarget) {
        let mut alarms = ALARM_LIST.lock();
        let previous_earliest = alarms.earliest_deadline();
        match self {
            Self::Cancel(token) => alarms.cancel(&token),
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
            Self::Cancel(token) => cancel_alarm_generation(&token),
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

fn cancel_alarm_generation(token: &AlarmToken) {
    let mut alarms = ALARM_LIST.lock();
    let previous_earliest = alarms.earliest_deadline();
    alarms.cancel(token);
    let earliest_changed = alarms.earliest_deadline() != previous_earliest;
    drop(alarms);
    if earliest_changed {
        EVENT_NEW_TIMER.notify(1);
    }
}

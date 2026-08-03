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
            publish_alarm_change();
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

fn cancel_alarm_generation(token: &AlarmToken) {
    let mut alarms = ALARM_LIST.lock();
    let previous_earliest = alarms.earliest_deadline();
    alarms.cancel(token);
    let earliest_changed = alarms.earliest_deadline() != previous_earliest;
    drop(alarms);
    if earliest_changed {
        publish_alarm_change();
    }
}

fn publish_alarm_change() {
    // Publish the queue mutation before waking the fixed alarm worker. Loading
    // this epoch before the worker snapshots ALARM_LIST closes both the
    // publish-before-park and publish-during-snapshot races.
    ALARM_EPOCH.fetch_add(1, Ordering::AcqRel);
    ALARM_WAIT.notify_one();
}

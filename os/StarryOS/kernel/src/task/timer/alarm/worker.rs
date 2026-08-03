struct AlarmWorkerSnapshot {
    epoch: u64,
    action: AlarmAction,
}

fn take_alarm_worker_snapshot(
    epoch: &AtomicU64,
    snapshot_action: impl FnOnce() -> AlarmAction,
) -> AlarmWorkerSnapshot {
    // Producers mutate ALARM_LIST before incrementing the epoch. Taking the
    // baseline first means a producer racing the queue snapshot cannot be
    // absorbed into the worker's sleep predicate.
    let observed_epoch = epoch.load(Ordering::Acquire);
    let action = snapshot_action();
    AlarmWorkerSnapshot {
        epoch: observed_epoch,
        action,
    }
}

fn alarm_task() {
    loop {
        let snapshot = take_alarm_worker_snapshot(&ALARM_EPOCH, || {
            next_alarm_action(wall_time())
        });
        match snapshot.action {
            AlarmAction::AwaitNewTimer => {
                ALARM_WAIT.wait_until(|| {
                    ALARM_EPOCH.load(Ordering::Acquire) != snapshot.epoch
                });
            }
            AlarmAction::Fire {
                token,
                target: AlarmTarget::Process(pid),
            } => poll_process_timer_for_alarm(pid, &token),
            AlarmAction::AwaitDeadline(deadline) => {
                let remaining = deadline.saturating_sub(wall_time());
                if !remaining.is_zero() {
                    let _timed_out = ALARM_WAIT.wait_timeout_until(remaining, || {
                        ALARM_EPOCH.load(Ordering::Acquire) != snapshot.epoch
                    });
                }
            }
        }
    }
}

enum AlarmAction {
    AwaitNewTimer,
    Fire {
        token: AlarmToken,
        target: AlarmTarget,
    },
    AwaitDeadline(Duration),
}

fn next_alarm_action(now: Duration) -> AlarmAction {
    let mut alarms = ALARM_LIST.lock();
    match alarms.next_action(now) {
        AlarmQueueAction::Empty => AlarmAction::AwaitNewTimer,
        AlarmQueueAction::Wait(deadline) => AlarmAction::AwaitDeadline(deadline),
        AlarmQueueAction::Fire(entry) => AlarmAction::Fire {
            token: entry.token,
            target: entry.target,
        },
    }
}

/// Spawns the alarm task.
pub fn spawn_alarm_task() {
    info!("Initialize alarm...");
    crate::task::try_spawn_kernel_thread_with_stack(
        alarm_task,
        "alarm_task".to_owned(),
        crate::config::KERNEL_STACK_SIZE,
    )
    .unwrap_or_else(|error| panic!("failed to spawn alarm task: {error}"));
}

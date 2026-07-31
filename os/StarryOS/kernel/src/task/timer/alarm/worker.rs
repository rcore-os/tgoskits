async fn alarm_task() {
    loop {
        match next_alarm_action(wall_time()) {
            AlarmAction::AwaitNewTimer => {
                listener!(EVENT_NEW_TIMER => listener);
                if ALARM_LIST.lock().is_empty() {
                    listener.await;
                }
            }
            AlarmAction::Fire {
                token,
                target: AlarmTarget::Process(pid),
            } => poll_process_timer_for_alarm(pid, &token),
            AlarmAction::AwaitDeadline(deadline) => {
                listener!(EVENT_NEW_TIMER => listener);
                let deadline_is_current = ALARM_LIST
                    .lock()
                    .earliest_deadline()
                    .is_some_and(|current| current == deadline);
                if deadline_is_current {
                    let _ = timeout_at_wall(Some(deadline), listener).await;
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
        || block_on(alarm_task()),
        "alarm_task".to_owned(),
        crate::config::KERNEL_STACK_SIZE,
    )
    .unwrap_or_else(|error| panic!("failed to spawn alarm task: {error}"));
}

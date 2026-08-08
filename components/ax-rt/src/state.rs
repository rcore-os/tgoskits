//! RT executor and task state enums.

/// Realtime CPU entry state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtState {
    /// The realtime CPU has not entered the RT executor yet.
    Offline = 0,
    /// The realtime CPU is executing the isolated cooperative executor.
    Running = 1,
}

/// Realtime task scheduler state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtTaskState {
    /// Task can be selected by the RT executor.
    Ready   = 0,
    /// Task is currently running on the RT CPU.
    Running = 1,
    /// Task is blocked until its deadline expires.
    Delayed = 2,
    /// Task is blocked on an RT synchronization primitive.
    Blocked = 3,
    /// Task finished and will not be scheduled again.
    Exited  = 4,
}

pub(crate) fn rt_state_from_usize(value: usize) -> RtState {
    match value {
        value if value == RtState::Running as usize => RtState::Running,
        _ => RtState::Offline,
    }
}

pub(crate) fn rt_task_state_from_usize(value: usize) -> RtTaskState {
    match value {
        value if value == RtTaskState::Running as usize => RtTaskState::Running,
        value if value == RtTaskState::Delayed as usize => RtTaskState::Delayed,
        value if value == RtTaskState::Blocked as usize => RtTaskState::Blocked,
        value if value == RtTaskState::Exited as usize => RtTaskState::Exited,
        _ => RtTaskState::Ready,
    }
}

use alloc::string::String;
use core::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    Clean,
    SystemDown,
    Forced,
    Fault(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmStatus {
    Uninit,
    Ready,
    Running,
    Pausing,
    Paused,
    Stopping,
    Stopped,
    Destroying,
    Destroyed,
    Failed,
}

impl VmStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            VmStatus::Uninit => "uninit",
            VmStatus::Ready => "ready",
            VmStatus::Running => "running",
            VmStatus::Pausing => "pausing",
            VmStatus::Paused => "paused",
            VmStatus::Stopping => "stopping",
            VmStatus::Stopped => "stopped",
            VmStatus::Destroying => "destroying",
            VmStatus::Destroyed => "destroyed",
            VmStatus::Failed => "failed",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, VmStatus::Destroyed | VmStatus::Failed)
    }

    pub const fn as_str_with_icon(self) -> &'static str {
        match self {
            VmStatus::Uninit => "[..] uninit",
            VmStatus::Ready => "[OK] ready",
            VmStatus::Running => "[RUN] running",
            VmStatus::Pausing => "[..] pausing",
            VmStatus::Paused => "[PAUSE] paused",
            VmStatus::Stopping => "[..] stopping",
            VmStatus::Stopped => "[STOP] stopped",
            VmStatus::Destroying => "[..] destroying",
            VmStatus::Destroyed => "[DEL] destroyed",
            VmStatus::Failed => "[ERR] failed",
        }
    }
}

impl fmt::Display for VmStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

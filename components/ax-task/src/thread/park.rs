//! Generation-checked thread park handshake.

use crate::{ScheduleDecision, ThreadId};

/// Identifies one prepared park attempt by a specific running thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParkToken {
    thread: ThreadId,
    generation: u64,
}

impl ParkToken {
    pub(crate) const fn new(thread: ThreadId, generation: u64) -> Self {
        Self { thread, generation }
    }

    /// Returns the thread that prepared this park attempt.
    pub const fn thread(self) -> ThreadId {
        self.thread
    }

    /// Returns the monotonically increasing attempt generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Result of publishing the `PARKING` phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParkPrepare {
    /// A preceding notification was consumed, so the caller must not block.
    Notified,
    /// The caller published `PARKING` and may proceed to the commit phase.
    Prepared(ParkToken),
}

/// Result of rechecking a prepared park at the scheduler safe point.
#[derive(Clone, Copy, Debug)]
pub enum ParkCommit {
    /// A concurrent notification cancelled the park before schedule-out.
    Notified,
    /// The thread committed `BLOCKED` and selected its replacement.
    Blocked(ScheduleDecision),
}

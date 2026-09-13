//! Public scheduler lifecycle observations.

/// Observable lifecycle state of a thread.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadState {
    /// Allocated but not admitted to a run queue.
    New     = 0,
    /// Linux-style `TASK_RUNNING`, whether queued or executing on a CPU.
    Running = 2,
    /// Publishing a block operation while racing with wake-up.
    Parking = 3,
    /// Asleep on a wait object.
    Blocked = 4,
    /// A wake operation won the block/wake race.
    Waking  = 5,
    /// Execution has terminated and resources await reaping.
    Exited  = 6,
}

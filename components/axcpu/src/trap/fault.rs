//! Machine-reported memory access faults.

bitflags::bitflags! {
    /// Access information reported by a CPU page-fault trap.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PageFaultFlags: usize {
        /// The faulting access was a read.
        const READ = 1 << 0;
        /// The faulting access was a write.
        const WRITE = 1 << 1;
        /// The faulting access was an instruction fetch.
        const EXECUTE = 1 << 2;
        /// The fault came from a less-privileged user context.
        const USER = 1 << 3;
    }
}

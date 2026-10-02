#![no_std]

/// One `net:queue_poll_round` payload. Written by the eBPF program into the
/// perf event array and read back verbatim by the loader, so the layout is
/// shared ABI between the two halves: `repr(C)` and a multiple of 8 bytes.
///
/// This struct holds the event-specific part only: the kernel prepends the
/// generic trace entry header (`common_type`, `common_flags`,
/// `common_preempt_count`, `common_pid`), and the fields below follow it in
/// declaration order.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QueuePollEvent {
    /// Device discovery order in the network runtime.
    pub discovery_order: u32,
    /// Driver-assigned queue group id, unique within its device.
    pub group_id: u32,
    /// CPU that owns the queue group.
    pub owner_cpu: u32,
    /// CPU work budget handed to the poll round.
    pub budget: u32,
    /// Executor work units completed by the round.
    pub work_units: u32,
    /// 0 = idle, 1 = more work, 2 = blocked, 3 = failed.
    pub outcome: u32,
}

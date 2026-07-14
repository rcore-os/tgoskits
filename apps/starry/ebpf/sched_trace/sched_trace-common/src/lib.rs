#![no_std]

/// One `sched:sched_switch` record. Written by the eBPF program into the perf
/// event array and read back verbatim by the loader, so the layout is shared
/// ABI between the two halves: `repr(C)`, no padding holes (`_pad` keeps
/// `ts_ns` 8-byte aligned and the size a multiple of 8, matching what the
/// kernel's perf ring copies out).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SchedSwitchEvent {
    /// Scheduler task id of the task switched out.
    pub prev_tid: u64,
    /// Scheduler task id of the task switched in.
    pub next_tid: u64,
    /// ax-task scheduler switch-reason code sampled before the architectural
    /// context switch.
    pub prev_state: u32,
    pub _pad: u32,
    /// `bpf_ktime_get_ns()` sampled inside the probe.
    pub ts_ns: u64,
}

#![no_std]
#![no_main]

use aya_ebpf::{
    EbpfContext,
    helpers::bpf_ktime_get_ns,
    macros::{map, raw_tracepoint},
    maps::PerfEventArray,
    programs::RawTracePointContext,
};
use sched_trace_common::SchedSwitchEvent;

// Per-CPU perf event array. The loader opens one buffer per CPU and the
// `output` below targets the current CPU's slot via `BPF_F_CURRENT_CPU`.
#[map]
static EVENTS: PerfEventArray<SchedSwitchEvent> = PerfEventArray::new(0);

// `sched:sched_switch` raw tracepoint. ktracepoint's `define_event_trace!`
// widens each `TP_PROTO` field to `u64` and hands the program that array as
// its context, so the layout is:
//   args[0] = prev_tid, args[1] = next_tid, args[2] = prev_state (u32 widened).
#[raw_tracepoint(tracepoint = "sched_switch")]
pub fn sched_trace(ctx: RawTracePointContext) -> i32 {
    // SAFETY: the raw tracepoint context points at the 3-`u64` arg array the
    // kernel built for `sched_switch`; reading `[u64; 3]` stays within it.
    let args = unsafe { &*(ctx.as_ptr() as *const [u64; 3]) };
    let event = SchedSwitchEvent {
        prev_tid: args[0],
        next_tid: args[1],
        prev_state: args[2] as u32,
        _pad: 0,
        // SAFETY: `bpf_ktime_get_ns` is a side-effect-free helper.
        ts_ns: unsafe { bpf_ktime_get_ns() },
    };
    EVENTS.output(&ctx, event, 0);
    0
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // The verifier rejects loops, so a spinning handler would be rejected at
    // load time; mark it unreachable as the other in-tree programs do.
    unsafe { core::hint::unreachable_unchecked() }
}

#[unsafe(link_section = "license")]
#[unsafe(no_mangle)]
static LICENSE: [u8; 13] = *b"Dual MIT/GPL\0";

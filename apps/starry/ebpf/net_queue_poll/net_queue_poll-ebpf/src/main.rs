#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::PerfEventArray,
    programs::TracePointContext,
};
use net_queue_poll_common::QueuePollEvent;

// Per-CPU perf event array. The loader opens one buffer per CPU before
// attaching, and `output` targets the current CPU's slot.
#[map]
static EVENTS: PerfEventArray<QueuePollEvent> = PerfEventArray::new(0);

// `net:queue_poll_round` tracepoint. The context starts with the common entry
// (u16 type, u8 flags, u8 preempt_count, i32 pid) and continues with the
// event's fields in `format` order, each a fixed-width u32.
#[tracepoint]
pub fn net_queue_poll(ctx: TracePointContext) -> u32 {
    const COMMON_LEN: usize = 8;
    let event = QueuePollEvent {
        // SAFETY: the offsets follow the record layout that
        // `net:queue_poll_round` declares (the generic entry header, then the
        // fields in declaration order).  The loader attaches by name only and
        // checks nothing, so changing the event's fields or their order means
        // updating this program in the same change.
        discovery_order: unsafe { ctx.read_at::<u32>(COMMON_LEN).unwrap_or(0) },
        group_id: unsafe { ctx.read_at::<u32>(COMMON_LEN + 4).unwrap_or(0) },
        owner_cpu: unsafe { ctx.read_at::<u32>(COMMON_LEN + 8).unwrap_or(0) },
        budget: unsafe { ctx.read_at::<u32>(COMMON_LEN + 12).unwrap_or(0) },
        work_units: unsafe { ctx.read_at::<u32>(COMMON_LEN + 16).unwrap_or(0) },
        outcome: unsafe { ctx.read_at::<u32>(COMMON_LEN + 20).unwrap_or(0) },
    };
    EVENTS.output(&ctx, &event, 0);
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

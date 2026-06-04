use std::{
    collections::BTreeSet,
    thread,
    time::{Duration, Instant},
};

use aya::{
    maps::{PerfEventArray, perf::PerfEvent},
    programs::RawTracePoint,
    util::online_cpus,
};
use sched_trace_common::SchedSwitchEvent;

fn main() -> anyhow::Result<()> {
    // Bump RLIMIT_MEMLOCK so map / ring buffer allocations are not capped on
    // kernels that still use rlimit-based BPF memory accounting.
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: a valid `rlimit` for a known resource id.
    unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };

    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/sched_trace"
    )))?;

    // Open one perf buffer per CPU *before* attaching, so the EVENTS array
    // already holds a destination fd for every CPU when the first
    // sched_switch fires — otherwise early records hit an empty slot and are
    // dropped. `online_cpus` reads /sys; fall back to CPU 0 when it is absent.
    let mut events: PerfEventArray<_> =
        PerfEventArray::try_from(ebpf.take_map("EVENTS").expect("EVENTS map missing"))?;
    let cpus = online_cpus().unwrap_or_else(|_| vec![0]);
    let mut buffers = Vec::with_capacity(cpus.len());
    for cpu in cpus {
        buffers.push(events.open(cpu, None)?);
    }

    let program: &mut RawTracePoint = ebpf
        .program_mut("sched_trace")
        .expect("sched_trace program missing")
        .try_into()?;
    program.load()?;
    program.attach("sched_switch")?;
    println!(
        "SCHED_TRACE: attached sched_switch, draining {} cpu buffer(s)",
        buffers.len()
    );

    // Drive scheduler activity: worker threads that alternately sleep and spin
    // force context switches so `sched_switch` fires with several distinct
    // next tids.
    for i in 0..4 {
        thread::spawn(move || {
            loop {
                // Mix of yielding and short sleeps to provoke switches.
                thread::yield_now();
                thread::sleep(Duration::from_millis(2 + i));
            }
        });
    }

    // Drain for a fixed window, then assert. The raw tracepoint unregisters
    // itself when this process exits and the perf fds close.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut total: u64 = 0;
    let mut next_tids: BTreeSet<u64> = BTreeSet::new();
    while Instant::now() < deadline {
        let mut got_any = false;
        for buf in buffers.iter_mut() {
            if !buf.readable() {
                continue;
            }
            buf.for_each(|event| match event {
                PerfEvent::Sample { head, tail } => {
                    if let Some(ev) = decode(head, tail) {
                        total += 1;
                        next_tids.insert(ev.next_tid);
                        if total <= 20 {
                            println!(
                                "prev={} next={} state={} ts={}",
                                ev.prev_tid, ev.next_tid, ev.prev_state, ev.ts_ns
                            );
                        }
                        got_any = true;
                    }
                }
                PerfEvent::Lost { count } => {
                    eprintln!("sched_trace: lost {count} records");
                }
            });
        }
        if !got_any {
            thread::sleep(Duration::from_millis(10));
        }
    }

    let distinct = next_tids.len();
    println!("SCHED_TRACE: total records = {total}, distinct next tids = {distinct}");

    // Anti-fallback: a working sched_switch raw tracepoint + perf ringbuf must
    // produce many records spanning more than one task (generous TCG slack).
    if total >= 20 && distinct >= 2 {
        println!("SCHED_TRACE_PASS: {total} records, {distinct} distinct next tids");
        Ok(())
    } else {
        println!("SCHED_TRACE_FAIL: only {total} records, {distinct} distinct next tids");
        std::process::exit(1);
    }
}

/// Reassemble a [`SchedSwitchEvent`] from the (possibly ring-wrapped) perf
/// sample bytes. The kernel copies exactly `size_of::<SchedSwitchEvent>()`
/// bytes, but the perf layer may append 8-byte alignment padding, so we only
/// require that a full record is present.
fn decode(head: &[u8], tail: &[u8]) -> Option<SchedSwitchEvent> {
    const N: usize = size_of::<SchedSwitchEvent>();
    let mut bytes = [0u8; N];
    if head.len() >= N {
        bytes.copy_from_slice(&head[..N]);
    } else if head.len() + tail.len() >= N {
        let (first, second) = bytes.split_at_mut(head.len());
        first.copy_from_slice(head);
        second.copy_from_slice(&tail[..N - head.len()]);
    } else {
        return None;
    }
    // SAFETY: `SchedSwitchEvent` is `repr(C)` and plain-old-data; `bytes` holds
    // a full copy of one record. `read_unaligned` makes no alignment claim.
    Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<SchedSwitchEvent>()) })
}

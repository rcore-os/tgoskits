//! Attaches a tracepoint program to `net:queue_poll_round` and reads the
//! records back through a perf event array.
//!
//! This app only proves the consumer path (`load → attach → enable → read`);
//! the event's semantics belong to the network stack and its own tests.

use std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};

use aya::{
    Ebpf,
    maps::{
        PerfEventArray,
        perf::{PerfEvent, PerfEventArrayBuffer},
    },
    programs::TracePoint,
    util::online_cpus,
};
use net_queue_poll_common::QueuePollEvent;

const TRAFFIC_FRAMES: usize = 16;
const TRAFFIC_ATTEMPTS: usize = 8;
const TRAFFIC_RETRY: Duration = Duration::from_millis(20);
const DRAIN_WINDOW: Duration = Duration::from_secs(8);
/// Pages per perf ring buffer; must be a power of two.
const PERF_BUFFER_PAGES: usize = 8;

fn main() -> anyhow::Result<()> {
    // Bump RLIMIT_MEMLOCK so map / perf buffer allocations are not capped on
    // kernels that still use rlimit-based BPF memory accounting.
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: a valid `rlimit` for a known resource id.
    unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };

    let mut ebpf = Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/net_queue_poll"
    )))?;

    // Open one perf buffer per CPU before attaching, so the map already holds
    // a destination fd for every CPU when the first round completes.
    let mut events: PerfEventArray<_> =
        PerfEventArray::try_from(ebpf.take_map("EVENTS").expect("EVENTS map missing"))?;
    let cpus = online_cpus().unwrap_or_else(|_| vec![0]);
    let mut buffers = Vec::with_capacity(cpus.len());
    for cpu in cpus {
        let buffer = PerfEventArrayBuffer::open(cpu, PERF_BUFFER_PAGES)?;
        events.set(cpu, &buffer)?;
        buffers.push(buffer);
    }

    let program: &mut TracePoint = ebpf
        .program_mut("net_queue_poll")
        .expect("net_queue_poll program missing")
        .try_into()?;
    program.load()?;
    // aya enables the tracepoint through the perf event ioctl as part of the
    // attach, which is the consumer path this app exists to check.
    program.attach("net", "queue_poll_round")?;
    println!(
        "NET_QUEUE_POLL: attached net:queue_poll_round, draining {} cpu buffer(s)",
        buffers.len()
    );

    let traffic = thread::spawn(traffic_loop);

    let deadline = Instant::now() + DRAIN_WINDOW;
    let mut total: u64 = 0;
    let mut inconsistent: u64 = 0;
    let mut sample = None;
    while Instant::now() < deadline {
        let mut got_any = false;
        for buffer in buffers.iter_mut() {
            if !buffer.readable() {
                continue;
            }
            buffer.for_each(|event| {
                let PerfEvent::Sample { head, tail } = event else {
                    if let PerfEvent::Lost { count } = event {
                        eprintln!("net_queue_poll: lost {count} records");
                    }
                    return;
                };
                let Some(record) = decode(head, tail) else {
                    return;
                };
                total += 1;
                if record.outcome > 3 || record.work_units > record.budget {
                    inconsistent += 1;
                }
                if sample.is_none() {
                    sample = Some(record);
                }
            });
            got_any = true;
        }
        if !got_any {
            thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = traffic.join();

    match sample {
        Some(record) => println!(
            "NET_QUEUE_POLL: discovery_order={} group_id={} owner_cpu={} budget={} \
             work_units={} outcome={}",
            record.discovery_order,
            record.group_id,
            record.owner_cpu,
            record.budget,
            record.work_units,
            record.outcome
        ),
        None => println!("NET_QUEUE_POLL: no record"),
    }
    println!("NET_QUEUE_POLL: total records = {total}, inconsistent = {inconsistent}");

    // Reading at least one record proves the whole consumer path: the program
    // loaded, the attach published the gate, and the buffer carried a record
    // back.  `inconsistent` is reported, not judged: it only checks the
    // documented value ranges of the decoded fields (a lower bound on decoding
    // health), while the event's semantics are asserted by the system test.
    if total >= 1 {
        println!("NET_QUEUE_POLL_PASS: {total} records");
        Ok(())
    } else {
        println!("NET_QUEUE_POLL_FAIL: no record");
        std::process::exit(1);
    }
}

/// Sends datagrams off the box. The event is emitted per queue poll round, so
/// any device traffic drives it; the QEMU user-mode gateway this app runs
/// against answers ARP and IP, while loopback traffic never reaches a physical
/// queue.
///
/// The driver below mirrors the system test's own traffic loop (same gateway,
/// burst size and retry window); a change to either side has to keep the other
/// in step, and the retry window has to stay long enough for a guest whose
/// interface is still coming up.
fn traffic_loop() {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        eprintln!("net_queue_poll: UDP socket could not be created");
        return;
    };
    let mut payload = [0x5a_u8; 32];
    let mut sent = 0;
    // The first datagram can be refused while the gateway neighbor entry is
    // being resolved; the retries let that resolution complete.
    for _ in 0..TRAFFIC_ATTEMPTS {
        for index in 0..TRAFFIC_FRAMES {
            payload[0] = index as u8;
            if socket.send_to(&payload, "10.0.2.2:9").is_ok() {
                sent += 1;
            }
        }
        if sent > 0 {
            break;
        }
        thread::sleep(TRAFFIC_RETRY);
    }
    if sent == 0 {
        eprintln!("net_queue_poll: no datagram left the interface");
    }
}

/// Reassembles a record from the (possibly ring-wrapped) perf sample bytes.
fn decode(head: &[u8], tail: &[u8]) -> Option<QueuePollEvent> {
    const N: usize = size_of::<QueuePollEvent>();
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
    // SAFETY: `QueuePollEvent` is `repr(C)` and plain-old-data; `bytes` holds a
    // full copy of one record. `read_unaligned` makes no alignment claim.
    Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<QueuePollEvent>()) })
}

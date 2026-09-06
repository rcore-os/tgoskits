// netmon: userspace loader for the full-stack ax-net eBPF network monitor.
//
// Attaches kprobe/kretprobe pairs across four layers of the queue runtime:
//
//   L3 protocol : DeviceHandle::count_tx / count_rx (the /proc/net/dev points)
//   L2 queue    : QueueFramePort::transmit / receive
//   L1 schedule : PollGroupState::schedule_irq + QueueGroupExecutor::poll
//   L0 SDIO     : SdioCard::submit_read_dma / submit_write_dma (CMD53)
//   control     : AicWifiControl::start (WifiControl)
//
// Every probe resolves to at most one /proc/kallsyms symbol; the exact-one
// assertion guards against generic-monomorphization double-counting, while
// SDIO/WiFi probes are skipped when their driver is absent from the image.
//
// Output format (parseable by scripts):
//
//   NETMON_BEGIN
//   tx_pkts=<N> tx_bytes=<N> rx_pkts=<N> rx_bytes=<N>
//   irq=<N> poll=<N> port_tx=<N> port_rx=<N> sdio_read=<N> sdio_write=<N> wifi_start=<N>
//   hist_irq_poll=<b0>,<b1>,...
//   hist_poll_dur=...
//   hist_port_tx_dur=...
//   hist_port_rx_dur=...
//   hist_sdio_dur=...
//   hist_wifi_start_dur=...
//   NETMON_END
//
// Histogram bucket i covers [2^i, 2^(i+1)) nanoseconds; the last bucket
// clamps every larger value.

use aya::{maps::PerCpuArray, programs::KProbe};
use clap::Parser;
#[rustfmt::skip]
use log::warn;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    thread,
};

use netmon_common::*;
use tokio::{signal, time};

/// Shorthand for the netmon PerCpuArray map type used by the loader.
type CounterMap<'a> = PerCpuArray<&'a aya::maps::MapData, u64>;

#[derive(Debug, Parser)]
struct Opt {
    /// Print one snapshot immediately and exit (for scripted sampling).
    #[clap(long)]
    once: bool,
    /// Attach probes, make a self-test loopback connection, print stats, then exit.
    #[clap(long)]
    test: bool,
    /// Interval in seconds between periodic snapshots (default 5).
    #[clap(long, default_value = "5")]
    interval: u64,
}

/// One probe: eBPF program name and kallsyms fragments. Entry or return
/// kind comes from the program's ELF section (`kprobe` / `kretprobe`).
/// Optional probes are skipped with a warning when their kernel component
/// is not built into the running image (e.g. SDIO/WiFi on virtio-only
/// QEMU builds); required probes must resolve to exactly one symbol.
struct ProbeSpec {
    program: &'static str,
    fragments: &'static [&'static str],
    optional: bool,
}

/// All probes. `queue_poll`/`port_tx`/`port_rx`/`sdio_*`/`wifi_start` each
/// carry a matching `*_ret` kretprobe on the same symbol.
const PROBES: &[ProbeSpec] = &[
    ProbeSpec {
        program: "count_tx",
        optional: false,
        fragments: &["6ax_net", "6router", "12DeviceHandle", "8count_tx"],
    },
    ProbeSpec {
        program: "count_rx",
        optional: false,
        fragments: &["6ax_net", "6router", "12DeviceHandle", "8count_rx"],
    },
    ProbeSpec {
        program: "sched_irq",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "5state",
            "15PollGroupState",
            "12schedule_irq",
        ],
    },
    ProbeSpec {
        program: "queue_poll",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "8executor",
            "18QueueGroupExecutor",
            "4poll",
        ],
    },
    ProbeSpec {
        program: "queue_poll_ret",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "8executor",
            "18QueueGroupExecutor",
            "4poll",
        ],
    },
    ProbeSpec {
        program: "port_tx",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "8executor",
            "14QueueFramePort",
            "8transmit",
        ],
    },
    ProbeSpec {
        program: "port_tx_ret",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "8executor",
            "14QueueFramePort",
            "8transmit",
        ],
    },
    ProbeSpec {
        program: "port_rx",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "8executor",
            "14QueueFramePort",
            "7receive",
        ],
    },
    ProbeSpec {
        program: "port_rx_ret",
        optional: false,
        fragments: &[
            "6ax_net",
            "13queue_runtime",
            "8executor",
            "14QueueFramePort",
            "7receive",
        ],
    },
    ProbeSpec {
        program: "sdio_read",
        optional: true,
        fragments: &[
            "15sdmmc_protocol",
            "4sdio",
            "2io",
            "8transfer",
            "8SdioCard",
            "15submit_read_dma",
        ],
    },
    ProbeSpec {
        program: "sdio_read_ret",
        optional: true,
        fragments: &[
            "15sdmmc_protocol",
            "4sdio",
            "2io",
            "8transfer",
            "8SdioCard",
            "15submit_read_dma",
        ],
    },
    ProbeSpec {
        program: "sdio_write",
        optional: true,
        fragments: &[
            "15sdmmc_protocol",
            "4sdio",
            "2io",
            "8transfer",
            "8SdioCard",
            "16submit_write_dma",
        ],
    },
    ProbeSpec {
        program: "sdio_write_ret",
        optional: true,
        fragments: &[
            "15sdmmc_protocol",
            "4sdio",
            "2io",
            "8transfer",
            "8SdioCard",
            "16submit_write_dma",
        ],
    },
    ProbeSpec {
        program: "wifi_start",
        optional: true,
        fragments: &[
            "7aic8800",
            "4rdif",
            "6device",
            "9endpoints",
            "7control",
            "13AicWifiControl",
            "10WifiControl",
            "5start",
        ],
    },
    ProbeSpec {
        program: "wifi_start_ret",
        optional: true,
        fragments: &[
            "7aic8800",
            "4rdif",
            "6device",
            "9endpoints",
            "7control",
            "13AicWifiControl",
            "10WifiControl",
            "5start",
        ],
    },
];

/// Find symbols whose name contains all `fragments`. An empty result means
/// the kernel component is absent from this image; callers decide whether
/// that is an error or an expected skip.
fn resolve_symbols(fragments: &[&str]) -> anyhow::Result<Vec<String>> {
    let buf = BufReader::new(fs::File::open("/proc/kallsyms")?);
    let mut syms = Vec::new();
    for line in buf.lines() {
        let line = line?;
        if let Some(name) = line.split_whitespace().nth(2)
            && fragments.iter().all(|f| name.contains(f))
        {
            syms.push(name.to_string());
        }
    }
    Ok(syms)
}

/// Read per-CPU counter values and sum across all CPUs.
fn per_cpu_sum(map: &CounterMap<'_>, idx: u32) -> u64 {
    match map.get(&idx, 0) {
        Ok(vals) => vals.iter().sum(),
        Err(e) => {
            warn!("per_cpu_sum({idx}) failed: {e}");
            0
        }
    }
}

fn hist_line(map: &CounterMap<'_>, base: u32) -> String {
    let mut out = String::new();
    for bucket in 0..H_BUCKETS {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&per_cpu_sum(map, base + bucket).to_string());
    }
    out
}

fn print_stats(counters: &CounterMap<'_>, hists: &CounterMap<'_>) {
    println!("NETMON_BEGIN");
    println!(
        "tx_pkts={} tx_bytes={} rx_pkts={} rx_bytes={}",
        per_cpu_sum(counters, CNT_TX_PKTS),
        per_cpu_sum(counters, CNT_TX_BYTES),
        per_cpu_sum(counters, CNT_RX_PKTS),
        per_cpu_sum(counters, CNT_RX_BYTES),
    );
    println!(
        "irq={} poll={} port_tx={} port_rx={} sdio_read={} sdio_write={} wifi_start={}",
        per_cpu_sum(counters, CNT_IRQ),
        per_cpu_sum(counters, CNT_POLL),
        per_cpu_sum(counters, CNT_PORT_TX),
        per_cpu_sum(counters, CNT_PORT_RX),
        per_cpu_sum(counters, CNT_SDIO_READ),
        per_cpu_sum(counters, CNT_SDIO_WRITE),
        per_cpu_sum(counters, CNT_WIFI_START),
    );
    println!("hist_irq_poll={}", hist_line(hists, HIST_IRQ_POLL));
    println!("hist_poll_dur={}", hist_line(hists, HIST_POLL_DUR));
    println!("hist_port_tx_dur={}", hist_line(hists, HIST_PORT_TX_DUR));
    println!("hist_port_rx_dur={}", hist_line(hists, HIST_PORT_RX_DUR));
    println!("hist_sdio_dur={}", hist_line(hists, HIST_SDIO_DUR));
    println!(
        "hist_wifi_start_dur={}",
        hist_line(hists, HIST_WIFI_START_DUR)
    );
    println!("NETMON_END");
}

fn attach_probe(ebpf: &mut aya::Ebpf, spec: &ProbeSpec, sym: &str) -> anyhow::Result<()> {
    let p: &mut KProbe = ebpf.program_mut(spec.program).unwrap().try_into()?;
    p.load()?;
    p.attach(sym, 0)?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .format_timestamp(None)
        .init();

    // Resolve every probe before attaching anything, so a refactor or
    // monomorphization change fails fast with the full list instead of
    // half-attaching and double counting. Optional probes are skipped when
    // their kernel component is absent from this image (e.g. SDIO/WiFi on
    // virtio-only QEMU builds).
    let mut resolved = Vec::new();
    for spec in PROBES {
        let syms = resolve_symbols(spec.fragments)?;
        if syms.is_empty() && spec.optional {
            warn!(
                "{}: symbol not present in this image, skipping",
                spec.program
            );
            continue;
        }
        anyhow::ensure!(
            syms.len() == 1,
            "probe {}: expected exactly 1 symbol, got {}: {:?}",
            spec.program,
            syms.len(),
            syms
        );
        resolved.push((spec.program, syms[0].clone()));
        warn!("resolved {} -> {}", spec.program, syms[0]);
    }

    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    let ret = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };
    if ret != 0 {
        warn!("setrlimit(RLIMIT_MEMLOCK) failed: {ret}");
    }

    let mut ebpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/netmon"
    )))?;

    if let Err(e) = aya_log::EbpfLogger::init(&mut ebpf) {
        warn!("failed to initialize eBPF logger: {e}");
    }

    for (program, sym) in &resolved {
        let spec = PROBES
            .iter()
            .find(|spec| spec.program == *program)
            .expect("resolved program must have a spec");
        attach_probe(&mut ebpf, spec, sym)?;
    }

    let counters: CounterMap<'_> = PerCpuArray::try_from(ebpf.map("COUNTERS").unwrap())?;
    let hists: CounterMap<'_> = PerCpuArray::try_from(ebpf.map("HISTS").unwrap())?;

    if opt.once {
        print_stats(&counters, &hists);
        return Ok(());
    }

    if opt.test {
        // Self-contained loopback test, mirroring net_stats. Loopback frames
        // take the Router fast path and bypass QueueFramePort, so only the
        // L3 counters (count_tx/count_rx) are asserted here; queue, SDIO and
        // WiFi probes require real device traffic (see board testing).
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let server = thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                if let Ok(n) = sock.read(&mut buf) {
                    let _ = sock.write_all(&buf[..n]);
                }
            }
        });

        match TcpStream::connect(addr) {
            Ok(mut stream) => {
                let payload = b"netmon-probe-traffic-payload\n";
                let _ = stream.write_all(payload);
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
            }
            Err(e) => warn!("test connect failed: {e}"),
        }
        let _ = server.join();

        let udp_server = UdpSocket::bind("127.0.0.1:0")?;
        let udp_addr = udp_server.local_addr()?;
        let udp_peer = UdpSocket::bind("127.0.0.1:0")?;
        let udp_payload = b"netmon-udp-payload\n";
        let _ = udp_peer.send_to(udp_payload, udp_addr);
        let mut udp_buf = [0u8; 1024];
        if let Ok((n, from)) = udp_server.recv_from(&mut udp_buf) {
            let _ = udp_server.send_to(&udp_buf[..n], from);
        }
        let _ = udp_peer.recv_from(&mut udp_buf);

        time::sleep(time::Duration::from_millis(300)).await;
        print_stats(&counters, &hists);

        let tx_pkts = per_cpu_sum(&counters, CNT_TX_PKTS);
        let tx_bytes = per_cpu_sum(&counters, CNT_TX_BYTES);
        let rx_pkts = per_cpu_sum(&counters, CNT_RX_PKTS);
        let rx_bytes = per_cpu_sum(&counters, CNT_RX_BYTES);

        if tx_pkts == 0 || tx_bytes == 0 || rx_pkts == 0 || rx_bytes == 0 {
            anyhow::bail!(
                "TEST FAILED: counter(s) are zero despite loopback traffic \
                 (tx_pkts={}, tx_bytes={}, rx_pkts={}, rx_bytes={})",
                tx_pkts,
                tx_bytes,
                rx_pkts,
                rx_bytes,
            );
        }

        println!("TEST PASSED: all counters non-zero");
        return Ok(());
    }

    let mut interval = time::interval(time::Duration::from_secs(opt.interval));
    interval.tick().await; // skip immediate first tick
    tokio::select! {
        _ = async { loop { interval.tick().await; print_stats(&counters, &hists); } } => {}
        _ = signal::ctrl_c() => {}
    }
    print_stats(&counters, &hists);
    Ok(())
}

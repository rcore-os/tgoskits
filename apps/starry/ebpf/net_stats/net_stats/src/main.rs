// net_stats: userspace loader for the ax-net kprobe statistics collector.
//
// Output format (parseable by summarize.py):
//
//   NET_STATS_BEGIN
//   tcp_tx_pkts=<N>  tcp_tx_bytes=<N>
//   tcp_rx_pkts=<N>  tcp_rx_bytes=<N>
//   udp_tx_pkts=<N>  udp_tx_bytes=<N>
//   udp_rx_pkts=<N>  udp_rx_bytes=<N>
//   NET_STATS_END

use aya::{maps::Array, programs::KProbe};
use clap::Parser;
#[rustfmt::skip]
use log::warn;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream, UdpSocket},
    thread,
};

use tokio::{signal, time};

const TCP_TX_PKTS: u32 = 0;
const TCP_TX_BYTES: u32 = 1;
const TCP_RX_PKTS: u32 = 2;
const TCP_RX_BYTES: u32 = 3;
const UDP_TX_PKTS: u32 = 4;
const UDP_TX_BYTES: u32 = 5;
const UDP_RX_PKTS: u32 = 6;
const UDP_RX_BYTES: u32 = 7;

#[derive(Debug, Parser)]
struct Opt {
    /// Print one snapshot immediately and exit (for scripted sampling).
    #[clap(long)]
    once: bool,
    /// Attach probes, make a self-test TCP connection, print stats, then exit.
    #[clap(long)]
    test: bool,
    /// Interval in seconds between periodic snapshots (default 5).
    #[clap(long, default_value = "5")]
    interval: u64,
}

/// Find all kallsyms symbols whose name contains all `fragments`.
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
    if syms.is_empty() {
        anyhow::bail!(
            "no symbols with fragments {:?} found in /proc/kallsyms",
            fragments
        );
    }
    Ok(syms)
}

fn print_stats(netstats: &Array<&aya::maps::MapData, u64>) {
    let get = |i: u32| netstats.get(&i, 0).unwrap_or(0);
    println!("NET_STATS_BEGIN");
    println!(
        "tcp_tx_pkts={}  tcp_tx_bytes={}",
        get(TCP_TX_PKTS),
        get(TCP_TX_BYTES)
    );
    println!(
        "tcp_rx_pkts={}  tcp_rx_bytes={}",
        get(TCP_RX_PKTS),
        get(TCP_RX_BYTES)
    );
    println!(
        "udp_tx_pkts={}  udp_tx_bytes={}",
        get(UDP_TX_PKTS),
        get(UDP_TX_BYTES)
    );
    println!(
        "udp_rx_pkts={}  udp_rx_bytes={}",
        get(UDP_RX_PKTS),
        get(UDP_RX_BYTES)
    );
    println!("NET_STATS_END");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .format_timestamp(None)
        .init();

    // Rust v0 mangled fragments for ax_net SocketOps send/recv variants
    let syms_tcp_send = resolve_symbols(&["6ax_net3tcp", "9TcpSocket", "9SocketOps4send"])?;
    let syms_tcp_recv = resolve_symbols(&["6ax_net3tcp", "9TcpSocket", "9SocketOps4recv"])?;
    let syms_udp_send = resolve_symbols(&["6ax_net3udp", "9UdpSocket", "9SocketOps4send"])?;
    let syms_udp_recv = resolve_symbols(&["6ax_net3udp", "9UdpSocket", "9SocketOps4recv"])?;

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
        "/net_stats"
    )))?;

    if let Err(e) = aya_log::EbpfLogger::init(&mut ebpf) {
        warn!("failed to initialize eBPF logger: {e}");
    }

    // A single kprobe program can be loaded once and attached to multiple
    // symbols. Each send/recv has several monomorphized variants (one per
    // buffer type), so we attach the same probe to every matching symbol.
    macro_rules! attach_all {
        ($ebpf:expr, $prog:literal, $syms:expr) => {{
            let p: &mut KProbe = $ebpf.program_mut($prog).unwrap().try_into()?;
            p.load()?;
            for sym in &$syms {
                p.attach(sym, 0)?;
            }
        }};
    }

    attach_all!(ebpf, "tcp_send_entry", syms_tcp_send);
    attach_all!(ebpf, "tcp_send_ret", syms_tcp_send);
    attach_all!(ebpf, "tcp_recv_entry", syms_tcp_recv);
    attach_all!(ebpf, "tcp_recv_ret", syms_tcp_recv);
    attach_all!(ebpf, "udp_send_entry", syms_udp_send);
    attach_all!(ebpf, "udp_send_ret", syms_udp_send);
    attach_all!(ebpf, "udp_recv_entry", syms_udp_recv);
    attach_all!(ebpf, "udp_recv_ret", syms_udp_recv);

    let netstats: Array<_, u64> = Array::try_from(ebpf.map("NETSTATS").unwrap())?;

    if opt.once {
        print_stats(&netstats);
        return Ok(());
    }

    if opt.test {
        // Self-contained loopback test: spawn a listener that echoes data,
        // then connect to it and exchange a payload. This drives real
        // ax_net TcpSocket send/recv through the kernel while probes are live.
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
                let payload = b"net_stats-probe-traffic-payload\n";
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
        let udp_payload = b"net_stats-udp-payload\n";
        let _ = udp_peer.send_to(udp_payload, udp_addr);
        let mut udp_buf = [0u8; 1024];
        if let Ok((n, from)) = udp_server.recv_from(&mut udp_buf) {
            let _ = udp_server.send_to(&udp_buf[..n], from);
        }
        let _ = udp_peer.recv_from(&mut udp_buf);

        time::sleep(time::Duration::from_millis(300)).await;
        print_stats(&netstats);

        // Validate that byte counters are non-zero when traffic was generated
        let get = |i: u32| netstats.get(&i, 0).unwrap_or(0);
        let tcp_tx_bytes = get(TCP_TX_BYTES);
        let tcp_rx_bytes = get(TCP_RX_BYTES);
        let udp_tx_bytes = get(UDP_TX_BYTES);
        let udp_rx_bytes = get(UDP_RX_BYTES);

        if tcp_tx_bytes == 0 || tcp_rx_bytes == 0 {
            anyhow::bail!(
                "TEST FAILED: TCP byte counters are zero (tx={}, rx={}) despite packet traffic",
                tcp_tx_bytes,
                tcp_rx_bytes
            );
        }
        if udp_tx_bytes == 0 || udp_rx_bytes == 0 {
            anyhow::bail!(
                "TEST FAILED: UDP byte counters are zero (tx={}, rx={}) despite packet traffic",
                udp_tx_bytes,
                udp_rx_bytes
            );
        }

        println!("TEST PASSED: all byte counters non-zero");
        return Ok(());
    }

    let mut interval = time::interval(time::Duration::from_secs(opt.interval));
    interval.tick().await; // skip immediate first tick
    tokio::select! {
        _ = async { loop { interval.tick().await; print_stats(&netstats); } } => {}
        _ = signal::ctrl_c() => {}
    }
    print_stats(&netstats);
    Ok(())
}

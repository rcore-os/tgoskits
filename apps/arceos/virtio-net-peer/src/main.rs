//! Persistent TCP heartbeat test for AxVisor's internal VirtIO-net switch.
//!
//! Every guest configures `eth0` from the compile-time environment and then runs
//! both roles at the same time: the server in its own thread, the client in the
//! main thread. Both stay alive forever:
//!
//! * the server accepts sessions in a loop and answers every heartbeat;
//! * the client keeps one logical session alive towards the peer address derived
//!   from its own address (last octet plus one), sends one heartbeat every four
//!   seconds and waits for the matching acknowledgement.
//!
//! Every transport error, EOF or mismatched acknowledgement tears the session
//! down; the client then reconnects on its own and the server goes back to
//! `accept`. Transient failures are reported as warnings, so
//! `VMx_VIRTIO_NET_FAIL` stays reserved for setup errors that cannot be
//! recovered from.

#[cfg(feature = "arceos")]
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "arceos")]
use ax_std as _;

const VM_TAG: &str = match option_env!("AXVIRTIO_VM_TAG") {
    Some(value) => value,
    None => "VM",
};
const LOCAL_IP: &str = match option_env!("AXVIRTIO_LOCAL_IP") {
    Some(value) => value,
    None => "10.0.2.15",
};
/// Heartbeat period in milliseconds (compile-time override).
const HEARTBEAT_PERIOD_MS: &str = match option_env!("AXVIRTIO_HEARTBEAT_PERIOD_MS") {
    Some(value) => value,
    None => "2000",
};
/// Successful heartbeats after which the pass marker is printed, once.
const PASS_AFTER_HEARTBEATS: &str = match option_env!("AXVIRTIO_HEARTBEAT_PASS_AFTER") {
    Some(value) => value,
    None => "3",
};

/// The peer of a guest is its own IPv4 address with the last octet offset by
/// this: `10.0.2.15` talks to `10.0.2.16`, `10.0.2.16` to `10.0.2.17`, and so
/// on, so a chain of guests forms a chain of sessions.
const PEER_OCTET_OFFSET: u8 = 1;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 4000;
const DEFAULT_CONNECT_START_DELAY_MS: u64 = 10000;
/// Budget for one connect attempt (compile-time override). A blocking connect to
/// an address nobody answers blocks for minutes on ARP backoff, so every attempt
/// gets its own deadline and an unreachable peer turns into a visible retry.
const CONNECT_TIMEOUT_MS: &str = match option_env!("AXVIRTIO_CONNECT_TIMEOUT_MS") {
    Some(value) => value,
    None => "4000",
};
/// Grace period before the client's first connect attempt (compile-time
/// override). All guests boot at the same time and only configure their address
/// after their DHCP bootstrap times out, so starting immediately would race the
/// neighbours' configuration and poison the ARP cache with exponential backoff.
const CONNECT_START_DELAY_MS: &str = match option_env!("AXVIRTIO_CONNECT_START_DELAY_MS") {
    Some(value) => value,
    None => "10000",
};
const DEFAULT_HEARTBEAT_PERIOD_MS: u64 = 4000;
const DEFAULT_PASS_AFTER_HEARTBEATS: u64 = 3;
/// Delay before the next connect attempt or session, so retries stay calm.
const RETRY_DELAY_MS: u64 = 1000;
/// Log every connect retry up to this count, then only every tenth attempt.
const CONNECT_RETRY_LOG_BURST: u64 = 5;
const TEST_PORT: u16 = 5001;
const IPV4_PREFIX_LEN: u8 = 24;
const MAX_LINE_LEN: usize = 256;
const STATUS_EVERY_HEARTBEATS: u64 = 10;

#[cfg(feature = "arceos")]
fn main() {
    if let Err(error) = run() {
        println!("{VM_TAG}_VIRTIO_NET_FAIL {error}");
        return;
    }
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
fn run() -> std::io::Result<()> {
    let interface = ax_net::interface_by_name("eth0")
        .ok_or_else(|| std::io::Error::other("eth0 was not discovered"))?;
    let local_ip: Ipv4Addr = LOCAL_IP
        .parse()
        .map_err(|_| std::io::Error::other("invalid local IPv4 address"))?;
    let peer_ip = peer_ip_of(local_ip)?;
    ax_net::set_interface_ipv4(interface.id, local_ip, IPV4_PREFIX_LEN)
        .map_err(|error| std::io::Error::other(format!("configure eth0: {error}")))?;

    println!(
        "{VM_TAG}_VIRTIO_NET_READY roles=server+client local={local_ip}/{IPV4_PREFIX_LEN} \
         peer={peer_ip}:{TEST_PORT} period_ms={} mac={:?}",
        heartbeat_period().as_millis(),
        interface.mac
    );
    println!("{VM_TAG}_VIRTIO_NET_IFACES {}", interface_report());

    // The server owns a thread of its own so that both roles run at the same
    // time; the client keeps the main thread busy for the rest of the guest's
    // life, which also keeps the process alive.
    thread::Builder::new()
        .name(String::from("virtio-net-server"))
        .spawn(run_server)
        .map_err(|error| std::io::Error::other(format!("spawn server thread: {error}")))?;
    run_client(peer_ip)
}

/// Derives the peer address from the local address: `10.0.2.15` -> `10.0.2.16`.
#[cfg(feature = "arceos")]
fn peer_ip_of(local_ip: Ipv4Addr) -> std::io::Result<Ipv4Addr> {
    let octets = local_ip.octets();
    let last = octets[3]
        .checked_add(PEER_OCTET_OFFSET)
        .ok_or_else(|| std::io::Error::other("peer IPv4 address octet overflows"))?;
    Ok(Ipv4Addr::new(octets[0], octets[1], octets[2], last))
}

/// Accepts sessions forever and answers every heartbeat with an acknowledgement
/// that repeats the client's sequence number.
#[cfg(feature = "arceos")]
fn run_server() -> std::io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, TEST_PORT))?;
    println!("{VM_TAG}_VIRTIO_NET_LISTEN addr=0.0.0.0:{TEST_PORT} local={LOCAL_IP}");
    let mut sessions = 0u64;
    let mut pass_reported = false;

    loop {
        let (mut stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) => {
                println!("{VM_TAG}_VIRTIO_NET_ACCEPT_RETRY error={error}");
                thread::sleep(retry_delay());
                continue;
            }
        };
        sessions += 1;
        println!("{VM_TAG}_VIRTIO_NET_ACCEPT session={sessions} peer={peer} local={LOCAL_IP}");
        let mut heartbeats = 0u64;
        let mut reason = String::from("peer closed the session");

        loop {
            let line = match read_line(&mut stream) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    reason = format!("recv: {error}");
                    break;
                }
            };
            let sequence = field(&line, "seq=").unwrap_or("?");
            heartbeats += 1;
            if let Err(error) =
                stream.write_all(format!("ACK {VM_TAG} seq={sequence}\n").as_bytes())
            {
                reason = format!("send ack: {error}");
                break;
            }
            println!(
                "{VM_TAG}_VIRTIO_NET_HB_RX session={sessions} count={heartbeats} seq={sequence} \
                 local={LOCAL_IP} peer={peer} payload=\"{line}\""
            );
            if !pass_reported && heartbeats >= pass_after_heartbeats() {
                pass_reported = true;
                println!("{VM_TAG}_VIRTIO_NET_PASS role=server heartbeats={heartbeats}");
            }
            if heartbeats % STATUS_EVERY_HEARTBEATS == 0 {
                println!(
                    "{VM_TAG}_VIRTIO_NET_STATUS role=server session={sessions} \
                     heartbeats={heartbeats} local={LOCAL_IP} peer={peer} ifaces={}",
                    interface_report()
                );
            }
        }

        let _ = stream.shutdown(Shutdown::Both);
        println!(
            "{VM_TAG}_VIRTIO_NET_SESSION_END role=server session={sessions} \
             heartbeats={heartbeats} reason={reason} (waiting for the next session)"
        );
    }
}

/// Keeps reconnecting forever: one session, one heartbeat every four seconds,
/// and a session restart as soon as anything about it fails.
#[cfg(feature = "arceos")]
fn run_client(peer_ip: Ipv4Addr) -> std::io::Result<()> {
    let target = (peer_ip, TEST_PORT);
    let started = Instant::now();
    let mut sessions = 0u64;
    let mut failed_sessions = 0u64;
    let mut pass_reported = false;

    // Let the neighbours finish booting and configuring their addresses before
    // the first attempt, so the chain connects instead of poisoning the ARP
    // cache with failed resolutions.
    thread::sleep(connect_start_delay());

    loop {
        let mut stream = connect_forever(target, &mut failed_sessions);
        sessions += 1;
        println!(
            "{VM_TAG}_VIRTIO_NET_CONNECTED session={sessions} local={LOCAL_IP}/{} \
             peer={peer_ip}:{TEST_PORT} stream={:?}->{:?} failed_sessions={failed_sessions}",
            IPV4_PREFIX_LEN,
            stream
                .local_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default(),
            stream
                .peer_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default()
        );

        let mut sequence = 0u64;
        let mut reason = String::from("peer closed the session");
        loop {
            sequence += 1;
            let uptime = started.elapsed().as_secs();
            let heartbeat = format!(
                "HB {VM_TAG} seq={sequence} local={LOCAL_IP} peer={peer_ip} uptime={uptime}s\n"
            );
            if let Err(error) = stream.write_all(heartbeat.as_bytes()) {
                reason = format!("send heartbeat: {error}");
                break;
            }

            // The acknowledgement gives a per-heartbeat EOF/error signal, so a
            // vanished peer is detected without any socket timeout support.
            let acknowledgement = match read_line(&mut stream) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    reason = format!("recv ack: {error}");
                    break;
                }
            };
            let expected = sequence.to_string();
            if field(&acknowledgement, "seq=") != Some(expected.as_str()) {
                reason = format!("unexpected ack \"{acknowledgement}\"");
                break;
            }
            println!(
                "{VM_TAG}_VIRTIO_NET_HB_TX session={sessions} count={sequence} local={LOCAL_IP} \
                 peer={peer_ip}:{TEST_PORT} uptime={uptime}s ack=\"{acknowledgement}\""
            );
            if !pass_reported && sequence >= pass_after_heartbeats() {
                pass_reported = true;
                println!(
                    "{VM_TAG}_VIRTIO_NET_PASS role=client heartbeats={sequence} \
                     sessions={sessions}"
                );
            }
            if sequence % STATUS_EVERY_HEARTBEATS == 0 {
                println!(
                    "{VM_TAG}_VIRTIO_NET_STATUS role=client session={sessions} \
                     heartbeats={sequence} uptime={uptime}s local={LOCAL_IP} peer={peer_ip} \
                     failed_sessions={failed_sessions} ifaces={}",
                    interface_report()
                );
            }
            thread::sleep(heartbeat_period());
        }

        failed_sessions += 1;
        let _ = stream.shutdown(Shutdown::Both);
        println!(
            "{VM_TAG}_VIRTIO_NET_SESSION_END session={sessions} heartbeats={sequence} \
             reason={reason} (reconnecting in {}ms)",
            RETRY_DELAY_MS
        );
        thread::sleep(retry_delay());
    }
}

/// Connects to `target`, retrying forever: a peer that is not listening yet is
/// an expected state while the guest chain boots, so this never gives up. The
/// first few attempts are logged, later ones only every tenth attempt.
#[cfg(feature = "arceos")]
fn connect_forever(target: (Ipv4Addr, u16), attempts: &mut u64) -> TcpStream {
    let (peer_ip, port) = target;
    let address = SocketAddr::from((peer_ip, port));
    loop {
        match TcpStream::connect_timeout(&address, connect_timeout()) {
            Ok(stream) => return stream,
            Err(error) => {
                *attempts += 1;
                if *attempts <= CONNECT_RETRY_LOG_BURST || *attempts % 10 == 0 {
                    println!(
                        "{VM_TAG}_VIRTIO_NET_CONNECT_RETRY attempt={} target={peer_ip}:{port} \
                         local={LOCAL_IP} error={error}",
                        *attempts
                    );
                }
                thread::sleep(retry_delay());
            }
        }
    }
}

/// Reads one `\n`-terminated line. `Ok(None)` means the peer closed the session
/// before sending anything, which callers treat as a dropped connection.
#[cfg(feature = "arceos")]
fn read_line(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let mut byte = [0u8; 1];
    loop {
        let length = stream.read(&mut byte)?;
        if length == 0 {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        match byte[0] {
            b'\n' => return Ok(Some(line)),
            b'\r' => {}
            other => line.push(other as char),
        }
        if line.len() > MAX_LINE_LEN {
            return Err(std::io::Error::other("peer line exceeded the size limit"));
        }
    }
}

/// Returns the value of a `key=value` token inside a heartbeat or ack line.
#[cfg(feature = "arceos")]
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(key))
}

/// One line summary of every interface known to `ax-net`, including the local
/// IPv4 configuration and the MAC address.
#[cfg(feature = "arceos")]
fn interface_report() -> String {
    let interfaces = ax_net::interfaces();
    let mut report = format!("count={}", interfaces.len());
    for interface in interfaces {
        let mac = interface
            .mac
            .map(|address| address.to_string())
            .unwrap_or_else(|| String::from("none"));
        let ipv4 = interface
            .ipv4
            .map(|config| match config.gateway {
                Some(gateway) => format!("{} gw={}", config.address, gateway),
                None => config.address.to_string(),
            })
            .unwrap_or_else(|| String::from("none"));
        report.push_str(&format!(
            " [name={} id={:?} mtu={} mac={} ipv4={}]",
            interface.name, interface.id, interface.mtu, mac, ipv4
        ));
    }
    report
}

#[cfg(feature = "arceos")]
fn heartbeat_period() -> Duration {
    Duration::from_millis(positive_u64(
        HEARTBEAT_PERIOD_MS,
        DEFAULT_HEARTBEAT_PERIOD_MS,
    ))
}

#[cfg(feature = "arceos")]
fn pass_after_heartbeats() -> u64 {
    positive_u64(PASS_AFTER_HEARTBEATS, DEFAULT_PASS_AFTER_HEARTBEATS)
}

#[cfg(feature = "arceos")]
fn retry_delay() -> Duration {
    Duration::from_millis(RETRY_DELAY_MS)
}

#[cfg(feature = "arceos")]
fn connect_timeout() -> Duration {
    Duration::from_millis(positive_u64(CONNECT_TIMEOUT_MS, DEFAULT_CONNECT_TIMEOUT_MS))
}

#[cfg(feature = "arceos")]
fn connect_start_delay() -> Duration {
    Duration::from_millis(positive_u64(
        CONNECT_START_DELAY_MS,
        DEFAULT_CONNECT_START_DELAY_MS,
    ))
}

/// Parses a compile-time override, falling back to `default` when it is missing
/// or not a positive number.
#[cfg(feature = "arceos")]
fn positive_u64(value: &str, default: u64) -> u64 {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .unwrap_or(default)
}

//! DHCP, DNS, TCP and HTTP acceptance guest for the AxVisor shared uplink.
//!
//! Several VMs run this same binary at once, each behind the AxVisor virtual
//! switch on one host uplink. The interface is located by name (`eth0`) rather
//! than by MAC, so a single build serves every guest MAC. A compile-time
//! `AXVIRTIO_VM_TAG` (default `VM`) prefixes each result line so the dual-guest
//! run can tell the two guests apart (`VM1_DHCP_PASS`, `VM2_DHCP_PASS`, ...),
//! matching the acceptance markers in
//! `AXVIRTIO_NET_MULTI_GUEST_SWITCH_DESIGN.md` §11.3.

#[cfg(feature = "arceos")]
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream, ToSocketAddrs},
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "arceos")]
use ax_std as _;

const VM_TAG: &str = match option_env!("AXVIRTIO_VM_TAG") {
    Some(tag) => tag,
    None => "VM",
};
#[cfg(feature = "arceos")]
const GUEST_IFACE: &str = "eth0";
#[cfg(feature = "arceos")]
const TEST_HOST: &str = env!("AXVIRTIO_TEST_HOST");
#[cfg(feature = "arceos")]
const TEST_PORT: u16 = parse_port(env!("AXVIRTIO_TEST_PORT"));
#[cfg(feature = "arceos")]
const TEST_PATH: &str = env!("AXVIRTIO_TEST_PATH");
#[cfg(feature = "arceos")]
const EXPECTED_TOKEN: &str = env!("AXVIRTIO_EXPECT_TOKEN");
#[cfg(feature = "arceos")]
const LOCAL_ROLE: &str = env!("AXVIRTIO_LOCAL_ROLE");
#[cfg(feature = "arceos")]
const LOCAL_PEER_IPV4: &str = env!("AXVIRTIO_LOCAL_PEER_IPV4");
#[cfg(feature = "arceos")]
const LOCAL_PORT: u16 = parse_port(env!("AXVIRTIO_LOCAL_PORT"));
#[cfg(feature = "arceos")]
const LOCAL_TEST_BYTES: usize = parse_usize(env!("AXVIRTIO_LOCAL_TEST_BYTES"));
#[cfg(feature = "arceos")]
const LOCAL_CHUNK_SIZE: usize = 16 * 1024;
#[cfg(feature = "arceos")]
const LOCAL_PROGRESS_INTERVAL: usize = 256 * 1024;
#[cfg(feature = "arceos")]
const LOCAL_CONNECT_ATTEMPTS: usize = 100;
#[cfg(feature = "arceos")]
const LOCAL_ACK_MAGIC: &[u8; 8] = b"AXVNET01";

#[cfg(feature = "arceos")]
const fn parse_port(value: &str) -> u16 {
    parse_usize(value) as u16
}

#[cfg(feature = "arceos")]
const fn parse_usize(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut number = 0usize;
    while index < bytes.len() {
        let digit = bytes[index] - b'0';
        number = number * 10 + digit as usize;
        index += 1;
    }
    number
}

#[cfg(feature = "arceos")]
fn run() -> std::io::Result<()> {
    // Locate the interface by name so the same binary works for any guest MAC
    // the AxVisor emulated device reports.
    let interface = ax_net::interface_by_name(GUEST_IFACE)
        .ok_or_else(|| std::io::Error::other("eth0 interface was not found"))?;
    let ipv4 = interface
        .ipv4
        .ok_or_else(|| std::io::Error::other("DHCP did not configure IPv4"))?;
    let route = ax_net::default_routes()
        .into_iter()
        .find(|route| route.interface_id == interface.id)
        .ok_or_else(|| std::io::Error::other("DHCP did not install a default route"))?;
    let dns = ax_net::dns_servers();
    let dns_server = dns
        .first()
        .ok_or_else(|| std::io::Error::other("DHCP did not install a DNS server"))?;
    println!(
        "{VM_TAG}_DHCP_PASS {} {:?} {} mac={:?}",
        ipv4.address, route.via, dns_server, interface.mac
    );
    let local_listener = if LOCAL_ROLE == "server" {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, LOCAL_PORT))?;
        println!("{VM_TAG}_LOCAL_LISTEN_PASS 0.0.0.0:{LOCAL_PORT}");
        Some(listener)
    } else {
        None
    };

    let destination = (TEST_HOST, TEST_PORT)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other("DNS returned no address"))?;
    println!("{VM_TAG}_DNS_PASS {TEST_HOST} {}", destination.ip());

    let mut stream = TcpStream::connect(destination)?;
    println!("{VM_TAG}_TCP_PASS {destination}");
    write!(
        stream,
        "GET {TEST_PATH} HTTP/1.1\r\nHost: {TEST_HOST}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = [0u8; 4096];
    let mut length = 0;
    while length < response.len() {
        let received = stream.read(&mut response[length..])?;
        if received == 0 {
            break;
        }
        length += received;
    }
    let response = core::str::from_utf8(&response[..length])
        .map_err(|_| std::io::Error::other("HTTP response was not UTF-8"))?;
    let status = response
        .lines()
        .next()
        .ok_or_else(|| std::io::Error::other("HTTP response had no status line"))?;
    if !status.contains(" 200 ") {
        return Err(std::io::Error::other("HTTP response status was not 200"));
    }
    let (_, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("HTTP response had no header terminator"))?;
    if body.is_empty() {
        return Err(std::io::Error::other("HTTP response body was empty"));
    }
    if !EXPECTED_TOKEN.is_empty() && !body.contains(EXPECTED_TOKEN) {
        return Err(std::io::Error::other(
            "HTTP response did not contain expected token",
        ));
    }
    println!(
        "{VM_TAG}_HTTP_PASS {status} bytes={length} body_bytes={} {EXPECTED_TOKEN}",
        body.len()
    );

    let local_ipv4 = ipv4.address.to_string();
    match LOCAL_ROLE {
        "server" => run_local_server(local_listener.expect("server listener must be bound"))?,
        "client" => run_local_client(resolve_local_peer(&local_ipv4)?)?,
        "disabled" => {}
        _ => return Err(std::io::Error::other("invalid AXVIRTIO_LOCAL_ROLE")),
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn run_local_server(listener: TcpListener) -> std::io::Result<()> {
    let (mut stream, peer) = listener.accept()?;
    let started = Instant::now();
    let (received, checksum) = receive_test_payload(&mut stream)?;
    println!("{VM_TAG}_LOCAL_PAYLOAD_RECEIVED bytes={received}");
    if received != LOCAL_TEST_BYTES {
        return Err(std::io::Error::other("local payload length mismatch"));
    }

    let mut ack = [0u8; 24];
    ack[..8].copy_from_slice(LOCAL_ACK_MAGIC);
    ack[8..16].copy_from_slice(&(received as u64).to_be_bytes());
    ack[16..].copy_from_slice(&checksum.to_be_bytes());
    stream.write_all(&ack)?;
    println!("{VM_TAG}_LOCAL_ACK_SENT");
    let mut confirmation = [0u8; 1];
    stream.read_exact(&mut confirmation)?;
    if confirmation[0] != 1 {
        return Err(std::io::Error::other(
            "local peer returned invalid confirmation",
        ));
    }
    print_local_result(peer.to_string(), received, checksum, started.elapsed());
    stream.write_all(&[1])?;
    println!("{VM_TAG}_LOCAL_COMPLETION_SENT");
    let mut close_confirmation = [0u8; 1];
    stream.read_exact(&mut close_confirmation)?;
    if close_confirmation[0] != 1 {
        return Err(std::io::Error::other(
            "local peer returned invalid close confirmation",
        ));
    }
    println!("{VM_TAG}_LOCAL_CLOSE_CONFIRMED");
    Ok(())
}

#[cfg(feature = "arceos")]
fn run_local_client(peer_ipv4: &str) -> std::io::Result<()> {
    let peer = format!("{peer_ipv4}:{LOCAL_PORT}");
    let mut stream = connect_with_retry(&peer)?;
    let started = Instant::now();
    let checksum = send_test_payload(&mut stream)?;
    println!("{VM_TAG}_LOCAL_PAYLOAD_SENT bytes={LOCAL_TEST_BYTES}");

    let mut ack = [0u8; 24];
    stream.read_exact(&mut ack)?;
    println!("{VM_TAG}_LOCAL_ACK_RECEIVED");
    if &ack[..8] != LOCAL_ACK_MAGIC {
        return Err(std::io::Error::other("local peer returned invalid ACK"));
    }
    let received = u64::from_be_bytes(ack[8..16].try_into().unwrap()) as usize;
    let peer_checksum = u64::from_be_bytes(ack[16..].try_into().unwrap());
    if received != LOCAL_TEST_BYTES || peer_checksum != checksum {
        return Err(std::io::Error::other(
            "local peer byte count or checksum mismatch",
        ));
    }
    stream.write_all(&[1])?;
    println!("{VM_TAG}_LOCAL_CONFIRMATION_SENT");
    let mut completion = [0u8; 1];
    stream.read_exact(&mut completion)?;
    if completion[0] != 1 {
        return Err(std::io::Error::other(
            "local peer returned invalid completion",
        ));
    }
    println!("{VM_TAG}_LOCAL_COMPLETION_RECEIVED");
    stream.write_all(&[1])?;
    println!("{VM_TAG}_LOCAL_CLOSE_CONFIRMATION_SENT");
    print_local_result(peer, received, checksum, started.elapsed());
    Ok(())
}

#[cfg(feature = "arceos")]
fn resolve_local_peer(local_ipv4: &str) -> std::io::Result<&'static str> {
    resolve_local_peer_from(LOCAL_PEER_IPV4, local_ipv4)
}

#[cfg(any(feature = "arceos", test))]
fn resolve_local_peer_from(
    configured_peer: &'static str,
    local_ipv4: &str,
) -> std::io::Result<&'static str> {
    if configured_peer != "auto" {
        return Ok(configured_peer);
    }
    if local_ipv4.starts_with("10.0.2.15/") {
        Ok("10.0.2.16")
    } else if local_ipv4.starts_with("10.0.2.16/") {
        Ok("10.0.2.15")
    } else {
        Err(std::io::Error::other(
            "automatic local peer discovery requires 10.0.2.15/24 or 10.0.2.16/24",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_local_peer_from;

    #[test]
    fn automatic_peer_resolution_handles_either_dhcp_lease_order() {
        assert_eq!(
            resolve_local_peer_from("auto", "10.0.2.15/24").unwrap(),
            "10.0.2.16"
        );
        assert_eq!(
            resolve_local_peer_from("auto", "10.0.2.16/24").unwrap(),
            "10.0.2.15"
        );
        assert!(resolve_local_peer_from("auto", "10.0.2.17/24").is_err());
    }
}

#[cfg(feature = "arceos")]
fn connect_with_retry(peer: &str) -> std::io::Result<TcpStream> {
    let mut last_error = None;
    for _ in 0..LOCAL_CONNECT_ATTEMPTS {
        match TcpStream::connect(peer) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("local connect failed")))
}

#[cfg(feature = "arceos")]
fn send_test_payload(stream: &mut TcpStream) -> std::io::Result<u64> {
    let mut chunk = [0u8; LOCAL_CHUNK_SIZE];
    for (index, byte) in chunk.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let mut remaining = LOCAL_TEST_BYTES;
    let mut sent = 0usize;
    let mut next_progress = LOCAL_PROGRESS_INTERVAL;
    let mut checksum = 0u64;
    while remaining != 0 {
        let length = remaining.min(chunk.len());
        stream.write_all(&chunk[..length])?;
        checksum = checksum.wrapping_add(chunk[..length].iter().map(|byte| u64::from(*byte)).sum());
        remaining -= length;
        sent += length;
        if sent >= next_progress {
            println!("{VM_TAG}_LOCAL_SEND_PROGRESS bytes={sent}");
            next_progress = next_progress.saturating_add(LOCAL_PROGRESS_INTERVAL);
        }
    }
    Ok(checksum)
}

#[cfg(feature = "arceos")]
fn receive_test_payload(stream: &mut TcpStream) -> std::io::Result<(usize, u64)> {
    let mut chunk = [0u8; LOCAL_CHUNK_SIZE];
    let mut received = 0usize;
    let mut next_progress = LOCAL_PROGRESS_INTERVAL;
    let mut checksum = 0u64;
    while received < LOCAL_TEST_BYTES {
        let length =
            stream.read(&mut chunk[..(LOCAL_TEST_BYTES - received).min(LOCAL_CHUNK_SIZE)])?;
        if length == 0 {
            break;
        }
        checksum = checksum.wrapping_add(chunk[..length].iter().map(|byte| u64::from(*byte)).sum());
        received += length;
        if received >= next_progress {
            println!("{VM_TAG}_LOCAL_RECEIVE_PROGRESS bytes={received}");
            next_progress = next_progress.saturating_add(LOCAL_PROGRESS_INTERVAL);
        }
    }
    Ok((received, checksum))
}

#[cfg(feature = "arceos")]
fn print_local_result(peer: String, bytes: usize, checksum: u64, elapsed: Duration) {
    let seconds = elapsed.as_secs_f64();
    let mib_per_second = bytes as f64 / (1024.0 * 1024.0) / seconds;
    println!("{VM_TAG}_LOCAL_SWITCH_PASS peer={peer} bytes={bytes} checksum={checksum}");
    println!(
        "{VM_TAG}_LOCAL_RATE_PASS bytes={bytes} elapsed_ms={} mib_per_second={mib_per_second:.2}",
        elapsed.as_millis()
    );
}

fn main() {
    println!("AxVisor virtio-net shared-uplink acceptance test ({VM_TAG})");
    #[cfg(feature = "arceos")]
    if let Err(error) = run() {
        println!("{VM_TAG}_UPLINK_FAIL {error}");
    }
}

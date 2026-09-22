//! Deterministic two-guest TCP test for AxVisor's internal VirtIO-net switch.

#[cfg(feature = "arceos")]
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "arceos")]
use ax_std as _;

const VM_TAG: &str = match option_env!("AXVIRTIO_VM_TAG") {
    Some(value) => value,
    None => "VM",
};
const ROLE: &str = match option_env!("AXVIRTIO_ROLE") {
    Some(value) => value,
    None => "server",
};
const LOCAL_IP: &str = match option_env!("AXVIRTIO_LOCAL_IP") {
    Some(value) => value,
    None => "10.0.2.15",
};
const PEER_IP: &str = match option_env!("AXVIRTIO_PEER_IP") {
    Some(value) => value,
    None => "10.0.2.16",
};
const TEST_PORT: u16 = 5001;
const PAYLOAD_LEN: usize = 64 * 1024;

const BENCH_MODE: bool = option_env!("AXVIRTIO_BENCH").is_some();
const BENCH_PORT: u16 = 5002;
const BENCH_ROUNDS: usize = 5;
const BENCH_PAYLOAD_BITS: usize = 512_000_000;
const BENCH_PAYLOAD_LEN: usize = BENCH_PAYLOAD_BITS / 8;
const BENCH_ACK_LEN: usize = 17;
const BENCH_HEADER_LEN: usize = 24;
#[cfg(feature = "arceos")]
const BENCH_CONSOLE_ATTACH_DELAY: Duration = Duration::from_millis(800);
#[cfg(feature = "arceos")]
const BENCH_START_DELAY: Duration = Duration::from_millis(300);
#[cfg(feature = "arceos")]
const BENCH_CONSOLE_SWITCH_DELAY: Duration = Duration::from_millis(3_000);
const BENCH_SERVER_IP: &str = "10.0.2.15";
const BENCH_CLIENT_IP: &str = "10.0.2.16";

#[cfg(feature = "arceos")]
fn main() {
    if BENCH_MODE {
        if let Err(error) = run_benchmark() {
            println!("AXVISOR_VIRTIO_NET_BENCH_FAIL {error}");
        }
        thread::sleep(Duration::from_millis(200));
        return;
    }

    if let Err(error) = run() {
        println!("{VM_TAG}_VIRTIO_NET_FAIL {error}");
        return;
    }
    println!("{VM_TAG}_VIRTIO_NET_PASS");
    thread::sleep(Duration::from_millis(200));
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
    ax_net::set_interface_ipv4(interface.id, local_ip, 24)
        .map_err(|error| std::io::Error::other(format!("configure eth0: {error}")))?;
    println!(
        "{VM_TAG}_VIRTIO_NET_READY ip={LOCAL_IP} mac={:?}",
        interface.mac
    );

    match ROLE {
        "server" => run_server(),
        "client" => run_client(),
        _ => Err(std::io::Error::other("invalid AXVIRTIO_ROLE")),
    }
}

#[cfg(feature = "arceos")]
fn run_benchmark() -> std::io::Result<()> {
    let interface = ax_net::interface_by_name("eth0")
        .ok_or_else(|| std::io::Error::other("eth0 was not discovered"))?;
    let mac_last_byte = interface.mac.as_ref().map(|mac| mac.0[5]);
    let role = match ROLE {
        "server" => "server",
        "client" => "client",
        "auto" => match mac_last_byte {
            Some(1) => "server",
            Some(2) => "client",
            _ => {
                return Err(std::io::Error::other(
                    "auto benchmark role requires MAC last byte 1 or 2",
                ));
            }
        },
        _ => return Err(std::io::Error::other("invalid AXVIRTIO_ROLE")),
    };
    let (local_ip, peer_ip) = if ROLE == "auto" {
        match role {
            "server" => (BENCH_SERVER_IP, BENCH_CLIENT_IP),
            "client" => (BENCH_CLIENT_IP, BENCH_SERVER_IP),
            _ => unreachable!("benchmark role is selected above"),
        }
    } else {
        (LOCAL_IP, PEER_IP)
    };
    let local_ip: Ipv4Addr = local_ip
        .parse()
        .map_err(|_| std::io::Error::other("invalid benchmark local IPv4 address"))?;
    let peer_ip: Ipv4Addr = peer_ip
        .parse()
        .map_err(|_| std::io::Error::other("invalid benchmark peer IPv4 address"))?;
    ax_net::set_interface_ipv4(interface.id, local_ip, 24)
        .map_err(|error| std::io::Error::other(format!("configure eth0: {error}")))?;
    let ready = format!(
        "AXVISOR_VIRTIO_NET_BENCH_READY role={role} ip={local_ip} mac={:?}",
        interface.mac
    );

    match role {
        "server" => {
            let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, BENCH_PORT))?;
            println!("{ready}");
            run_benchmark_server(listener)
        }
        "client" => run_benchmark_client(peer_ip, &ready),
        _ => unreachable!("benchmark role is selected above"),
    }
}

#[cfg(feature = "arceos")]
fn run_benchmark_server(listener: TcpListener) -> std::io::Result<()> {
    let (mut stream, _peer) = listener.accept()?;
    let mut samples = [(0usize, 0u128, 0.0f64); BENCH_ROUNDS];
    for (round, sample) in samples.iter_mut().enumerate() {
        let start = Instant::now();
        let (received, checksum) = receive_benchmark_payload(&mut stream, round)?;
        let elapsed_ns = start.elapsed().as_nanos();
        let throughput_mbps = benchmark_throughput_mbps(received, elapsed_ns)?;
        *sample = (received, elapsed_ns, throughput_mbps);
        send_benchmark_ack(&mut stream, received, checksum)?;
    }
    // The server and client rounds complete concurrently. Wait for the client's
    // completion signal so the runner has switched back to this console before
    // the rx samples and result are printed.
    let mut completion = [0u8; 1];
    stream.read_exact(&mut completion)?;
    if completion[0] != 1 {
        return Err(std::io::Error::other("benchmark completion marker missing"));
    }
    let mut throughputs = [0.0f64; BENCH_ROUNDS];
    for (round, (bytes, elapsed_ns, throughput_mbps)) in samples.iter().enumerate() {
        throughputs[round] = *throughput_mbps;
        println!(
            "AXVISOR_VIRTIO_NET_BENCH_SAMPLE direction=rx index={round} bits={} elapsed={} \
             throughput={}",
            human_bits(*bytes),
            human_elapsed(*elapsed_ns),
            human_throughput(*throughput_mbps),
        );
    }
    print_benchmark_result("rx", throughputs)
}

#[cfg(feature = "arceos")]
fn run_benchmark_client(peer_ip: Ipv4Addr, ready: &str) -> std::io::Result<()> {
    let mut stream = connect_with_retry(peer_ip, BENCH_PORT)?;
    // Give the board runner time to attach VM 2 and wait for this marker.
    // The delay only serializes console control and is outside timed rounds.
    thread::sleep(BENCH_CONSOLE_ATTACH_DELAY);
    println!("{ready}");
    // Let the runner process the switch back to VM 1 before the first round.
    thread::sleep(BENCH_START_DELAY);
    let mut samples = [0.0f64; BENCH_ROUNDS];
    for (round, sample) in samples.iter_mut().enumerate() {
        let expected_checksum = benchmark_checksum(round);
        let start = Instant::now();
        send_benchmark_payload(&mut stream, round, expected_checksum)?;
        let mut ack = [0u8; BENCH_ACK_LEN];
        stream.read_exact(&mut ack)?;
        let elapsed_ns = start.elapsed().as_nanos();
        if ack[0] != 1 {
            return Err(std::io::Error::other("benchmark acknowledgement failed"));
        }
        let mut length_bytes = [0u8; 8];
        length_bytes.copy_from_slice(&ack[1..9]);
        let received = u64::from_be_bytes(length_bytes) as usize;
        let mut checksum_bytes = [0u8; 8];
        checksum_bytes.copy_from_slice(&ack[9..17]);
        let received_checksum = u64::from_be_bytes(checksum_bytes);
        if received != BENCH_PAYLOAD_LEN || received_checksum != expected_checksum {
            return Err(std::io::Error::other(
                "benchmark acknowledgement length or checksum mismatch",
            ));
        }
        let throughput_mbps = benchmark_throughput_mbps(received, elapsed_ns)?;
        *sample = throughput_mbps;
        println!(
            "AXVISOR_VIRTIO_NET_BENCH_SAMPLE direction=tx index={round} bits={} elapsed={} \
             throughput={}",
            human_bits(received),
            human_elapsed(elapsed_ns),
            human_throughput(throughput_mbps),
        );
    }
    print_benchmark_result("tx", samples)?;
    // Give the runner time to switch back to the server console before signaling
    // completion; the server prints its rx result only after this signal.
    thread::sleep(BENCH_CONSOLE_SWITCH_DELAY);
    stream.write_all(&[1])
}

#[cfg(feature = "arceos")]
fn receive_benchmark_payload(
    stream: &mut TcpStream,
    expected_round: usize,
) -> std::io::Result<(usize, u64)> {
    let mut header = [0u8; BENCH_HEADER_LEN];
    stream.read_exact(&mut header)?;
    let mut round_bytes = [0u8; 8];
    round_bytes.copy_from_slice(&header[0..8]);
    let round = u64::from_be_bytes(round_bytes) as usize;
    let mut length_bytes = [0u8; 8];
    length_bytes.copy_from_slice(&header[8..16]);
    let length = u64::from_be_bytes(length_bytes) as usize;
    let mut checksum_bytes = [0u8; 8];
    checksum_bytes.copy_from_slice(&header[16..24]);
    let expected_checksum = u64::from_be_bytes(checksum_bytes);
    if round != expected_round
        || length != BENCH_PAYLOAD_LEN
        || expected_checksum != benchmark_checksum(expected_round)
    {
        return Err(std::io::Error::other(
            "benchmark payload header does not match the expected round",
        ));
    }
    let (received, checksum) = receive_payload(stream, length)?;
    if received != length || checksum != expected_checksum {
        return Err(std::io::Error::other(
            "benchmark payload length or checksum mismatch",
        ));
    }
    Ok((received, checksum))
}

#[cfg(feature = "arceos")]
fn send_benchmark_payload(
    stream: &mut TcpStream,
    round: usize,
    checksum: u64,
) -> std::io::Result<()> {
    let mut header = [0u8; BENCH_HEADER_LEN];
    header[0..8].copy_from_slice(&(round as u64).to_be_bytes());
    header[8..16].copy_from_slice(&(BENCH_PAYLOAD_LEN as u64).to_be_bytes());
    header[16..24].copy_from_slice(&checksum.to_be_bytes());
    stream.write_all(&header)?;

    let mut buffer = [0u8; 4096];
    for offset in (0..BENCH_PAYLOAD_LEN).step_by(buffer.len()) {
        let length = (BENCH_PAYLOAD_LEN - offset).min(buffer.len());
        fill_benchmark_buffer(&mut buffer[..length], round, offset);
        stream.write_all(&buffer[..length])?;
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn send_benchmark_ack(stream: &mut TcpStream, length: usize, checksum: u64) -> std::io::Result<()> {
    let mut ack = [0u8; BENCH_ACK_LEN];
    ack[0] = 1;
    ack[1..9].copy_from_slice(&(length as u64).to_be_bytes());
    ack[9..17].copy_from_slice(&checksum.to_be_bytes());
    stream.write_all(&ack)
}

#[cfg(feature = "arceos")]
fn fill_benchmark_buffer(buffer: &mut [u8], round: usize, offset: usize) {
    let round_term = (round as u8).wrapping_mul(17).wrapping_add(3);
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = ((offset + index) as u8)
            .wrapping_mul(31)
            .wrapping_add(7)
            .wrapping_add(round_term);
    }
}

#[cfg(feature = "arceos")]
fn benchmark_checksum(round: usize) -> u64 {
    let mut block = [0u8; 256];
    fill_benchmark_buffer(&mut block, round, 0);
    let block_checksum = block.iter().map(|byte| u64::from(*byte)).sum::<u64>();
    let full_blocks = BENCH_PAYLOAD_LEN / block.len();
    let remainder = BENCH_PAYLOAD_LEN % block.len();
    let mut checksum = block_checksum * full_blocks as u64;
    if remainder != 0 {
        let mut tail = [0u8; 256];
        let tail_start = BENCH_PAYLOAD_LEN - remainder;
        fill_benchmark_buffer(&mut tail[..remainder], round, tail_start);
        checksum += tail[..remainder]
            .iter()
            .map(|byte| u64::from(*byte))
            .sum::<u64>();
    }
    checksum
}

#[cfg(feature = "arceos")]
fn benchmark_throughput_mbps(bytes: usize, elapsed_ns: u128) -> std::io::Result<f64> {
    if elapsed_ns == 0 {
        return Err(std::io::Error::other("benchmark elapsed time is zero"));
    }
    let seconds = elapsed_ns as f64 / 1e9;
    let mbps = (bytes as f64 * 8.0) / seconds / 1e6;
    if mbps.is_finite() && mbps > 0.0 {
        Ok(mbps)
    } else {
        Err(std::io::Error::other(
            "benchmark throughput is not a positive finite value",
        ))
    }
}

#[cfg(feature = "arceos")]
fn trim_fraction(mut text: String) -> String {
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

#[cfg(feature = "arceos")]
fn human_bits(bytes: usize) -> String {
    let bits = bytes as f64 * 8.0;
    if bits >= 1e9 {
        trim_fraction(format!("{:.3}Gb", bits / 1e9))
    } else if bits >= 1e6 {
        trim_fraction(format!("{:.3}Mb", bits / 1e6))
    } else if bits >= 1e3 {
        trim_fraction(format!("{:.3}Kb", bits / 1e3))
    } else {
        trim_fraction(format!("{:.3}b", bits))
    }
}

#[cfg(feature = "arceos")]
fn human_elapsed(elapsed_ns: u128) -> String {
    let nanos = elapsed_ns as f64;
    if nanos >= 1e9 {
        trim_fraction(format!("{:.3}s", nanos / 1e9))
    } else if nanos >= 1e6 {
        trim_fraction(format!("{:.3}ms", nanos / 1e6))
    } else if nanos >= 1e3 {
        trim_fraction(format!("{:.3}us", nanos / 1e3))
    } else {
        format!("{elapsed_ns}ns")
    }
}

#[cfg(feature = "arceos")]
fn human_throughput(mbps: f64) -> String {
    if mbps >= 1000.0 {
        trim_fraction(format!("{:.3}Gbps", mbps / 1000.0))
    } else if mbps >= 1.0 {
        trim_fraction(format!("{:.3}Mbps", mbps))
    } else {
        trim_fraction(format!("{:.3}Kbps", mbps * 1000.0))
    }
}

#[cfg(feature = "arceos")]
fn print_benchmark_result(direction: &str, samples: [f64; BENCH_ROUNDS]) -> std::io::Result<()> {
    let average = samples.iter().sum::<f64>() / BENCH_ROUNDS as f64;
    if !average.is_finite() || average <= 0.0 {
        return Err(std::io::Error::other(
            "benchmark average throughput is not a positive finite value",
        ));
    }
    println!(
        "AXVISOR_VIRTIO_NET_BENCH_RESULT=PASS direction={direction} avg={} samples={BENCH_ROUNDS} \
         bits={} rounds={BENCH_ROUNDS}",
        human_throughput(average),
        human_bits(BENCH_PAYLOAD_LEN),
    );
    Ok(())
}

#[cfg(feature = "arceos")]
fn run_server() -> std::io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, TEST_PORT))?;
    let (mut stream, peer) = listener.accept()?;
    let (received, checksum) = receive_payload(&mut stream, PAYLOAD_LEN)?;
    if received != PAYLOAD_LEN {
        return Err(std::io::Error::other("payload length mismatch"));
    }
    stream.write_all(&(received as u64).to_be_bytes())?;
    stream.write_all(&checksum.to_be_bytes())?;
    let mut completion = [0u8; 1];
    stream.read_exact(&mut completion)?;
    if completion[0] != 1 {
        return Err(std::io::Error::other("invalid client completion marker"));
    }
    println!("{VM_TAG}_VIRTIO_NET_RX bytes={received} checksum={checksum:#x} peer={peer}");
    Ok(())
}

#[cfg(feature = "arceos")]
fn run_client() -> std::io::Result<()> {
    let peer_ip: Ipv4Addr = PEER_IP
        .parse()
        .map_err(|_| std::io::Error::other("invalid peer IPv4 address"))?;
    let mut stream = connect_with_retry(peer_ip, TEST_PORT)?;
    let checksum = send_payload(&mut stream)?;
    let mut reply = [0u8; 16];
    stream.read_exact(&mut reply)?;
    let received = u64::from_be_bytes(reply[0..8].try_into().unwrap()) as usize;
    let peer_checksum = u64::from_be_bytes(reply[8..16].try_into().unwrap());
    if received != PAYLOAD_LEN || peer_checksum != checksum {
        return Err(std::io::Error::other(
            "peer checksum acknowledgement mismatch",
        ));
    }
    stream.write_all(&[1])?;
    println!("{VM_TAG}_VIRTIO_NET_TX bytes={received} checksum={checksum:#x}");
    Ok(())
}

#[cfg(feature = "arceos")]
fn connect_with_retry(peer_ip: Ipv4Addr, port: u16) -> std::io::Result<TcpStream> {
    let mut last_error = None;
    for _ in 0..100 {
        match TcpStream::connect((peer_ip, port)) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("connection retry exhausted")))
}

#[cfg(feature = "arceos")]
fn send_payload(stream: &mut TcpStream) -> std::io::Result<u64> {
    let mut checksum = 0u64;
    let mut buffer = [0u8; 1024];
    for offset in (0..PAYLOAD_LEN).step_by(buffer.len()) {
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = ((offset + index) as u8).wrapping_mul(31).wrapping_add(7);
            checksum = checksum.wrapping_add(u64::from(*byte));
        }
        stream.write_all(&buffer)?;
    }
    Ok(checksum)
}

#[cfg(feature = "arceos")]
fn receive_payload(stream: &mut TcpStream, payload_len: usize) -> std::io::Result<(usize, u64)> {
    let mut received = 0usize;
    let mut checksum = 0u64;
    let mut buffer = [0u8; 1024];
    while received < payload_len {
        let read_len = (payload_len - received).min(buffer.len());
        let length = stream.read(&mut buffer[..read_len])?;
        if length == 0 {
            break;
        }
        checksum = buffer[..length]
            .iter()
            .fold(checksum, |sum, byte| sum.wrapping_add(u64::from(*byte)));
        received += length;
    }
    Ok((received, checksum))
}

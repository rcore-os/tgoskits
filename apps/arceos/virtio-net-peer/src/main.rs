//! Deterministic two-guest TCP test for AxVisor's internal VirtIO-net switch.

#[cfg(feature = "arceos")]
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    thread,
    time::Duration,
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

#[cfg(feature = "arceos")]
fn main() {
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
fn run_server() -> std::io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, TEST_PORT))?;
    let (mut stream, peer) = listener.accept()?;
    let (received, checksum) = receive_payload(&mut stream)?;
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
    let mut stream = connect_with_retry(peer_ip)?;
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
fn connect_with_retry(peer_ip: Ipv4Addr) -> std::io::Result<TcpStream> {
    let mut last_error = None;
    for _ in 0..100 {
        match TcpStream::connect((peer_ip, TEST_PORT)) {
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
fn receive_payload(stream: &mut TcpStream) -> std::io::Result<(usize, u64)> {
    let mut received = 0usize;
    let mut checksum = 0u64;
    let mut buffer = [0u8; 1024];
    while received < PAYLOAD_LEN {
        let read_len = (PAYLOAD_LEN - received).min(buffer.len());
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

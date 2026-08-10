//! Task-2 controller/managed UDP endpoint.
//!
//! The endpoint intentionally uses a nonblocking socket so retransmission and
//! heartbeat timers keep progressing while the peer is silent. Role and static
//! addresses are compile-time inputs supplied by the ArceOS build config; the
//! host build uses the same state machine for deterministic local testing.

use core::net::{Ipv4Addr, SocketAddr};
#[cfg(not(feature = "arceos"))]
use std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "arceos")]
use ax_std::{
    net::UdpSocket,
    thread,
    time::{Duration, Instant},
};
use task2_net_protocol::{
    ControlAction, ControlMessage, Endpoint, EndpointState, MAX_DATAGRAM_LEN, MessageKind,
    PollEvent, ReceiveEvent, RetryPolicy, SessionId, StatusMessage, StatusState,
};

const LOCAL_PORT: u16 = 4242;
const SESSION_ID: SessionId = SessionId::new(0x5452_5432);
// Device discovery and first ARP resolution happen after the endpoint starts.
// Keep the initial reliable exchange alive long enough for that real link
// setup; otherwise a healthy Guest pair can exhaust retries before the first
// packet reaches the peer.
const POLICY: RetryPolicy = match RetryPolicy::new(500, 5, 200, 5_000) {
    Ok(policy) => policy,
    Err(_) => panic!("task-2 protocol policy constants must be valid"),
};

const ROLE: &str = match option_env!("TASK2_ROLE") {
    Some(role) => role,
    None => "managed",
};
const LOCAL_IP: &str = match option_env!("TASK2_LOCAL_IP") {
    Some(ip) => ip,
    None => "10.0.42.2",
};
const PEER_IP: &str = match option_env!("TASK2_PEER_IP") {
    Some(ip) => ip,
    None => "10.0.42.1",
};
// Presence of this build-time variable enables the legacy raw UDP probe. It
// is intentionally absent from P2/P3 builds so those runs contain only T2N1
// frames; keeping the switch at compile time prevents a runtime test mode
// from accidentally changing the protocol evidence.
const SEND_P1_PROBE: bool = option_env!("TASK2_SEND_P1_PROBE").is_some();

fn main() {
    if let Err(message) = run() {
        println!("TASK2_ERROR={message}");
    }
}

fn run() -> Result<(), &'static str> {
    let local_ip = parse_ipv4(LOCAL_IP).ok_or("TASK2_LOCAL_IP is invalid")?;
    let peer_ip = parse_ipv4(PEER_IP).ok_or("TASK2_PEER_IP is invalid")?;
    configure_network(local_ip)?;

    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, LOCAL_PORT)))
        .map_err(|_| "failed to bind UDP socket")?;
    let peer = SocketAddr::from((peer_ip, LOCAL_PORT));
    let start = Instant::now();
    let mut endpoint = Endpoint::new(SESSION_ID, POLICY, 0);
    let mut inbound = [0; MAX_DATAGRAM_LEN];
    let mut response = [0; MAX_DATAGRAM_LEN];
    let mut outbound = [0; MAX_DATAGRAM_LEN];

    println!("TASK2_READY role={ROLE} local={LOCAL_IP}:{LOCAL_PORT} peer={PEER_IP}:{LOCAL_PORT}");
    if ROLE == "controller" {
        send_control(&socket, &peer, &mut endpoint, &mut outbound, now_ms(&start))?;
        if SEND_P1_PROBE {
            let probe_len = socket
                .send_to(b"TASK2_P1_PROBE", peer)
                .map_err(|_| "failed to send P1 UDP probe")?;
            println!("TASK2_P1_PROBE_SENT bytes={probe_len}");
        }
        flush_network();
        #[cfg(feature = "arceos")]
        for stats in ax_net::net_dev_stats() {
            println!(
                "TASK2_NET_STATS name={} tx_packets={} tx_errors={} tx_dropped={} rx_packets={}",
                stats.name, stats.tx_packets, stats.tx_errors, stats.tx_dropped, stats.rx_packets
            );
        }
    }
    socket
        .set_nonblocking(true)
        .map_err(|_| "failed to enable nonblocking UDP mode")?;

    loop {
        let now = now_ms(&start);
        match socket.recv_from(&mut inbound) {
            Ok((length, source)) => {
                let state_before_receive = endpoint.state();
                let result = endpoint
                    .receive(&inbound[..length], now, &mut response)
                    .map_err(|_| "protocol receive failed")?;
                if result.response_len > 0 {
                    if let Err(error) = socket.send_to(&response[..result.response_len], source) {
                        println!("TASK2_SEND_ERROR kind=response error={error:?}");
                        return Err("failed to send protocol response");
                    }
                    flush_network();
                }
                handle_receive_event(
                    result.event,
                    &socket,
                    &peer,
                    &mut endpoint,
                    &mut outbound,
                    now,
                )?;
                if state_before_receive == EndpointState::Safe
                    && endpoint.state() == EndpointState::Active
                {
                    println!("TASK2_RECOVERED state=Active elapsed_ms={now}");
                }
            }
            Err(error) if is_would_block(&error) => {}
            Err(_) => return Err("UDP receive failed"),
        }

        let poll = endpoint
            .poll(now, &mut outbound)
            .map_err(|_| "protocol timer failed")?;
        if poll.datagram_len > 0 {
            if let Err(error) = socket.send_to(&outbound[..poll.datagram_len], peer) {
                println!("TASK2_SEND_ERROR kind=timer error={error:?}");
                return Err("failed to send protocol timer frame");
            }
            flush_network();
        }
        if let PollEvent::Retransmit { sequence, attempt } = poll.event {
            println!(
                "TASK2_RETRANSMIT seq={} attempt={}",
                sequence.get(),
                attempt
            );
        }
        if matches!(
            poll.event,
            PollEvent::RetryExhausted { .. } | PollEvent::HeartbeatTimeout
        ) {
            println!(
                "TASK2_SAFE state={:?} event={:?}",
                endpoint.state(),
                poll.event
            );
        }
        #[cfg(feature = "arceos")]
        ax_net::request_poll();
        thread::sleep(Duration::from_millis(10));
    }
}

fn send_control(
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    outbound: &mut [u8; MAX_DATAGRAM_LEN],
    now_ms: u64,
) -> Result<(), &'static str> {
    let mut payload = [0; 12];
    let command = ControlMessage::new(ControlAction::SetOutput, 100, 1)
        .map_err(|_| "invalid built-in control command")?;
    let payload_len = command
        .encode(&mut payload)
        .map_err(|_| "failed to encode control command")?;
    let transmission = endpoint
        .queue_reliable(
            MessageKind::Control,
            &payload[..payload_len],
            now_ms,
            outbound,
        )
        .map_err(|_| "failed to queue control command")?;
    if let Err(error) = socket.send_to(&outbound[..transmission.datagram_len()], peer) {
        println!("TASK2_SEND_ERROR kind=control error={error:?}");
        return Err("failed to send control command");
    }
    flush_network();
    println!(
        "TASK2_CONTROL_SENT seq={} request=1",
        transmission.sequence().get()
    );
    Ok(())
}

fn handle_receive_event(
    event: ReceiveEvent<'_>,
    socket: &UdpSocket,
    peer: &SocketAddr,
    endpoint: &mut Endpoint,
    outbound: &mut [u8; MAX_DATAGRAM_LEN],
    now_ms: u64,
) -> Result<(), &'static str> {
    match event {
        ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Control => {
            let command = ControlMessage::decode(frame.payload())
                .map_err(|_| "validated control payload failed to decode")?;
            println!(
                "TASK2_CONTROL_RECEIVED seq={} request={} action={:?} value={}",
                frame.sequence().get(),
                command.request_id(),
                command.action(),
                command.value()
            );
            let mut status_payload = [0; 12];
            let status = StatusMessage::new(
                if command.action() == ControlAction::Stop {
                    StatusState::Stopped
                } else {
                    StatusState::Active
                },
                0,
                command.value(),
                command.request_id(),
            )
            .map_err(|_| "invalid status state")?;
            let status_len = status
                .encode(&mut status_payload)
                .map_err(|_| "failed to encode status")?;
            let transmission = endpoint
                .queue_reliable(
                    MessageKind::Status,
                    &status_payload[..status_len],
                    now_ms,
                    outbound,
                )
                .map_err(|_| "failed to queue status")?;
            if let Err(error) = socket.send_to(&outbound[..transmission.datagram_len()], peer) {
                println!("TASK2_SEND_ERROR kind=status error={error:?}");
                return Err("failed to send status");
            }
            flush_network();
            println!("TASK2_STATUS_SENT seq={}", transmission.sequence().get());
        }
        ReceiveEvent::Delivered { frame } if frame.kind() == MessageKind::Status => {
            let status = StatusMessage::decode(frame.payload())
                .map_err(|_| "validated status payload failed to decode")?;
            println!(
                "TASK2_STATUS_RECEIVED seq={} state={:?} request={}",
                frame.sequence().get(),
                status.state(),
                status.last_control_request()
            );
        }
        ReceiveEvent::Acknowledged { sequence } => {
            println!("TASK2_ACK seq={}", sequence.get());
        }
        ReceiveEvent::DuplicateAcknowledgement { sequence } => {
            println!("TASK2_DUPLICATE_ACK seq={}", sequence.get());
        }
        ReceiveEvent::InvalidPayload { error } => {
            println!("TASK2_PROTOCOL_ERROR invalid_payload={error}");
        }
        ReceiveEvent::OutOfOrder { sequence, expected } => {
            println!(
                "TASK2_PROTOCOL_ERROR out_of_order={} expected={}",
                sequence.get(),
                expected.get()
            );
        }
        ReceiveEvent::RemoteError { code, sequence } => {
            println!(
                "TASK2_REMOTE_ERROR code={code:?} sequence={}",
                sequence.get()
            );
        }
        ReceiveEvent::Heartbeat { message } => {
            println!(
                "TASK2_HEARTBEAT_RECEIVED peer_uptime_ms={}",
                message.uptime_ms()
            );
        }
        ReceiveEvent::Duplicate { sequence } => {
            println!("TASK2_DUPLICATE seq={}", sequence.get());
        }
        ReceiveEvent::Rejected { error } => println!("TASK2_REJECTED error={error}"),
        ReceiveEvent::SessionMismatch => println!("TASK2_REJECTED session_mismatch"),
        ReceiveEvent::Delivered { .. } => {}
    }
    Ok(())
}

#[inline]
fn flush_network() {
    #[cfg(feature = "arceos")]
    {
        ax_net::flush_egress();
        thread::yield_now();
    }
}

#[cfg(feature = "arceos")]
fn configure_network(local_ip: Ipv4Addr) -> Result<(), &'static str> {
    if option_env!("TASK2_USE_DHCP").is_some() {
        println!("TASK2_NET_DHCP mode=dhcp requested_ip={local_ip}");
        return Ok(());
    }
    let interface = ax_net::interfaces()
        .into_iter()
        .find(|interface| matches!(interface.kind, ax_net::InterfaceKind::Ethernet))
        .ok_or("no Ethernet interface discovered")?;
    if let Some(current) = ax_net::ipv4_config(&interface.name) {
        if current.address.address().octets() == local_ip.octets()
            && current.address.prefix_len() == 24
        {
            println!(
                "TASK2_NET_CONFIGURED interface={} ip={local_ip}/24",
                interface.name
            );
            return Ok(());
        }
        ax_net::remove_interface_ipv4(
            interface.id,
            current.address.address(),
            current.address.prefix_len(),
        )
        .map_err(|_| "failed to remove dynamic IPv4")?;
    }
    ax_net::set_interface_ipv4(interface.id, local_ip, 24)
        .map_err(|_| "failed to configure static IPv4")?;
    println!(
        "TASK2_NET_CONFIGURED interface={} ip={local_ip}/24",
        interface.name
    );
    Ok(())
}

#[cfg(not(feature = "arceos"))]
fn configure_network(_local_ip: Ipv4Addr) -> Result<(), &'static str> {
    Ok(())
}

fn now_ms(start: &Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn parse_ipv4(value: &str) -> Option<Ipv4Addr> {
    let mut octets = [0; 4];
    let mut index = 0;
    let mut current = 0u16;
    let mut has_digit = false;
    for byte in value.bytes() {
        match byte {
            b'0'..=b'9' => {
                has_digit = true;
                current = current
                    .checked_mul(10)?
                    .checked_add(u16::from(byte - b'0'))?;
                if current > 255 {
                    return None;
                }
            }
            b'.' if has_digit && index < 3 => {
                octets[index] = current as u8;
                index += 1;
                current = 0;
                has_digit = false;
            }
            _ => return None,
        }
    }
    if !has_digit || index != 3 || current > 255 {
        return None;
    }
    octets[3] = current as u8;
    Some(Ipv4Addr::from(octets))
}

#[cfg(feature = "arceos")]
fn is_would_block(error: &ax_errno::AxError) -> bool {
    *error == ax_errno::AxError::WouldBlock
}

#[cfg(not(feature = "arceos"))]
fn is_would_block(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

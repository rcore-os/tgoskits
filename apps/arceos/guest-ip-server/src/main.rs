//! Minimal RTOS endpoint for the Starry/Linux guest IP protocol.

#[cfg(feature = "arceos")]
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
};

#[cfg(feature = "arceos")]
use ax_std as _;
#[cfg(feature = "arceos")]
use guest_ip_protocol::{
    ErrorCode, HEADER_LEN, Header, MAX_PAYLOAD, MessageType, ReceiveSequence, ReliableSession,
    RetryPolicy, decode_frame, encode_frame,
};

#[cfg(feature = "arceos")]
const LISTEN_PORT: u16 = 4242;
#[cfg(feature = "arceos")]
const CONTROL_PAYLOAD_LEN: usize = 8;

#[cfg(feature = "arceos")]
fn main() {
    if let Err(error) = run() {
        println!("GIPC_RTOS_ERROR {error}");
        return;
    }
    println!("GIPC_RTOS_READY");
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
fn run() -> std::io::Result<()> {
    let interface = ax_std::net::interface_by_name("eth0")
        .ok_or_else(|| std::io::Error::other("eth0 was not discovered"))?;
    ax_std::net::set_interface_ipv4(interface.id, Ipv4Addr::new(10, 0, 42, 2), 24)
        .map_err(|error| std::io::Error::other(format!("configure eth0: {error}")))?;

    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, LISTEN_PORT))?;
    println!("GIPC_RTOS_LISTEN ip=10.0.42.2 port={LISTEN_PORT}");
    let (mut stream, peer) = listener.accept()?;
    serve_connection(&mut stream)?;
    println!("GIPC_RTOS_PEER {peer}");
    Ok(())
}

#[cfg(feature = "arceos")]
fn serve_connection(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut frame = [0u8; HEADER_LEN + MAX_PAYLOAD];
    let policy =
        RetryPolicy::new(1000, 3).ok_or_else(|| std::io::Error::other("invalid retry policy"))?;
    let mut session = ReliableSession::new(policy);
    loop {
        let header = read_header(stream, &mut frame)?;
        let payload_len = header.payload_len as usize;
        stream.read_exact(&mut frame[HEADER_LEN..HEADER_LEN + payload_len])?;
        let frame_len = HEADER_LEN + payload_len;
        let (decoded, payload) = decode_frame(&frame[..frame_len])
            .map_err(|error| std::io::Error::other(format!("decode frame: {error}")))?;
        match session.observe(decoded.sequence) {
            ReceiveSequence::Duplicate => {
                if decoded.message_type == MessageType::Control {
                    send_status(stream, decoded.sequence, payload)?;
                }
                continue;
            }
            ReceiveSequence::OutOfOrder => {
                send_error(stream, decoded.sequence, ErrorCode::InvalidSequence)?;
                continue;
            }
            ReceiveSequence::New => {}
        }
        match decoded.message_type {
            MessageType::Hello | MessageType::Heartbeat => {
                send_status(stream, decoded.sequence, payload)?;
            }
            MessageType::Control => {
                if payload.len() != CONTROL_PAYLOAD_LEN {
                    send_error(stream, decoded.sequence, ErrorCode::InvalidPayload)?;
                } else {
                    send_status(stream, decoded.sequence, payload)?;
                }
            }
            MessageType::Ack => {}
            MessageType::Status | MessageType::Error => {
                send_error(stream, decoded.sequence, ErrorCode::UnsupportedMessage)?;
            }
        }
    }
}

#[cfg(feature = "arceos")]
fn read_header(stream: &mut TcpStream, frame: &mut [u8]) -> std::io::Result<Header> {
    stream.read_exact(&mut frame[..HEADER_LEN])?;
    guest_ip_protocol::decode_header(&frame[..HEADER_LEN])
        .map_err(|error| std::io::Error::other(format!("invalid frame header: {error}")))
}

#[cfg(feature = "arceos")]
fn send_status(stream: &mut TcpStream, sequence: u32, payload: &[u8]) -> std::io::Result<()> {
    let header = Header::new(
        MessageType::Status,
        0,
        payload.len(),
        sequence,
        0,
        ErrorCode::None,
    )
    .ok_or_else(|| std::io::Error::other("status payload too large"))?;
    let mut output = [0u8; HEADER_LEN + MAX_PAYLOAD];
    let length = encode_frame(header, payload, &mut output)
        .map_err(|error| std::io::Error::other(format!("encode status: {error}")))?;
    stream.write_all(&output[..length])
}

#[cfg(feature = "arceos")]
fn send_error(stream: &mut TcpStream, sequence: u32, error: ErrorCode) -> std::io::Result<()> {
    let header = Header::new(MessageType::Error, 0, 0, sequence, 0, error)
        .ok_or_else(|| std::io::Error::other("error payload too large"))?;
    let mut output = [0u8; HEADER_LEN + MAX_PAYLOAD];
    let length = encode_frame(header, &[], &mut output)
        .map_err(|encode_error| std::io::Error::other(format!("encode error: {encode_error}")))?;
    stream.write_all(&output[..length])
}

extern crate alloc;

use alloc::{format, string::String};
use core::{net::Ipv4Addr, str, time::Duration};

use uefi::{
    Status,
    boot::{self, OpenProtocolAttributes, OpenProtocolParams},
    proto::network::{
        ip4config2::Ip4Config2,
        snp::{NetworkState, ReceiveFlags, SimpleNetwork},
    },
};

use crate::boards;

const DISCOVERY_PORT: u16 = 2998;
const IPV4_ETHERTYPE: u16 = 0x0800;
const IPV4_HEADER_LEN: usize = 20;
const UDP_HEADER_LEN: usize = 8;
const ETHERNET_HEADER_LEN: usize = 14;
const DISCOVERY_PROTOCOL_VERSION: u16 = 1;
const DISCOVERY_ADVERTISE_TYPE: &str = "ostool_httpboot_advertise";
const DISCOVERY_SOLICIT_TYPE: &str = "ostool_httpboot_solicit";
const DISCOVERY_ROUNDS: usize = 5;
const DISCOVERY_STALL: Duration = Duration::from_millis(500);
const RAW_PACKET_LIMIT: usize = 1536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    NoSimpleNetwork,
    NoIpv4Config,
    OpenFailed,
    StartFailed,
    SendFailed,
    NoAdvertise,
    InvalidIpv4Config,
    SnpInitFailed,
}

pub fn discover_server(mac: &str) -> Result<String, DiscoveryError> {
    match discover_server_with_snp(mac) {
        Ok(url) => return Ok(url),
        Err(err) => crate::logln!("discovery_snp_error: {err:?}"),
    }
    Err(DiscoveryError::NoAdvertise)
}

fn discover_server_with_snp(mac: &str) -> Result<String, DiscoveryError> {
    let (station_ip, subnet_mask) = ipv4_config().ok_or(DiscoveryError::NoIpv4Config)?;
    crate::logln!("discovery_snp_ipv4: ip={station_ip} mask={subnet_mask}");
    if station_ip == Ipv4Addr::UNSPECIFIED {
        return Err(DiscoveryError::InvalidIpv4Config);
    }

    let handles =
        boot::find_handles::<SimpleNetwork>().map_err(|_| DiscoveryError::NoSimpleNetwork)?;
    let solicit = solicit_body(mac);
    for handle in handles.iter().copied() {
        let snp = match unsafe {
            boot::open_protocol::<SimpleNetwork>(
                OpenProtocolParams {
                    handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            Ok(snp) => snp,
            Err(_) => continue,
        };
        match discover_with_snp(&snp, station_ip, subnet_mask, solicit.as_bytes()) {
            Ok(url) => return Ok(url),
            Err(err) => crate::logln!("discovery_snp_handle_error: {err:?}"),
        }
    }

    Err(DiscoveryError::OpenFailed)
}

fn discover_with_snp(
    snp: &SimpleNetwork,
    station_ip: Ipv4Addr,
    subnet_mask: Ipv4Addr,
    solicit: &[u8],
) -> Result<String, DiscoveryError> {
    ensure_snp_initialized(snp)?;
    let source_mac = snp.mode().current_address.octets();
    let source_mac = first_six(&source_mac);
    let broadcast_ip = subnet_broadcast(station_ip, subnet_mask);
    let mut frame = [0u8; RAW_PACKET_LIMIT];
    let frame_len = write_udp_broadcast_frame(
        &mut frame,
        source_mac,
        station_ip,
        broadcast_ip,
        DISCOVERY_PORT,
        DISCOVERY_PORT,
        solicit,
    )
    .ok_or(DiscoveryError::SendFailed)?;
    snp.transmit(0, &frame[..frame_len], None, None, Some(IPV4_ETHERTYPE))
        .map_err(|_| DiscoveryError::SendFailed)?;
    crate::logln!("discovery_snp_solicit_sent: dst={broadcast_ip}:{DISCOVERY_PORT}");

    let mut receive = [0u8; RAW_PACKET_LIMIT];
    for round in 1..=DISCOVERY_ROUNDS {
        crate::logln!("discovery_snp_wait: {round}/{DISCOVERY_ROUNDS}");
        for _ in 0..8 {
            match snp.receive(&mut receive, None, None, None, None) {
                Ok(len) => {
                    if let Some(body) = udp_payload_from_frame(&receive[..len], DISCOVERY_PORT)
                        && let Ok(text) = str::from_utf8(body)
                    {
                        if let Some(url) = parse_advertise(text) {
                            crate::logln!("discovery_snp_advertise: base_url={url}");
                            return Ok(url.into());
                        }
                    }
                }
                Err(err)
                    if err.status() == Status::NOT_READY || err.status() == Status::TIMEOUT => {}
                Err(_) => {}
            }
            boot::stall(Duration::from_millis(50));
        }
        boot::stall(DISCOVERY_STALL);
    }

    Err(DiscoveryError::NoAdvertise)
}

fn ipv4_config() -> Option<(Ipv4Addr, Ipv4Addr)> {
    let handles = boot::find_handles::<Ip4Config2>().ok()?;
    for handle in handles.iter().copied() {
        let mut protocol = unsafe {
            boot::open_protocol::<Ip4Config2>(
                OpenProtocolParams {
                    handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        }
        .ok()?;
        if protocol.ifup().is_err() {
            continue;
        }
        let info = protocol.get_interface_info().ok()?;
        let station = Ipv4Addr::from(info.station_addr.octets());
        let mask = Ipv4Addr::from(info.subnet_mask.octets());
        return Some((station, mask));
    }
    None
}

fn ensure_snp_initialized(snp: &SimpleNetwork) -> Result<(), DiscoveryError> {
    match snp.mode().state {
        NetworkState::STOPPED => {
            snp.start().map_err(|_| DiscoveryError::StartFailed)?;
            snp.initialize(0, 0)
                .map_err(|_| DiscoveryError::SnpInitFailed)?;
        }
        NetworkState::STARTED => {
            snp.initialize(0, 0)
                .map_err(|_| DiscoveryError::SnpInitFailed)?;
        }
        NetworkState::INITIALIZED => {}
        _ => return Err(DiscoveryError::SnpInitFailed),
    }
    let _ = snp.receive_filters(
        ReceiveFlags::UNICAST | ReceiveFlags::BROADCAST,
        ReceiveFlags::empty(),
        false,
        None,
    );
    Ok(())
}

fn solicit_body(mac: &str) -> String {
    format!(
        concat!(
            "{{\"type\":\"{}\",",
            "\"version\":{},",
            "\"arch\":\"{}\",",
            "\"board\":\"{}\",",
            "\"mac\":\"{}\",",
            "\"nonce\":\"{}\"}}"
        ),
        DISCOVERY_SOLICIT_TYPE,
        DISCOVERY_PROTOCOL_VERSION,
        boards::active::ARCH_NAME,
        boards::active::BOARD_NAME,
        mac,
        boards::active::BOARD_NAME
    )
}

fn write_udp_broadcast_frame(
    frame: &mut [u8],
    source_mac: [u8; 6],
    source_ip: Ipv4Addr,
    dest_ip: Ipv4Addr,
    source_port: u16,
    dest_port: u16,
    payload: &[u8],
) -> Option<usize> {
    let total_len = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + UDP_HEADER_LEN + payload.len();
    if frame.len() < total_len || payload.len() > u16::MAX as usize - UDP_HEADER_LEN {
        return None;
    }

    frame[..6].fill(0xff);
    frame[6..12].copy_from_slice(&source_mac);
    frame[12..14].copy_from_slice(&IPV4_ETHERTYPE.to_be_bytes());

    let ip_offset = ETHERNET_HEADER_LEN;
    let udp_offset = ip_offset + IPV4_HEADER_LEN;
    let ip_total_len = (IPV4_HEADER_LEN + UDP_HEADER_LEN + payload.len()) as u16;
    frame[ip_offset] = 0x45;
    frame[ip_offset + 1] = 0;
    frame[ip_offset + 2..ip_offset + 4].copy_from_slice(&ip_total_len.to_be_bytes());
    frame[ip_offset + 4..ip_offset + 6].copy_from_slice(&0u16.to_be_bytes());
    frame[ip_offset + 6..ip_offset + 8].copy_from_slice(&0u16.to_be_bytes());
    frame[ip_offset + 8] = 64;
    frame[ip_offset + 9] = 17;
    frame[ip_offset + 10..ip_offset + 12].fill(0);
    frame[ip_offset + 12..ip_offset + 16].copy_from_slice(&source_ip.octets());
    frame[ip_offset + 16..ip_offset + 20].copy_from_slice(&dest_ip.octets());
    let checksum = internet_checksum(&frame[ip_offset..ip_offset + IPV4_HEADER_LEN]);
    frame[ip_offset + 10..ip_offset + 12].copy_from_slice(&checksum.to_be_bytes());

    let udp_len = (UDP_HEADER_LEN + payload.len()) as u16;
    frame[udp_offset..udp_offset + 2].copy_from_slice(&source_port.to_be_bytes());
    frame[udp_offset + 2..udp_offset + 4].copy_from_slice(&dest_port.to_be_bytes());
    frame[udp_offset + 4..udp_offset + 6].copy_from_slice(&udp_len.to_be_bytes());
    frame[udp_offset + 6..udp_offset + 8].fill(0);
    frame[udp_offset + UDP_HEADER_LEN..total_len].copy_from_slice(payload);
    Some(total_len)
}

fn udp_payload_from_frame(frame: &[u8], expected_dest_port: u16) -> Option<&[u8]> {
    if frame.len() < ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + UDP_HEADER_LEN {
        return None;
    }
    if u16::from_be_bytes([frame[12], frame[13]]) != IPV4_ETHERTYPE {
        return None;
    }
    let ip_offset = ETHERNET_HEADER_LEN;
    let version_ihl = frame[ip_offset];
    if version_ihl >> 4 != 4 {
        return None;
    }
    let ihl = ((version_ihl & 0x0f) as usize) * 4;
    if ihl < IPV4_HEADER_LEN || frame.len() < ETHERNET_HEADER_LEN + ihl + UDP_HEADER_LEN {
        return None;
    }
    if frame[ip_offset + 9] != 17 {
        return None;
    }
    let ip_total_len = u16::from_be_bytes([frame[ip_offset + 2], frame[ip_offset + 3]]) as usize;
    if ip_total_len < ihl + UDP_HEADER_LEN || frame.len() < ETHERNET_HEADER_LEN + ip_total_len {
        return None;
    }
    let udp_offset = ip_offset + ihl;
    let dest_port = u16::from_be_bytes([frame[udp_offset + 2], frame[udp_offset + 3]]);
    if dest_port != expected_dest_port {
        return None;
    }
    let udp_len = u16::from_be_bytes([frame[udp_offset + 4], frame[udp_offset + 5]]) as usize;
    if udp_len < UDP_HEADER_LEN || udp_offset + udp_len > ETHERNET_HEADER_LEN + ip_total_len {
        return None;
    }
    Some(&frame[udp_offset + UDP_HEADER_LEN..udp_offset + udp_len])
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&byte) = chunks.remainder().first() {
        sum += (byte as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn subnet_broadcast(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(ip) | !u32::from(mask))
}

fn first_six(bytes: &[u8; 32]) -> [u8; 6] {
    [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]]
}

fn parse_advertise(input: &str) -> Option<&str> {
    if json_string_field(input, "type")? != DISCOVERY_ADVERTISE_TYPE {
        return None;
    }
    if json_u16_field(input, "version")? != DISCOVERY_PROTOCOL_VERSION {
        return None;
    }
    json_string_field(input, "base_url").map(trim_trailing_slash)
}

fn trim_trailing_slash(input: &str) -> &str {
    input.strip_suffix('/').unwrap_or(input)
}

fn json_string_field<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    parse_json_string(field_value(input, key)?)
}

fn json_u16_field(input: &str, key: &str) -> Option<u16> {
    let value = field_value(input, key)?;
    let end = value
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(value.len());
    value.get(..end)?.parse().ok()
}

fn field_value<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let key_start = input.find(&pattern)?;
    let after_key = input.get(key_start + pattern.len()..)?;
    let colon = after_key.find(':')?;
    Some(after_key.get(colon + 1..)?.trim_start())
}

fn parse_json_string(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => return None,
            b'"' => return input.get(1..index),
            _ => index += 1,
        }
    }
    None
}

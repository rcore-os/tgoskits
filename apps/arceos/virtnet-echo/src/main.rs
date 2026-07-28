//! ArceOS guest smoke app for the AxVisor virtio-net UDP echo flow.
//!
//! Probes the virtio-mmio net device (enumerated from the guest DTB), verifies the
//! fixed IPv4 assigned during network initialization, sends a UDP
//! datagram with a unique token to the peer, and prints `UDP_ECHO_PASS <token>`
//! when the echoed payload, source address and length all match.
//!
//! The peer/echo constants mirror `os/axvisor/src/virtio_net/config.rs`.

#[cfg(feature = "arceos")]
use ax_std as _;

/// Fixed peer (the deterministic virtual echo node).
#[cfg(feature = "arceos")]
const PEER_IP: [u8; 4] = [10, 0, 0, 1];
#[cfg(feature = "arceos")]
const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
/// UDP port the echo peer answers on.
#[cfg(feature = "arceos")]
const ECHO_PORT: u16 = 4433;
/// Unique payload echoed back by the peer.
#[cfg(feature = "arceos")]
const TOKEN: &[u8] = b"VIRTIO_NET_ECHO_OK_123456";

#[cfg(feature = "arceos")]
fn arceos_main() {
    use ax_std::net::{Ipv4Addr, SocketAddr, UdpSocket};

    let peer_ip = Ipv4Addr::from(PEER_IP);

    match ax_net::interfaces()
        .into_iter()
        .find(|iface| iface.mac.is_some_and(|mac| mac.0 == GUEST_MAC))
    {
        Some(iface) if iface.ipv4.is_some() => {
            println!(
                "[virtnet-echo] {} configured as {:?}",
                iface.name, iface.ipv4
            )
        }
        Some(iface) => {
            println!("[virtnet-echo] {} has no IPv4 configuration", iface.name);
            return;
        }
        None => {
            println!("[virtnet-echo] expected virtio-net MAC was not found");
            return;
        }
    }

    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(sock) => sock,
        Err(err) => {
            println!("[virtnet-echo] UDP bind failed: {err:?}");
            return;
        }
    };

    let peer = SocketAddr::new(peer_ip.into(), ECHO_PORT);
    println!(
        "[virtnet-echo] sending {} byte token to {peer}",
        TOKEN.len()
    );
    if let Err(err) = sock.send_to(TOKEN, peer) {
        println!("[virtnet-echo] send_to failed: {err:?}");
        return;
    }

    // Blocking receive: the echo arrives within one round trip when the data
    // path is healthy. ax_std does not expose a UDP read timeout, so a broken
    // path hangs here (the run harness kills it).
    let mut buf = [0u8; 64];
    match sock.recv_from(&mut buf) {
        Ok((len, src)) => {
            let token_str = core::str::from_utf8(TOKEN).unwrap_or("");
            if len == TOKEN.len() && &buf[..len] == TOKEN && src.ip() == peer.ip() {
                println!("UDP_ECHO_PASS {token_str}");
            } else {
                println!(
                    "UDP_ECHO_MISMATCH len={len} src={src:?} payload={:?}",
                    core::str::from_utf8(&buf[..len]).ok()
                );
            }
        }
        Err(err) => println!("[virtnet-echo] recv_from failed: {err:?}"),
    }
}

fn main() {
    println!("ArceOS virtio-net UDP echo smoke test");
    #[cfg(feature = "arceos")]
    arceos_main();
}

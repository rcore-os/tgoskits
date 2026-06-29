// SPDX-License-Identifier: Apache-2.0
// Copyright 2025 KylinSoft Co., Ltd. <https://www.kylinos.cn/>
// See LICENSES for license details.

//! Raw IP socket implementation for ICMP-style traffic.
//!
//! Raw sockets expose packet-oriented access above IP and below TCP/UDP. They
//! are primarily used by ICMP/ICMPv6 tests and tools, but still share the same
//! global smoltcp `SocketSet`, route selection, device binding, and readiness
//! model as UDP/TCP sockets.
//!
//! # Packet Format
//!
//! smoltcp raw sockets receive complete IP packets. The public raw socket API
//! returns protocol payloads for normal IPv4/IPv6 raw sockets while preserving
//! enough packet context for peer filtering and `MSG_PEEK`. Deferred packets
//! must therefore be stored in a consistent wire-packet form until delivery is
//! decided.
//!
//! # Loopback And Peer Filtering
//!
//! Loopback ICMP-style traffic may be delivered through a local fast path. For
//! connected raw sockets, packets from other peers can be skipped or deferred
//! without corrupting the smoltcp receive queue format.
//!
//! # Locking
//!
//! Raw sockets keep their small deferred-packet slots behind IRQ-off spin locks
//! because packet delivery may be inspected while the net poll worker is
//! servicing device-originated receive work. These locks are only held around
//! `Option<Vec<u8>>` swaps and never across route lookup, smoltcp polling, or
//! userspace buffer I/O.

use alloc::vec;
use core::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError, ax_bail};
use ax_io::prelude::*;
use ax_kspin::{SpinNoIrq as Mutex, SpinRwLock as RwLock};
use axpoll::{IoEvents, Pollable};
pub use smoltcp::wire::{IpProtocol, IpVersion};
use smoltcp::{
    iface::SocketHandle,
    socket::raw as smol,
    storage::PacketMetadata,
    wire::{Icmpv6Packet, IpAddress, IpListenEndpoint, Ipv4Packet, Ipv4Repr, Ipv6Packet, Ipv6Repr},
};

use crate::{
    RecvFlags, RecvOptions, SOCKET_SET, SendFlags, SendOptions, Shutdown, SocketAddrEx, SocketOps,
    config::{DeviceBinding, InterfaceId},
    consts::{RAW_RX_BUF_LEN, RAW_TX_BUF_LEN},
    general::GeneralOptions,
    get_control, interface_by_id,
    ip_tos::apply_ip_tos,
    options::{Configurable, GetSocketOption, SetSocketOption},
    request_poll,
};

enum RawIpHeader {
    Ipv4(Ipv4Repr),
    Ipv6(Ipv6Repr),
}

impl RawIpHeader {
    fn buffer_len(&self) -> usize {
        match self {
            Self::Ipv4(header) => header.buffer_len(),
            Self::Ipv6(header) => header.buffer_len(),
        }
    }

    fn emit(&self, buf: &mut [u8]) {
        match self {
            Self::Ipv4(header) => header.emit(
                &mut Ipv4Packet::new_unchecked(buf),
                &smoltcp::phy::ChecksumCapabilities::ignored(),
            ),
            Self::Ipv6(header) => header.emit(&mut Ipv6Packet::new_unchecked(buf)),
        }
    }
}

/// A raw IP socket used for ICMP and ICMPv6 traffic.
pub struct RawSocket {
    /// Handle into the global smoltcp socket set.
    handle: SocketHandle,
    /// IP version accepted by this socket.
    ip_version: IpVersion,
    /// Optional local address filter.
    local_addr: RwLock<Option<IpAddress>>,
    /// Optional connected peer filter.
    peer_addr: RwLock<Option<IpAddress>>,
    /// Locally generated loopback packet waiting to be received.
    loopback_rx: Mutex<Option<(IpAddress, vec::Vec<u8>)>>,
    /// Non-peer packet held after filtering without corrupting wire format.
    deferred_rx: Mutex<Option<(IpAddress, vec::Vec<u8>)>>,
    /// Optional outgoing TTL/hop-limit override.
    ttl: RwLock<Option<u8>>,
    /// Public read-half closed state.
    rx_closed: AtomicBool,
    /// Public write-half closed state.
    tx_closed: AtomicBool,
    /// Shared socket options and blocking helpers.
    general: GeneralOptions,
}

impl RawSocket {
    /// Creates a raw socket for the given IP version and protocol.
    pub fn new(ip_version: IpVersion, ip_protocol: IpProtocol) -> Self {
        let general = GeneralOptions::new(3, 2, u8::from(ip_protocol) as i32); // SOCK_RAW
        general.set_device_binding(DeviceBinding::default());
        Self {
            handle: SOCKET_SET.add(smol::Socket::new(
                Some(ip_version),
                Some(ip_protocol),
                smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; RAW_RX_BUF_LEN]),
                smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; RAW_TX_BUF_LEN]),
            )),
            ip_version,
            local_addr: RwLock::new(None),
            peer_addr: RwLock::new(None),
            loopback_rx: Mutex::new(None),
            deferred_rx: Mutex::new(None),
            ttl: RwLock::new(None),
            rx_closed: AtomicBool::new(false),
            tx_closed: AtomicBool::new(false),
            general,
        }
    }

    /// Restricts this socket to one interface for route selection.
    pub fn bind_device(&self, interface_id: InterfaceId) -> AxResult {
        if interface_by_id(interface_id).is_none() {
            return Err(AxError::NoSuchDevice);
        }
        self.general.set_device_binding(DeviceBinding {
            bound_if: Some(interface_id),
        });
        Ok(())
    }

    /// Borrows the underlying smoltcp raw socket by handle.
    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }

    fn outgoing_ip_header(
        &self,
        local: IpAddress,
        remote: IpAddress,
        next_header: IpProtocol,
        payload_len: usize,
        hop_limit: u8,
    ) -> RawIpHeader {
        match (self.ip_version, local, remote) {
            (IpVersion::Ipv4, IpAddress::Ipv4(src_addr), IpAddress::Ipv4(dst_addr)) => {
                RawIpHeader::Ipv4(Ipv4Repr {
                    src_addr,
                    dst_addr,
                    next_header,
                    payload_len,
                    hop_limit,
                })
            }
            (IpVersion::Ipv6, IpAddress::Ipv6(src_addr), IpAddress::Ipv6(dst_addr)) => {
                RawIpHeader::Ipv6(Ipv6Repr {
                    src_addr,
                    dst_addr,
                    next_header,
                    payload_len,
                    hop_limit,
                })
            }
            _ => unreachable!(),
        }
    }

    /// Validates that an address belongs to this socket's IP version.
    fn check_ip_version(&self, addr: IpAddress) -> AxResult<IpAddress> {
        match (self.ip_version, addr) {
            (IpVersion::Ipv4, IpAddress::Ipv4(_)) | (IpVersion::Ipv6, IpAddress::Ipv6(_)) => {
                Ok(addr)
            }
            _ => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
        }
    }

    /// Resolves the per-call or connected remote address.
    fn remote_address(&self, options: &SendOptions) -> AxResult<IpAddress> {
        match &options.to {
            Some(addr) => {
                let remote = addr.clone().into_ip()?;
                self.check_ip_version(remote.ip().into())
            }
            None => (*self.peer_addr.read()).ok_or(AxError::NotConnected),
        }
    }

    /// Selects the local source address used for an outgoing raw packet.
    fn local_address_for(&self, remote: IpAddress) -> AxResult<IpAddress> {
        if let Some(local) = *self.local_addr.read() {
            return Ok(local);
        }
        if is_loopback_address(remote) {
            return Ok(remote);
        }
        Ok(get_control()
            .select_route_with_binding(&remote, self.general.device_binding())?
            .source)
    }

    /// Splits a complete IP packet into source and bytes returned to userspace.
    ///
    /// Linux raw IPv4 receive returns the IP header plus payload, while raw IPv6
    /// receive returns only the transport payload. The returned slice preserves
    /// that ABI difference.
    fn split_packet_for_delivery<'a>(&self, packet: &'a [u8]) -> AxResult<(IpAddress, &'a [u8])> {
        match self.ip_version {
            IpVersion::Ipv4 => {
                let packet = Ipv4Packet::new_checked(packet)
                    .map_err(|_| AxError::from(LinuxError::EINVAL))?;
                Ok((IpAddress::Ipv4(packet.src_addr()), packet.into_inner()))
            }
            IpVersion::Ipv6 => {
                let packet = Ipv6Packet::new_checked(packet)
                    .map_err(|_| AxError::from(LinuxError::EINVAL))?;
                Ok((IpAddress::Ipv6(packet.src_addr()), packet.payload()))
            }
        }
    }

    /// Returns whether a received source passes the connected-peer filter.
    fn source_matches_peer(&self, source: IpAddress) -> bool {
        self.peer_addr.read().is_none_or(|peer| source == peer)
    }

    /// Delivers one parsed raw packet to the caller's receive buffer.
    fn deliver_packet(
        &self,
        source: IpAddress,
        packet: &[u8],
        dst: &mut (impl Write + IoBufMut),
        options: &mut RecvOptions<'_>,
    ) -> AxResult<usize> {
        if let Some(from) = options.from.as_deref_mut() {
            *from = SocketAddrEx::Ip(SocketAddr::new(source.into(), 0));
        }

        let written = dst.write(packet)?;
        Ok(if options.flags.contains(RecvFlags::TRUNCATE) {
            packet.len()
        } else {
            written
        })
    }
}

fn is_loopback_address(addr: IpAddress) -> bool {
    match addr {
        IpAddress::Ipv4(addr) => addr.is_loopback(),
        IpAddress::Ipv6(addr) => addr.is_loopback(),
    }
}

fn icmp_checksum(packet: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = packet.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&byte) = chunks.remainder().first() {
        sum += u16::from_be_bytes([byte, 0]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn build_loopback_icmp_reply(packet: &[u8]) -> Option<vec::Vec<u8>> {
    if packet.len() < 8 || packet[0] != 8 || packet[1] != 0 {
        return None;
    }

    let mut reply = packet.to_vec();
    reply[0] = 0;
    reply[2] = 0;
    reply[3] = 0;
    let checksum = icmp_checksum(&reply);
    reply[2..4].copy_from_slice(&checksum.to_be_bytes());
    Some(reply)
}

impl Configurable for RawSocket {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> AxResult<bool> {
        use GetSocketOption as O;

        if self.general.get_option_inner(option)? {
            return Ok(true);
        }

        match option {
            O::Ttl(ttl) => {
                **ttl = (*self.ttl.read()).unwrap_or(64);
            }
            O::SendBuffer(size) => {
                **size = RAW_TX_BUF_LEN;
            }
            O::ReceiveBuffer(size) => {
                **size = RAW_RX_BUF_LEN;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_option_inner(&self, option: SetSocketOption) -> AxResult<bool> {
        use SetSocketOption as O;

        if self.general.set_option_inner(option)? {
            return Ok(true);
        }

        match option {
            O::Ttl(ttl) => {
                if *ttl == 0 {
                    return Err(AxError::InvalidInput);
                }
                *self.ttl.write() = Some(*ttl);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl SocketOps for RawSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        let local_addr = local_addr.into_ip()?;
        let local = self.check_ip_version(local_addr.ip().into())?;
        *self.local_addr.write() = Some(local);
        let binding = if local.is_unspecified() {
            DeviceBinding::default()
        } else {
            get_control().local_binding_for(&IpListenEndpoint {
                addr: Some(local),
                port: 0,
            })?
        };
        self.general.set_device_binding(binding);
        Ok(())
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_ip()?;
        let remote = self.check_ip_version(remote_addr.ip().into())?;
        if self.local_addr.read().is_none() {
            *self.local_addr.write() = Some(
                get_control()
                    .select_route_with_binding(&remote, self.general.device_binding())?
                    .source,
            );
        }
        *self.peer_addr.write() = Some(remote);
        let local = (*self.local_addr.read()).expect("raw socket local address");
        self.general
            .set_device_binding(get_control().local_binding_for(&IpListenEndpoint {
                addr: Some(local),
                port: 0,
            })?);
        Ok(())
    }

    fn send(&self, mut src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        // TODO: MSG_DONTROUTE should bypass the routing table for this datagram.
        if options.flags.contains(SendFlags::OOB) {
            ax_bail!(OperationNotSupported);
        }
        if self.tx_closed.load(Ordering::Acquire) {
            return Err(AxError::BrokenPipe);
        }

        let remote = self.remote_address(&options)?;
        let local = self.local_address_for(remote)?;
        let payload_len = src.remaining();
        let extra_nb = options.flags.contains(crate::SendFlags::DONTWAIT);
        let loopback_ipv4 = self.ip_version == IpVersion::Ipv4 && is_loopback_address(remote);

        self.general.send_poller_with(self, extra_nb, || {
            request_poll();
            let written = self.with_smol_socket(|socket| {
                if !socket.can_send() {
                    return Err(AxError::WouldBlock);
                }
                let next_header = socket.ip_protocol().expect("raw socket protocol");
                let hop_limit = (*self.ttl.read()).unwrap_or(64);

                let header =
                    self.outgoing_ip_header(local, remote, next_header, payload_len, hop_limit);
                let header_len = header.buffer_len();

                let buf = socket
                    .send(header_len + payload_len)
                    .map_err(|_| AxError::WouldBlock)?;
                header.emit(&mut *buf);
                let ip_tos = self.general.ip_tos();
                if ip_tos != 0 {
                    apply_ip_tos(buf, ip_tos);
                }

                let written = src.read(&mut buf[header_len..])?;
                if next_header == IpProtocol::Icmpv6 {
                    let (IpAddress::Ipv6(src_addr), IpAddress::Ipv6(dst_addr)) = (local, remote)
                    else {
                        unreachable!();
                    };
                    Icmpv6Packet::new_unchecked(&mut buf[header_len..])
                        .fill_checksum(&src_addr, &dst_addr);
                }
                if let Some(reply) = loopback_ipv4
                    .then(|| build_loopback_icmp_reply(&buf[header_len..header_len + written]))
                    .flatten()
                {
                    *self.loopback_rx.lock() = Some((local, reply));
                }
                Ok(written)
            })?;
            request_poll();
            Ok(written)
        })
    }

    fn recv(&self, mut dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize> {
        if self.rx_closed.load(Ordering::Acquire) {
            return Err(AxError::NotConnected);
        }
        let extra_nb = options.flags.contains(RecvFlags::DONTWAIT);
        let mut options = options;

        self.general.recv_poller_with(self, extra_nb, || {
            request_poll();
            self.with_smol_socket(|socket| {
                if let Some((source, packet)) = if options.flags.contains(RecvFlags::PEEK) {
                    self.deferred_rx.lock().clone()
                } else {
                    self.deferred_rx.lock().take()
                } {
                    if !self.source_matches_peer(source) {
                        *self.deferred_rx.lock() = Some((source, packet));
                        return Err(AxError::WouldBlock);
                    }
                    let (_, payload) = self.split_packet_for_delivery(&packet)?;
                    return self.deliver_packet(source, payload, &mut dst, &mut options);
                }

                if let Some((source, packet)) = if options.flags.contains(RecvFlags::PEEK) {
                    self.loopback_rx.lock().clone()
                } else {
                    self.loopback_rx.lock().take()
                } {
                    if !self.source_matches_peer(source) {
                        *self.loopback_rx.lock() = Some((source, packet));
                        return Err(AxError::WouldBlock);
                    }
                    return self.deliver_packet(source, &packet, &mut dst, &mut options);
                }

                let wire_packet = if options.flags.contains(RecvFlags::PEEK) {
                    let packet = socket.peek().map_err(|_| AxError::WouldBlock)?;
                    let (source, _) = self.split_packet_for_delivery(packet)?;
                    if let Some(peer) = *self.peer_addr.read()
                        && source != peer
                    {
                        return Err(AxError::WouldBlock);
                    }
                    packet
                } else {
                    socket.recv().map_err(|_| AxError::WouldBlock)?
                };
                let (source, packet) = self.split_packet_for_delivery(wire_packet)?;

                if !self.source_matches_peer(source) {
                    *self.deferred_rx.lock() = Some((source, wire_packet.to_vec()));
                    return Err(AxError::WouldBlock);
                }

                self.deliver_packet(source, packet, &mut dst, &mut options)
            })
        })
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        let local = (*self.local_addr.read()).unwrap_or(match self.ip_version {
            IpVersion::Ipv4 => IpAddress::Ipv4(Ipv4Addr::UNSPECIFIED),
            IpVersion::Ipv6 => IpAddress::Ipv6(Ipv6Addr::UNSPECIFIED),
        });
        Ok(SocketAddrEx::Ip(SocketAddr::new(local.into(), 0)))
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        let peer = (*self.peer_addr.read()).ok_or(AxError::NotConnected)?;
        Ok(SocketAddrEx::Ip(SocketAddr::new(peer.into(), 0)))
    }

    fn shutdown(&self, how: Shutdown) -> AxResult {
        if how.has_read() {
            self.rx_closed.store(true, Ordering::Release);
        }
        if how.has_write() {
            self.tx_closed.store(true, Ordering::Release);
        }
        Ok(())
    }
}

impl Pollable for RawSocket {
    fn poll(&self) -> IoEvents {
        request_poll();
        let mut events = IoEvents::empty();
        self.with_smol_socket(|socket| {
            events.set(
                IoEvents::IN,
                !self.rx_closed.load(Ordering::Acquire) && socket.can_recv(),
            );
            events.set(
                IoEvents::OUT,
                !self.tx_closed.load(Ordering::Acquire) && socket.can_send(),
            );
        });
        events.set(
            IoEvents::IN,
            events.contains(IoEvents::IN)
                || self
                    .loopback_rx
                    .lock()
                    .as_ref()
                    .is_some_and(|(source, _)| self.source_matches_peer(*source))
                || self
                    .deferred_rx
                    .lock()
                    .as_ref()
                    .is_some_and(|(source, _)| self.source_matches_peer(*source)),
        );
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.with_smol_socket(|socket| {
            if events.contains(IoEvents::IN) {
                socket.register_recv_waker(context.waker());
            }
            if events.contains(IoEvents::OUT) {
                socket.register_send_waker(context.waker());
            }
        });
        if events.intersects(IoEvents::IN | IoEvents::OUT) {
            self.general.register_waker(context.waker());
        }
    }
}

impl Drop for RawSocket {
    fn drop(&mut self) {
        self.shutdown(Shutdown::Both).ok();
        SOCKET_SET.remove(self.handle);
    }
}

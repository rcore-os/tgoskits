//! UDP socket implementation.
//!
//! UDP sockets wrap smoltcp datagram sockets with POSIX-style bind/connect,
//! per-address port ownership, connected-peer filtering, route-aware source
//! selection, and MSG_MORE corking for datagram coalescing.
//!
//! # Bind And Routing Semantics
//!
//! Public binds are checked through `SocketSetWrapper` so wildcard and specific
//! address conflicts match Linux expectations. When a socket is connected or
//! sends to a destination, the control plane selects the source address and
//! device binding from the route table unless the socket was explicitly bound
//! to a concrete local address/interface.
//!
//! # Datagram Semantics
//!
//! smoltcp stores UDP payload plus metadata, while the POSIX surface exposes
//! per-call source addresses, `MSG_TRUNC`, `MSG_PEEK`, `MSG_DONTWAIT`, and
//! `MSG_MORE`. This module is responsible for preserving message boundaries and
//! for filtering connected sockets to their expected peer.
//!
//! # Polling
//!
//! UDP send/recv operations request the shared net-poll worker after socket
//! state changes. They do not run the interface poll loop directly.

use alloc::{vec, vec::Vec};
use core::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    task::Context,
};

use ax_errno::{AxError, AxResult, ax_bail, ax_err_type};
use ax_io::prelude::*;
use ax_sync::Mutex;
use axpoll::{IoEvents, Pollable};
use smoltcp::{
    iface::SocketHandle,
    phy::PacketMeta,
    socket::udp::{self as smol, UdpMetadata},
    storage::PacketMetadata,
    wire::{IpAddress, IpEndpoint, IpListenEndpoint},
};
use spin::RwLock;

use crate::{
    RecvFlags, RecvOptions, SOCKET_SET, SendFlags, SendOptions, Shutdown, SocketAddrEx, SocketOps,
    addr::allocate_ephemeral_port,
    config::{DeviceBinding, InterfaceId},
    consts::{UDP_RX_BUF_LEN, UDP_TX_BUF_LEN},
    general::GeneralOptions,
    get_control, interface_by_id,
    options::{Configurable, GetSocketOption, SetSocketOption},
    request_poll,
};

/// Buffered state for MSG_MORE corking: captures the target endpoint
/// and source address at the first MSG_MORE call so the merged datagram
/// is always delivered to the correct peer regardless of subsequent
/// calls' addresses.
struct CorkState {
    buf: Vec<u8>,
    remote: IpEndpoint,
    source: IpAddress,
}

/// A UDP socket that provides POSIX-like APIs.
pub struct UdpSocket {
    /// Handle into the global smoltcp socket set.
    handle: SocketHandle,
    /// Bound local endpoint as exposed by POSIX socket calls.
    local_addr: RwLock<Option<IpEndpoint>>,
    /// Connected remote endpoint plus selected source address.
    peer_addr: RwLock<Option<(IpEndpoint, IpAddress)>>,

    /// Shared socket options and blocking helpers.
    general: GeneralOptions,
    /// MSG_MORE corking state: captures endpoint at first MSG_MORE
    /// so the merged datagram always goes to the correct peer.
    cork: Mutex<Option<CorkState>>,
}

impl UdpSocket {
    /// Creates a new UDP socket.
    pub fn new() -> Self {
        Self {
            handle: SOCKET_SET.add(smol::Socket::new(
                smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; UDP_RX_BUF_LEN]),
                smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; UDP_TX_BUF_LEN]),
            )),
            local_addr: RwLock::new(None),
            peer_addr: RwLock::new(None),

            general: GeneralOptions::new(2, 2, 17), // SOCK_DGRAM
            cork: Mutex::new(None),
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

    /// Borrows the underlying smoltcp UDP socket by handle.
    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }

    /// Returns the connected peer and cached source address.
    fn remote_endpoint(&self) -> AxResult<(IpEndpoint, IpAddress)> {
        match self.peer_addr.try_read() {
            Some(addr) => addr.ok_or(AxError::NotConnected),
            None => Err(AxError::NotConnected),
        }
    }

    /// Selects the source address used to reach `remote`.
    fn source_for_remote(&self, remote: &IpAddress) -> AxResult<IpAddress> {
        Ok(get_control()
            .select_route_with_binding(remote, self.general.device_binding())?
            .source)
    }

    fn send_source_for_remote(&self, remote: &IpAddress) -> AxResult<IpAddress> {
        if let Some(local_ep) = *self.local_addr.read()
            && !local_ep.addr.is_unspecified()
        {
            Ok(local_ep.addr)
        } else {
            self.source_for_remote(remote)
        }
    }

    fn source_and_binding_update_for_remote(
        &self,
        remote: &IpAddress,
    ) -> AxResult<(IpAddress, bool)> {
        if let Some(local_ep) = *self.local_addr.read()
            && !local_ep.addr.is_unspecified()
        {
            Ok((local_ep.addr, false))
        } else {
            Ok((self.source_for_remote(remote)?, true))
        }
    }
}

impl Configurable for UdpSocket {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> AxResult<bool> {
        use GetSocketOption as O;

        if self.general.get_option_inner(option)? {
            return Ok(true);
        }
        match option {
            O::Ttl(ttl) => {
                self.with_smol_socket(|socket| {
                    **ttl = socket.hop_limit().unwrap_or(64);
                });
            }
            O::SendBuffer(size) => {
                **size = UDP_TX_BUF_LEN;
            }
            O::ReceiveBuffer(size) => {
                **size = UDP_RX_BUF_LEN;
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
                self.with_smol_socket(|socket| {
                    socket.set_hop_limit(Some(*ttl));
                });
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
impl SocketOps for UdpSocket {
    /// Binds the UDP socket and records public port ownership.
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        let mut local_addr = local_addr.into_ip()?;
        let mut guard = self.local_addr.write();

        if local_addr.port() == 0 {
            local_addr.set_port(get_ephemeral_port()?);
        }
        if guard.is_some() {
            ax_bail!(InvalidInput, "already bound");
        }

        let local_endpoint = IpEndpoint::from(local_addr);
        let endpoint = IpListenEndpoint {
            addr: (!local_endpoint.addr.is_unspecified()).then_some(local_endpoint.addr),
            port: local_endpoint.port,
        };
        let binding = get_control().local_binding_for(&endpoint)?;

        self.with_smol_socket(|socket| {
            socket.bind(endpoint).map_err(|e| match e {
                smol::BindError::InvalidState => ax_err_type!(InvalidInput, "already bound"),
                smol::BindError::Unaddressable => ax_err_type!(ConnectionRefused, "unaddressable"),
            })
        })?;
        if !self.general.reuse_address()
            && let Err(err) =
                SOCKET_SET.udp_bind(self.handle, local_endpoint.addr, local_endpoint.port)
        {
            self.with_smol_socket(|socket| socket.close());
            return Err(err);
        }
        if binding.bound_if.is_some() {
            self.general.set_device_binding(binding);
        }

        *guard = Some(local_endpoint);
        info!("UDP socket {}: bound on {}", self.handle, endpoint);
        Ok(())
    }

    /// Stores a default peer and source address for connected UDP semantics.
    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_ip()?;
        let mut guard = self.peer_addr.write();

        if self.local_addr.read().is_none() {
            self.bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))?;
        }

        let remote_addr = IpEndpoint::from(remote_addr);
        let local_port = self.local_addr.read().map_or(0, |endpoint| endpoint.port);
        let (src, should_update_binding) =
            self.source_and_binding_update_for_remote(&remote_addr.addr)?;

        *guard = Some((remote_addr, src));

        if should_update_binding {
            self.general
                .set_device_binding(get_control().local_binding_for(&IpListenEndpoint {
                    addr: Some(src),
                    port: local_port,
                })?);
        }

        debug!("UDP socket {}: connected to {}", self.handle, remote_addr);
        Ok(())
    }

    /// Sends one datagram, or appends to/flushed a MSG_MORE corked datagram.
    fn send(&self, mut src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        // MSG_OOB is only valid on stream sockets (SOCK_STREAM), not DGRAM.
        if options.flags.contains(SendFlags::OOB) {
            ax_bail!(OperationNotSupported);
        }

        if self.local_addr.read().is_none() {
            self.bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))?;
        }
        // MSG_MORE corking: buffer data instead of sending immediately.
        // Cap corked data to the UDP TX buffer size to prevent
        // unbounded kernel memory allocation from user input.
        const CORK_MAX: usize = UDP_TX_BUF_LEN;
        let more = options.flags.contains(SendFlags::MORE);

        if more {
            let (remote_addr, source_addr) = match options.to {
                Some(addr) => {
                    let addr = IpEndpoint::from(addr.into_ip()?);
                    let src = self.send_source_for_remote(&addr.addr)?;
                    (addr, src)
                }
                None => match self.remote_endpoint() {
                    Ok((endpoint, src)) => (endpoint, src),
                    Err(_) => ax_bail!(DestAddrRequired),
                },
            };
            if remote_addr.port == 0 || remote_addr.addr.is_unspecified() {
                ax_bail!(InvalidInput, "invalid address");
            }
            let len = src.remaining();
            if len > CORK_MAX {
                ax_bail!(MessageTooLong);
            }
            let mut tmp = alloc::vec![0u8; len];
            let read = src.read(&mut tmp)?;
            let mut cork = self.cork.lock();
            if cork.is_none() {
                *cork = Some(CorkState {
                    buf: tmp[..read].to_vec(),
                    remote: remote_addr,
                    source: source_addr,
                });
            } else {
                let prev = cork.as_ref().unwrap().buf.len();
                let new_len = prev.checked_add(read).ok_or(AxError::MessageTooLong)?;
                if new_len > CORK_MAX {
                    ax_bail!(MessageTooLong);
                }
                cork.as_mut().unwrap().buf.extend_from_slice(&tmp[..read]);
            }
            return Ok(read);
        }

        // Resolve destination for direct send or cork flush.
        // None means unconnected socket without explicit destination;
        // the poller closure checks cork before demanding an address.
        let resolved = match options.to {
            Some(addr) => {
                let addr = IpEndpoint::from(addr.into_ip()?);
                let src = self.send_source_for_remote(&addr.addr)?;
                Some((addr, src))
            }
            None => self.remote_endpoint().ok(),
        };

        let extra_nb = options.flags.contains(SendFlags::DONTWAIT);
        self.general.send_poller_with(self, extra_nb, || {
            request_poll();
            let mut cork_guard = self.cork.lock();
            // When flushing corked data, always use the endpoint captured
            // at the first MSG_MORE call (matching Linux semantics).
            let (endpoint, local_addr, payload_len) = if let Some(ref c) = *cork_guard {
                let total = c
                    .buf
                    .len()
                    .checked_add(src.remaining())
                    .ok_or(AxError::MessageTooLong)?;
                if total > CORK_MAX {
                    ax_bail!(MessageTooLong);
                }
                (c.remote, Some(c.source), total)
            } else {
                match resolved {
                    Some((remote, source)) => {
                        if remote.port == 0 || remote.addr.is_unspecified() {
                            ax_bail!(InvalidInput, "invalid address");
                        }
                        (remote, Some(source), src.remaining())
                    }
                    None => ax_bail!(DestAddrRequired),
                }
            };
            let result = self.with_smol_socket(|socket| {
                if !socket.is_open() {
                    // not connected
                    Err(ax_err_type!(NotConnected))
                } else if !socket.can_send() {
                    Err(AxError::WouldBlock)
                } else {
                    // UDP allows zero-length payloads (IP header + UDP header only).
                    if payload_len == 0 {
                        socket
                            .send(
                                0,
                                UdpMetadata {
                                    endpoint,
                                    local_address: local_addr,
                                    meta: PacketMeta::default(),
                                },
                            )
                            .map_err(|e| match e {
                                smol::SendError::BufferFull => AxError::WouldBlock,
                                smol::SendError::Unaddressable => {
                                    ax_err_type!(ConnectionRefused, "unaddressable")
                                }
                            })?;
                        *cork_guard = None;
                        return Ok(0);
                    }
                    let buf = socket
                        .send(
                            payload_len,
                            UdpMetadata {
                                endpoint,
                                local_address: local_addr,
                                meta: PacketMeta::default(),
                            },
                        )
                        .map_err(|e| match e {
                            smol::SendError::BufferFull => AxError::WouldBlock,
                            smol::SendError::Unaddressable => {
                                ax_err_type!(ConnectionRefused, "unaddressable")
                            }
                        })?;
                    let mut total_written = 0;
                    let mut cur_read = 0;
                    if let Some(ref c) = *cork_guard {
                        let n = c.buf.len().min(buf.len());
                        buf[..n].copy_from_slice(&c.buf[..n]);
                        total_written += n;
                    }
                    if total_written < buf.len() {
                        cur_read = src.read(&mut buf[total_written..])?;
                        total_written += cur_read;
                    }
                    assert_eq!(total_written, buf.len());
                    // Success — clear cork state.
                    *cork_guard = None;
                    // Return only bytes consumed from the *current* user buffer.
                    Ok(cur_read)
                }
            })?;
            request_poll();
            Ok(result)
        })
    }

    /// Receives one datagram while honoring peer filters and recv flags.
    fn recv(&self, mut dst: impl Write, mut options: RecvOptions) -> AxResult<usize> {
        enum ExpectedRemote<'a> {
            Any(&'a mut SocketAddrEx),
            AnyDiscard,
            Expecting(IpEndpoint),
        }
        let mut expected_remote = match options.from {
            Some(addr) => ExpectedRemote::Any(addr),
            None => match self.remote_endpoint() {
                Ok((endpoint, _)) => ExpectedRemote::Expecting(endpoint),
                Err(_) => ExpectedRemote::AnyDiscard,
            },
        };

        let extra_nb = options.flags.contains(RecvFlags::DONTWAIT);
        self.general.recv_poller_with(self, extra_nb, || {
            request_poll();
            self.with_smol_socket(|socket| {
                if !socket.can_recv() {
                    Err(AxError::WouldBlock)
                } else {
                    let result = if options.flags.contains(RecvFlags::PEEK) {
                        socket.peek().map(|(data, meta)| (data, *meta))
                    } else {
                        socket.recv()
                    };
                    match result {
                        Ok((src, meta)) => {
                            match &mut expected_remote {
                                ExpectedRemote::Any(remote_addr) => {
                                    **remote_addr = SocketAddrEx::Ip(meta.endpoint.into());
                                }
                                ExpectedRemote::AnyDiscard => {
                                    // recv() with no addr buffer and no peer — accept from any
                                }
                                ExpectedRemote::Expecting(expected) => {
                                    if (!expected.addr.is_unspecified()
                                        && expected.addr != meta.endpoint.addr)
                                        || (expected.port != 0
                                            && expected.port != meta.endpoint.port)
                                    {
                                        return Err(AxError::WouldBlock);
                                    }
                                }
                            }

                            let read = dst.write(src)?;
                            if read < src.len() {
                                warn!("UDP message truncated: {} -> {} bytes", src.len(), read);
                                if let Some(ref mut truncated) = options.truncated {
                                    **truncated = true;
                                }
                            }

                            Ok(if options.flags.contains(RecvFlags::TRUNCATE) {
                                src.len()
                            } else {
                                read
                            })
                        }
                        Err(smol::RecvError::Exhausted) => Err(AxError::WouldBlock),
                        Err(smol::RecvError::Truncated) => {
                            unreachable!("UDP socket recv never returns Err(Truncated)")
                        }
                    }
                }
            })
        })
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        match self.local_addr.try_read() {
            Some(addr) => addr
                .map(Into::into)
                .map(SocketAddrEx::Ip)
                .ok_or(AxError::NotConnected),
            None => Err(AxError::NotConnected),
        }
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        self.remote_endpoint()
            .map(|it| it.0.into())
            .map(SocketAddrEx::Ip)
    }

    fn shutdown(&self, _how: Shutdown) -> AxResult {
        // TODO(mivik): shutdown
        request_poll();

        self.with_smol_socket(|socket| {
            debug!("UDP socket {}: shutting down", self.handle);
            socket.close();
        });
        Ok(())
    }
}

impl Pollable for UdpSocket {
    fn poll(&self) -> IoEvents {
        if self.local_addr.read().is_none() {
            return IoEvents::empty();
        }

        let mut events = IoEvents::empty();
        self.with_smol_socket(|socket| {
            events.set(IoEvents::IN, socket.can_recv());
            events.set(IoEvents::OUT, socket.can_send());
        });
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

impl Default for UdpSocket {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        SOCKET_SET.remove(self.handle);
    }
}

fn get_ephemeral_port() -> AxResult<u16> {
    allocate_ephemeral_port(|port| {
        SOCKET_SET.udp_port_available(IpAddress::Ipv4(Ipv4Addr::UNSPECIFIED), port)
    })
}

#[cfg(test)]
mod tests {
    use core::net::{IpAddr, SocketAddr};

    use super::*;
    use crate::test_support::{
        LOCAL_ADDR, LOCAL_IF, PEER_ADDR, PEER_IF, init_split_route_network, network_test_guard,
    };

    #[test]
    fn connect_preserves_bound_interface() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = UdpSocket::new();
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(LOCAL_ADDR), 0)))
            .unwrap();
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(LOCAL_IF)
            }
        );

        // Connect to different network - should NOT change interface binding
        // because we're bound to a specific local address
        socket
            .connect(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(PEER_ADDR), 53)))
            .unwrap();

        // Interface binding should remain LOCAL_IF (not changed to PEER_IF)
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(LOCAL_IF)
            }
        );
    }

    #[test]
    fn connect_uses_peer_route_when_unbound() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = UdpSocket::new();

        // Bind to 0.0.0.0 (unspecified) - interface should be determined by route
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))
            .unwrap();

        socket
            .connect(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(PEER_ADDR), 53)))
            .unwrap();

        // Interface binding should use route decision (PEER_IF)
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(PEER_IF)
            }
        );
    }

    #[test]
    fn connect_rejects_unroutable_bound_device() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = UdpSocket::new();
        socket.bind_device(LOCAL_IF).unwrap();
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))
            .unwrap();

        assert!(
            socket
                .connect(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(PEER_ADDR), 53)))
                .is_err()
        );
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(LOCAL_IF)
            }
        );
    }
}

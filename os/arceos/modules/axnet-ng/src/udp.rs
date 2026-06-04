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
    consts::{UDP_RX_BUF_LEN, UDP_TX_BUF_LEN},
    general::GeneralOptions,
    get_service,
    options::{Configurable, GetSocketOption, SetSocketOption},
    poll_interfaces,
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

pub(crate) fn new_udp_socket() -> smol::Socket<'static> {
    // TODO(mivik): buffer size
    smol::Socket::new(
        smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; UDP_RX_BUF_LEN]),
        smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; UDP_TX_BUF_LEN]),
    )
}

/// A UDP socket that provides POSIX-like APIs.
pub struct UdpSocket {
    handle: SocketHandle,
    local_addr: RwLock<Option<IpEndpoint>>,
    peer_addr: RwLock<Option<(IpEndpoint, IpAddress)>>,

    general: GeneralOptions,
    /// MSG_MORE corking state: captures endpoint at first MSG_MORE
    /// so the merged datagram always goes to the correct peer.
    cork: Mutex<Option<CorkState>>,
}

impl UdpSocket {
    /// Creates a new UDP socket.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let socket = new_udp_socket();
        let handle = SOCKET_SET.add(socket);

        Self {
            handle,
            local_addr: RwLock::new(None),
            peer_addr: RwLock::new(None),

            general: GeneralOptions::new(2, 2, 17), // SOCK_DGRAM
            cork: Mutex::new(None),
        }
    }

    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }

    fn remote_endpoint(&self) -> AxResult<(IpEndpoint, IpAddress)> {
        match self.peer_addr.try_read() {
            Some(addr) => addr.ok_or(AxError::NotConnected),
            None => Err(AxError::NotConnected),
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

        if !self.general.reuse_address() {
            // Check if the address is already in use
            SOCKET_SET.udp_bind_check(local_endpoint.addr, local_endpoint.port)?;
        }

        self.with_smol_socket(|socket| {
            socket.bind(endpoint).map_err(|e| match e {
                smol::BindError::InvalidState => ax_err_type!(InvalidInput, "already bound"),
                smol::BindError::Unaddressable => ax_err_type!(ConnectionRefused, "unaddressable"),
            })
        })?;
        self.general
            .set_device_mask(get_service().device_mask_for(&endpoint));

        *guard = Some(local_endpoint);
        info!("UDP socket {}: bound on {}", self.handle, endpoint);
        Ok(())
    }

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
        let src = get_service().get_source_address(&remote_addr.addr);
        *guard = Some((remote_addr, src));
        self.general.set_device_mask(
            get_service().device_mask_for(&endpoint_from_ip_endpoint(remote_addr)),
        );
        debug!("UDP socket {}: connected to {}", self.handle, remote_addr);
        Ok(())
    }

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
                    let src = get_service().get_source_address(&addr.addr);
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
                let src = get_service().get_source_address(&addr.addr);
                Some((addr, src))
            }
            None => self.remote_endpoint().ok(),
        };

        let extra_nb = options.flags.contains(SendFlags::DONTWAIT);
        self.general.send_poller_with(self, extra_nb, || {
            poll_interfaces();
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
            // Flush TX so loopback packets reach the receiver immediately.
            poll_interfaces();
            Ok(result)
        })
    }

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
            poll_interfaces();
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
        poll_interfaces();

        self.with_smol_socket(|socket| {
            debug!("UDP socket {}: shutting down", self.handle);
            socket.close();
        });
        Ok(())
    }
}

impl Pollable for UdpSocket {
    fn poll(&self) -> IoEvents {
        poll_interfaces();
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
        if events.intersects(IoEvents::IN | IoEvents::OUT) {
            self.general.register_waker(context.waker());
        }
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        self.shutdown(Shutdown::Both).ok();
        SOCKET_SET.remove(self.handle);
    }
}

fn endpoint_from_ip_endpoint(endpoint: IpEndpoint) -> IpListenEndpoint {
    IpListenEndpoint {
        addr: Some(endpoint.addr),
        port: endpoint.port,
    }
}

fn get_ephemeral_port() -> AxResult<u16> {
    const PORT_START: u16 = 0xc000;
    const PORT_END: u16 = 0xffff;
    static CURR: Mutex<u16> = Mutex::new(PORT_START);
    let mut curr = CURR.lock();

    let port = *curr;
    if *curr == PORT_END {
        *curr = PORT_START;
    } else {
        *curr += 1;
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use core::net::{IpAddr, SocketAddr};

    use super::*;
    use crate::test_support::{
        LOCAL_ADDR, LOCAL_MASK, PEER_ADDR, PEER_MASK, init_split_route_network, network_test_guard,
    };

    #[test]
    fn connect_uses_peer_route_for_device_mask() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = UdpSocket::new();
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(LOCAL_ADDR), 0)))
            .unwrap();
        assert_eq!(socket.general.device_mask(), LOCAL_MASK);

        socket
            .connect(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(PEER_ADDR), 53)))
            .unwrap();

        assert_eq!(socket.general.device_mask(), PEER_MASK);
    }
}

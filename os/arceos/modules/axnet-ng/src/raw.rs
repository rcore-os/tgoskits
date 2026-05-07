// SPDX-License-Identifier: Apache-2.0
// Copyright 2025 KylinSoft Co., Ltd. <https://www.kylinos.cn/>
// See LICENSES for license details.

//! Raw IP socket implementation for ICMP-style traffic.

use alloc::vec;
use core::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_io::prelude::*;
use axpoll::{IoEvents, Pollable};
pub use smoltcp::wire::{IpProtocol, IpVersion};
use smoltcp::{
    iface::SocketHandle,
    socket::raw as smol,
    storage::PacketMetadata,
    wire::{Icmpv6Packet, IpAddress, IpListenEndpoint, Ipv4Packet, Ipv4Repr, Ipv6Packet, Ipv6Repr},
};
use spin::RwLock;

use crate::{
    RecvFlags, RecvOptions, SOCKET_SET, SendOptions, Shutdown, SocketAddrEx, SocketOps,
    consts::{RAW_RX_BUF_LEN, RAW_TX_BUF_LEN},
    general::GeneralOptions,
    get_service,
    options::{Configurable, GetSocketOption, SetSocketOption},
    poll_interfaces,
};

pub(crate) fn new_raw_socket(
    ip_version: IpVersion,
    ip_protocol: IpProtocol,
) -> smol::Socket<'static> {
    smol::Socket::new(
        Some(ip_version),
        Some(ip_protocol),
        smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; RAW_RX_BUF_LEN]),
        smol::PacketBuffer::new(vec![PacketMetadata::EMPTY; 256], vec![0; RAW_TX_BUF_LEN]),
    )
}

/// A raw IP socket used for ICMP and ICMPv6 traffic.
pub struct RawSocket {
    handle: SocketHandle,
    ip_version: IpVersion,
    local_addr: RwLock<Option<IpAddress>>,
    peer_addr: RwLock<Option<IpAddress>>,
    ttl: RwLock<Option<u8>>,
    rx_closed: AtomicBool,
    tx_closed: AtomicBool,
    general: GeneralOptions,
}

impl RawSocket {
    /// Creates a raw socket for the given IP version and protocol.
    pub fn new(ip_version: IpVersion, ip_protocol: IpProtocol) -> Self {
        let handle = SOCKET_SET.add(new_raw_socket(ip_version, ip_protocol));
        let general = GeneralOptions::new();
        general.set_device_mask(u32::MAX);
        Self {
            handle,
            ip_version,
            local_addr: RwLock::new(None),
            peer_addr: RwLock::new(None),
            ttl: RwLock::new(None),
            rx_closed: AtomicBool::new(false),
            tx_closed: AtomicBool::new(false),
            general,
        }
    }

    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }

    fn check_ip_version(&self, addr: IpAddress) -> AxResult<IpAddress> {
        match (self.ip_version, addr) {
            (IpVersion::Ipv4, IpAddress::Ipv4(_)) | (IpVersion::Ipv6, IpAddress::Ipv6(_)) => {
                Ok(addr)
            }
            _ => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
        }
    }

    fn remote_address(&self, options: &SendOptions) -> AxResult<IpAddress> {
        match &options.to {
            Some(addr) => {
                let remote = addr.clone().into_ip()?;
                self.check_ip_version(remote.ip().into())
            }
            None => (*self.peer_addr.read()).ok_or(AxError::NotConnected),
        }
    }

    fn local_address_for(&self, remote: IpAddress) -> IpAddress {
        if let Some(local) = *self.local_addr.read() {
            return local;
        }
        get_service().get_source_address(&remote)
    }

    fn parse_ip_packet<'a>(&self, packet: &'a [u8]) -> AxResult<(IpAddress, &'a [u8])> {
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
        let device_mask = if local.is_unspecified() {
            u32::MAX
        } else {
            get_service().device_mask_for(&IpListenEndpoint {
                addr: Some(local),
                port: 0,
            })
        };
        self.general.set_device_mask(device_mask);
        Ok(())
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_ip()?;
        let remote = self.check_ip_version(remote_addr.ip().into())?;
        if self.local_addr.read().is_none() {
            *self.local_addr.write() = Some(get_service().get_source_address(&remote));
        }
        *self.peer_addr.write() = Some(remote);
        self.general
            .set_device_mask(get_service().device_mask_for(&IpListenEndpoint {
                addr: Some(remote),
                port: 0,
            }));
        Ok(())
    }

    fn send(&self, mut src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        if self.tx_closed.load(Ordering::Acquire) {
            return Err(AxError::BrokenPipe);
        }

        let remote = self.remote_address(&options)?;
        let local = self.local_address_for(remote);
        let payload_len = src.remaining();

        self.general.send_poller(self, || {
            poll_interfaces();
            self.with_smol_socket(|socket| {
                if !socket.can_send() {
                    return Err(AxError::WouldBlock);
                }
                let next_header = socket.ip_protocol().expect("raw socket protocol");
                let hop_limit = (*self.ttl.read()).unwrap_or(64);

                let header_len = match self.ip_version {
                    IpVersion::Ipv4 => Ipv4Repr {
                        src_addr: match local {
                            IpAddress::Ipv4(addr) => addr,
                            _ => unreachable!(),
                        },
                        dst_addr: match remote {
                            IpAddress::Ipv4(addr) => addr,
                            _ => unreachable!(),
                        },
                        next_header,
                        payload_len,
                        hop_limit,
                    }
                    .buffer_len(),
                    IpVersion::Ipv6 => Ipv6Repr {
                        src_addr: match local {
                            IpAddress::Ipv6(addr) => addr,
                            _ => unreachable!(),
                        },
                        dst_addr: match remote {
                            IpAddress::Ipv6(addr) => addr,
                            _ => unreachable!(),
                        },
                        next_header,
                        payload_len,
                        hop_limit,
                    }
                    .buffer_len(),
                };

                let buf = socket
                    .send(header_len + payload_len)
                    .map_err(|_| AxError::WouldBlock)?;
                match self.ip_version {
                    IpVersion::Ipv4 => {
                        let header = Ipv4Repr {
                            src_addr: match local {
                                IpAddress::Ipv4(addr) => addr,
                                _ => unreachable!(),
                            },
                            dst_addr: match remote {
                                IpAddress::Ipv4(addr) => addr,
                                _ => unreachable!(),
                            },
                            next_header,
                            payload_len,
                            hop_limit,
                        };
                        header.emit(
                            &mut Ipv4Packet::new_unchecked(&mut *buf),
                            &smoltcp::phy::ChecksumCapabilities::ignored(),
                        );
                    }
                    IpVersion::Ipv6 => {
                        let header = Ipv6Repr {
                            src_addr: match local {
                                IpAddress::Ipv6(addr) => addr,
                                _ => unreachable!(),
                            },
                            dst_addr: match remote {
                                IpAddress::Ipv6(addr) => addr,
                                _ => unreachable!(),
                            },
                            next_header,
                            payload_len,
                            hop_limit,
                        };
                        header.emit(&mut Ipv6Packet::new_unchecked(&mut *buf));
                    }
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
                Ok(written)
            })
        })
    }

    fn recv(&self, mut dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize> {
        if self.rx_closed.load(Ordering::Acquire) {
            return Err(AxError::NotConnected);
        }
        let mut options = options;

        self.general.recv_poller(self, || {
            poll_interfaces();
            self.with_smol_socket(|socket| {
                loop {
                    let packet = if options.flags.contains(RecvFlags::PEEK) {
                        let packet = socket.peek().map_err(|_| AxError::WouldBlock)?;
                        let (source, _) = self.parse_ip_packet(packet)?;
                        if let Some(peer) = *self.peer_addr.read()
                            && source != peer
                        {
                            return Err(AxError::WouldBlock);
                        }
                        packet
                    } else {
                        socket.recv().map_err(|_| AxError::WouldBlock)?
                    };
                    let (source, packet) = self.parse_ip_packet(packet)?;

                    if let Some(peer) = *self.peer_addr.read()
                        && source != peer
                    {
                        continue;
                    }

                    if let Some(from) = options.from.as_deref_mut() {
                        *from = SocketAddrEx::Ip(SocketAddr::new(source.into(), 0));
                    }

                    let written = dst.write(packet)?;
                    return Ok(if options.flags.contains(RecvFlags::TRUNCATE) {
                        packet.len()
                    } else {
                        written
                    });
                }
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
        poll_interfaces();
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
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
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

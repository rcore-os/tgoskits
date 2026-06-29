use crate::io::AxPollState;
use ax_errno::AxResult;
use ax_net::{
    RecvFlags, RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx, SocketOps,
    options::{Configurable, SetSocketOption},
    tcp::TcpSocket,
    udp::UdpSocket,
};
use axpoll::{IoEvents, Pollable};
use core::net::{IpAddr, SocketAddr};

/// A handle to a TCP socket.
pub struct AxTcpSocketHandle(TcpSocket);

/// A handle to a UDP socket.
pub struct AxUdpSocketHandle(UdpSocket);

////////////////////////////////////////////////////////////////////////////////
// TCP socket
////////////////////////////////////////////////////////////////////////////////

pub fn ax_tcp_socket() -> AxTcpSocketHandle {
    AxTcpSocketHandle(TcpSocket::new())
}

pub fn ax_tcp_socket_addr(socket: &AxTcpSocketHandle) -> AxResult<SocketAddr> {
    into_ip_addr(socket.0.local_addr()?)
}

pub fn ax_tcp_peer_addr(socket: &AxTcpSocketHandle) -> AxResult<SocketAddr> {
    into_ip_addr(socket.0.peer_addr()?)
}

pub fn ax_tcp_set_nonblocking(socket: &AxTcpSocketHandle, nonblocking: bool) -> AxResult {
    socket
        .0
        .set_option(SetSocketOption::NonBlocking(&nonblocking))
}

pub fn ax_tcp_connect(socket: &AxTcpSocketHandle, addr: SocketAddr) -> AxResult {
    socket.0.connect(SocketAddrEx::Ip(addr))
}

pub fn ax_tcp_bind(socket: &AxTcpSocketHandle, addr: SocketAddr) -> AxResult {
    socket.0.bind(SocketAddrEx::Ip(addr))
}

pub fn ax_tcp_listen(socket: &AxTcpSocketHandle, backlog: usize) -> AxResult {
    socket.0.listen(backlog)
}

pub fn ax_tcp_accept(socket: &AxTcpSocketHandle) -> AxResult<(AxTcpSocketHandle, SocketAddr)> {
    let new_sock = socket.0.accept()?;
    let Socket::Tcp(new_sock) = new_sock else {
        unreachable!("TCP listener accepted a non-TCP socket");
    };
    let addr = into_ip_addr(new_sock.peer_addr()?)?;
    Ok((AxTcpSocketHandle(*new_sock), addr))
}

pub fn ax_tcp_send(socket: &AxTcpSocketHandle, buf: &[u8]) -> AxResult<usize> {
    socket.0.send(buf, SendOptions::default())
}

pub fn ax_tcp_recv(socket: &AxTcpSocketHandle, buf: &mut [u8]) -> AxResult<usize> {
    socket.0.recv(buf, RecvOptions::default())
}

pub fn ax_tcp_poll(socket: &AxTcpSocketHandle) -> AxResult<AxPollState> {
    Ok(poll_state(socket.0.poll()))
}

pub fn ax_tcp_shutdown(socket: &AxTcpSocketHandle) -> AxResult {
    socket.0.shutdown(Shutdown::Both)
}

////////////////////////////////////////////////////////////////////////////////
// UDP socket
////////////////////////////////////////////////////////////////////////////////

pub fn ax_udp_socket() -> AxUdpSocketHandle {
    AxUdpSocketHandle(UdpSocket::new())
}

pub fn ax_udp_socket_addr(socket: &AxUdpSocketHandle) -> AxResult<SocketAddr> {
    into_ip_addr(socket.0.local_addr()?)
}

pub fn ax_udp_peer_addr(socket: &AxUdpSocketHandle) -> AxResult<SocketAddr> {
    into_ip_addr(socket.0.peer_addr()?)
}

pub fn ax_udp_set_nonblocking(socket: &AxUdpSocketHandle, nonblocking: bool) -> AxResult {
    socket
        .0
        .set_option(SetSocketOption::NonBlocking(&nonblocking))
}

pub fn ax_udp_bind(socket: &AxUdpSocketHandle, addr: SocketAddr) -> AxResult {
    socket.0.bind(SocketAddrEx::Ip(addr))
}

pub fn ax_udp_recv_from(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> AxResult<(usize, SocketAddr)> {
    let mut from = SocketAddrEx::Ip("0.0.0.0:0".parse().unwrap());
    let len = socket.0.recv(
        buf,
        RecvOptions {
            from: Some(&mut from),
            ..RecvOptions::default()
        },
    )?;
    Ok((len, into_ip_addr(from)?))
}

pub fn ax_udp_peek_from(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> AxResult<(usize, SocketAddr)> {
    let mut from = SocketAddrEx::Ip("0.0.0.0:0".parse().unwrap());
    let len = socket.0.recv(
        buf,
        RecvOptions {
            from: Some(&mut from),
            flags: RecvFlags::PEEK,
            ..RecvOptions::default()
        },
    )?;
    Ok((len, into_ip_addr(from)?))
}

pub fn ax_udp_send_to(socket: &AxUdpSocketHandle, buf: &[u8], addr: SocketAddr) -> AxResult<usize> {
    socket.0.send(
        buf,
        SendOptions {
            to: Some(SocketAddrEx::Ip(addr)),
            ..SendOptions::default()
        },
    )
}

pub fn ax_udp_connect(socket: &AxUdpSocketHandle, addr: SocketAddr) -> AxResult {
    socket.0.connect(SocketAddrEx::Ip(addr))
}

pub fn ax_udp_send(socket: &AxUdpSocketHandle, buf: &[u8]) -> AxResult<usize> {
    socket.0.send(buf, SendOptions::default())
}

pub fn ax_udp_recv(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> AxResult<usize> {
    socket.0.recv(buf, RecvOptions::default())
}

pub fn ax_udp_poll(socket: &AxUdpSocketHandle) -> AxResult<AxPollState> {
    Ok(poll_state(socket.0.poll()))
}

////////////////////////////////////////////////////////////////////////////////
// Miscellaneous
////////////////////////////////////////////////////////////////////////////////

pub fn ax_dns_query(domain_name: &str) -> AxResult<alloc::vec::Vec<IpAddr>> {
    ax_net::dns_query(domain_name)
}

pub fn ax_poll_interfaces() -> AxResult {
    ax_net::request_poll();
    Ok(())
}

fn into_ip_addr(addr: SocketAddrEx) -> AxResult<SocketAddr> {
    addr.into_ip()
}

fn poll_state(events: IoEvents) -> AxPollState {
    AxPollState {
        readable: events.intersects(IoEvents::IN | IoEvents::RDHUP | IoEvents::HUP),
        writable: events.contains(IoEvents::OUT),
        readiness_version: 0,
    }
}

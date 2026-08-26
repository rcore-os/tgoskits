use crate::io::AxPollState;
use crate::ApiResult;
use ax_net::{
    ConnectStatus, NetError, RecvFlags, RecvOptions, SendOptions, Shutdown, Socket,
    SocketAddrEx, SocketOps, SocketWaitPolicy, poll_socket_io,
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

pub fn ax_tcp_socket_addr(socket: &AxTcpSocketHandle) -> ApiResult<SocketAddr> {
    into_ip_addr(socket.0.local_addr()?)
}

pub fn ax_tcp_peer_addr(socket: &AxTcpSocketHandle) -> ApiResult<SocketAddr> {
    into_ip_addr(socket.0.peer_addr()?)
}

pub fn ax_tcp_set_nonblocking(socket: &AxTcpSocketHandle, nonblocking: bool) -> ApiResult {
    socket
        .0
        .set_option(SetSocketOption::NonBlocking(&nonblocking))?;
    Ok(())
}

pub fn ax_tcp_connect(socket: &AxTcpSocketHandle, addr: SocketAddr) -> ApiResult {
    let policy = socket.0.send_wait_policy(false)?;
    match socket.0.start_connect(SocketAddrEx::Ip(addr))? {
        ConnectStatus::Connected => Ok(()),
        ConnectStatus::InProgress if policy.nonblocking => Err(NetError::InProgress.into()),
        ConnectStatus::InProgress => wait_socket_io(
            &socket.0,
            IoEvents::OUT,
            policy,
            NetError::TimedOut,
            || match socket.0.connect_status()? {
                ConnectStatus::Connected => Ok(()),
                ConnectStatus::InProgress => Err(NetError::WouldBlock),
            },
        ),
    }
}

pub fn ax_tcp_bind(socket: &AxTcpSocketHandle, addr: SocketAddr) -> ApiResult {
    socket.0.bind(SocketAddrEx::Ip(addr))?;
    Ok(())
}

pub fn ax_tcp_listen(socket: &AxTcpSocketHandle, backlog: usize) -> ApiResult {
    socket.0.listen(backlog)?;
    Ok(())
}

pub fn ax_tcp_accept(socket: &AxTcpSocketHandle) -> ApiResult<(AxTcpSocketHandle, SocketAddr)> {
    let policy = socket.0.recv_wait_policy(false)?;
    let new_sock = wait_socket_io(
        &socket.0,
        IoEvents::IN,
        policy,
        NetError::WouldBlock,
        || socket.0.try_accept(),
    )?;
    let Socket::Tcp(new_sock) = new_sock else {
        unreachable!("TCP listener accepted a non-TCP socket");
    };
    let addr = into_ip_addr(new_sock.peer_addr()?)?;
    Ok((AxTcpSocketHandle(*new_sock), addr))
}

pub fn ax_tcp_send(socket: &AxTcpSocketHandle, buf: &[u8]) -> ApiResult<usize> {
    let src = buf;
    let mut options = SendOptions::default();
    let policy = socket.0.send_wait_policy(false)?;
    wait_socket_io(
        &socket.0,
        IoEvents::OUT,
        policy,
        NetError::WouldBlock,
        || socket.0.try_send(src, &mut options),
    )
}

pub fn ax_tcp_recv(socket: &AxTcpSocketHandle, buf: &mut [u8]) -> ApiResult<usize> {
    let mut dst = buf;
    let mut options = RecvOptions::default();
    let policy = socket.0.recv_wait_policy(false)?;
    wait_socket_io(
        &socket.0,
        IoEvents::IN,
        policy,
        NetError::WouldBlock,
        || socket.0.try_recv(&mut dst, &mut options),
    )
}

pub fn ax_tcp_poll(socket: &AxTcpSocketHandle) -> ApiResult<AxPollState> {
    Ok(poll_state(socket.0.poll()))
}

pub fn ax_tcp_shutdown(socket: &AxTcpSocketHandle) -> ApiResult {
    socket.0.shutdown(Shutdown::Both)?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// UDP socket
////////////////////////////////////////////////////////////////////////////////

pub fn ax_udp_socket() -> AxUdpSocketHandle {
    AxUdpSocketHandle(UdpSocket::new())
}

pub fn ax_udp_socket_addr(socket: &AxUdpSocketHandle) -> ApiResult<SocketAddr> {
    into_ip_addr(socket.0.local_addr()?)
}

pub fn ax_udp_peer_addr(socket: &AxUdpSocketHandle) -> ApiResult<SocketAddr> {
    into_ip_addr(socket.0.peer_addr()?)
}

pub fn ax_udp_set_nonblocking(socket: &AxUdpSocketHandle, nonblocking: bool) -> ApiResult {
    socket
        .0
        .set_option(SetSocketOption::NonBlocking(&nonblocking))?;
    Ok(())
}

pub fn ax_udp_bind(socket: &AxUdpSocketHandle, addr: SocketAddr) -> ApiResult {
    socket.0.bind(SocketAddrEx::Ip(addr))?;
    Ok(())
}

pub fn ax_udp_recv_from(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> ApiResult<(usize, SocketAddr)> {
    let mut from = SocketAddrEx::Ip("0.0.0.0:0".parse().unwrap());
    let mut dst = buf;
    let mut options = RecvOptions {
            from: Some(&mut from),
            ..RecvOptions::default()
        };
    let policy = socket.0.recv_wait_policy(false)?;
    let len = wait_socket_io(
        &socket.0,
        IoEvents::IN,
        policy,
        NetError::WouldBlock,
        || socket.0.try_recv(&mut dst, &mut options),
    )?;
    Ok((len, into_ip_addr(from)?))
}

pub fn ax_udp_peek_from(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> ApiResult<(usize, SocketAddr)> {
    let mut from = SocketAddrEx::Ip("0.0.0.0:0".parse().unwrap());
    let mut dst = buf;
    let mut options = RecvOptions {
            from: Some(&mut from),
            flags: RecvFlags::PEEK,
            ..RecvOptions::default()
        };
    let policy = socket.0.recv_wait_policy(false)?;
    let len = wait_socket_io(
        &socket.0,
        IoEvents::IN,
        policy,
        NetError::WouldBlock,
        || socket.0.try_recv(&mut dst, &mut options),
    )?;
    Ok((len, into_ip_addr(from)?))
}

pub fn ax_udp_send_to(socket: &AxUdpSocketHandle, buf: &[u8], addr: SocketAddr) -> ApiResult<usize> {
    let src = buf;
    let mut options = SendOptions {
            to: Some(SocketAddrEx::Ip(addr)),
            ..SendOptions::default()
        };
    let policy = socket.0.send_wait_policy(false)?;
    wait_socket_io(
        &socket.0,
        IoEvents::OUT,
        policy,
        NetError::WouldBlock,
        || socket.0.try_send(src, &mut options),
    )
}

pub fn ax_udp_connect(socket: &AxUdpSocketHandle, addr: SocketAddr) -> ApiResult {
    let status = socket.0.start_connect(SocketAddrEx::Ip(addr))?;
    debug_assert_eq!(status, ConnectStatus::Connected);
    Ok(())
}

pub fn ax_udp_send(socket: &AxUdpSocketHandle, buf: &[u8]) -> ApiResult<usize> {
    let src = buf;
    let mut options = SendOptions::default();
    let policy = socket.0.send_wait_policy(false)?;
    wait_socket_io(
        &socket.0,
        IoEvents::OUT,
        policy,
        NetError::WouldBlock,
        || socket.0.try_send(src, &mut options),
    )
}

pub fn ax_udp_recv(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> ApiResult<usize> {
    let mut dst = buf;
    let mut options = RecvOptions::default();
    let policy = socket.0.recv_wait_policy(false)?;
    wait_socket_io(
        &socket.0,
        IoEvents::IN,
        policy,
        NetError::WouldBlock,
        || socket.0.try_recv(&mut dst, &mut options),
    )
}

pub fn ax_udp_poll(socket: &AxUdpSocketHandle) -> ApiResult<AxPollState> {
    Ok(poll_state(socket.0.poll()))
}

////////////////////////////////////////////////////////////////////////////////
// Miscellaneous
////////////////////////////////////////////////////////////////////////////////

pub fn ax_dns_query(domain_name: &str) -> ApiResult<alloc::vec::Vec<IpAddr>> {
    Ok(ax_net::dns_query(domain_name)?)
}

fn into_ip_addr(addr: SocketAddrEx) -> ApiResult<SocketAddr> {
    Ok(addr.into_ip()?)
}

fn wait_socket_io<P, F, T>(
    pollable: &P,
    events: IoEvents,
    policy: SocketWaitPolicy,
    timeout_error: NetError,
    mut operation: F,
) -> ApiResult<T>
where
    P: Pollable + ?Sized,
    F: FnMut() -> ax_net::NetResult<T>,
{
    if policy.nonblocking {
        return Ok(ax_runtime::task::block_on(poll_socket_io(
            pollable,
            events,
            true,
            &mut operation,
        ))?);
    }
    match policy.timeout {
        Some(timeout) => match ax_runtime::task::block_on_timeout(
            timeout,
            poll_socket_io(pollable, events, false, &mut operation),
        ) {
            Ok(result) => Ok(result?),
            Err(_) => Err(timeout_error.into()),
        },
        None => Ok(ax_runtime::task::block_on(poll_socket_io(
            pollable,
            events,
            false,
            operation,
        ))?),
    }
}

fn poll_state(events: IoEvents) -> AxPollState {
    AxPollState {
        readable: events.intersects(IoEvents::IN | IoEvents::RDHUP | IoEvents::HUP),
        writable: events.contains(IoEvents::OUT),
        readiness_version: 0,
    }
}

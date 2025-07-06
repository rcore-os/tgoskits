use core::net::{Ipv4Addr, SocketAddr};

use axerrno::LinuxResult;
use axnet::{RecvFlags, SendFlags, SocketOps};
use linux_raw_sys::net::{MSG_PEEK, MSG_TRUNC, sockaddr, socklen_t};

use crate::{
    file::{FileLike, Socket},
    ptr::{UserConstPtr, UserPtr, nullable},
    socket::SocketAddrExt,
};

pub fn sys_sendto(
    fd: i32,
    buf: UserConstPtr<u8>,
    len: usize,
    flags: u32,
    addr: UserConstPtr<sockaddr>,
    addrlen: u32,
) -> LinuxResult<isize> {
    let addr = if addr.is_null() || addrlen == 0 {
        None
    } else {
        Some(SocketAddr::read_from_user(addr, addrlen)?)
    };

    debug!(
        "sys_sendto <= fd: {}, len: {}, flags: {}, addr: {:?}",
        fd, len, flags, addr
    );

    let bytes = buf.get_as_slice(len)?;
    let socket = Socket::from_fd(fd)?;
    let sent = socket.send(bytes, addr, SendFlags::empty())?;

    Ok(sent as isize)
}

pub fn sys_recvfrom(
    fd: i32,
    buf: UserPtr<u8>,
    len: usize,
    flags: u32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    debug!("sys_recvfrom <= fd: {}, len: {}, flags: {}", fd, len, flags);

    let socket = Socket::from_fd(fd)?;
    let buf = buf.get_as_mut_slice(len)?;
    let mut recv_flags = RecvFlags::empty();
    if flags & MSG_PEEK != 0 {
        recv_flags |= RecvFlags::PEEK;
    }
    if flags & MSG_TRUNC != 0 {
        recv_flags |= RecvFlags::TRUNCATE;
    }

    let mut remote_addr = (!addr.is_null()).then(|| (Ipv4Addr::UNSPECIFIED, 0).into());
    let recv = socket.recv(buf, remote_addr.as_mut(), recv_flags)?;

    if let Some(remote_addr) = remote_addr {
        let len = remote_addr.write_to_user(addr)?;
        if let Some(addrlen) = nullable!(addrlen.get_as_mut())? {
            *addrlen = len;
        }
    }

    debug!("sys_recvfrom => fd: {}, recv: {}", fd, recv);
    Ok(recv as isize)
}

use core::net::SocketAddr;

use axerrno::LinuxResult;
use linux_raw_sys::net::{sockaddr, socklen_t};

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

    let sent = if let Some(addr) = addr {
        socket.sendto(bytes, addr)?
    } else {
        socket.send(bytes)?
    };

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
    let (recv, remote_addr) = socket.recvfrom(buf)?;

    if let Some(remote_addr) = remote_addr
        && !addr.is_null()
    {
        let len = remote_addr.write_to_user(addr)?;
        if let Some(addrlen) = nullable!(addrlen.get_as_mut())? {
            *addrlen = len;
        }
    }

    debug!("sys_recvfrom => fd: {}, recv: {}", fd, recv);
    Ok(recv as isize)
}

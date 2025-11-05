use axerrno::AxResult;
use axnet::SocketOps;
use linux_raw_sys::net::{sockaddr, socklen_t};

use crate::{
    file::{FileLike, Socket},
    mm::UserPtr,
    socket::SocketAddrExt,
};

pub fn sys_getsockname(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> AxResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let local_addr = socket.local_addr()?;
    debug!("sys_getsockname <= fd: {fd}, addr: {local_addr:?}");

    local_addr.write_to_user(addr, addrlen.get_as_mut()?)?;
    Ok(0)
}

pub fn sys_getpeername(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> AxResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let peer_addr = socket.peer_addr()?;
    debug!("sys_getpeername <= fd: {fd}, addr: {peer_addr:?}");

    peer_addr.write_to_user(addr, addrlen.get_as_mut()?)?;
    Ok(0)
}

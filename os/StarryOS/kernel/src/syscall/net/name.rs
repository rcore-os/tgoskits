use ax_errno::AxResult;
use axnet::SocketOps;
use linux_raw_sys::net::{sockaddr, socklen_t};

use super::addr::SocketAddrExt;
use crate::{
    file::{FileLike, PacketSocket, Socket, netlink::NetlinkSocket},
    mm::UserPtr,
};

pub fn sys_getsockname(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> AxResult<isize> {
    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let local_addr = packet.local_addr();
        local_addr.write_to_user(
            addr.address().as_usize() as *mut sockaddr,
            addrlen.get_as_mut()?,
        )?;
        return Ok(0);
    }

    if let Ok(socket) = NetlinkSocket::from_fd(fd) {
        let local_addr = socket.local_addr();
        debug!("sys_getsockname <= fd: {fd}, netlink_addr: {local_addr:?}");
        super::addr::write_netlink_addr(&local_addr, addr, addrlen.get_as_mut()?)?;
        return Ok(0);
    }

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

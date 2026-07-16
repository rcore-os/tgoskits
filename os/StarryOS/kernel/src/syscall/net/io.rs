use alloc::{boxed::Box, vec::Vec};
use core::{net::Ipv4Addr, time::Duration};

use ax_errno::{AxError, AxResult};
use ax_io::prelude::*;
use ax_net::{
    CMsgData, IpCmsg, RecvFlags, RecvOptions, SendFlags, SendOptions, SocketAddrEx, SocketOps,
};
use ax_runtime::hal::time::wall_time;
use linux_raw_sys::{
    general::timespec,
    net::{
        IP_TOS, IPPROTO_IPV6, IPV6_TCLASS, MSG_DONTWAIT, MSG_PEEK, MSG_TRUNC, SCM_RIGHTS,
        SOL_SOCKET, cmsghdr, mmsghdr, msghdr, sockaddr, socklen_t,
    },
};

use super::addr::{
    SocketAddrExt, normalize_socket_addr_ex_for_ip_stack, socket_addr_ex_for_user_name,
};
use crate::{
    file::{FileLike, PacketSocket, Socket, add_file_like, get_file_like, netlink::NetlinkSocket},
    mm::{IoVec, IoVectorBuf, UserConstPtr, UserPtr, VmBytes, VmBytesMut},
    syscall::net::{CMsg, CMsgBuilder, cmsg_space},
    time::TimeValueLike,
};

// Linux ABI for sendmmsg/recvmmsg limits vlen to UIO_MAXIOV (1024).
const MMSG_MAX_VLEN: u32 = 1024;
const PROTO_IP: u32 = linux_raw_sys::net::IPPROTO_IP as u32;

fn parse_recvmmsg_timeout(timeout: UserConstPtr<timespec>) -> AxResult<Option<Duration>> {
    if timeout.is_null() {
        return Ok(None);
    }
    // SAFETY: timespec contains only signed integer fields; semantic range
    // validation is performed by try_into_time_value below.
    let ts = unsafe { timeout.read_abi()? };
    let tv = ts.try_into_time_value()?;
    Ok(Some(Duration::new(tv.as_secs(), tv.subsec_nanos())))
}

fn decode_msg_namelen(value: i32) -> AxResult<socklen_t> {
    value.try_into().map_err(|_| AxError::InvalidInput)
}

fn encode_msg_namelen(value: socklen_t) -> AxResult<i32> {
    value.try_into().map_err(|_| AxError::InvalidInput)
}

fn write_msghdr_outputs(
    msg: UserPtr<msghdr>,
    namelen: i32,
    controllen: usize,
    flags: u32,
) -> AxResult<()> {
    let base = msg.address().as_usize();
    UserPtr::<i32>::from(base + core::mem::offset_of!(msghdr, msg_namelen)).write(namelen)?;
    UserPtr::<usize>::from(base + core::mem::offset_of!(msghdr, msg_controllen))
        .write(controllen)?;
    UserPtr::<u32>::from(base + core::mem::offset_of!(msghdr, msg_flags)).write(flags)
}

fn mmsghdr_address(msgvec: UserPtr<mmsghdr>, index: usize) -> AxResult<usize> {
    index
        .checked_mul(size_of::<mmsghdr>())
        .and_then(|offset| msgvec.address().as_usize().checked_add(offset))
        .ok_or(AxError::InvalidInput)
}

fn write_mmsghdr_len(msgvec: UserPtr<mmsghdr>, index: usize, len: u32) -> AxResult<()> {
    let base = mmsghdr_address(msgvec, index)?;
    UserPtr::<u32>::from(base + core::mem::offset_of!(mmsghdr, msg_len)).write(len)
}

fn parse_send_cmsgs(control_ptr: usize, control_len: usize) -> AxResult<Vec<CMsgData>> {
    let mut cmsg = Vec::new();
    if control_ptr == 0 || control_len == 0 {
        return Ok(cmsg);
    }

    let mut ptr = control_ptr;
    let ptr_end = ptr.checked_add(control_len).ok_or(AxError::InvalidInput)?;

    while let Some(next) = ptr.checked_add(size_of::<cmsghdr>()) {
        if next > ptr_end {
            break;
        }

        // SAFETY: cmsghdr is an integer-only C ABI record. Every copied bit
        // pattern is valid; length and level/type semantics are checked below.
        let hdr = unsafe { UserConstPtr::<cmsghdr>::from(ptr).read_abi()? };
        if hdr.cmsg_len < size_of::<cmsghdr>() || ptr_end - ptr < hdr.cmsg_len {
            return Err(AxError::InvalidInput);
        }

        let Some(next_ptr) = cmsg_space(hdr.cmsg_len - size_of::<cmsghdr>())
            .and_then(|space| ptr.checked_add(space))
        else {
            return Err(AxError::InvalidInput);
        };

        cmsg.push(Box::new(CMsg::parse(ptr, &hdr)?) as CMsgData);
        ptr = next_ptr;
    }

    Ok(cmsg)
}

fn send_impl(
    fd: i32,
    mut src: impl Read + IoBuf,
    flags: u32,
    addr: UserConstPtr<sockaddr>,
    addrlen: socklen_t,
    cmsg: Vec<CMsgData>,
) -> AxResult<isize> {
    if let Ok(packet) = PacketSocket::from_fd(fd) {
        return Ok(packet.send_packet(&mut src)? as isize);
    }

    if let Ok(socket) = Socket::from_fd(fd) {
        let addr = if addr.is_null() {
            // addr == NULL: treat as no address regardless of addrlen.
            // Linux sendto(..., NULL, nonzero) sends to connected peer or
            // returns EDESTADDRREQ on unconnected socket, never EINVAL.
            None
        } else if addrlen == 0 {
            return Err(AxError::InvalidInput);
        } else {
            let mut addr = SocketAddrEx::read_from_user(addr, addrlen)?;
            if socket.ip_domain() == linux_raw_sys::net::AF_INET6 {
                addr = normalize_socket_addr_ex_for_ip_stack(addr, false)?;
            }
            Some(addr)
        };

        let send_flags = SendFlags::from_bits_retain(flags);

        debug!("sys_send <= fd: {fd}, flags: {flags:#x}, addr: {addr:?}");

        let sent = socket.send(
            &mut src,
            SendOptions {
                to: addr,
                flags: send_flags,
                cmsg,
            },
        )?;

        return Ok(sent as isize);
    }

    if let Ok(netlink) = NetlinkSocket::from_fd(fd) {
        let sent = netlink.write(&mut src)?;
        return Ok(sent as isize);
    }

    get_file_like(fd)?;
    Err(AxError::NotASocket)
}

pub fn sys_sendto(
    fd: i32,
    buf: *const u8,
    len: usize,
    flags: u32,
    addr: UserConstPtr<sockaddr>,
    addrlen: socklen_t,
) -> AxResult<isize> {
    send_impl(fd, VmBytes::new(buf, len), flags, addr, addrlen, Vec::new())
}

pub fn sys_sendmsg(fd: i32, msg: UserConstPtr<msghdr>, flags: u32) -> AxResult<isize> {
    // SAFETY: msghdr consists of raw addresses and integer lengths/flags.
    // Every bit pattern is valid before the syscall validates each field.
    let msg = unsafe { msg.read_abi()? };
    let cmsg = parse_send_cmsgs(msg.msg_control as usize, msg.msg_controllen)?;
    send_impl(
        fd,
        IoVectorBuf::new(msg.msg_iov as *const IoVec, msg.msg_iovlen)?.into_io(),
        flags,
        UserConstPtr::from(msg.msg_name as usize),
        decode_msg_namelen(msg.msg_namelen)?,
        cmsg,
    )
}

fn recv_impl(
    fd: i32,
    mut dst: impl Write + IoBufMut,
    flags: u32,
    addr: UserPtr<sockaddr>,
    addrlen: &mut socklen_t,
    mut cmsg_builder: Option<CMsgBuilder>,
    truncated_out: &mut bool,
) -> AxResult<isize> {
    debug!("sys_recv <= fd: {fd}, flags: {flags}");

    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let (recv, from) = packet.recv_packet(&mut dst)?;
        if !addr.is_null() {
            from.write_to_user(addr.address().as_usize() as *mut sockaddr, addrlen)?;
        }
        if let Some(builder) = cmsg_builder.take() {
            builder.finish();
        }
        return Ok(recv as isize);
    }

    let Ok(socket) = Socket::from_fd(fd) else {
        if let Ok(netlink) = NetlinkSocket::from_fd(fd) {
            // Netlink is a FileLike, not an ax_net Socket, so the flag-aware recv
            // path below is unreachable for it. Honor the recv flags here:
            // MSG_PEEK (do not consume the dump — getifaddrs/dnsmasq peek-then-
            // read to size their buffer), MSG_TRUNC (full datagram length),
            // MSG_DONTWAIT (non-blocking).
            let (recv, truncated) = netlink.recv(
                &mut dst,
                flags & MSG_PEEK != 0,
                flags & MSG_TRUNC != 0,
                flags & MSG_DONTWAIT != 0,
            )?;
            // Surface MSG_TRUNC in the returned `msg_flags` when the datagram
            // did not fit (Linux sets it; getifaddrs sizes its buffer from it).
            *truncated_out = truncated;
            if !addr.is_null() {
                super::addr::write_netlink_addr(&netlink.kernel_addr(), addr, addrlen)?;
            }
            if let Some(builder) = cmsg_builder.take() {
                builder.finish();
            }
            return Ok(recv as isize);
        }

        get_file_like(fd)?;
        return Err(AxError::NotASocket);
    };
    let mut recv_flags = RecvFlags::empty();
    if flags & MSG_PEEK != 0 {
        recv_flags |= RecvFlags::PEEK;
    }
    if flags & MSG_TRUNC != 0 {
        recv_flags |= RecvFlags::TRUNCATE;
    }
    if flags & MSG_DONTWAIT != 0 {
        recv_flags |= RecvFlags::DONTWAIT;
    }

    let mut cmsg = Vec::new();

    let mut remote_addr =
        (!addr.is_null()).then(|| SocketAddrEx::Ip((Ipv4Addr::UNSPECIFIED, 0).into()));
    let recv = socket.recv(
        &mut dst,
        RecvOptions {
            from: remote_addr.as_mut(),
            flags: recv_flags,
            cmsg: Some(&mut cmsg),
            truncated: Some(truncated_out),
        },
    )?;

    if let Some(remote_addr) = remote_addr {
        socket_addr_ex_for_user_name(socket.ip_domain(), remote_addr)
            .write_to_user(addr, addrlen)?;
    }

    if let Some(mut builder) = cmsg_builder {
        for cmsg in cmsg {
            let pushed = match cmsg.downcast::<CMsg>() {
                Ok(cmsg) => match *cmsg {
                    CMsg::Rights { fds } => {
                        let body_len = fds.len() * size_of::<i32>();
                        builder.push_sized(SOL_SOCKET, SCM_RIGHTS, body_len, |data| {
                            let mut written = 0;
                            for (f, chunk) in
                                fds.into_iter().zip(data.chunks_exact_mut(size_of::<i32>()))
                            {
                                let fd = add_file_like(f, false)?;
                                chunk.copy_from_slice(&fd.to_ne_bytes());
                                written += size_of::<i32>();
                            }
                            Ok(written)
                        })?
                    }
                },
                Err(cmsg) => match cmsg.downcast::<IpCmsg>() {
                    Ok(cmsg) => match *cmsg {
                        IpCmsg::Ipv4Tos(tos) => {
                            builder.push_sized(PROTO_IP, IP_TOS, 1, |data| {
                                data[0] = tos;
                                Ok(1)
                            })?
                        }
                        IpCmsg::Ipv6TrafficClass(tclass) => builder.push_sized(
                            IPPROTO_IPV6 as u32,
                            IPV6_TCLASS,
                            size_of::<i32>(),
                            |data| {
                                data.copy_from_slice(&i32::from(tclass).to_ne_bytes());
                                Ok(size_of::<i32>())
                            },
                        )?,
                    },
                    Err(_) => {
                        warn!("received unexpected cmsg");
                        continue;
                    }
                },
            };
            if !pushed {
                break;
            }
        }
        builder.finish();
    }

    debug!("sys_recv => fd: {fd}, recv: {recv}");
    Ok(recv as isize)
}

pub fn sys_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: usize,
    flags: u32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> AxResult<isize> {
    let mut addrlen_value = if addr.is_null() { 0 } else { addrlen.read()? };
    let result = recv_impl(
        fd,
        VmBytesMut::new(buf, len),
        flags,
        addr,
        &mut addrlen_value,
        None,
        &mut false,
    );
    if result.is_ok() && !addr.is_null() {
        addrlen.write(addrlen_value)?;
    }
    result
}

pub fn sys_recvmsg(fd: i32, msg: UserPtr<msghdr>, flags: u32) -> AxResult<isize> {
    // SAFETY: msghdr consists of raw addresses and integer lengths/flags.
    // Every bit pattern is valid before the syscall validates each field.
    let mut msg_value = unsafe { msg.read_abi()? };
    let mut msg_namelen = decode_msg_namelen(msg_value.msg_namelen)?;
    let mut truncated = false;
    let recv = recv_impl(
        fd,
        IoVectorBuf::new(msg_value.msg_iov as *mut IoVec, msg_value.msg_iovlen)?.into_io(),
        flags,
        UserPtr::from(msg_value.msg_name as usize),
        &mut msg_namelen,
        (!msg_value.msg_control.is_null()).then(|| {
            CMsgBuilder::new(
                UserPtr::from(msg_value.msg_control as *mut cmsghdr),
                &mut msg_value.msg_controllen,
            )
        }),
        &mut truncated,
    );
    // Linux: on success, set msg.msg_flags to indicate truncation etc.
    match recv {
        Ok(received) => {
            write_msghdr_outputs(
                msg,
                encode_msg_namelen(msg_namelen)?,
                msg_value.msg_controllen,
                if truncated { MSG_TRUNC } else { 0 },
            )?;
            Ok(received)
        }
        Err(error) => Err(error),
    }
}

/// Send multiple datagrams in one syscall.
pub fn sys_sendmmsg(fd: i32, msgvec: UserPtr<mmsghdr>, vlen: u32, flags: u32) -> AxResult<isize> {
    if vlen == 0 {
        return Ok(0);
    }
    if vlen > MMSG_MAX_VLEN {
        return Err(AxError::InvalidInput);
    }

    let msgvec_ptr = msgvec;
    // SAFETY: mmsghdr/msghdr contain only raw addresses and integer fields.
    // Every bit pattern is valid before each message is validated below.
    let mut msgvec = unsafe { msgvec_ptr.read_abi_slice(vlen as usize)? };
    let mut sent = 0;
    for (index, msg) in msgvec.iter_mut().enumerate() {
        let cmsg = parse_send_cmsgs(msg.msg_hdr.msg_control as usize, msg.msg_hdr.msg_controllen)?;
        match send_impl(
            fd,
            IoVectorBuf::new(msg.msg_hdr.msg_iov as *const IoVec, msg.msg_hdr.msg_iovlen)?
                .into_io(),
            flags,
            UserConstPtr::from(msg.msg_hdr.msg_name as usize),
            decode_msg_namelen(msg.msg_hdr.msg_namelen)?,
            cmsg,
        ) {
            Ok(n) => {
                msg.msg_len = n as u32;
                write_mmsghdr_len(msgvec_ptr, index, msg.msg_len)?;
                sent += 1;
            }
            Err(e) => {
                if sent == 0 {
                    return Err(e);
                }
                break;
            }
        }
    }
    Ok(sent)
}

/// Receive multiple datagrams in one syscall.
pub fn sys_recvmmsg(
    fd: i32,
    msgvec: UserPtr<mmsghdr>,
    vlen: u32,
    flags: u32,
    timeout: UserConstPtr<timespec>,
) -> AxResult<isize> {
    if vlen == 0 {
        return Ok(0);
    }
    if vlen > MMSG_MAX_VLEN {
        return Err(AxError::InvalidInput);
    }

    let timeout = parse_recvmmsg_timeout(timeout)?;
    // TODO: deadline is only checked between recv_impl calls. If a single
    // recv_impl blocks waiting for data (socket has nothing to read), the
    // deadline cannot interrupt it. Needs a non-blocking recv path or
    // SO_RCVTIMEO support at the socket layer to fix.
    let deadline = timeout.map(|t| wall_time() + t);
    let _socket = Socket::from_fd(fd)?;
    let msgvec_ptr = msgvec;
    // SAFETY: mmsghdr/msghdr contain only raw addresses and integer fields.
    // Every bit pattern is valid before each message is validated below.
    let mut msgvec = unsafe { msgvec_ptr.read_abi_slice(vlen as usize)? };
    let mut received = 0;
    for (index, msg) in msgvec.iter_mut().enumerate() {
        if let Some(deadline) = deadline
            && wall_time() >= deadline
        {
            if received == 0 {
                return Err(AxError::WouldBlock);
            }
            break;
        }

        let mut msg_namelen = match decode_msg_namelen(msg.msg_hdr.msg_namelen) {
            Ok(value) => value,
            Err(error) if received == 0 => return Err(error),
            Err(_) => break,
        };
        let mut truncated = false;
        let recv = recv_impl(
            fd,
            IoVectorBuf::new(msg.msg_hdr.msg_iov as *mut IoVec, msg.msg_hdr.msg_iovlen)?.into_io(),
            flags,
            UserPtr::from(msg.msg_hdr.msg_name as usize),
            &mut msg_namelen,
            (!msg.msg_hdr.msg_control.is_null()).then(|| {
                CMsgBuilder::new(
                    UserPtr::from(msg.msg_hdr.msg_control as *mut cmsghdr),
                    &mut msg.msg_hdr.msg_controllen,
                )
            }),
            &mut truncated,
        );

        match recv {
            Ok(n) => {
                msg.msg_hdr.msg_namelen = encode_msg_namelen(msg_namelen)?;
                msg.msg_hdr.msg_flags = if truncated { MSG_TRUNC } else { 0 };
                msg.msg_len = n as u32;
                let msg_addr = mmsghdr_address(msgvec_ptr, index)?;
                write_msghdr_outputs(
                    UserPtr::from(msg_addr + core::mem::offset_of!(mmsghdr, msg_hdr)),
                    msg.msg_hdr.msg_namelen,
                    msg.msg_hdr.msg_controllen,
                    msg.msg_hdr.msg_flags,
                )?;
                write_mmsghdr_len(msgvec_ptr, index, msg.msg_len)?;
                received += 1;
            }
            Err(e) => {
                if received == 0 {
                    return Err(e);
                }
                break;
            }
        }
    }

    Ok(received)
}

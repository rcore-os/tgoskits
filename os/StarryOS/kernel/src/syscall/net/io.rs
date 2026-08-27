use alloc::{boxed::Box, vec::Vec};
use core::{net::Ipv4Addr, time::Duration};

use ax_io::prelude::*;
use ax_net::{
    CMsgData, IpCmsg, RecvFlags, RecvOptions, SendFlags, SendOptions, SocketAddrEx, SocketCmsg,
    SocketOps,
};
use ax_runtime::hal::time::wall_time;
use linux_raw_sys::{
    general::{timespec, timeval},
    net::{
        IP_TOS, IP_TTL, IPPROTO_IPV6, IPV6_TCLASS, MSG_CMSG_CLOEXEC, MSG_CTRUNC, MSG_DONTWAIT,
        MSG_OOB, MSG_PEEK, MSG_TRUNC, SCM_CREDENTIALS, SCM_RIGHTS, SCM_TIMESTAMP, SOL_SOCKET,
        cmsghdr, mmsghdr, msghdr, sockaddr, socklen_t, ucred,
    },
};

use super::addr::{
    SocketAddrExt, normalize_socket_addr_ex_for_ip_stack, socket_addr_ex_for_user_name,
};
use crate::{
    StarryError, StarryResult,
    file::{FileLike, PacketSocket, Socket, add_file_like, get_file_like, netlink::NetlinkSocket},
    mm::{IoVec, IoVectorBuf, UserConstPtr, UserPtr, VmBytes, VmBytesMut},
    syscall::net::{CMsg, CMsgBuilder, cmsg_space},
    time::TimeValueLike,
};

// Linux ABI for sendmmsg/recvmmsg limits vlen to UIO_MAXIOV (1024).
const MMSG_MAX_VLEN: u32 = 1024;
// recvmmsg-only flag (uapi/linux/socket.h): after the first datagram is
// received, the remaining recvs behave as if MSG_DONTWAIT were set.
const MSG_WAITFORONE: u32 = 0x10000;
const PROTO_IP: u32 = linux_raw_sys::net::IPPROTO_IP as u32;

fn parse_recvmmsg_timeout(timeout: UserConstPtr<timespec>) -> StarryResult<Option<Duration>> {
    if timeout.is_null() {
        return Ok(None);
    }
    let ts = timeout.get_as_ref()?;
    let tv = (*ts).try_into_time_value()?;
    Ok(Some(Duration::new(tv.as_secs(), tv.subsec_nanos())))
}

fn parse_send_cmsgs(control_ptr: usize, control_len: usize) -> StarryResult<Vec<CMsgData>> {
    let mut cmsg = Vec::new();
    if control_ptr == 0 || control_len == 0 {
        return Ok(cmsg);
    }

    let mut ptr = control_ptr;
    let ptr_end = ptr
        .checked_add(control_len)
        .ok_or(StarryError::InvalidInput)?;

    while let Some(next) = ptr.checked_add(size_of::<cmsghdr>()) {
        if next > ptr_end {
            break;
        }

        let hdr = UserConstPtr::<cmsghdr>::from(ptr).get_as_ref()?;
        if hdr.cmsg_len < size_of::<cmsghdr>() || ptr_end - ptr < hdr.cmsg_len {
            return Err(StarryError::InvalidInput);
        }

        let Some(next_ptr) = cmsg_space(hdr.cmsg_len - size_of::<cmsghdr>())
            .and_then(|space| ptr.checked_add(space))
        else {
            return Err(StarryError::InvalidInput);
        };

        cmsg.push(Box::new(CMsg::parse(hdr)?) as CMsgData);
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
) -> StarryResult<isize> {
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
            return Err(StarryError::InvalidInput);
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
            Socket::with_current_sender_credentials(SendOptions {
                to: addr,
                flags: send_flags,
                cmsg,
                ..Default::default()
            }),
        )?;

        return Ok(sent as isize);
    }

    if let Ok(netlink) = NetlinkSocket::from_fd(fd) {
        let sent = netlink.write(&mut src)?;
        return Ok(sent as isize);
    }

    get_file_like(fd)?;
    Err(StarryError::NotASocket)
}

pub fn sys_sendto(
    fd: i32,
    buf: *const u8,
    len: usize,
    flags: u32,
    addr: UserConstPtr<sockaddr>,
    addrlen: socklen_t,
) -> StarryResult<isize> {
    send_impl(fd, VmBytes::new(buf, len), flags, addr, addrlen, Vec::new())
}

pub fn sys_sendmsg(fd: i32, msg: UserConstPtr<msghdr>, flags: u32) -> StarryResult<isize> {
    let msg = msg.get_as_ref()?;
    let cmsg = parse_send_cmsgs(msg.msg_control as usize, msg.msg_controllen)?;
    send_impl(
        fd,
        IoVectorBuf::new(msg.msg_iov as *const IoVec, msg.msg_iovlen)?.into_io(),
        flags,
        UserConstPtr::from(msg.msg_name as usize),
        msg.msg_namelen as socklen_t,
        cmsg,
    )
}

// Data-truncation and control-truncation are reported through separate out
// flags because they feed different sinks (one into RecvOptions, one set
// directly), so they stay as distinct parameters rather than a bundled struct.
#[allow(clippy::too_many_arguments)]
fn recv_impl(
    fd: i32,
    mut dst: impl Write + IoBufMut,
    flags: u32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
    mut cmsg_builder: Option<CMsgBuilder>,
    truncated_out: &mut bool,
    control_truncated_out: &mut bool,
) -> StarryResult<isize> {
    debug!("sys_recv <= fd: {fd}, flags: {flags}");

    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let (recv, from) = packet.recv_packet(&mut dst)?;
        if !addr.is_null() {
            from.write_to_user(
                addr.address().as_usize() as *mut sockaddr,
                addrlen.get_as_mut()?,
            )?;
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
                super::addr::write_netlink_addr(
                    &netlink.kernel_addr(),
                    addr,
                    addrlen.get_as_mut()?,
                )?;
            }
            if let Some(builder) = cmsg_builder.take() {
                builder.finish();
            }
            return Ok(recv as isize);
        }

        get_file_like(fd)?;
        return Err(StarryError::NotASocket);
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
    if flags & MSG_OOB != 0 {
        recv_flags |= RecvFlags::OOB;
    }
    // Received SCM_RIGHTS fds get O_CLOEXEC when the caller passes
    // MSG_CMSG_CLOEXEC (recvmsg(2)); Linux net/core/scm.c scm_detach_fds.
    let cmsg_cloexec = flags & MSG_CMSG_CLOEXEC != 0;

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
            .write_to_user(addr, addrlen.get_as_mut()?)?;
    }

    if cmsg_builder.is_none() && !cmsg.is_empty() {
        *control_truncated_out = true;
    }
    if let Some(mut builder) = cmsg_builder {
        for cmsg in cmsg {
            let pushed = match cmsg.into_any().downcast::<CMsg>() {
                Ok(cmsg) => match *cmsg {
                    CMsg::Rights { fds } => {
                        // Deliver as many fds as fit; excess are dropped (closed)
                        // and MSG_CTRUNC is flagged, matching Linux scm_detach_fds.
                        let total = fds.len();
                        let install = total.min(builder.rights_capacity());
                        if install < total {
                            *control_truncated_out = true;
                        }
                        if install == 0 {
                            false
                        } else {
                            let body_len = install * size_of::<i32>();
                            builder.push_sized(SOL_SOCKET, SCM_RIGHTS, body_len, |data| {
                                let mut written = 0;
                                for (f, chunk) in fds
                                    .into_iter()
                                    .take(install)
                                    .zip(data.as_chunks_mut::<{ size_of::<i32>() }>().0)
                                {
                                    let fd = add_file_like(f, cmsg_cloexec)?;
                                    chunk.copy_from_slice(&fd.to_ne_bytes());
                                    written += size_of::<i32>();
                                }
                                Ok(written)
                            })?
                        }
                    }
                },
                Err(cmsg) => match cmsg.downcast::<IpCmsg>() {
                    Ok(cmsg) => match *cmsg {
                        IpCmsg::Ipv4Ttl(ttl) => {
                            builder.push_sized(PROTO_IP, IP_TTL, size_of::<i32>(), |data| {
                                data.copy_from_slice(&i32::from(ttl).to_ne_bytes());
                                Ok(size_of::<i32>())
                            })?
                        }
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
                    Err(cmsg) => match cmsg.downcast::<SocketCmsg>() {
                        Ok(cmsg) => match *cmsg {
                            SocketCmsg::Credentials(credentials) => {
                                let credentials = Socket::project_unix_credentials(&credentials);
                                builder.push_sized(
                                    SOL_SOCKET,
                                    SCM_CREDENTIALS,
                                    size_of::<ucred>(),
                                    |data| {
                                        let credentials = ucred {
                                            pid: credentials.pid as _,
                                            uid: credentials.uid,
                                            gid: credentials.gid,
                                        };
                                        // SAFETY: `credentials` lives through the
                                        // copy, and `ucred` is a plain C ABI record.
                                        data.copy_from_slice(unsafe {
                                            core::slice::from_raw_parts(
                                                (&credentials as *const ucred).cast::<u8>(),
                                                size_of::<ucred>(),
                                            )
                                        });
                                        Ok(size_of::<ucred>())
                                    },
                                )?
                            }
                            SocketCmsg::Timestamp(timestamp) => builder.push_sized(
                                SOL_SOCKET,
                                SCM_TIMESTAMP,
                                size_of::<timeval>(),
                                |data| {
                                    let timestamp = timeval::from_time_value(timestamp);
                                    // SAFETY: `timestamp` lives through the
                                    // copy, and `timeval` is a plain C ABI
                                    // record with no padding requirements for
                                    // reading its initialized byte layout.
                                    data.copy_from_slice(unsafe {
                                        core::slice::from_raw_parts(
                                            (&timestamp as *const timeval).cast::<u8>(),
                                            size_of::<timeval>(),
                                        )
                                    });
                                    Ok(size_of::<timeval>())
                                },
                            )?,
                        },
                        Err(_) => {
                            warn!("received unexpected cmsg");
                            continue;
                        }
                    },
                },
            };
            if !pushed {
                *control_truncated_out = true;
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
) -> StarryResult<isize> {
    recv_impl(
        fd,
        VmBytesMut::new(buf, len),
        flags,
        addr,
        addrlen,
        None,
        &mut false,
        &mut false,
    )
}

pub fn sys_recvmsg(fd: i32, msg: UserPtr<msghdr>, flags: u32) -> StarryResult<isize> {
    let msg = msg.get_as_mut()?;
    let mut truncated = false;
    let mut control_truncated = false;
    let recv = recv_impl(
        fd,
        IoVectorBuf::new(msg.msg_iov as *mut IoVec, msg.msg_iovlen)?.into_io(),
        flags,
        UserPtr::from(msg.msg_name as usize),
        UserPtr::from(&mut msg.msg_namelen as *mut _ as *mut socklen_t),
        (!msg.msg_control.is_null()).then(|| {
            CMsgBuilder::new(
                UserPtr::from(msg.msg_control as *mut cmsghdr),
                &mut msg.msg_controllen,
            )
        }),
        &mut truncated,
        &mut control_truncated,
    );
    // Linux: on success, set msg.msg_flags to indicate truncation etc.
    if recv.is_ok() {
        let mut mf = 0;
        if truncated {
            mf |= MSG_TRUNC;
        }
        if control_truncated {
            mf |= MSG_CTRUNC;
        }
        msg.msg_flags = mf;
    }
    recv
}

/// Send multiple datagrams in one syscall.
pub fn sys_sendmmsg(
    fd: i32,
    msgvec: UserPtr<mmsghdr>,
    vlen: u32,
    flags: u32,
) -> StarryResult<isize> {
    if vlen == 0 {
        return Ok(0);
    }
    // Linux clamps vlen to UIO_MAXIOV and proceeds (net/socket.c:2796); it
    // never rejects an over-cap batch with EINVAL.
    let vlen = vlen.min(MMSG_MAX_VLEN);

    let msgvec = msgvec.get_as_mut_slice(vlen as usize)?;
    let mut sent = 0;
    for msg in msgvec.iter_mut() {
        let cmsg = parse_send_cmsgs(msg.msg_hdr.msg_control as usize, msg.msg_hdr.msg_controllen)?;
        match send_impl(
            fd,
            IoVectorBuf::new(msg.msg_hdr.msg_iov as *const IoVec, msg.msg_hdr.msg_iovlen)?
                .into_io(),
            flags,
            UserConstPtr::from(msg.msg_hdr.msg_name as usize),
            msg.msg_hdr.msg_namelen as socklen_t,
            cmsg,
        ) {
            Ok(n) => {
                msg.msg_len = n as u32;
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
) -> StarryResult<isize> {
    if vlen == 0 {
        return Ok(0);
    }
    // Linux do_recvmmsg does not cap vlen; StarryOS bounds the batch to
    // UIO_MAXIOV so `get_as_mut_slice` copies a bounded user array. Clamp
    // rather than reject with EINVAL so an over-cap batch still makes
    // progress, matching sendmmsg's UIO_MAXIOV clamp (net/socket.c:2796).
    let vlen = vlen.min(MMSG_MAX_VLEN);

    let timeout = parse_recvmmsg_timeout(timeout)?;
    // TODO: deadline is only checked between recv_impl calls. If a single
    // recv_impl blocks waiting for data (socket has nothing to read), the
    // deadline cannot interrupt it. Needs a non-blocking recv path or
    // SO_RCVTIMEO support at the socket layer to fix.
    let deadline = timeout.map(|t| wall_time() + t);
    let _socket = Socket::from_fd(fd)?;
    let msgvec = msgvec.get_as_mut_slice(vlen as usize)?;
    let mut received = 0;
    let mut flags = flags;
    for msg in msgvec.iter_mut() {
        if let Some(deadline) = deadline
            && wall_time() >= deadline
        {
            if received == 0 {
                return Err(StarryError::WouldBlock);
            }
            break;
        }

        let recv = recv_impl(
            fd,
            IoVectorBuf::new(msg.msg_hdr.msg_iov as *mut IoVec, msg.msg_hdr.msg_iovlen)?.into_io(),
            flags,
            UserPtr::from(msg.msg_hdr.msg_name as usize),
            UserPtr::from(&mut msg.msg_hdr.msg_namelen as *mut _ as *mut socklen_t),
            (!msg.msg_hdr.msg_control.is_null()).then(|| {
                CMsgBuilder::new(
                    UserPtr::from(msg.msg_hdr.msg_control as *mut cmsghdr),
                    &mut msg.msg_hdr.msg_controllen,
                )
            }),
            &mut false,
            &mut false,
        );

        match recv {
            Ok(n) => {
                msg.msg_len = n as u32;
                received += 1;
                // MSG_WAITFORONE: once a datagram is received, remaining
                // recvs must not block (Linux do_recvmmsg net/socket.c:3055
                // sets MSG_DONTWAIT after the first packet). Without this a
                // vlen>1 recvmmsg on a socket with fewer datagrams blocks
                // forever on the next recv.
                if flags & MSG_WAITFORONE != 0 {
                    flags |= MSG_DONTWAIT;
                }
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

#[cfg(all(test, not(axtest)))]
fn net_io_constants_hold_for_test() -> bool {
    const {
        assert!(MMSG_MAX_VLEN == 1024);
        assert!(PROTO_IP == 0);
    }

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn net_io_constants_hold() {
        assert!(super::net_io_constants_hold_for_test());
    }
}

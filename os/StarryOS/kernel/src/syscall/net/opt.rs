use alloc::vec;

use ax_errno::{AxError, AxResult, LinuxError};
use ax_net::{
    InterfaceId,
    options::{Configurable, GetSocketOption, SetSocketOption, TcpInfo, TcpInfoOptions, TcpState},
};
use linux_raw_sys::net::{
    AF_INET6, IP_TOS, IPPROTO_IPV6, IPV6_TCLASS, IPV6_V6ONLY, TCP_INFO, TCPI_OPT_ECN,
    TCPI_OPT_ECN_SEEN, TCPI_OPT_SACK, TCPI_OPT_SYN_DATA, TCPI_OPT_TIMESTAMPS, TCPI_OPT_WSCALE,
    socklen_t, tcp_info,
};
use starry_vm::vm_write_slice;

use crate::{
    file::{FileLike, Socket, netlink::NetlinkSocket},
    mm::{UserConstPtr, UserPtr},
};

const PROTO_TCP: u32 = linux_raw_sys::net::IPPROTO_TCP as u32;

const PROTO_IP: u32 = linux_raw_sys::net::IPPROTO_IP as u32;

const IP_TOS_ECN_MASK: u8 = 0x03;

fn read_int_sockopt(optval: UserConstPtr<u8>, optlen: socklen_t) -> AxResult<i32> {
    if (optlen as usize) < size_of::<i32>() {
        return Err(AxError::InvalidInput);
    }
    Ok(*optval.cast::<i32>().get_as_ref()?)
}

fn normalize_ip_tos(value: i32) -> u8 {
    (value as u8) & !IP_TOS_ECN_MASK
}

fn normalize_ipv6_tclass(value: i32) -> AxResult<u8> {
    if value == -1 {
        return Ok(0);
    }
    if !(0..=u8::MAX as i32).contains(&value) {
        return Err(AxError::InvalidInput);
    }
    Ok(normalize_ip_tos(value))
}

fn read_bind_to_device(
    optval: UserConstPtr<u8>,
    optlen: socklen_t,
) -> AxResult<Option<InterfaceId>> {
    if optlen == 0 {
        return Ok(None);
    }
    let buf = optval.get_as_slice(optlen as usize)?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    if end == 0 {
        return Ok(None);
    }
    let name = core::str::from_utf8(&buf[..end]).map_err(|_| AxError::InvalidInput)?;
    ax_net::interface_by_name(name)
        .map(|info| Some(info.id))
        .ok_or(AxError::NoSuchDevice)
}

fn write_bind_to_device(
    socket: &Socket,
    optval: UserPtr<u8>,
    optlen: &mut socklen_t,
) -> AxResult<()> {
    let mut binding = None;
    socket.get_option(GetSocketOption::BindToDevice(&mut binding))?;
    let name = binding
        .and_then(ax_net::interface_by_id)
        .map(|info| info.name)
        .unwrap_or_default();
    let bytes = name.as_bytes();
    let write_len = (*optlen as usize).min(bytes.len() + 1);
    *optlen = write_len as socklen_t;
    if write_len == 0 {
        return Ok(());
    }
    let mut out = vec![0u8; write_len];
    let name_len = write_len.saturating_sub(1).min(bytes.len());
    out[..name_len].copy_from_slice(&bytes[..name_len]);
    Ok(vm_write_slice(optval.as_ptr(), &out)?)
}

fn tcp_state_to_linux(state: TcpState) -> u8 {
    match state {
        TcpState::Established => 1,
        TcpState::SynSent => 2,
        TcpState::SynReceived => 3,
        TcpState::FinWait1 => 4,
        TcpState::FinWait2 => 5,
        TcpState::TimeWait => 6,
        TcpState::Closed => 7,
        TcpState::CloseWait => 8,
        TcpState::LastAck => 9,
        TcpState::Listen => 10,
        TcpState::Closing => 11,
    }
}

fn tcp_options_to_linux(options: TcpInfoOptions) -> u8 {
    let mut raw = 0u8;
    if options.contains(TcpInfoOptions::TIMESTAMPS) {
        raw |= TCPI_OPT_TIMESTAMPS as u8;
    }
    if options.contains(TcpInfoOptions::SACK) {
        raw |= TCPI_OPT_SACK as u8;
    }
    if options.contains(TcpInfoOptions::WSCALE) {
        raw |= TCPI_OPT_WSCALE as u8;
    }
    if options.contains(TcpInfoOptions::ECN) {
        raw |= TCPI_OPT_ECN as u8;
    }
    if options.contains(TcpInfoOptions::ECN_SEEN) {
        raw |= TCPI_OPT_ECN_SEEN as u8;
    }
    if options.contains(TcpInfoOptions::SYN_DATA) {
        raw |= TCPI_OPT_SYN_DATA as u8;
    }
    raw
}

fn to_linux_tcp_info(info: TcpInfo) -> tcp_info {
    // SAFETY: linux-raw-sys tcp_info is a plain C data record. An all-zero
    // value is a valid baseline before the supported fields are filled below.
    let mut raw = unsafe { core::mem::zeroed::<tcp_info>() };
    raw.tcpi_state = tcp_state_to_linux(info.state);
    raw.tcpi_ca_state = info.ca_state;
    raw.tcpi_retransmits = info.retransmits;
    raw.tcpi_probes = info.probes;
    raw.tcpi_backoff = info.backoff;
    raw.tcpi_options = tcp_options_to_linux(info.options);
    raw.set_tcpi_snd_wscale(info.snd_wscale);
    raw.set_tcpi_rcv_wscale(info.rcv_wscale);
    raw.tcpi_rto = info.rto_micros;
    raw.tcpi_ato = info.ato_micros;
    raw.tcpi_snd_mss = info.snd_mss;
    raw.tcpi_rcv_mss = info.rcv_mss;
    raw.tcpi_notsent_bytes = info.notsent_bytes;
    raw.tcpi_pmtu = info.pmtu;
    raw.tcpi_advmss = info.advmss;
    raw.tcpi_snd_cwnd = info.snd_cwnd;
    raw.tcpi_reordering = info.reordering;
    raw.tcpi_rcv_space = info.rcv_space;
    raw.tcpi_snd_wnd = info.snd_wnd;
    raw.tcpi_rcv_wnd = info.rcv_wnd;
    raw
}

fn write_tcp_info(socket: &Socket, optval: UserPtr<u8>, optlen: &mut socklen_t) -> AxResult<()> {
    let mut info = TcpInfo::default();
    socket.get_option(GetSocketOption::TcpInfo(&mut info))?;

    let write_len = (*optlen as usize).min(size_of::<tcp_info>());
    *optlen = write_len as socklen_t;
    if write_len == 0 {
        return Ok(());
    }

    let raw = to_linux_tcp_info(info);
    // SAFETY: raw lives for the whole copy and is viewed as its C byte layout.
    let raw_bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw as *const tcp_info).cast::<u8>(),
            size_of::<tcp_info>(),
        )
    };
    Ok(vm_write_slice(optval.as_ptr(), &raw_bytes[..write_len])?)
}

fn ensure_ipv6_socket(socket: &Socket) -> AxResult<()> {
    if socket.ip_domain() == AF_INET6 {
        Ok(())
    } else {
        Err(AxError::from(LinuxError::ENOPROTOOPT))
    }
}

mod conv {
    use ax_errno::{AxError, AxResult};
    use ax_net::options::UnixCredentials;
    use linux_raw_sys::{general::timeval, net::ucred};

    use crate::time::TimeValueLike;

    pub struct Int<T>(T);

    impl<T: TryFrom<i32> + TryInto<i32>> Int<T> {
        pub fn sys_to_rust(val: i32) -> AxResult<T> {
            T::try_from(val).map_err(|_| AxError::InvalidInput)
        }

        pub fn rust_to_sys(val: T) -> AxResult<i32> {
            val.try_into().map_err(|_| AxError::InvalidInput)
        }
    }

    pub struct IntBool;

    impl IntBool {
        pub fn sys_to_rust(val: i32) -> AxResult<bool> {
            Ok(val != 0)
        }

        pub fn rust_to_sys(val: bool) -> AxResult<i32> {
            Ok(val as _)
        }
    }

    pub struct Duration;

    impl Duration {
        pub fn sys_to_rust(val: timeval) -> AxResult<core::time::Duration> {
            val.try_into_time_value()
        }

        pub fn rust_to_sys(val: core::time::Duration) -> AxResult<timeval> {
            Ok(timeval::from_time_value(val))
        }
    }

    pub struct Ucred;

    impl Ucred {
        pub fn sys_to_rust(val: ucred) -> AxResult<UnixCredentials> {
            Ok(UnixCredentials {
                pid: val.pid,
                uid: val.uid,
                gid: val.gid,
            })
        }

        pub fn rust_to_sys(val: UnixCredentials) -> AxResult<ucred> {
            Ok(ucred {
                pid: val.pid,
                uid: val.uid,
                gid: val.gid,
            })
        }
    }
}

macro_rules! call_dispatch {
    ($dispatch:ident, $pat:expr) => {{
        use conv::*;
        use linux_raw_sys::net::*;

        call_dispatch! {
            $dispatch, $pat,
            // ---- Implemented socket options ----
            (SOL_SOCKET, SO_REUSEADDR) => ReuseAddress as IntBool,
            (SOL_SOCKET, SO_ERROR) => Error,
            (SOL_SOCKET, SO_DONTROUTE) => DontRoute as IntBool,   // stored but routing logic ignores it
            (SOL_SOCKET, SO_SNDBUF) => SendBuffer as Int<usize>,  // TODO: set is no-op, smoltcp uses fixed buffer
            (SOL_SOCKET, SO_RCVBUF) => ReceiveBuffer as Int<usize>,// TODO: set is no-op, smoltcp uses fixed buffer
            (SOL_SOCKET, SO_KEEPALIVE) => KeepAlive as IntBool,
            (SOL_SOCKET, SO_RCVTIMEO) => ReceiveTimeout as Duration,
            (SOL_SOCKET, SO_SNDTIMEO) => SendTimeout as Duration,
            (SOL_SOCKET, SO_PASSCRED) => PassCredentials as IntBool, // TODO: set accepted but no-op for non-unix
            (SOL_SOCKET, SO_PEERCRED) => PeerCredentials as Ucred,
            (SOL_SOCKET, SO_TYPE) => SocketType as Int<i32>,       // read-only
            (SOL_SOCKET, SO_PROTOCOL) => SocketProtocol as Int<i32>,// read-only
            (SOL_SOCKET, SO_DOMAIN) => SocketDomain as Int<i32>,   // read-only
            (SOL_SOCKET, SO_PRIORITY) => Priority as Int<i32>,      // stored; qdisc/device priority is not modeled

            (PROTO_TCP, TCP_NODELAY) => NoDelay as IntBool,
            (PROTO_TCP, TCP_MAXSEG) => MaxSegment as Int<usize>,  // TODO: hardcoded 1460, get actual MSS
            (PROTO_TCP, TCP_KEEPIDLE) => TcpKeepIdle as Int<u32>,
            (PROTO_TCP, TCP_KEEPINTVL) => TcpKeepInterval as Int<u32>,
            (PROTO_TCP, TCP_KEEPCNT) => TcpKeepCount as Int<u32>,
            (PROTO_TCP, TCP_USER_TIMEOUT) => TcpUserTimeout as Int<u32>,

            (PROTO_IP, IP_TTL) => Ttl as Int<u8>,
            (PROTO_IP, IP_RECVERR) => RecvErr as IntBool,  // TODO: hardcoded false, no errqueue support
            // ---- Not yet implemented (add as needed) ----
            // (SOL_SOCKET, SO_LINGER) => ...,         // TODO: needs close() linger semantics
            // (SOL_SOCKET, SO_REUSEPORT) => ...,     // TODO: needs kernel support
            // (SOL_SOCKET, SO_RCVLOWAT) => ...,       // TODO: needs kernel support
            // (SOL_SOCKET, SO_SNDLOWAT) => ...,       // TODO: needs kernel support
            // (PROTO_TCP, TCP_CORK) => ...,           // TODO: needs smoltcp support
            // (PROTO_TCP, TCP_DEFER_ACCEPT) => ...,   // TODO: needs kernel support
            // (PROTO_TCP, TCP_QUICKACK) => ...,       // TODO: needs kernel support
            // (PROTO_TCP, TCP_SYNCNT) => ...,         // TODO: needs kernel support
            // (PROTO_TCP, TCP_WINDOW_CLAMP) => ...,   // TODO: needs kernel support
            // (PROTO_IP, IP_OPTIONS) => ...,          // TODO: needs kernel support
            // (IPPROTO_IPV6, IPV6_V6ONLY) => ...,     // TODO: currently hardcoded inline
        }
    }};
    ($dispatch:ident, $in:expr, $($pat:pat => $which:ident $(as $conv:ty)?),* $(,)?) => {
        match $in {
            $(
                $pat => {
                    dispatch!($which $(as $conv)?);
                }
            )*
            _ => return Err(AxError::from(LinuxError::ENOPROTOOPT)),
        }
    }
}

pub fn sys_getsockopt(
    fd: i32,
    level: u32,
    optname: u32,
    optval: UserPtr<u8>,
    optlen: UserPtr<socklen_t>,
) -> AxResult<isize> {
    let optlen = optlen.get_as_mut()?;
    debug!(
        "sys_getsockopt <= fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
        fd,
        level,
        optname,
        optval.address(),
        optlen,
    );

    fn get<'a, T: 'static>(val: UserPtr<u8>, len: &mut socklen_t) -> AxResult<&'a mut T> {
        if (*len as usize) < size_of::<T>() {
            return Err(AxError::InvalidInput);
        }
        *len = size_of::<T>() as socklen_t;
        val.cast().get_as_mut()
    }

    let socket = Socket::from_fd(fd)?;

    // SO_TYPE is handled at the kernel level because the socket type is
    // known from the Socket enum variant, not from a per-protocol option.
    {
        use ax_net::Socket as SocketInner;
        use linux_raw_sys::net::{
            SO_BINDTODEVICE, SO_TYPE, SOCK_DGRAM, SOCK_RAW, SOCK_STREAM, SOL_SOCKET,
        };

        if level == SOL_SOCKET && optname == SO_TYPE {
            if *optlen == 0 {
                return Ok(0);
            }
            let so_type = match &**socket {
                SocketInner::Tcp(_) => SOCK_STREAM,
                SocketInner::Udp(_) => SOCK_DGRAM,
                SocketInner::Raw(_) => SOCK_RAW,
                SocketInner::Unix(_) => SOCK_STREAM,
                #[cfg(feature = "vsock")]
                SocketInner::Vsock(_) => SOCK_STREAM,
            };
            *get(optval, optlen)? = so_type as i32;
            return Ok(0);
        }
        if level == SOL_SOCKET && optname == SO_BINDTODEVICE {
            write_bind_to_device(&socket, optval, optlen)?;
            return Ok(0);
        }
    }

    if level == IPPROTO_IPV6 as u32 && optname == IPV6_V6ONLY {
        // TODO: Store and enforce IPV6_V6ONLY once native IPv6 sockets exist.
        *get::<i32>(optval, optlen)? = 0;
        return Ok(0);
    }

    if level == PROTO_IP && optname == IP_TOS {
        let mut tos = 0;
        socket.get_option(GetSocketOption::IpTos(&mut tos))?;
        *get::<i32>(optval, optlen)? = i32::from(tos);
        return Ok(0);
    }

    if level == IPPROTO_IPV6 as u32 && optname == IPV6_TCLASS {
        ensure_ipv6_socket(&socket)?;
        let mut tclass = 0;
        socket.get_option(GetSocketOption::IpTos(&mut tclass))?;
        *get::<i32>(optval, optlen)? = i32::from(tclass);
        return Ok(0);
    }

    if level == PROTO_TCP && optname == TCP_INFO {
        write_tcp_info(&socket, optval, optlen)?;
        return Ok(0);
    }

    macro_rules! dispatch {
        ($which:ident) => {
            socket.get_option(GetSocketOption::$which(get(optval, optlen)?))?;
        };
        ($which:ident as $conv:ty) => {
            let mut val = Default::default();
            socket.get_option(GetSocketOption::$which(&mut val))?;
            *get(optval, optlen)? = <$conv>::rust_to_sys(val)?;
        };
    }
    call_dispatch!(dispatch, (level, optname));

    Ok(0)
}

pub fn sys_setsockopt(
    fd: i32,
    level: u32,
    optname: u32,
    optval: UserConstPtr<u8>,
    optlen: socklen_t,
) -> AxResult<isize> {
    debug!(
        "sys_setsockopt <= fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
        fd,
        level,
        optname,
        optval.address(),
        optlen
    );

    if let Ok(socket) = NetlinkSocket::from_fd(fd) {
        use linux_raw_sys::net::{
            SO_ATTACH_FILTER, SO_LOCK_FILTER, SO_PASSCRED, SO_RCVBUF, SO_RCVBUFFORCE, SOL_SOCKET,
        };

        match (level, optname) {
            (SOL_SOCKET, SO_ATTACH_FILTER | SO_LOCK_FILTER) => {
                return Ok(0);
            }
            (SOL_SOCKET, SO_RCVBUF | SO_RCVBUFFORCE) => {
                let value = read_int_sockopt(optval, optlen)?;
                socket.set_receive_buffer_size(value.max(0) as usize);
                return Ok(0);
            }
            (SOL_SOCKET, SO_PASSCRED) => {
                let value = read_int_sockopt(optval, optlen)?;
                socket.set_passcred(value != 0);
                return Ok(0);
            }
            _ => return Err(AxError::from(LinuxError::ENOPROTOOPT)),
        }
    }

    {
        use linux_raw_sys::net::{SO_BINDTODEVICE, SO_BROADCAST, SOL_SOCKET};

        if (level, optname) == (SOL_SOCKET, SO_BROADCAST) {
            let _ = read_int_sockopt(optval, optlen)?;
            return Ok(0);
        }
        if (level, optname) == (SOL_SOCKET, SO_BINDTODEVICE) {
            let binding = read_bind_to_device(optval, optlen)?;
            Socket::from_fd(fd)?.set_option(SetSocketOption::BindToDevice(&binding))?;
            return Ok(0);
        }
    }

    fn get<'a, T: 'static>(val: UserConstPtr<u8>, len: socklen_t) -> AxResult<&'a T> {
        if len as usize != size_of::<T>() {
            return Err(AxError::InvalidInput);
        }
        val.cast().get_as_ref()
    }

    let socket = Socket::from_fd(fd)?;
    if level == PROTO_TCP && optname == TCP_INFO {
        return Err(AxError::from(LinuxError::ENOPROTOOPT));
    }

    if level == IPPROTO_IPV6 as u32 && optname == IPV6_V6ONLY {
        // TODO: Store and enforce IPV6_V6ONLY once native IPv6 sockets exist.
        let _ = *get::<i32>(optval, optlen)?;
        return Ok(0);
    }

    if level == PROTO_IP && optname == IP_TOS {
        let tos = normalize_ip_tos(*get::<i32>(optval, optlen)?);
        socket.set_option(SetSocketOption::IpTos(&tos))?;
        return Ok(0);
    }

    if level == IPPROTO_IPV6 as u32 && optname == IPV6_TCLASS {
        ensure_ipv6_socket(&socket)?;
        let tclass = normalize_ipv6_tclass(*get::<i32>(optval, optlen)?)?;
        socket.set_option(SetSocketOption::IpTos(&tclass))?;
        return Ok(0);
    }

    macro_rules! dispatch {
        ($which:ident) => {
            socket.set_option(SetSocketOption::$which(get(optval, optlen)?))?;
        };
        ($which:ident as $conv:ty) => {
            let mut val = <$conv>::sys_to_rust(*get(optval, optlen)?)?;
            socket.set_option(SetSocketOption::$which(&mut val))?;
        };
    }
    call_dispatch!(dispatch, (level, optname));

    Ok(0)
}

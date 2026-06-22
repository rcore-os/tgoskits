//! Socket option data structures and dispatch traits.
//!
//! This module defines Linux-compatible option payloads plus the `Configurable`
//! trait used by each socket implementation to handle getsockopt/setsockopt.
//! The goal is to keep the syscall layer protocol-neutral: it builds one option
//! request enum, then the concrete socket decides which values it supports.
//!
//! # Compatibility Boundary
//!
//! Option structs model Linux-visible ABI state, but not every field maps to a
//! smoltcp feature. Unsupported or synthetic fields should be filled
//! conservatively in the socket implementation, with defaults documented near
//! the protocol that reports them.

use core::time::Duration;

use ax_errno::{AxError, AxResult, LinuxError};
use enum_dispatch::enum_dispatch;

use crate::InterfaceId;

/// Linux-like TCP connection state reported by TCP_INFO.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    /// No active TCP connection.
    #[default]
    Closed,
    /// Passive listener.
    Listen,
    /// SYN sent, waiting for a matching SYN+ACK.
    SynSent,
    /// SYN received, waiting for the final ACK.
    SynReceived,
    /// Fully established connection.
    Established,
    /// Local endpoint has closed and sent FIN.
    FinWait1,
    /// Local FIN has been acknowledged.
    FinWait2,
    /// Remote endpoint has closed first.
    CloseWait,
    /// Both endpoints have closed simultaneously.
    Closing,
    /// Waiting for ACK of the local FIN after remote close.
    LastAck,
    /// Waiting for delayed packets to expire.
    TimeWait,
}

bitflags::bitflags! {
    /// Negotiated TCP options reported by TCP_INFO.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub struct TcpInfoOptions: u8 {
        /// TCP timestamps are enabled.
        const TIMESTAMPS = 1 << 0;
        /// Selective ACK is enabled.
        const SACK = 1 << 1;
        /// Window scaling is enabled.
        const WSCALE = 1 << 2;
        /// Explicit congestion notification is enabled.
        const ECN = 1 << 3;
        /// ECN has been seen on the connection.
        const ECN_SEEN = 1 << 4;
        /// SYN data was used by the connection.
        const SYN_DATA = 1 << 5;
    }
}

/// Transport-independent TCP_INFO snapshot.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TcpInfo {
    /// Current TCP state.
    pub state: TcpState,
    /// Congestion-avoidance state. Zero means open.
    pub ca_state: u8,
    /// Number of unacknowledged retransmits.
    pub retransmits: u8,
    /// Number of pending keepalive or zero-window probes.
    pub probes: u8,
    /// Exponential backoff counter.
    pub backoff: u8,
    /// Negotiated TCP options.
    pub options: TcpInfoOptions,
    /// Send window scale.
    pub snd_wscale: u8,
    /// Receive window scale.
    pub rcv_wscale: u8,
    /// Retransmission timeout in microseconds.
    pub rto_micros: u32,
    /// Delayed ACK timeout in microseconds.
    pub ato_micros: u32,
    /// Send maximum segment size.
    pub snd_mss: u32,
    /// Receive maximum segment size.
    pub rcv_mss: u32,
    /// Bytes currently queued for transmit.
    pub notsent_bytes: u32,
    /// Advertised path MTU.
    pub pmtu: u32,
    /// Advertised maximum segment size.
    pub advmss: u32,
    /// Current send congestion window, in segments.
    pub snd_cwnd: u32,
    /// Packet reordering tolerance.
    pub reordering: u32,
    /// Available receive buffer space.
    pub rcv_space: u32,
    /// Current send window estimate.
    pub snd_wnd: u32,
    /// Current receive window estimate.
    pub rcv_wnd: u32,
}

macro_rules! define_options {
    ($($name:ident($value:ty),)*) => {
        /// Operation to get a socket option.
        ///
        /// See [`Configurable::get_option`].
        #[allow(missing_docs)]
        pub enum GetSocketOption<'a> {
            $(
                $name(&'a mut $value),
            )*
        }

        /// Operation to set a socket option.
        ///
        /// See [`Configurable::set_option`].
        #[allow(missing_docs)]
        #[derive(Clone, Copy)]
        pub enum SetSocketOption<'a> {
            $(
                $name(&'a $value),
            )*
        }
    };
}

/// Corresponds to `struct ucred` in Linux.
#[repr(C)]
#[derive(Default, Debug, Clone)]
pub struct UnixCredentials {
    /// Process ID.
    pub pid: u32,
    /// User ID.
    pub uid: u32,
    /// Group ID.
    pub gid: u32,
}
impl UnixCredentials {
    /// Create a new `UnixCredentials` with the given PID and default UID/GID.
    pub fn new(pid: u32) -> Self {
        UnixCredentials {
            pid,
            uid: 0,
            gid: 0,
        }
    }
}

define_options! {
    // ---- Socket level options (SO_*) ----
    ReuseAddress(bool),
    Error(i32),
    DontRoute(bool),
    SendBuffer(usize),
    ReceiveBuffer(usize),
    KeepAlive(bool),
    SendTimeout(Duration),
    ReceiveTimeout(Duration),
    SendBufferForce(usize),
    PassCredentials(bool),
    PeerCredentials(UnixCredentials),
    SocketType(i32),
    SocketProtocol(i32),
    SocketDomain(i32),
    BindToDevice(Option<InterfaceId>),

    // --- TCP level options (TCP_*) ----
    NoDelay(bool),
    MaxSegment(usize),
    TcpKeepIdle(u32),
    TcpKeepInterval(u32),
    TcpKeepCount(u32),
    TcpUserTimeout(u32),
    TcpInfo(TcpInfo),

    // ---- IP level options (IP_*) ----
    Ttl(u8),
    RecvErr(bool),

    // ---- Extra options ----
    NonBlocking(bool),
}

/// Trait for configurable socket-like objects.
#[enum_dispatch]
pub trait Configurable {
    /// Get a socket option, returns `true` if the socket supports the option.
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool>;
    /// Set a socket option, returns `true` if the socket supports the option.
    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool>;

    /// Get a socket option. Dispatches to [`Configurable::get_option_inner`].
    fn get_option(&self, mut opt: GetSocketOption) -> AxResult {
        self.get_option_inner(&mut opt).and_then(|supported| {
            if !supported {
                Err(AxError::from(LinuxError::ENOPROTOOPT))
            } else {
                Ok(())
            }
        })
    }
    /// Set a socket option. Dispatches to [`Configurable::set_option_inner`].
    fn set_option(&self, opt: SetSocketOption) -> AxResult {
        self.set_option_inner(opt).and_then(|supported| {
            if !supported {
                Err(AxError::from(LinuxError::ENOPROTOOPT))
            } else {
                Ok(())
            }
        })
    }
}

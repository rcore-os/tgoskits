//! Shared socket options and blocking helpers.
//!
//! Protocol-specific sockets embed `GeneralOptions` for common POSIX socket
//! state such as nonblocking mode, reuse-address, timeouts, socket identity, and
//! device binding. Keeping these fields here avoids duplicating subtly
//! different getsockopt/setsockopt behavior in TCP, UDP, raw, Unix, and vsock
//! transports.
//!
//! # Blocking Semantics
//!
//! The helpers in this module bridge poll-based readiness with synchronous
//! socket operations. They should only wait on protocol-specific pollers and
//! must not drive the smoltcp interface directly. Progress is requested through
//! the net-poll worker so application threads do not become temporary protocol
//! stack owners.

use core::{
    sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering},
    task::Waker,
    time::Duration,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_task::future::{block_on, poll_io, timeout};
use axpoll::{IoEvents, Pollable};

use crate::{
    config::{DeviceBinding, InterfaceId},
    get_service, interface_by_id,
    options::{Configurable, GetSocketOption, SetSocketOption},
};

const SO_PRIORITY_UNPRIVILEGED_MAX: i32 = 6;
const IP_TOS_ECN_MASK: u8 = 0x03;

/// General options for all sockets.
pub(crate) struct GeneralOptions {
    /// Whether the socket is non-blocking.
    nonblock: AtomicBool,
    /// Whether the socket should reuse the address.
    reuse_address: AtomicBool,
    /// Whether the socket should reuse the port (SO_REUSEPORT).
    reuse_port: AtomicBool,

    /// Per-socket send timeout in nanoseconds; zero means no timeout.
    send_timeout_nanos: AtomicU64,
    /// Per-socket receive timeout in nanoseconds; zero means no timeout.
    recv_timeout_nanos: AtomicU64,

    /// Bound interface id encoded as zero for "not bound".
    bound_if: AtomicU32,

    /// IP_TOS value used by protocol sockets when marking outgoing packets.
    ip_tos: AtomicU8,
    /// Whether recvmsg should report IPv4 TOS as IP_TOS ancillary data.
    recv_tos: AtomicBool,
    /// Whether recvmsg should report IPv6 traffic class as IPV6_TCLASS ancillary data.
    recv_traffic_class: AtomicBool,
    /// SO_PRIORITY value. ax-net stores it for Linux compatibility; packet
    /// queue scheduling is not modeled yet.
    priority: AtomicI32,

    /// Socket type: SOCK_STREAM (1), SOCK_DGRAM (2), SOCK_RAW (3).
    socket_type: AtomicI32,
    /// Socket domain: AF_INET (2), AF_UNIX (1), AF_VSOCK (40).
    domain: i32,
    /// IP protocol: IPPROTO_TCP (6), IPPROTO_UDP (17), IPPROTO_ICMP (1), etc.
    protocol: i32,
}
impl GeneralOptions {
    /// Create new GeneralOptions. `socket_type` is the SOCK_* constant
    /// (e.g. SOCK_STREAM=1, SOCK_DGRAM=2, SOCK_RAW=3).
    /// `domain` is the AF_* constant (e.g. AF_INET=2, AF_UNIX=1, AF_VSOCK=40).
    /// `protocol` is the IPPROTO_* constant (e.g. IPPROTO_TCP=6, IPPROTO_UDP=17, IPPROTO_ICMP=1).
    pub fn new(socket_type: i32, domain: i32, protocol: i32) -> Self {
        Self {
            nonblock: AtomicBool::new(false),
            reuse_address: AtomicBool::new(false),
            reuse_port: AtomicBool::new(false),

            send_timeout_nanos: AtomicU64::new(0),
            recv_timeout_nanos: AtomicU64::new(0),

            bound_if: AtomicU32::new(0),

            ip_tos: AtomicU8::new(0),
            recv_tos: AtomicBool::new(false),
            recv_traffic_class: AtomicBool::new(false),
            priority: AtomicI32::new(0),

            socket_type: AtomicI32::new(socket_type),
            domain,
            protocol,
        }
    }

    /// Returns whether this socket is in non-blocking mode.
    pub fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }

    /// Returns whether SO_REUSEADDR-style bind reuse is enabled.
    pub fn reuse_address(&self) -> bool {
        self.reuse_address.load(Ordering::Relaxed)
    }

    /// Returns whether SO_REUSEPORT is enabled.
    ///
    /// Under a single-core smoltcp stack there is one accept queue per
    /// endpoint, so port reuse degrades to the same rebind allowance as
    /// SO_REUSEADDR rather than fanning connections across a socket group.
    pub fn reuse_port(&self) -> bool {
        self.reuse_port.load(Ordering::Relaxed)
    }

    /// Returns the configured send timeout, or `None` for blocking forever.
    pub fn send_timeout(&self) -> Option<Duration> {
        let nanos = self.send_timeout_nanos.load(Ordering::Relaxed);
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }

    /// Returns the configured receive timeout, or `None` for blocking forever.
    pub fn recv_timeout(&self) -> Option<Duration> {
        let nanos = self.recv_timeout_nanos.load(Ordering::Relaxed);
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }

    /// Updates the interface binding used by route selection.
    pub fn set_device_binding(&self, binding: DeviceBinding) {
        self.bound_if.store(
            binding.bound_if.map_or(0, InterfaceId::get),
            Ordering::Release,
        );
    }

    /// Returns the current interface binding.
    pub fn device_binding(&self) -> DeviceBinding {
        let raw = self.bound_if.load(Ordering::Acquire);
        DeviceBinding {
            bound_if: (raw != 0).then_some(InterfaceId::new(raw)),
        }
    }

    /// Returns the IPv4 TOS / IPv6 traffic-class byte configured on this socket.
    pub fn ip_tos(&self) -> u8 {
        self.ip_tos.load(Ordering::Relaxed)
    }

    /// Updates the IPv4 TOS / IPv6 traffic-class byte configured on this socket.
    pub fn set_ip_tos(&self, tos: u8) {
        self.ip_tos.store(tos & !IP_TOS_ECN_MASK, Ordering::Relaxed);
    }

    /// Returns whether IPv4 TOS ancillary data is enabled for receive calls.
    pub fn recv_tos(&self) -> bool {
        self.recv_tos.load(Ordering::Relaxed)
    }

    /// Updates whether IPv4 TOS ancillary data is enabled for receive calls.
    pub fn set_recv_tos(&self, enabled: bool) {
        self.recv_tos.store(enabled, Ordering::Relaxed);
    }

    /// Returns whether IPv6 traffic-class ancillary data is enabled for receive calls.
    pub fn recv_traffic_class(&self) -> bool {
        self.recv_traffic_class.load(Ordering::Relaxed)
    }

    /// Updates whether IPv6 traffic-class ancillary data is enabled for receive calls.
    pub fn set_recv_traffic_class(&self, enabled: bool) {
        self.recv_traffic_class.store(enabled, Ordering::Relaxed);
    }

    /// Returns the Linux SO_PRIORITY value configured on this socket.
    pub fn priority(&self) -> i32 {
        self.priority.load(Ordering::Relaxed)
    }

    /// Updates SO_PRIORITY using Linux's ordinary unprivileged range.
    pub fn set_priority(&self, priority: i32) -> AxResult<()> {
        if !(0..=SO_PRIORITY_UNPRIVILEGED_MAX).contains(&priority) {
            return Err(AxError::from(LinuxError::EPERM));
        }
        self.priority.store(priority, Ordering::Relaxed);
        Ok(())
    }

    /// Registers a waker with the service/device path for the bound interface.
    pub fn register_waker(&self, waker: &Waker) {
        get_service().register_waker(self.device_binding(), waker);
    }

    /// Runs a send operation through the standard blocking/nonblocking poller.
    pub fn send_poller<P: Pollable, F: FnMut() -> AxResult<T>, T>(
        &self,
        pollable: &P,
        f: F,
    ) -> AxResult<T> {
        self.send_poller_with(pollable, false, f)
    }

    /// Runs a receive operation through the standard blocking/nonblocking poller.
    pub fn recv_poller<P: Pollable, F: FnMut() -> AxResult<T>, T>(
        &self,
        pollable: &P,
        f: F,
    ) -> AxResult<T> {
        self.recv_poller_with(pollable, false, f)
    }

    /// Like [`send_poller`] but lets the caller force non-blocking
    /// behavior for this call only (e.g. `MSG_DONTWAIT`). The effective
    /// non-blocking state is the OR of the socket's own `nonblocking()`
    /// and `extra_nonblocking`.
    pub fn send_poller_with<P: Pollable, F: FnMut() -> AxResult<T>, T>(
        &self,
        pollable: &P,
        extra_nonblocking: bool,
        f: F,
    ) -> AxResult<T> {
        block_on(timeout(
            self.send_timeout(),
            poll_io(
                pollable,
                IoEvents::OUT,
                self.nonblocking() || extra_nonblocking,
                f,
            ),
        ))?
    }

    /// Like [`recv_poller`] but lets the caller force non-blocking
    /// behavior for this call only (e.g. `MSG_DONTWAIT`).
    pub fn recv_poller_with<P: Pollable, F: FnMut() -> AxResult<T>, T>(
        &self,
        pollable: &P,
        extra_nonblocking: bool,
        f: F,
    ) -> AxResult<T> {
        block_on(timeout(
            self.recv_timeout(),
            poll_io(
                pollable,
                IoEvents::IN,
                self.nonblocking() || extra_nonblocking,
                f,
            ),
        ))?
    }
}
impl Configurable for GeneralOptions {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> AxResult<bool> {
        use GetSocketOption as O;
        match option {
            O::Error(error) => {
                // TODO(mivik): actual logic
                **error = 0;
            }
            O::NonBlocking(nonblock) => {
                **nonblock = self.nonblocking();
            }
            O::ReuseAddress(reuse) => {
                **reuse = self.reuse_address();
            }
            O::ReusePort(reuse) => {
                **reuse = self.reuse_port();
            }
            O::SendTimeout(timeout) => {
                **timeout = Duration::from_nanos(self.send_timeout_nanos.load(Ordering::Relaxed));
            }
            O::ReceiveTimeout(timeout) => {
                **timeout = Duration::from_nanos(self.recv_timeout_nanos.load(Ordering::Relaxed));
            }
            O::RecvErr(val) => {
                **val = false;
            }
            O::IpTos(tos) => {
                **tos = self.ip_tos.load(Ordering::Relaxed);
            }
            O::RecvTos(enabled) => {
                **enabled = self.recv_tos();
            }
            O::RecvTrafficClass(enabled) => {
                **enabled = self.recv_traffic_class();
            }
            O::Priority(priority) => {
                **priority = self.priority();
            }
            O::SocketType(t) => {
                **t = self.socket_type.load(Ordering::Relaxed);
            }
            O::SocketProtocol(proto) => {
                **proto = self.protocol;
            }
            O::SocketDomain(domain) => {
                **domain = self.domain;
            }
            O::BindToDevice(binding) => {
                **binding = self.device_binding().bound_if;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_option_inner(&self, option: SetSocketOption) -> AxResult<bool> {
        use SetSocketOption as O;

        match option {
            O::NonBlocking(nonblock) => {
                self.nonblock.store(*nonblock, Ordering::Relaxed);
            }
            O::ReuseAddress(reuse) => {
                self.reuse_address.store(*reuse, Ordering::Relaxed);
            }
            O::ReusePort(reuse) => {
                self.reuse_port.store(*reuse, Ordering::Relaxed);
            }
            O::SendTimeout(timeout) => {
                self.send_timeout_nanos
                    .store(timeout.as_nanos() as u64, Ordering::Relaxed);
            }
            O::ReceiveTimeout(timeout) => {
                self.recv_timeout_nanos
                    .store(timeout.as_nanos() as u64, Ordering::Relaxed);
            }
            O::SendBuffer(_) | O::ReceiveBuffer(_) => {
                // TODO(mivik): implement buffer size options
            }
            O::BindToDevice(interface_id) => {
                if let Some(id) = *interface_id
                    && interface_by_id(id).is_none()
                {
                    return Err(AxError::NoSuchDevice);
                }
                self.set_device_binding(DeviceBinding {
                    bound_if: *interface_id,
                });
            }
            O::RecvErr(_) => {
                // TODO: Retrieve ICMP errors via errqueue
            }
            O::IpTos(tos) => {
                self.set_ip_tos(*tos);
            }
            O::RecvTos(enabled) => {
                self.set_recv_tos(*enabled);
            }
            O::RecvTrafficClass(enabled) => {
                self.set_recv_traffic_class(*enabled);
            }
            O::Priority(priority) => {
                self.set_priority(*priority)?;
            }
            O::SocketType(_) | O::SocketProtocol(_) | O::SocketDomain(_) => {
                // Read-only options
                return Err(AxError::from(LinuxError::ENOPROTOOPT));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_binding_round_trips_none_and_some_interface() {
        let options = GeneralOptions::new(1, 2, 6);
        assert_eq!(options.device_binding(), DeviceBinding { bound_if: None });

        let interface_id = InterfaceId::new(7);
        options.set_device_binding(DeviceBinding {
            bound_if: Some(interface_id),
        });
        assert_eq!(
            options.device_binding(),
            DeviceBinding {
                bound_if: Some(interface_id)
            }
        );

        options.set_device_binding(DeviceBinding { bound_if: None });
        assert_eq!(options.device_binding(), DeviceBinding { bound_if: None });
    }

    #[test]
    fn reuse_address_and_reuse_port_are_independent_flags() {
        let options = GeneralOptions::new(1, 2, 6);

        assert!(!options.reuse_address());
        assert!(!options.reuse_port());

        options
            .set_option(SetSocketOption::ReusePort(&true))
            .unwrap();
        assert!(options.reuse_port());
        assert!(!options.reuse_address());

        let mut reuse_port = false;
        options
            .get_option(GetSocketOption::ReusePort(&mut reuse_port))
            .unwrap();
        assert!(reuse_port);

        options
            .set_option(SetSocketOption::ReusePort(&false))
            .unwrap();
        assert!(!options.reuse_port());
    }

    #[test]
    fn socket_priority_matches_unprivileged_linux_range() {
        let options = GeneralOptions::new(1, 2, 6);

        assert_eq!(options.priority(), 0);
        options.set_priority(6).unwrap();
        assert_eq!(options.priority(), 6);

        assert_eq!(
            options.set_priority(7).unwrap_err(),
            AxError::from(LinuxError::EPERM)
        );
        assert_eq!(
            options.set_priority(-1).unwrap_err(),
            AxError::from(LinuxError::EPERM)
        );
        assert_eq!(options.priority(), 6);
    }

    #[test]
    fn ip_tos_storage_masks_user_controlled_ecn_bits() {
        let options = GeneralOptions::new(1, 2, 6);

        options.set_ip_tos(0x2e);
        assert_eq!(options.ip_tos(), 0x2c);

        options.set_ip_tos(0xff);
        assert_eq!(options.ip_tos(), 0xfc);
    }

    #[test]
    fn receive_qos_metadata_toggles_are_independent() {
        let options = GeneralOptions::new(2, 2, 17);

        assert!(!options.recv_tos());
        assert!(!options.recv_traffic_class());

        options.set_recv_tos(true);
        assert!(options.recv_tos());
        assert!(!options.recv_traffic_class());

        options.set_recv_traffic_class(true);
        assert!(options.recv_tos());
        assert!(options.recv_traffic_class());

        options.set_recv_tos(false);
        assert!(!options.recv_tos());
        assert!(options.recv_traffic_class());
    }
}

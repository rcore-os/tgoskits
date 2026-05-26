use core::{
    sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering},
    task::Waker,
    time::Duration,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_task::future::{block_on, poll_io, timeout};
use axpoll::{IoEvents, Pollable};

use crate::{
    get_service,
    options::{Configurable, GetSocketOption, SetSocketOption},
};

/// General options for all sockets.
pub(crate) struct GeneralOptions {
    /// Whether the socket is non-blocking.
    nonblock: AtomicBool,
    /// Whether the socket should reuse the address.
    reuse_address: AtomicBool,

    send_timeout_nanos: AtomicU64,
    recv_timeout_nanos: AtomicU64,

    device_mask: AtomicU32,

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

            send_timeout_nanos: AtomicU64::new(0),
            recv_timeout_nanos: AtomicU64::new(0),

            device_mask: AtomicU32::new(0),

            socket_type: AtomicI32::new(socket_type),
            domain,
            protocol,
        }
    }

    pub fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }

    pub fn reuse_address(&self) -> bool {
        self.reuse_address.load(Ordering::Relaxed)
    }

    pub fn send_timeout(&self) -> Option<Duration> {
        let nanos = self.send_timeout_nanos.load(Ordering::Relaxed);
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }

    pub fn recv_timeout(&self) -> Option<Duration> {
        let nanos = self.recv_timeout_nanos.load(Ordering::Relaxed);
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }

    pub fn set_device_mask(&self, mask: u32) {
        self.device_mask.store(mask, Ordering::Release);
    }

    pub fn device_mask(&self) -> u32 {
        self.device_mask.load(Ordering::Acquire)
    }

    pub fn register_waker(&self, waker: &Waker) {
        get_service().register_waker(self.device_mask(), waker);
    }

    pub fn send_poller<P: Pollable, F: FnMut() -> AxResult<T>, T>(
        &self,
        pollable: &P,
        f: F,
    ) -> AxResult<T> {
        self.send_poller_with(pollable, false, f)
    }

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
            O::SendTimeout(timeout) => {
                **timeout = Duration::from_nanos(self.send_timeout_nanos.load(Ordering::Relaxed));
            }
            O::ReceiveTimeout(timeout) => {
                **timeout = Duration::from_nanos(self.recv_timeout_nanos.load(Ordering::Relaxed));
            }
            O::RecvErr(val) => {
                **val = false;
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
            O::RecvErr(_) => {
                // TODO: Retrieve ICMP errors via errqueue
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

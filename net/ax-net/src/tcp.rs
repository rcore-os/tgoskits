//! TCP socket implementation.
//!
//! TCP sockets wrap smoltcp stream sockets with POSIX-like behavior: bind and
//! listen bookkeeping, accept queues, nonblocking readiness, keepalive and
//! TCP_INFO options, orphan cleanup, and route-aware device binding.
//!
//! # smoltcp Boundary
//!
//! The actual TCP state machine, retransmission timers, and stream buffers live
//! in smoltcp. This module owns the public socket state around that core:
//! ephemeral port allocation, wildcard/specific bind registration, listener
//! setup, accepted child socket construction, shutdown semantics, and
//! Linux-compatible error reporting.
//!
//! # Polling Model
//!
//! Socket methods never synchronously drive the full interface poll loop.
//! Instead they mutate the smoltcp socket, call `request_poll()`, register
//! wakers through `PollSet`, and let the dedicated net-poll worker advance
//! timers, handshakes, retransmission, and close states.
//!
//! # Related Side Tables
//!
//! - `TCP_BOUND_PORTS` records public bind ownership.
//! - `LISTEN_TABLE` owns passive-open child sockets and accept wakeups.
//! - `orphan` keeps dropped sockets alive long enough for FIN/TIME-WAIT cleanup.

use alloc::{sync::Arc, vec, vec::Vec};
use core::{
    net::{Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    task::{Context, Waker},
};

use ax_errno::{AxError, AxResult, LinuxError, ax_bail, ax_err_type};
use ax_io::prelude::*;
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet, Pollable};
use hashbrown::HashMap;
use smoltcp::{
    iface::SocketHandle,
    socket::tcp as smol,
    time::Duration,
    wire::{IpEndpoint, IpListenEndpoint, IpProtocol},
};
use spin::LazyLock;

use crate::{
    DeferPollWake, LISTEN_TABLE, RecvFlags, RecvOptions, SOCKET_SET, SendOptions, Shutdown, Socket,
    SocketAddrEx, SocketOps,
    addr::{allocate_ephemeral_port, listen_addrs_conflict},
    config::{DeviceBinding, InterfaceId},
    consts::{TCP_RX_BUF_LEN, TCP_TX_BUF_LEN},
    general::GeneralOptions,
    get_control, get_service, interface_by_id,
    ip_tos::{EgressIpTosKey, clear_egress_ip_tos, set_egress_ip_tos},
    options::{Configurable, GetSocketOption, SetSocketOption, TcpInfo, TcpInfoOptions, TcpState},
    request_poll,
    state::*,
};

const TCP_KEEPIDLE_DEFAULT_SECS: u32 = 7200;
const TCP_KEEPINTVL_DEFAULT_SECS: u32 = 75;
const TCP_KEEPCNT_DEFAULT: u32 = 9;
const TCP_USER_TIMEOUT_DEFAULT_MS: u32 = 0;
const TCP_KEEPIDLE_MAX_SECS: u32 = 32767;
const TCP_KEEPINTVL_MAX_SECS: u32 = 32767;
const TCP_KEEPCNT_MAX: u32 = 127;
const TCP_INFO_DEFAULT_MSS: u32 = 1460;
const TCP_INFO_DEFAULT_PMTU: u32 = 1500;
const TCP_INFO_INITIAL_RTO_MICROS: u32 = 1_000_000;
const TCP_INFO_DEFAULT_REORDERING: u32 = 3;

/// A TCP socket that provides POSIX-like APIs.
pub struct TcpSocket {
    /// Public high-level socket state gate.
    state: StateLock,
    /// Handle into the global smoltcp socket set.
    handle: SocketHandle,
    /// Bound listen endpoint, or an empty endpoint before bind/connect.
    bound_endpoint: Mutex<IpListenEndpoint>,
    /// Connected peer endpoint once established.
    peer_endpoint: Mutex<Option<IpEndpoint>>,
    /// Currently registered egress IP_TOS policy for this TCP socket.
    tos_key: Mutex<Option<EgressIpTosKey>>,
    /// Whether `bound_endpoint` is registered in `TCP_BOUND_PORTS`.
    bound_registered: AtomicBool,

    /// Shared socket options and blocking helpers.
    general: GeneralOptions,
    /// Pending Linux errno-style connection error.
    pending_error: AtomicI32,
    /// TCP_KEEPIDLE value in seconds.
    keep_idle_secs: AtomicU32,
    /// TCP_KEEPINTVL value in seconds.
    keep_interval_secs: AtomicU32,
    /// TCP_KEEPCNT value.
    keep_count: AtomicU32,
    /// TCP_USER_TIMEOUT value in milliseconds.
    user_timeout_millis: AtomicU32,
    /// Whether the read half was shut down from the public API.
    rx_closed: AtomicBool,
    /// Shared RX readiness poll set.
    poll_rx: Arc<PollSet>,
    /// Shared TX readiness poll set.
    poll_tx: Arc<PollSet>,
    /// Wakes waiters when the receive side becomes closed.
    poll_rx_closed: PollSet,
}

unsafe impl Sync for TcpSocket {}

impl TcpSocket {
    /// Creates a new TCP socket.
    pub fn new() -> Self {
        Self {
            state: StateLock::new(State::Idle),
            handle: SOCKET_SET.add(smol::Socket::new(
                smol::SocketBuffer::new(vec![0; TCP_RX_BUF_LEN]),
                smol::SocketBuffer::new(vec![0; TCP_TX_BUF_LEN]),
            )),
            bound_endpoint: Mutex::new(empty_endpoint()),
            peer_endpoint: Mutex::new(None),
            tos_key: Mutex::new(None),
            bound_registered: AtomicBool::new(false),

            general: GeneralOptions::new(1, 2, 6), // SOCK_STREAM
            pending_error: AtomicI32::new(0),
            keep_idle_secs: AtomicU32::new(TCP_KEEPIDLE_DEFAULT_SECS),
            keep_interval_secs: AtomicU32::new(TCP_KEEPINTVL_DEFAULT_SECS),
            keep_count: AtomicU32::new(TCP_KEEPCNT_DEFAULT),
            user_timeout_millis: AtomicU32::new(TCP_USER_TIMEOUT_DEFAULT_MS),
            rx_closed: AtomicBool::new(false),
            poll_rx: Arc::new(PollSet::new()),
            poll_tx: Arc::new(PollSet::new()),
            poll_rx_closed: PollSet::new(),
        }
    }

    /// Restricts this socket to one interface for route selection.
    pub fn bind_device(&self, interface_id: InterfaceId) -> AxResult {
        if interface_by_id(interface_id).is_none() {
            return Err(AxError::NoSuchDevice);
        }
        self.general.set_device_binding(DeviceBinding {
            bound_if: Some(interface_id),
        });
        Ok(())
    }

    /// Creates a new TCP socket that is already connected.
    fn new_connected(
        handle: SocketHandle,
        local_endpoint: IpEndpoint,
        remote_endpoint: IpEndpoint,
    ) -> Self {
        let result = Self {
            state: StateLock::new(State::Connected),
            handle,
            bound_endpoint: Mutex::new(empty_endpoint()),
            peer_endpoint: Mutex::new(Some(remote_endpoint)),
            tos_key: Mutex::new(None),
            bound_registered: AtomicBool::new(false),

            general: GeneralOptions::new(1, 2, 6), // SOCK_STREAM
            pending_error: AtomicI32::new(0),
            keep_idle_secs: AtomicU32::new(TCP_KEEPIDLE_DEFAULT_SECS),
            keep_interval_secs: AtomicU32::new(TCP_KEEPINTVL_DEFAULT_SECS),
            keep_count: AtomicU32::new(TCP_KEEPCNT_DEFAULT),
            user_timeout_millis: AtomicU32::new(TCP_USER_TIMEOUT_DEFAULT_MS),
            rx_closed: AtomicBool::new(false),
            poll_rx: Arc::new(PollSet::new()),
            poll_tx: Arc::new(PollSet::new()),
            poll_rx_closed: PollSet::new(),
        };
        let endpoint = IpListenEndpoint {
            addr: Some(local_endpoint.addr),
            port: local_endpoint.port,
        };
        *result.bound_endpoint.lock() = endpoint;
        result.general.set_device_binding(
            get_control()
                .local_binding_for(&endpoint)
                .unwrap_or_default(),
        );
        result
    }
}

impl Default for TcpSocket {
    fn default() -> Self {
        Self::new()
    }
}

/// Private methods
impl TcpSocket {
    fn state(&self) -> State {
        self.state.get()
    }

    #[inline]
    fn is_listening(&self) -> bool {
        self.state() == State::Listening
    }

    fn with_smol_socket<R>(&self, f: impl FnOnce(&mut smol::Socket) -> R) -> R {
        SOCKET_SET.with_socket_mut::<smol::Socket, _, _>(self.handle, f)
    }

    fn egress_ip_tos_key(&self) -> Option<EgressIpTosKey> {
        if self.is_listening() {
            return EgressIpTosKey::listener(IpProtocol::Tcp, *self.bound_endpoint.lock());
        }

        let local = self
            .with_smol_socket(|socket| socket.local_endpoint())
            .or_else(|| {
                let endpoint = *self.bound_endpoint.lock();
                endpoint.addr.map(|addr| IpEndpoint {
                    addr,
                    port: endpoint.port,
                })
            });
        let remote = self
            .with_smol_socket(|socket| socket.remote_endpoint())
            .or_else(|| *self.peer_endpoint.lock());

        EgressIpTosKey::exact(IpProtocol::Tcp, local?, remote?)
    }

    fn sync_egress_ip_tos(&self) {
        let key = self.egress_ip_tos_key();
        let tos = self.general.ip_tos();
        let mut tracked = self.tos_key.lock();
        if *tracked != key {
            if let Some(old) = *tracked {
                clear_egress_ip_tos(old);
            }
            *tracked = key;
        }
        if let Some(key) = key {
            set_egress_ip_tos(key, tos);
        }
    }

    fn clear_tracked_egress_ip_tos(&self) {
        if let Some(key) = self.tos_key.lock().take() {
            clear_egress_ip_tos(key);
        }
    }

    fn tcp_info_snapshot(&self) -> TcpInfo {
        self.with_smol_socket(|socket| {
            let send_queue = socket.send_queue().min(u32::MAX as usize) as u32;
            let snd_mss = TCP_INFO_DEFAULT_MSS;

            let mut options = TcpInfoOptions::empty();
            if socket.timestamp_enabled() {
                options |= TcpInfoOptions::TIMESTAMPS;
            }

            TcpInfo {
                state: tcp_state_info(socket.state()),
                options,
                rto_micros: socket
                    .timeout()
                    .map(duration_micros_u32)
                    .unwrap_or(TCP_INFO_INITIAL_RTO_MICROS),
                ato_micros: socket.ack_delay().map(duration_micros_u32).unwrap_or(0),
                snd_mss,
                rcv_mss: snd_mss,
                notsent_bytes: send_queue,
                pmtu: TCP_INFO_DEFAULT_PMTU,
                advmss: snd_mss,
                reordering: TCP_INFO_DEFAULT_REORDERING,
                snd_wnd: 0,
                ..Default::default()
            }
        })
    }

    fn bound_endpoint(&self) -> AxResult<IpListenEndpoint> {
        let endpoint = *self.bound_endpoint.lock();
        if endpoint.port == 0 {
            ax_bail!(InvalidInput, "not bound");
        }
        Ok(endpoint)
    }

    fn poll_connect(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        self.with_smol_socket(|socket| match socket.state() {
            smol::State::SynSent | smol::State::SynReceived => {
                // wait for connection
            }
            smol::State::Established => {
                self.pending_error.store(0, Ordering::Release);
                self.state.set(State::Connected); // connected
                *self.peer_endpoint.lock() = socket.remote_endpoint();
                debug!(
                    "TCP socket {}: connected to {}",
                    self.handle,
                    socket.remote_endpoint().unwrap(),
                );
                events.set(IoEvents::OUT, true);
            }
            state => {
                *self.peer_endpoint.lock() = None;
                self.pending_error
                    .store(LinuxError::ECONNREFUSED.code(), Ordering::Release);
                self.state.set(State::Closed); // connection failed
                debug!(
                    "TCP socket {}: connect failed in state {:?}",
                    self.handle, state
                );
                events.set(IoEvents::OUT, true);
                events.set(IoEvents::ERR, true);
                events.set(IoEvents::HUP, true);
            }
        });
        events
    }

    fn poll_stream(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        self.with_smol_socket(|socket| {
            events.set(
                IoEvents::IN,
                !self.rx_closed.load(Ordering::Acquire)
                    && (!socket.may_recv() || socket.can_recv()),
            );
            events.set(IoEvents::OUT, !socket.may_send() || socket.can_send());
        });
        events
    }

    fn poll_listener(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        let endpoint = self.bound_endpoint().unwrap();
        let sockets = SOCKET_SET.inner.lock();
        events.set(
            IoEvents::IN,
            LISTEN_TABLE.can_accept(endpoint, &sockets).unwrap(),
        );
        events
    }
}

impl Configurable for TcpSocket {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> AxResult<bool> {
        use GetSocketOption as O;

        if let O::Error(error) = option {
            **error = self.pending_error.swap(0, Ordering::AcqRel);
            return Ok(true);
        }

        if self.general.get_option_inner(option)? {
            return Ok(true);
        }

        match option {
            O::NoDelay(no_delay) => {
                **no_delay = self.with_smol_socket(|socket| !socket.nagle_enabled());
            }
            O::KeepAlive(keep_alive) => {
                **keep_alive = self.with_smol_socket(|socket| socket.keep_alive().is_some());
            }
            O::MaxSegment(max_segment) => {
                // TODO(mivik): get actual MSS
                **max_segment = 1460;
            }
            O::TcpKeepIdle(keep_idle) => {
                **keep_idle = self.keep_idle_secs.load(Ordering::Relaxed);
            }
            O::TcpKeepInterval(keep_interval) => {
                **keep_interval = self.keep_interval_secs.load(Ordering::Relaxed);
            }
            O::TcpKeepCount(keep_count) => {
                **keep_count = self.keep_count.load(Ordering::Relaxed);
            }
            O::TcpUserTimeout(user_timeout) => {
                **user_timeout = self.user_timeout_millis.load(Ordering::Relaxed);
            }
            O::SendBuffer(size) => {
                **size = TCP_TX_BUF_LEN;
            }
            O::ReceiveBuffer(size) => {
                **size = TCP_RX_BUF_LEN;
            }
            O::TcpInfo(info) => {
                **info = self.tcp_info_snapshot();
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_option_inner(&self, option: SetSocketOption) -> AxResult<bool> {
        use SetSocketOption as O;

        if let O::IpTos(tos) = option {
            self.general.set_ip_tos(*tos);
            self.sync_egress_ip_tos();
            return Ok(true);
        }

        if self.general.set_option_inner(option)? {
            return Ok(true);
        }

        match option {
            O::NoDelay(no_delay) => {
                self.with_smol_socket(|socket| {
                    socket.set_nagle_enabled(!no_delay);
                });
            }
            O::KeepAlive(keep_alive) => {
                let interval =
                    Duration::from_secs(self.keep_idle_secs.load(Ordering::Relaxed) as u64);
                self.with_smol_socket(|socket| {
                    socket.set_keep_alive(keep_alive.then_some(interval));
                });
            }
            O::TcpKeepIdle(keep_idle) => {
                if *keep_idle == 0 || *keep_idle > TCP_KEEPIDLE_MAX_SECS {
                    return Err(AxError::InvalidInput);
                }
                self.keep_idle_secs.store(*keep_idle, Ordering::Relaxed);
                let interval = Duration::from_secs(*keep_idle as u64);
                self.with_smol_socket(|socket| {
                    if socket.keep_alive().is_some() {
                        socket.set_keep_alive(Some(interval));
                    }
                });
            }
            O::TcpKeepInterval(keep_interval) => {
                if *keep_interval == 0 || *keep_interval > TCP_KEEPINTVL_MAX_SECS {
                    return Err(AxError::InvalidInput);
                }
                self.keep_interval_secs
                    .store(*keep_interval, Ordering::Relaxed);
            }
            O::TcpKeepCount(keep_count) => {
                if *keep_count == 0 || *keep_count > TCP_KEEPCNT_MAX {
                    return Err(AxError::InvalidInput);
                }
                self.keep_count.store(*keep_count, Ordering::Relaxed);
            }
            O::TcpUserTimeout(user_timeout) => {
                self.user_timeout_millis
                    .store(*user_timeout, Ordering::Relaxed);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
impl SocketOps for TcpSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        let mut local_addr = local_addr.into_ip()?;
        self.state
            .lock(State::Idle)
            .map_err(|_| ax_err_type!(InvalidInput, "already bound"))?
            .transit(State::Idle, || {
                // TODO: check addr is available
                if local_addr.port() == 0 {
                    local_addr.set_port(get_ephemeral_port()?);
                }
                if self.bound_endpoint.lock().port != 0 {
                    return Err(AxError::InvalidInput);
                }
                let endpoint = IpListenEndpoint {
                    addr: if local_addr.ip().is_unspecified() {
                        None
                    } else {
                        Some(local_addr.ip().into())
                    },
                    port: local_addr.port(),
                };
                if !self.general.reuse_address()
                    && !self.general.reuse_port()
                    && !LISTEN_TABLE.can_listen(endpoint)
                {
                    return Err(AxError::AddrInUse);
                }
                let binding = get_control().local_binding_for(&endpoint)?;
                self.register_bound_endpoint(endpoint)?;
                *self.bound_endpoint.lock() = endpoint;
                if binding.bound_if.is_some() {
                    self.general.set_device_binding(binding);
                }
                debug!("TCP socket {}: binding to {}", self.handle, local_addr);
                Ok(())
            })
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_ip()?;
        self.start_connect(remote_addr)?;
        request_poll();

        // Here our state must be `CONNECTING`, and only one thread can run here.
        self.general.send_poller(self, || {
            request_poll();
            let events = self.poll_connect();
            if !events.contains(IoEvents::OUT) {
                Err(AxError::WouldBlock)
            } else if self.state.get() == State::Connected {
                Ok(())
            } else {
                Err(
                    LinuxError::try_from(self.pending_error.load(Ordering::Acquire))
                        .map_or(AxError::ConnectionRefused, AxError::from),
                )
            }
        })
    }

    fn listen(&self, backlog: usize) -> AxResult {
        if let Ok(guard) = self.state.lock(State::Idle) {
            guard.transit(State::Listening, || {
                let mut bound_endpoint = *self.bound_endpoint.lock();
                if bound_endpoint.port == 0 {
                    bound_endpoint.port = get_ephemeral_port()?;
                }
                let binding = get_control().local_binding_for(&bound_endpoint)?;
                self.with_bound_endpoint_registered(bound_endpoint, || {
                    LISTEN_TABLE.listen(bound_endpoint, backlog, self.general.reuse_port())
                })?;
                *self.bound_endpoint.lock() = bound_endpoint;
                self.sync_egress_ip_tos();
                if binding.bound_if.is_some() {
                    self.general.set_device_binding(binding);
                }
                debug!("listening on {}", bound_endpoint);
                Ok(())
            })?;
        } else {
            // ignore simultaneous `listen`s.
        }
        Ok(())
    }

    fn accept(&self) -> AxResult<Socket> {
        if self.state.get() != State::Listening {
            ax_bail!(InvalidInput, "not listening");
        }

        let bound_endpoint = self.bound_endpoint()?;
        self.general.recv_poller(self, || {
            request_poll();
            let accepted = {
                let mut sockets = SOCKET_SET.inner.lock();
                LISTEN_TABLE.accept(bound_endpoint, &mut sockets)?
            };
            Ok({
                let socket = TcpSocket::new_connected(
                    accepted.handle,
                    accepted.local_endpoint,
                    accepted.remote_endpoint,
                );
                socket.general.set_ip_tos(self.general.ip_tos());
                socket.sync_egress_ip_tos();
                debug!(
                    "accepted connection from {}, {}",
                    accepted.handle, accepted.remote_endpoint
                );
                socket.into()
            })
        })
    }

    fn send(&self, mut src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        // SAFETY: `self.handle` should be initialized in a connected socket.
        let extra_nb = options.flags.contains(crate::SendFlags::DONTWAIT);
        let target_len = src.remaining();
        if target_len == 0 {
            return Ok(0);
        }
        let mut total_sent = 0;
        let result = self.general.send_poller_with(self, extra_nb, || {
            request_poll();
            let step = self.with_smol_socket(|socket| {
                if !socket.is_active() {
                    Err(AxError::NotConnected)
                } else if !socket.can_send() {
                    Err(AxError::WouldBlock)
                } else {
                    // connected, and the tx buffer is not full
                    let len = socket
                        .send(|buffer| {
                            let result = src.read(buffer);
                            let len = result.unwrap_or(0);
                            (len, result)
                        })
                        .map_err(|_| ax_err_type!(NotConnected, "not connected?"))??;
                    Ok(len)
                }
            });
            if step.as_ref().is_ok_and(|sent| *sent > 0) {
                request_poll();
            }
            finish_tcp_send_step(&mut total_sent, target_len, extra_nb, step)
        });
        if result.is_ok() {
            request_poll();
        }
        result
    }

    fn recv(&self, mut dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize> {
        if self.rx_closed.load(Ordering::Acquire) {
            return Err(AxError::NotConnected);
        }
        if self.state.get() == State::Closed {
            return Err(AxError::NotConnected);
        }
        let extra_nb = options.flags.contains(RecvFlags::DONTWAIT);
        self.general.recv_poller_with(self, extra_nb, || {
            request_poll();
            self.with_smol_socket(|socket| {
                if socket.recv_queue() > 0 {
                    if options.flags.contains(RecvFlags::PEEK) {
                        dst.write(
                            socket
                                .peek(dst.remaining_mut())
                                .map_err(|_| ax_err_type!(NotConnected, "not connected?"))?,
                        )
                    } else {
                        // Drain currently available bytes from RX queue without waiting.
                        // This loop copies across smoltcp's internal buffer segments to fill
                        // the user buffer with as many bytes as are ready, but does not block
                        // waiting for more data to arrive.
                        let mut total = 0;
                        while socket.recv_queue() > 0 && dst.remaining_mut() > 0 {
                            let len = socket
                                .recv(|buf| {
                                    let result = dst.write(buf);
                                    let len = result.unwrap_or(0);
                                    (len, result)
                                })
                                .map_err(|_| ax_err_type!(NotConnected, "not connected?"))??;
                            if len == 0 {
                                break;
                            }
                            total += len;
                        }
                        Ok(total)
                    }
                } else if !socket.may_recv() {
                    Ok(0)
                } else {
                    Err(AxError::WouldBlock)
                }
            })
        })
    }

    fn recv_available(&self) -> AxResult<usize> {
        if self.state.get() == State::Listening {
            return Err(AxError::InvalidInput);
        }
        let available = self.with_smol_socket(|socket| socket.recv_queue());
        if available > 0 {
            return Ok(available);
        }
        request_poll();
        Ok(self.with_smol_socket(|socket| socket.recv_queue()))
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        let endpoint = self.with_smol_socket(|socket| {
            socket
                .local_endpoint()
                .map(|endpoint| IpListenEndpoint {
                    addr: Some(endpoint.addr),
                    port: endpoint.port,
                })
                .unwrap_or_else(|| *self.bound_endpoint.lock())
        });
        Ok(SocketAddrEx::Ip(SocketAddr::new(
            endpoint
                .addr
                .map_or_else(|| Ipv4Addr::UNSPECIFIED.into(), Into::into),
            endpoint.port,
        )))
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        self.with_smol_socket(|socket| {
            Ok(SocketAddrEx::Ip(
                socket
                    .remote_endpoint()
                    .or_else(|| *self.peer_endpoint.lock())
                    .ok_or(AxError::NotConnected)?
                    .into(),
            ))
        })
    }

    fn shutdown(&self, how: Shutdown) -> AxResult {
        // TODO(mivik): shutdown
        if how.has_read() {
            self.rx_closed.store(true, Ordering::Release);
            // rx_closed is visible before waking RDHUP/EOF waiters.
            unsafe { self.poll_rx_closed.wake(IoEvents::RDHUP | IoEvents::IN) };
        }

        // stream
        if let Ok(guard) = self.state.lock(State::Connected) {
            if how.has_read() && how.has_write() {
                guard.transit(State::Closed, || {
                    self.with_smol_socket(|socket| {
                        debug!("TCP socket {}: shutting down", self.handle);
                        socket.close();
                    });
                    self.clear_tracked_egress_ip_tos();
                    self.unregister_bound_endpoint();
                    *self.bound_endpoint.lock() = empty_endpoint();
                    request_poll();
                    Ok(())
                })?;
            } else if how.has_write() {
                self.with_smol_socket(|socket| {
                    debug!("TCP socket {}: shutting down write side", self.handle);
                    socket.close();
                });
                request_poll();
            }
        }

        // listener
        if let Ok(guard) = self.state.lock(State::Listening) {
            guard.transit(State::Closed, || {
                LISTEN_TABLE.unlisten(self.bound_endpoint()?);
                self.clear_tracked_egress_ip_tos();
                self.unregister_bound_endpoint();
                *self.bound_endpoint.lock() = empty_endpoint();
                request_poll();
                Ok(())
            })?;
        }

        // ignore for other states
        Ok(())
    }
}

impl Pollable for TcpSocket {
    fn poll(&self) -> IoEvents {
        request_poll();
        let mut events = match self.state.get() {
            State::Connecting => self.poll_connect(),
            State::Connected | State::Idle | State::Closed => self.poll_stream(),
            State::Listening => self.poll_listener(),
            State::Busy => IoEvents::empty(),
        };
        events.set(IoEvents::RDHUP, self.rx_closed.load(Ordering::Acquire));
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let mut accept_registration = None;
        if self.state.get() == State::Listening && events.intersects(IoEvents::IN | IoEvents::RDHUP)
        {
            let port = self.bound_endpoint.lock().port;
            if port != 0 {
                let endpoint = *self.bound_endpoint.lock();
                if let Some(accept_poll) = LISTEN_TABLE.accept_poll(endpoint) {
                    // accept registration runs from task poll context after
                    // releasing the listen-table lock.
                    unsafe { accept_poll.register(context.waker(), IoEvents::IN) };
                    let accept_waker = LISTEN_TABLE.accept_waker(accept_poll.clone());
                    accept_registration = Some((endpoint, accept_poll, accept_waker));
                }
            }
        }
        let recv_waker = if events.intersects(IoEvents::IN | IoEvents::RDHUP) {
            // Socket registration runs from task poll context before taking the
            // socket-set lock.
            unsafe {
                self.poll_rx
                    .register(context.waker(), IoEvents::IN | IoEvents::RDHUP)
            };
            Some(Waker::from(Arc::new(DeferPollWake {
                poll: self.poll_rx.clone(),
                ready: IoEvents::IN | IoEvents::RDHUP,
            })))
        } else {
            None
        };
        let send_waker = if events.contains(IoEvents::OUT) {
            // Socket registration runs from task poll context before taking the
            // socket-set lock.
            unsafe { self.poll_tx.register(context.waker(), IoEvents::OUT) };
            Some(Waker::from(Arc::new(DeferPollWake {
                poll: self.poll_tx.clone(),
                ready: IoEvents::OUT,
            })))
        } else {
            None
        };
        if let Some((endpoint, accept_poll, accept_waker)) = accept_registration.as_ref() {
            let mut sockets = SOCKET_SET.inner.lock();
            LISTEN_TABLE.register_pending_accept_wakers(
                *endpoint,
                &mut sockets,
                accept_poll,
                accept_waker,
            );
        }
        self.with_smol_socket(|socket| {
            if let Some(waker) = recv_waker.as_ref() {
                socket.register_recv_waker(waker);
            }
            if let Some(waker) = send_waker.as_ref() {
                socket.register_send_waker(waker);
            }
        });
        if events.intersects(IoEvents::IN | IoEvents::OUT | IoEvents::RDHUP) {
            self.general.register_waker(context.waker());
        }
        if events.contains(IoEvents::RDHUP) {
            // Registration happens from socket poll task context.
            unsafe {
                self.poll_rx_closed
                    .register(context.waker(), IoEvents::RDHUP | IoEvents::IN)
            };
        }
    }
}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        let endpoint = *self.bound_endpoint.lock();
        if self.state.get() == State::Listening && endpoint.port != 0 {
            LISTEN_TABLE.unlisten(endpoint);
        }

        let should_orphan = self.with_smol_socket(|socket| {
            let state = socket.state();
            let should_orphan = matches!(
                state,
                smol::State::Established
                    | smol::State::CloseWait
                    | smol::State::FinWait1
                    | smol::State::FinWait2
                    | smol::State::Closing
                    | smol::State::LastAck
                    | smol::State::TimeWait
            ) || socket.send_queue() > 0;
            if matches!(
                state,
                smol::State::Established
                    | smol::State::SynSent
                    | smol::State::SynReceived
                    | smol::State::CloseWait
                    | smol::State::FinWait1
                    | smol::State::FinWait2
                    | smol::State::Closing
                    | smol::State::LastAck
            ) {
                debug!("TCP socket {}: closing on drop", self.handle);
                socket.close();
            }
            should_orphan
        });

        // Unbind from API layer (port registry, etc.)
        self.clear_tracked_egress_ip_tos();
        self.unregister_bound_endpoint();

        if should_orphan {
            // Keep the smoltcp socket alive after the user-facing handle is gone.
            let timestamp = smoltcp::time::Instant::from_micros_const(
                (ax_hal::time::monotonic_time_nanos() / 1_000) as i64,
            );
            crate::orphan::add_orphan(self.handle, timestamp);
        } else {
            SOCKET_SET.remove(self.handle);
        }

        // Wake net-poll worker to process teardown
        crate::request_poll();
    }
}

fn duration_micros_u32(value: Duration) -> u32 {
    value.total_micros().min(u32::MAX as u64) as u32
}

fn tcp_state_info(state: smol::State) -> TcpState {
    match state {
        smol::State::Closed => TcpState::Closed,
        smol::State::Listen => TcpState::Listen,
        smol::State::SynSent => TcpState::SynSent,
        smol::State::SynReceived => TcpState::SynReceived,
        smol::State::Established => TcpState::Established,
        smol::State::FinWait1 => TcpState::FinWait1,
        smol::State::FinWait2 => TcpState::FinWait2,
        smol::State::CloseWait => TcpState::CloseWait,
        smol::State::Closing => TcpState::Closing,
        smol::State::LastAck => TcpState::LastAck,
        smol::State::TimeWait => TcpState::TimeWait,
    }
}

const fn empty_endpoint() -> IpListenEndpoint {
    IpListenEndpoint {
        addr: None,
        port: 0,
    }
}

impl TcpSocket {
    /// Starts an active open and leaves completion to the net-poll worker.
    fn start_connect(&self, remote_addr: SocketAddr) -> AxResult {
        self.state
            .lock(State::Idle)
            .map_err(|state| {
                if state == State::Connecting {
                    AxError::InProgress
                } else {
                    // TODO(mivik): error code
                    ax_err_type!(AlreadyConnected)
                }
            })?
            .transit(State::Connecting, || {
                self.pending_error.store(0, Ordering::Release);
                // TODO: check remote addr unreachable
                // let (bound_endpoint, remote_endpoint) = self.get_endpoint_pair(remote_addr)?;
                let remote_endpoint = IpEndpoint::from(remote_addr);
                let mut bound_endpoint = *self.bound_endpoint.lock();

                // Record original bind state before modifying
                let was_unbound_or_unspecified =
                    bound_endpoint.addr.is_none_or(|addr| addr.is_unspecified());
                let had_explicit_device_binding = self.general.device_binding().bound_if.is_some();

                // Fill source address if unbound or bound to 0.0.0.0
                if bound_endpoint.addr.is_none_or(|addr| addr.is_unspecified()) {
                    bound_endpoint.addr = Some(
                        get_control()
                            .select_route_with_binding(
                                &remote_endpoint.addr,
                                self.general.device_binding(),
                            )?
                            .source,
                    );
                }
                if bound_endpoint.port == 0 {
                    bound_endpoint.port = get_ephemeral_port()?;
                }
                info!(
                    "TCP connection from {} to {}",
                    bound_endpoint, remote_endpoint
                );
                self.with_bound_endpoint_registered(bound_endpoint, || {
                    let mut service = get_service();
                    let context = service.iface.context();
                    self.with_smol_socket(|socket| {
                        socket
                            .connect(context, remote_endpoint, bound_endpoint)
                            .map_err(|e| match e {
                                smol::ConnectError::InvalidState => {
                                    ax_err_type!(AlreadyConnected)
                                }
                                smol::ConnectError::Unaddressable => {
                                    ax_err_type!(ConnectionRefused, "unaddressable")
                                }
                            })?;
                        Ok::<(), AxError>(())
                    })
                })?;
                *self.bound_endpoint.lock() = bound_endpoint;

                // Only set device binding if was originally unbound or bound to 0.0.0.0
                // Binding to a specific IP should lock the interface
                if !had_explicit_device_binding && was_unbound_or_unspecified {
                    self.general
                        .set_device_binding(get_control().local_binding_for(&bound_endpoint)?);
                }
                // else: bound to specific IP, keep existing interface binding
                self.sync_egress_ip_tos();

                Ok(())
            })
    }

    /// Registers the public TCP bind side table if not already registered.
    fn register_bound_endpoint(&self, endpoint: IpListenEndpoint) -> AxResult {
        if !self.bound_registered.load(Ordering::Acquire) {
            register_tcp_bound(endpoint, self.general.reuse_port())?;
            self.bound_registered.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn with_bound_endpoint_registered<R>(
        &self,
        endpoint: IpListenEndpoint,
        f: impl FnOnce() -> AxResult<R>,
    ) -> AxResult<R> {
        let register_bound = !self.bound_registered.load(Ordering::Acquire);
        if register_bound {
            register_tcp_bound(endpoint, self.general.reuse_port())?;
        }
        match f() {
            Ok(value) => {
                if register_bound {
                    self.bound_registered.store(true, Ordering::Release);
                }
                Ok(value)
            }
            Err(err) => {
                if register_bound {
                    unregister_tcp_bound(endpoint);
                }
                Err(err)
            }
        }
    }

    /// Removes the public TCP bind side-table entry, if present.
    fn unregister_bound_endpoint(&self) {
        if self.bound_registered.swap(false, Ordering::AcqRel) {
            unregister_tcp_bound(*self.bound_endpoint.lock());
        }
    }
}

/// One TCP bind ownership record. Several records may share a port only when
/// every binder requested SO_REUSEPORT on the identical local address, mirroring
/// Linux's reuseport group semantics.
struct TcpBoundEntry {
    addr: Option<smoltcp::wire::IpAddress>,
    reuse_port: bool,
}

static TCP_BOUND_PORTS: LazyLock<Mutex<HashMap<u16, Vec<TcpBoundEntry>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers TCP bind ownership with wildcard/specific address conflicts.
///
/// A binder joins an existing reuseport group only when it and every colliding
/// owner requested SO_REUSEPORT on the exact same local address; any other
/// address overlap on the port is rejected with `EADDRINUSE`.
fn register_tcp_bound(endpoint: IpListenEndpoint, reuse_port: bool) -> AxResult {
    if endpoint.port == 0 {
        return Ok(());
    }

    let mut bound_ports = TCP_BOUND_PORTS.lock();
    let entries = bound_ports.entry(endpoint.port).or_default();
    for entry in entries.iter() {
        if listen_addrs_conflict(entry.addr, endpoint.addr)
            && !(reuse_port && entry.reuse_port && entry.addr == endpoint.addr)
        {
            return Err(AxError::AddrInUse);
        }
    }
    entries.push(TcpBoundEntry {
        addr: endpoint.addr,
        reuse_port,
    });
    Ok(())
}

/// Removes one TCP bind registration.
fn unregister_tcp_bound(endpoint: IpListenEndpoint) {
    if endpoint.port == 0 {
        return;
    }
    let mut bound_ports = TCP_BOUND_PORTS.lock();
    let Some(entries) = bound_ports.get_mut(&endpoint.port) else {
        return;
    };
    if let Some(index) = entries.iter().position(|entry| entry.addr == endpoint.addr) {
        entries.swap_remove(index);
    }
    if entries.is_empty() {
        bound_ports.remove(&endpoint.port);
    }
}

/// Returns whether a port is safe for ephemeral TCP allocation.
fn tcp_port_available(port: u16) -> bool {
    // Ephemeral ports are selected conservatively: avoid any port that has a
    // listener or bound socket on any local address.
    LISTEN_TABLE.can_listen(IpListenEndpoint { addr: None, port })
        && !TCP_BOUND_PORTS.lock().contains_key(&port)
}

fn finish_tcp_send_step(
    total_sent: &mut usize,
    target_len: usize,
    extra_nonblocking: bool,
    step: AxResult<usize>,
) -> AxResult<usize> {
    match step {
        Ok(sent) => {
            *total_sent += sent;
            if *total_sent >= target_len || extra_nonblocking {
                Ok(*total_sent)
            } else {
                Err(AxError::WouldBlock)
            }
        }
        Err(AxError::WouldBlock) if *total_sent > 0 && extra_nonblocking => Ok(*total_sent),
        Err(AxError::WouldBlock) => Err(AxError::WouldBlock),
        Err(_) if *total_sent > 0 => Ok(*total_sent),
        Err(err) => Err(err),
    }
}

fn get_ephemeral_port() -> AxResult<u16> {
    allocate_ephemeral_port(tcp_port_available)
}

#[cfg(test)]
mod tests {
    use core::net::{IpAddr, SocketAddr};

    use super::*;
    use crate::{
        options::{Configurable, GetSocketOption, SetSocketOption, TcpState},
        test_support::{
            LOCAL_ADDR, LOCAL_IF, PEER_ADDR, PEER_IF, init_split_route_network, network_test_guard,
        },
    };

    #[test]
    fn blocking_tcp_send_waits_after_partial_write() {
        let mut total = 0;

        assert_eq!(
            finish_tcp_send_step(&mut total, 10, false, Ok(4)),
            Err(AxError::WouldBlock),
        );
        assert_eq!(total, 4);
        assert_eq!(finish_tcp_send_step(&mut total, 10, false, Ok(6)), Ok(10),);
        assert_eq!(total, 10);
    }

    #[test]
    fn dontwait_tcp_send_returns_first_partial_write() {
        let mut total = 0;

        assert_eq!(finish_tcp_send_step(&mut total, 10, true, Ok(4)), Ok(4),);
        assert_eq!(total, 4);
    }

    #[test]
    fn tcp_send_returns_partial_count_after_later_error() {
        let mut total = 4;

        assert_eq!(
            finish_tcp_send_step(&mut total, 10, false, Err(AxError::NotConnected)),
            Ok(4),
        );
        assert_eq!(total, 4);
    }

    #[test]
    fn blocking_tcp_send_keeps_waiting_after_partial_wouldblock() {
        let mut total = 4;

        assert_eq!(
            finish_tcp_send_step(&mut total, 10, false, Err(AxError::WouldBlock)),
            Err(AxError::WouldBlock),
        );
        assert_eq!(total, 4);
    }

    #[test]
    fn tcp_info_reports_default_socket_metrics() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = TcpSocket::new();
        let mut info = TcpInfo::default();

        socket
            .get_option(GetSocketOption::TcpInfo(&mut info))
            .unwrap();

        assert_eq!(info.state, TcpState::Closed);
        assert_eq!(info.snd_mss, TCP_INFO_DEFAULT_MSS);
        assert_eq!(info.rcv_mss, TCP_INFO_DEFAULT_MSS);
        assert_eq!(info.pmtu, TCP_INFO_DEFAULT_PMTU);
        assert_eq!(info.notsent_bytes, 0);
        assert_eq!(info.snd_wnd, 0);
        assert_eq!(info.snd_cwnd, 0);
        assert_eq!(info.rcv_space, 0);
        assert_eq!(info.rcv_wnd, 0);
    }

    #[test]
    fn connect_preserves_bound_interface() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = TcpSocket::new();
        let nonblocking = true;
        socket
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
            .unwrap();
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(IpAddr::V4(LOCAL_ADDR), 0)))
            .unwrap();
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(LOCAL_IF)
            }
        );

        // Connect to different network - should NOT change interface binding
        // because we're bound to a specific local address
        socket
            .start_connect(SocketAddr::new(IpAddr::V4(PEER_ADDR), 80))
            .unwrap();

        // Interface binding should remain LOCAL_IF (not changed to PEER_IF)
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(LOCAL_IF)
            }
        );
    }

    #[test]
    fn connect_uses_peer_route_when_unbound() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = TcpSocket::new();
        let nonblocking = true;
        socket
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
            .unwrap();

        // Bind to 0.0.0.0 (unspecified) - interface should be determined by route
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))
            .unwrap();

        socket
            .start_connect(SocketAddr::new(IpAddr::V4(PEER_ADDR), 80))
            .unwrap();

        // Interface binding should use route decision (PEER_IF)
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(PEER_IF)
            }
        );
    }

    #[test]
    fn connect_rejects_unroutable_bound_device() {
        let _guard = network_test_guard();
        init_split_route_network();

        let socket = TcpSocket::new();
        let nonblocking = true;
        socket
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
            .unwrap();
        socket.bind_device(LOCAL_IF).unwrap();
        socket
            .bind(SocketAddrEx::Ip(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                0,
            )))
            .unwrap();

        assert!(
            socket
                .start_connect(SocketAddr::new(IpAddr::V4(PEER_ADDR), 80))
                .is_err()
        );
        assert_eq!(
            socket.general.device_binding(),
            DeviceBinding {
                bound_if: Some(LOCAL_IF)
            }
        );
    }

    #[test]
    fn reuseport_group_shares_a_port_while_plain_binders_conflict() {
        let _guard = network_test_guard();

        let endpoint = IpListenEndpoint {
            addr: None,
            port: 0xB70F,
        };

        // A plain binder owns the port exclusively.
        register_tcp_bound(endpoint, false).unwrap();
        assert_eq!(
            register_tcp_bound(endpoint, false).unwrap_err(),
            AxError::AddrInUse
        );
        // SO_REUSEPORT cannot join a group started by a non-reuseport owner.
        assert_eq!(
            register_tcp_bound(endpoint, true).unwrap_err(),
            AxError::AddrInUse
        );
        unregister_tcp_bound(endpoint);

        // Two reuseport binders share the port, mirroring Linux's group model.
        register_tcp_bound(endpoint, true).unwrap();
        register_tcp_bound(endpoint, true).unwrap();
        // A plain binder still cannot steal a reuseport-owned port.
        assert_eq!(
            register_tcp_bound(endpoint, false).unwrap_err(),
            AxError::AddrInUse
        );

        // Each unregister drops exactly one group member.
        unregister_tcp_bound(endpoint);
        unregister_tcp_bound(endpoint);
        assert!(!TCP_BOUND_PORTS.lock().contains_key(&endpoint.port));
    }
}

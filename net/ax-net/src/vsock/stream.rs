//! Vsock stream transport.
//!
//! Stream sockets are backed by entries in the vsock connection manager and are
//! driven by the adaptive vsock device poll task rather than the smoltcp IP
//! poller.
//!
//! # Public State
//!
//! The transport uses `StateLock` for POSIX-facing socket transitions while the
//! connection manager tracks host-visible vsock connection state. Operations
//! must keep those two views synchronized at bind, listen, connect, accept, and
//! shutdown boundaries.
//!
//! # Readiness
//!
//! Poll readiness comes from connection-manager wait queues and poll sets. The
//! stream transport must not acquire smoltcp service/socket locks because vsock
//! is independent from the IP protocol core.

use alloc::sync::Arc;
use core::task::Context;

use ax_errno::{AxError, AxResult, ax_bail, ax_err_type};
use ax_io::prelude::*;
use ax_sync::PiMutex;
use axpoll::{IoEvents, Pollable};

use super::connection_manager::*;
use crate::{
    RecvFlags, RecvOptions, SendOptions, Shutdown,
    device::*,
    general::GeneralOptions,
    options::{Configurable, GetSocketOption, SetSocketOption},
    state::*,
    vsock::{VsockAddr, VsockConnId},
};

/// Stream transport for vsock sockets.
pub struct VsockStreamTransport {
    /// Connection id registered with the vsock manager.
    conn_id: PiMutex<Option<VsockConnId>>,
    /// Shared connection state once bound, connecting, or connected.
    connection: PiMutex<Option<Arc<Connection>>>,
    /// Public POSIX-facing stream state.
    state: StateLock,
    /// Shared socket options.
    general: GeneralOptions,
}

fn retire_listener(port: u32) {
    let retired = VSOCK_CONN_MANAGER.lock().unlisten(port);
    for (conn_id, connection) in retired {
        retire_connection(conn_id, connection);
    }
}

impl VsockStreamTransport {
    /// Create a new idle vsock stream transport.
    pub fn new() -> Self {
        Self {
            conn_id: PiMutex::new(None),
            connection: PiMutex::new(None),
            state: StateLock::new(State::Idle),
            general: GeneralOptions::new(1, 40, 0), // SOCK_STREAM
        }
    }

    /// Returns the manager connection associated with this stream.
    fn get_connection(&self) -> AxResult<Arc<Connection>> {
        self.connection.lock().clone().ok_or(AxError::NotConnected)
    }
}

impl Default for VsockStreamTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Configurable for VsockStreamTransport {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        self.general.get_option_inner(opt)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool> {
        self.general.set_option_inner(opt)
    }
}

impl VsockStreamTransport {
    pub(super) fn bind(&self, mut local_addr: VsockAddr) -> AxResult<()> {
        self.state
            .lock(State::Idle)
            .map_err(|_| ax_err_type!(InvalidInput, "already bound"))?
            .transit(State::Idle, || {
                let poll_lease = start_vsock_poll()?;
                let conn = {
                    let mut manager = VSOCK_CONN_MANAGER.lock();
                    if local_addr.port == 0 {
                        local_addr.port = manager.allocate_port()?;
                    }
                    let conn_id = VsockConnId::listening(local_addr.port);
                    manager.create_connection(
                        conn_id,
                        local_addr,
                        None,
                        ConnectionState::Idle,
                        poll_lease,
                    )?
                };
                let conn_id = VsockConnId::listening(local_addr.port);

                *self.conn_id.lock() = Some(conn_id);
                *self.connection.lock() = Some(conn);
                trace!("Vsock binding to {:?}", local_addr);
                Ok(())
            })?;
        Ok(())
    }

    pub(super) fn listen(&self) -> AxResult<()> {
        let guard = self
            .state
            .lock(State::Idle)
            .map_err(|_| ax_err_type!(InvalidInput, "invalid state for listen"))?;

        guard.transit(State::Listening, || {
            let conn = self.get_connection()?;
            let local_addr = {
                let state = conn.lock();
                state.local_addr()
            };

            VSOCK_CONN_MANAGER.lock().listen(local_addr)?;
            if let Err(error) = vsock_listen(local_addr) {
                retire_listener(local_addr.port);
                return Err(error);
            }
            conn.lock().set_state(ConnectionState::Listening);
            trace!("Vsock listening on {:?}", local_addr);
            Ok(())
        })
    }

    pub(super) fn accept(&self) -> AxResult<(VsockStreamTransport, VsockAddr)> {
        if self.state.get() != State::Listening {
            ax_bail!(InvalidInput, "not listening");
        }

        let conn = self.get_connection()?;
        let local_port = conn.lock().local_addr().port;

        // wait for connection
        self.general.recv_poller(self, || {
            let mut manager = VSOCK_CONN_MANAGER.lock();

            if !manager.can_accept(local_port) {
                return Err(AxError::WouldBlock);
            }

            let (conn_id, peer_addr) = manager.accept(local_port)?;
            let conn = manager.get_connection(conn_id).ok_or(AxError::NotFound)?;
            drop(manager);

            // create new VsockStreamTransport
            let new_transport = VsockStreamTransport {
                conn_id: PiMutex::new(Some(conn_id)),
                connection: PiMutex::new(Some(conn)),
                state: StateLock::new(State::Connected),
                general: GeneralOptions::new(1, 40, 0), // SOCK_STREAM
            };

            Ok((new_transport, peer_addr))
        })
    }

    pub(super) fn connect(&self, peer_addr: VsockAddr) -> AxResult<()> {
        let guard = self.state.lock(State::Idle).map_err(|state| match state {
            State::Idle => unreachable!(),
            State::Listening => ax_err_type!(InvalidInput, "already listening"),
            State::Connecting => ax_err_type!(InProgress),
            State::Connected => ax_err_type!(AlreadyConnected),
            _ => ax_err_type!(AlreadyConnected),
        })?;

        guard.transit(State::Connecting, || {
            let existing_conn = self.connection.lock().clone();
            let old_conn_id = *self.conn_id.lock();
            let local_port = if let Some(conn) = existing_conn.as_ref() {
                let state = conn.lock();
                match state.state() {
                    ConnectionState::Idle => state.local_addr().port,
                    _ => {
                        ax_bail!(InvalidInput, "already connected or listening");
                    }
                }
            } else {
                VSOCK_CONN_MANAGER.lock().allocate_port()?
            };
            let local_addr = VsockAddr {
                cid: vsock_guest_cid()?,
                port: local_port,
            };

            // create connection
            let conn_id = VsockConnId {
                peer_addr,
                local_port,
            };
            let poll_lease = start_vsock_poll()?;
            let conn = VSOCK_CONN_MANAGER.lock().create_connection(
                conn_id,
                local_addr,
                Some(peer_addr),
                ConnectionState::Connecting,
                poll_lease,
            )?;

            if let Err(error) = vsock_connect(conn_id) {
                VSOCK_CONN_MANAGER
                    .lock()
                    .remove_connection_if(conn_id, &conn);
                return Err(error);
            }

            let _ = match (old_conn_id, existing_conn.as_ref()) {
                (Some(old), Some(existing)) if old != conn_id => VSOCK_CONN_MANAGER
                    .lock()
                    .remove_connection_if(old, existing),
                _ => None,
            };
            *self.conn_id.lock() = Some(conn_id);
            *self.connection.lock() = Some(conn);
            debug!("Vsock connecting from {} to {:?}", local_port, peer_addr);
            Ok(())
        })?;

        // wait for connection established
        self.general.send_poller(self, || {
            let conn = self.get_connection()?;
            let state = conn.lock().state();
            match state {
                ConnectionState::Connected => Ok(()),
                ConnectionState::Connecting => Err(AxError::WouldBlock),
                _ => Err(ax_err_type!(ConnectionRefused)),
            }
        })
    }

    pub(super) fn send(
        &self,
        mut src: impl Read + IoBuf,
        _options: SendOptions,
    ) -> AxResult<usize> {
        let conn = self.get_connection()?;
        let conn_guard = conn.lock();

        if conn_guard.state() != ConnectionState::Connected {
            return Err(AxError::NotConnected);
        }

        if conn_guard.tx_closed() {
            return Err(AxError::NotConnected);
        }

        let conn_id = self.conn_id.lock().ok_or(AxError::NotConnected)?;
        drop(conn_guard);

        // now virtio-driver only support non-blocking send
        let result = src.write_to(&mut ax_io::write_fn(|buf| vsock_send(conn_id, buf)));
        conn.lock().add_tx_bytes(result.unwrap_or(0));
        result
    }

    pub(super) fn recv(&self, mut dst: impl Write, options: RecvOptions) -> AxResult<usize> {
        let conn = self.get_connection()?;
        let extra_nb = options.flags.contains(RecvFlags::DONTWAIT);

        self.general.recv_poller_with(self, extra_nb, || {
            let mut conn_guard = conn.lock();

            if conn_guard.rx_closed() && conn_guard.rx_buffer_used() == 0 {
                return Ok(0); // EOF
            }

            // should allow read when connection is closed, to read remaining data
            if !matches!(
                conn_guard.state(),
                ConnectionState::Connected | ConnectionState::Closed
            ) {
                return Err(AxError::NotConnected);
            }

            if conn_guard.rx_buffer_used() == 0 {
                return Err(AxError::WouldBlock);
            }

            let (left, right) = conn_guard.rx_slices();
            let mut count = dst.write(left)?;

            if count >= left.len() && !right.is_empty() {
                count += dst.write(right)?;
            }
            if !options.flags.contains(RecvFlags::PEEK) {
                conn_guard.advance_rx_read(count);
            }

            if count > 0 {
                trace!(
                    "Recv {} bytes from connection (buffer_remaining={}/{})",
                    count,
                    conn_guard.rx_buffer_used(),
                    VSOCK_RX_BUFFER_SIZE
                );
                Ok(count)
            } else {
                Err(AxError::WouldBlock)
            }
        })
    }

    pub(super) fn shutdown(&self, how: Shutdown) -> AxResult<()> {
        let conn = self.get_connection()?;
        let previous_state = {
            let mut state = conn.lock();
            if how.has_read() {
                state.set_rx_closed(true);
            }
            if how.has_write() {
                state.set_tx_closed(true);
            }
            let previous = state.state();
            state.set_state(ConnectionState::Closed);
            previous
        };
        let conn_id = *self.conn_id.lock();
        if let Some(conn_id) = conn_id {
            match previous_state {
                ConnectionState::Connected => vsock_disconnect(conn_id)?,
                ConnectionState::Listening => {
                    retire_listener(conn_id.local_port);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn local_addr(&self) -> AxResult<Option<VsockAddr>> {
        Ok(self
            .get_connection()
            .ok()
            .map(|conn| conn.lock().local_addr()))
    }

    pub(super) fn peer_addr(&self) -> AxResult<Option<VsockAddr>> {
        Ok(self
            .get_connection()
            .ok()
            .and_then(|conn| conn.lock().peer_addr()))
    }
}

impl Pollable for VsockStreamTransport {
    fn poll(&self) -> IoEvents {
        let Ok(conn) = self.get_connection() else {
            return IoEvents::empty();
        };

        let state = conn.lock();
        let connection_state = state.state();
        let rx_ready = state.rx_buffer_used() > 0 || state.rx_closed();
        let tx_ready = !state.tx_closed();
        let rx_closed = state.rx_closed();
        drop(state);
        let mut events = IoEvents::empty();

        match connection_state {
            ConnectionState::Listening => {
                let conn_id = *self.conn_id.lock();
                if let Some(conn_id) = conn_id {
                    events.set(
                        IoEvents::IN,
                        VSOCK_CONN_MANAGER.lock().can_accept(conn_id.local_port),
                    );
                }
            }
            ConnectionState::Connected | ConnectionState::Closed => {
                events.set(IoEvents::IN, rx_ready);
                events.set(IoEvents::OUT, tx_ready);
            }
            ConnectionState::Connecting => {
                events.set(IoEvents::OUT, false);
            }
            _ => {}
        }
        events.set(IoEvents::RDHUP, rx_closed);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if let Ok(conn) = self.get_connection() {
            let (state, local_port) = {
                let state = conn.lock();
                (state.state(), state.local_addr().port)
            };
            match state {
                ConnectionState::Listening if events.contains(IoEvents::IN) => {
                    let queue = VSOCK_CONN_MANAGER.lock().get_listen_queue(local_port);
                    if let Some(queue) = queue {
                        queue.lock().register_poll(context);
                    }
                }
                ConnectionState::Connected => {
                    if events.contains(IoEvents::IN) {
                        conn.register_rx_poll(context);
                    }
                    if events.contains(IoEvents::OUT) {
                        warn!(
                            "VsockStreamTransport: OUT event on connected socket is not supported"
                        );
                    }
                }
                ConnectionState::Connecting if events.contains(IoEvents::OUT) => {
                    conn.register_connect_poll(context);
                }
                _ => {}
            }
        }
    }
}

impl Drop for VsockStreamTransport {
    fn drop(&mut self) {
        let _ = self.shutdown(Shutdown::Both);

        let conn_id = *self.conn_id.lock();
        if let Some(conn_id) = conn_id {
            let connection = self.connection.lock().clone();
            let removed = connection.as_ref().and_then(|connection| {
                VSOCK_CONN_MANAGER
                    .lock()
                    .remove_connection_if(conn_id, connection)
            });
            if let Some(connection) = removed {
                retire_connection(conn_id, connection);
            }
        }
    }
}

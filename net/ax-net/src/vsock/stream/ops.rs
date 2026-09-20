//! Vsock stream lifecycle and I/O operations.

use ax_io::prelude::*;
use ax_sync::Mutex;

use super::VsockStreamTransport;
use crate::{
    ConnectStatus, NetError, NetResult, RecvFlags, RecvOptions, SendOptions, Shutdown,
    device::*,
    general::GeneralOptions,
    state::*,
    vsock::{VsockAddr, VsockConnId, connection_manager::*},
};

fn retire_listener(port: u32) {
    let retired = VSOCK_CONN_MANAGER.lock().unlisten(port);
    for (conn_id, connection) in retired {
        retire_connection(conn_id, connection);
    }
}

impl VsockStreamTransport {
    pub(in crate::vsock) fn bind(&self, mut local_addr: VsockAddr) -> NetResult<()> {
        self.state
            .lock(State::Idle)
            .map_err(|_| NetError::InvalidInput)?
            .transit(State::Idle, || {
                let conn = {
                    let mut manager = VSOCK_CONN_MANAGER.lock();
                    if local_addr.port == 0 {
                        local_addr.port = manager.allocate_port()?;
                    }
                    let conn_id = VsockConnId::listening(local_addr.port);
                    manager.create_connection(conn_id, local_addr, None, ConnectionState::Idle)?
                };
                let conn_id = VsockConnId::listening(local_addr.port);

                *self.conn_id.lock() = Some(conn_id);
                *self.connection.lock() = Some(conn);
                trace!("Vsock binding to {:?}", local_addr);
                Ok(())
            })?;
        Ok(())
    }

    pub(in crate::vsock) fn listen(&self) -> NetResult<()> {
        let guard = self
            .state
            .lock(State::Idle)
            .map_err(|_| NetError::InvalidInput)?;

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

    pub(in crate::vsock) fn try_accept(&self) -> NetResult<(VsockStreamTransport, VsockAddr)> {
        if self.state.get() != State::Listening {
            return Err(NetError::InvalidInput);
        }

        let conn = self.get_connection()?;
        let local_port = conn.lock().local_addr().port;

        let mut manager = VSOCK_CONN_MANAGER.lock();

        if !manager.can_accept(local_port) {
            return Err(NetError::WouldBlock);
        }

        let (conn_id, peer_addr) = manager.accept(local_port)?;
        let conn = manager.get_connection(conn_id).ok_or(NetError::NotFound)?;
        drop(manager);

        let new_transport = VsockStreamTransport {
            conn_id: Mutex::new(Some(conn_id)),
            connection: Mutex::new(Some(conn)),
            state: StateLock::new(State::Connected),
            general: GeneralOptions::new(1, 40, 0), // SOCK_STREAM
        };

        Ok((new_transport, peer_addr))
    }

    pub(in crate::vsock) fn start_connect(&self, peer_addr: VsockAddr) -> NetResult<()> {
        let guard = self.state.lock(State::Idle).map_err(|state| match state {
            State::Idle => unreachable!(),
            State::Listening => NetError::InvalidInput,
            State::Connecting => NetError::InProgress,
            State::Connected => NetError::AlreadyConnected,
            _ => NetError::AlreadyConnected,
        })?;

        guard.transit(State::Connecting, || {
            let existing_conn = self.connection.lock().clone();
            let old_conn_id = *self.conn_id.lock();
            let local_port = if let Some(conn) = existing_conn.as_ref() {
                let state = conn.lock();
                match state.state() {
                    ConnectionState::Idle => state.local_addr().port,
                    _ => {
                        return Err(NetError::InvalidInput);
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
            let conn = VSOCK_CONN_MANAGER.lock().create_connection(
                conn_id,
                local_addr,
                Some(peer_addr),
                ConnectionState::Connecting,
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
        })
    }

    pub(in crate::vsock) fn connect_status(&self) -> NetResult<ConnectStatus> {
        let conn = self.get_connection()?;
        let state = conn.lock().state();
        match state {
            ConnectionState::Connected => {
                self.state.set(State::Connected);
                Ok(ConnectStatus::Connected)
            }
            ConnectionState::Connecting => Ok(ConnectStatus::InProgress),
            _ => {
                self.state.set(State::Closed);
                Err(NetError::ConnectionRefused)
            }
        }
    }

    pub(in crate::vsock) fn try_send(
        &self,
        mut src: impl Read + IoBuf,
        _options: &mut SendOptions,
    ) -> NetResult<usize> {
        let conn = self.get_connection()?;
        let state = conn.lock();
        if state.state() != ConnectionState::Connected || state.tx_closed() {
            return Err(NetError::NotConnected);
        }
        drop(state);
        if src.remaining() == 0 {
            return Ok(0);
        }

        let conn_id = self.conn_id.lock().ok_or(NetError::NotConnected)?;
        let capacity = vsock_send_capacity(conn_id)?;
        if capacity == 0 {
            return Err(NetError::WouldBlock);
        }

        let result = src.write_to(&mut ax_io::write_fn(|buffer| {
            let send_length = buffer.len().min(capacity);
            vsock_send(conn_id, &buffer[..send_length]).map_err(ax_io::IoError::from)
        }));
        conn.lock()
            .add_tx_bytes(result.as_ref().copied().unwrap_or(0));
        Ok(result?)
    }

    pub(in crate::vsock) fn try_recv(
        &self,
        mut dst: impl Write,
        options: &mut RecvOptions,
    ) -> NetResult<usize> {
        let conn = self.get_connection()?;
        let mut conn_guard = conn.lock();

        if conn_guard.rx_closed() && conn_guard.rx_buffer_used() == 0 {
            return Ok(0); // EOF
        }

        // should allow read when connection is closed, to read remaining data
        if !matches!(
            conn_guard.state(),
            ConnectionState::Connected | ConnectionState::Closed
        ) {
            return Err(NetError::NotConnected);
        }

        if conn_guard.rx_buffer_used() == 0 {
            return Err(NetError::WouldBlock);
        }

        let (left, right) = conn_guard.rx_slices();
        let mut count = dst.write(left)?;

        if count >= left.len() && !right.is_empty() {
            count += dst.write(right)?;
        }
        let consumed = !options.flags.contains(RecvFlags::PEEK);
        if consumed {
            conn_guard.advance_rx_read(count);
        }

        if count > 0 {
            trace!(
                "Recv {} bytes from connection (buffer_remaining={}/{})",
                count,
                conn_guard.rx_buffer_used(),
                VSOCK_RX_BUFFER_SIZE
            );
            drop(conn_guard);
            if consumed {
                request_vsock_work();
            }
            Ok(count)
        } else {
            Err(NetError::WouldBlock)
        }
    }

    pub(in crate::vsock) fn shutdown(&self, how: Shutdown) -> NetResult<()> {
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

    pub(in crate::vsock) fn local_addr(&self) -> NetResult<Option<VsockAddr>> {
        Ok(self
            .get_connection()
            .ok()
            .map(|conn| conn.lock().local_addr()))
    }

    pub(in crate::vsock) fn peer_addr(&self) -> NetResult<Option<VsockAddr>> {
        Ok(self
            .get_connection()
            .ok()
            .and_then(|conn| conn.lock().peer_addr()))
    }
}

//! Vsock readiness publication and final connection retirement.

use core::task::Context;

use axpoll::{IoEvents, Pollable};

use super::VsockStreamTransport;
use crate::{Shutdown, vsock::connection_manager::*};

const fn tx_poll_ready(state: ConnectionState, tx_closed: bool, send_capacity: usize) -> bool {
    matches!(state, ConnectionState::Connected | ConnectionState::Closed)
        && !tx_closed
        && send_capacity != 0
}

impl Pollable for VsockStreamTransport {
    fn poll(&self) -> IoEvents {
        let Ok(conn) = self.get_connection() else {
            return IoEvents::empty();
        };

        let state = conn.lock();
        let connection_state = state.state();
        let rx_ready = state.rx_buffer_used() > 0 || state.rx_closed();
        let tx_closed = state.tx_closed();
        let rx_closed = state.rx_closed();
        drop(state);
        let send_capacity = if tx_closed {
            0
        } else {
            let connection_id = *self.conn_id.lock();
            connection_id
                .and_then(|conn_id| crate::device::vsock_send_capacity(conn_id).ok())
                .unwrap_or(0)
        };
        let tx_ready = tx_poll_ready(connection_state, tx_closed, send_capacity);
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
                        conn.register_tx_poll(context);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_socket_without_transport_credit_is_not_writable() {
        assert!(!tx_poll_ready(ConnectionState::Connected, false, 0));
        assert!(tx_poll_ready(ConnectionState::Connected, false, 1));
        assert!(!tx_poll_ready(ConnectionState::Connected, true, 1));
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

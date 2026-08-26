//! Vsock connection registry.
//!
//! The manager tracks listening, connecting, and established vsock stream
//! connections, owns their byte rings, and provides wakeups used by the vsock
//! transport and fixed-owner IRQ worker.
//!
//! # Event Flow
//!
//! The IRQ worker turns host events into manager calls such as connection
//! request, connected, received data, credit update, and disconnect. Socket
//! transports then observe manager state through connection handles and poll
//! sets.
//!
//! # Buffering
//!
//! Each connection owns an RX byte ring. When the ring is full, the device event
//! path keeps the event pending rather than dropping data, so backpressure is
//! expressed through poll readiness and later receive calls.

use alloc::{collections::BTreeMap, sync::Arc};

use ax_sync::Mutex;

use super::{VsockAddr, VsockConnId};
use crate::{NetError, NetResult};

mod connection;
mod queue;

pub(crate) use connection::*;
use queue::*;

type RetiredConnections = heapless::Vec<(VsockConnId, Arc<Connection>), VSOCK_ACCEPT_QUEUE_SIZE>;

pub(crate) struct IncomingConnection {
    conn_id: VsockConnId,
    connection: Arc<Connection>,
    queue: Arc<Mutex<ListenQueue>>,
}

impl IncomingConnection {
    pub(crate) fn publish(self) {
        let queue_is_current = VSOCK_CONN_MANAGER
            .lock()
            .get_listen_queue(self.conn_id.local_port)
            .is_some_and(|current| Arc::ptr_eq(&current, &self.queue));
        if !queue_is_current {
            self.rollback();
            return;
        }
        self.queue.lock().wake();
    }

    fn rollback(&self) {
        let removed = VSOCK_CONN_MANAGER
            .lock()
            .remove_connection_if(self.conn_id, &self.connection);
        self.queue.lock().accept_queue.remove(self.conn_id);
        if let Some(connection) = removed {
            retire_connection(self.conn_id, connection);
        }
    }
}

pub(crate) fn retire_connection(conn_id: VsockConnId, connection: Arc<Connection>) {
    let stats = connection.stats();
    debug!(
        "Removed connection {:?}: rx={} bytes, tx={} bytes, dropped={} bytes",
        conn_id, stats.rx_bytes, stats.tx_bytes, stats.dropped_bytes
    );
    crate::device::request_vsock_work();
}

/// Global connection manager
pub struct VsockConnectionManager {
    connections: BTreeMap<VsockConnId, Arc<Connection>>,
    listen_queues: BTreeMap<u32, Arc<Mutex<ListenQueue>>>,
    next_ephemeral_port: u32,
}

impl VsockConnectionManager {
    const EPHEMERAL_PORT_END: u32 = 0xffff;
    const EPHEMERAL_PORT_START: u32 = 0xc000;

    pub const fn new() -> Self {
        Self {
            connections: BTreeMap::new(),
            listen_queues: BTreeMap::new(),
            next_ephemeral_port: Self::EPHEMERAL_PORT_START,
        }
    }

    /// Get listen queue from specified port
    pub fn get_listen_queue(&self, port: u32) -> Option<Arc<Mutex<ListenQueue>>> {
        self.listen_queues.get(&port).cloned()
    }

    /// allocate an ephemeral port
    pub fn allocate_port(&mut self) -> NetResult<u32> {
        let start = self.next_ephemeral_port;
        loop {
            let port = self.next_ephemeral_port;
            self.next_ephemeral_port = if port >= Self::EPHEMERAL_PORT_END {
                Self::EPHEMERAL_PORT_START
            } else {
                port + 1
            };

            // check if port is in use by listen queue
            if !self.listen_queues.contains_key(&port) {
                // check if port is in use by existing connections
                let port_in_use = self.connections.keys().any(|id| id.local_port == port);
                if !port_in_use {
                    return Ok(port);
                }
            }

            if self.next_ephemeral_port == start {
                return Err(NetError::AddrInUse);
            }
        }
    }

    /// create a listen queue
    pub fn listen(&mut self, local_addr: VsockAddr) -> NetResult<()> {
        if self.listen_queues.contains_key(&local_addr.port) {
            return Err(NetError::AddrInUse);
        }

        let queue = Arc::new(Mutex::new(ListenQueue::new(local_addr)));
        self.listen_queues.insert(local_addr.port, queue);
        Ok(())
    }

    /// stop listening
    pub fn unlisten(&mut self, port: u32) -> RetiredConnections {
        let mut retired = RetiredConnections::new();
        let Some(queue) = self.listen_queues.remove(&port) else {
            return retired;
        };

        let mut queue = queue.lock();
        while let Some(conn_id) = queue.accept_queue.pop() {
            if let Some(connection) = self.connections.remove(&conn_id) {
                assert!(
                    retired.push((conn_id, connection)).is_ok(),
                    "accept queue capacity bounds listener retirement"
                );
            }
        }
        debug!("Vsock unlisten on port {}", port);
        retired
    }

    /// check if port accept
    pub fn can_accept(&self, port: u32) -> bool {
        self.listen_queues
            .get(&port)
            .map(|q| !q.lock().accept_queue.is_empty())
            .unwrap_or(false)
    }

    /// accept a connection
    pub fn accept(&mut self, port: u32) -> NetResult<(VsockConnId, VsockAddr)> {
        let queue = self
            .listen_queues
            .get(&port)
            .ok_or(NetError::InvalidInput)?;

        let conn_id = queue
            .lock()
            .accept_queue
            .pop()
            .ok_or(NetError::WouldBlock)?;

        let conn = self.connections.get(&conn_id).ok_or(NetError::NotFound)?;

        let peer_addr = conn.lock().peer_addr().ok_or(NetError::NotFound)?;

        debug!("Accepted connection: {:?} from {:?}", conn_id, peer_addr);
        Ok((conn_id, peer_addr))
    }

    /// create a new connection
    pub fn create_connection(
        &mut self,
        conn_id: VsockConnId,
        local_addr: VsockAddr,
        peer_addr: Option<VsockAddr>,
        state: ConnectionState,
    ) -> NetResult<Arc<Connection>> {
        if self.connections.contains_key(&conn_id) {
            return Err(NetError::AlreadyExists);
        }
        let conn = Connection::new_shared(local_addr, peer_addr, state);
        self.connections.insert(conn_id, conn.clone());
        debug!(
            "Created connection {:?}: local={:?}, peer={:?}",
            conn_id, local_addr, peer_addr
        );
        Ok(conn)
    }

    /// get a connection by id
    pub fn get_connection(&self, conn_id: VsockConnId) -> Option<Arc<Connection>> {
        self.connections.get(&conn_id).cloned()
    }

    /// remove a connection
    pub fn remove_connection_if(
        &mut self,
        conn_id: VsockConnId,
        expected: &Arc<Connection>,
    ) -> Option<Arc<Connection>> {
        let registered = self.connections.get(&conn_id)?;
        if !Arc::ptr_eq(registered, expected) {
            return None;
        }
        self.connections.remove(&conn_id)
    }

    /// handle a new connection request (by driver event)
    pub fn on_connection_request(
        &mut self,
        conn_id: VsockConnId,
    ) -> NetResult<Option<IncomingConnection>> {
        let queue = self
            .listen_queues
            .get(&conn_id.local_port)
            .ok_or(NetError::NotFound)?
            .clone();

        let local_addr = queue.lock().local_addr;

        // check if connection already exists
        if self.connections.contains_key(&conn_id) {
            warn!("Connection {:?} already exists, ignoring request", conn_id);
            return Ok(None);
        }

        // create new connection
        let connection = self.create_connection(
            conn_id,
            local_addr,
            Some(conn_id.peer_addr),
            ConnectionState::Connected,
        )?;

        // enqueue connection to accept queue
        let mut queue_guard = queue.lock();
        if queue_guard.accept_queue.push(conn_id).is_err() {
            info!(
                "Accept queue full for port {}, dropping connection from {:?}",
                conn_id.local_port, conn_id.peer_addr
            );
            // full -- remove the connection
            drop(queue_guard);
            self.remove_connection_if(conn_id, &connection);
            return Err(NetError::ResourceBusy);
        }

        drop(queue_guard);

        trace!(
            "New connection request from {:?} on port {}",
            conn_id.peer_addr, conn_id.local_port
        );
        Ok(Some(IncomingConnection {
            conn_id,
            connection,
            queue,
        }))
    }
}

pub static VSOCK_CONN_MANAGER: Mutex<VsockConnectionManager> =
    Mutex::new(VsockConnectionManager::new());

//! Vsock connection registry.
//!
//! The manager tracks listening, connecting, and established vsock stream
//! connections, owns their byte rings, and provides wakeups used by the vsock
//! transport and device polling glue.
//!
//! # Event Flow
//!
//! Device polling turns host events into manager calls such as connection
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

use ax_errno::{AxError, AxResult, ax_bail};
use ax_sync::PiMutex;

use super::{VsockAddr, VsockConnId};
use crate::device::VsockPollLease;

mod connection;
mod queue;

pub(crate) use connection::*;
use queue::*;

type RetiredConnections = heapless::Vec<(VsockConnId, Arc<Connection>), VSOCK_ACCEPT_QUEUE_SIZE>;

pub(crate) struct IncomingConnection {
    conn_id: VsockConnId,
    connection: Arc<Connection>,
    queue: Arc<PiMutex<ListenQueue>>,
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
}

/// Global connection manager
pub struct VsockConnectionManager {
    connections: BTreeMap<VsockConnId, Arc<Connection>>,
    listen_queues: BTreeMap<u32, Arc<PiMutex<ListenQueue>>>,
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
    pub fn get_listen_queue(&self, port: u32) -> Option<Arc<PiMutex<ListenQueue>>> {
        self.listen_queues.get(&port).cloned()
    }

    /// allocate an ephemeral port
    pub fn allocate_port(&mut self) -> AxResult<u32> {
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
                ax_bail!(AddrInUse, "no available ports");
            }
        }
    }

    /// create a listen queue
    pub fn listen(&mut self, local_addr: VsockAddr) -> AxResult<()> {
        if self.listen_queues.contains_key(&local_addr.port) {
            ax_bail!(AddrInUse, "port already in use");
        }

        let queue = Arc::new(PiMutex::new(ListenQueue::new(local_addr)));
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
    pub fn accept(&mut self, port: u32) -> AxResult<(VsockConnId, VsockAddr)> {
        let queue = self.listen_queues.get(&port).ok_or(AxError::InvalidInput)?;

        let conn_id = queue.lock().accept_queue.pop().ok_or(AxError::WouldBlock)?;

        let conn = self.connections.get(&conn_id).ok_or(AxError::NotFound)?;

        let peer_addr = conn.lock().peer_addr().ok_or(AxError::NotFound)?;

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
        poll_lease: VsockPollLease,
    ) -> AxResult<Arc<Connection>> {
        if self.connections.contains_key(&conn_id) {
            ax_bail!(
                AlreadyExists,
                "vsock connection identity already registered"
            );
        }
        let conn = Connection::new_shared(local_addr, peer_addr, state, poll_lease);
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
        poll_lease: VsockPollLease,
    ) -> AxResult<Option<IncomingConnection>> {
        let queue = self
            .listen_queues
            .get(&conn_id.local_port)
            .ok_or(AxError::NotFound)?
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
            poll_lease,
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
            return Err(AxError::ResourceBusy);
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

pub static VSOCK_CONN_MANAGER: PiMutex<VsockConnectionManager> =
    PiMutex::new(VsockConnectionManager::new());

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Waker},
    };

    use ax_task::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

    use super::*;

    fn test_poll_lease() -> VsockPollLease {
        VsockPollLease::inactive_for_test()
    }

    struct WakeFlag(AtomicBool);

    impl Wake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn tx_poll_capability_is_owned_outside_connection_state() {
        let system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime = crate::test_runtime::install(&system, cpu.as_mut());
        let connection = Connection::new_shared(
            VsockAddr { cid: 3, port: 4 },
            Some(VsockAddr { cid: 5, port: 6 }),
            ConnectionState::Connected,
            test_poll_lease(),
        );

        let state = connection.lock();
        drop(state);
        let wake_flag = Arc::new(WakeFlag(AtomicBool::new(false)));
        let waker = Waker::from(wake_flag.clone());
        connection.register_tx_poll(&mut Context::from_waker(&waker));
        connection.wake_tx();

        assert!(wake_flag.0.load(Ordering::Acquire));
    }

    #[test]
    fn duplicate_connection_identity_never_replaces_the_live_owner() {
        let mut manager = VsockConnectionManager::new();
        let conn_id = VsockConnId {
            peer_addr: VsockAddr { cid: 7, port: 8 },
            local_port: 9,
        };
        let original = manager
            .create_connection(
                conn_id,
                VsockAddr { cid: 3, port: 9 },
                Some(conn_id.peer_addr),
                ConnectionState::Connected,
                test_poll_lease(),
            )
            .unwrap();

        assert!(matches!(
            manager.create_connection(
                conn_id,
                VsockAddr { cid: 3, port: 9 },
                Some(conn_id.peer_addr),
                ConnectionState::Connecting,
                test_poll_lease(),
            ),
            Err(AxError::AlreadyExists)
        ));
        assert!(Arc::ptr_eq(
            &original,
            &manager.get_connection(conn_id).unwrap()
        ));
    }

    #[test]
    fn stale_connection_owner_cannot_remove_the_registered_identity() {
        let mut manager = VsockConnectionManager::new();
        let conn_id = VsockConnId {
            peer_addr: VsockAddr { cid: 10, port: 11 },
            local_port: 12,
        };
        let registered = manager
            .create_connection(
                conn_id,
                VsockAddr { cid: 3, port: 12 },
                Some(conn_id.peer_addr),
                ConnectionState::Connected,
                test_poll_lease(),
            )
            .unwrap();
        let stale = Connection::new_shared(
            VsockAddr { cid: 3, port: 12 },
            Some(conn_id.peer_addr),
            ConnectionState::Closed,
            test_poll_lease(),
        );

        assert!(manager.remove_connection_if(conn_id, &stale).is_none());
        assert!(Arc::ptr_eq(
            &registered,
            &manager.get_connection(conn_id).unwrap()
        ));
    }

    #[test]
    fn unlisten_retires_connections_waiting_in_accept_queue() {
        let system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime = crate::test_runtime::install(&system, cpu.as_mut());

        let mut manager = VsockConnectionManager::new();
        let local_addr = VsockAddr { cid: 3, port: 13 };
        let conn_id = VsockConnId {
            peer_addr: VsockAddr { cid: 14, port: 15 },
            local_port: local_addr.port,
        };
        manager.listen(local_addr).unwrap();
        let _incoming = manager
            .on_connection_request(conn_id, test_poll_lease())
            .unwrap()
            .unwrap();

        let retired = manager.unlisten(local_addr.port);

        assert!(
            manager.get_connection(conn_id).is_none(),
            "closing a listener must retire connections that have not been accepted"
        );
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].0, conn_id);
    }
}

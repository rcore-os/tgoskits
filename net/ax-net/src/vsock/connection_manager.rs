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
use ax_sync::{PiMutex, PiMutexGuard};
use ax_task::WaitQueue;
use axpoll::{IoEvents, PollSet};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};

use super::{VsockAddr, VsockConnId};
use crate::device::VsockPollLease;

pub const VSOCK_RX_BUFFER_SIZE: usize = 64 * 1024; // 64KB receive buffer
const VSOCK_ACCEPT_QUEUE_SIZE: usize = 128; // accept queue size
type RetiredConnections = heapless::Vec<(VsockConnId, Arc<Connection>), VSOCK_ACCEPT_QUEUE_SIZE>;

/// Public state of a vsock connection tracked by the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Allocated but not listening/connecting.
    Idle,
    /// Registered as a listener.
    Listening,
    /// Outgoing connection request in progress.
    Connecting,
    /// Connected and usable for I/O.
    Connected,
    /// Disconnected or closed.
    Closed,
}

/// Stable connection capability shared by device events and stream transports.
///
/// The wait queue lives outside mutable connection state so a blocked sender
/// never sleeps while owning `state`.
pub struct Connection {
    state: PiMutex<ConnectionData>,
    tx_wait_queue: WaitQueue,
    rx_wakers: PiMutex<PollSet>,
    connect_wakers: PiMutex<PollSet>,
    _poll_lease: VsockPollLease,
}

/// Mutable state serialized in ordinary task context.
pub(crate) struct ConnectionData {
    /// Manager-level connection state.
    state: ConnectionState,
    /// Local vsock address.
    local_addr: VsockAddr,
    /// Peer address, if known.
    peer_addr: Option<VsockAddr>,

    /// Producer side filled by device receive events.
    rx_producer: HeapProd<u8>,
    /// Consumer side drained by socket recv.
    rx_consumer: HeapCons<u8>,

    /// Whether the receive half is closed.
    rx_closed: bool,
    /// Whether the transmit half is closed.
    tx_closed: bool,

    /// Received byte count.
    rx_bytes: usize,
    /// Transmitted byte count.
    tx_bytes: usize,
    /// Dropped byte count.
    dropped_bytes: usize,
}

impl Connection {
    pub(crate) fn new_shared(
        local_addr: VsockAddr,
        peer_addr: Option<VsockAddr>,
        state: ConnectionState,
        poll_lease: VsockPollLease,
    ) -> Arc<Self> {
        let rb = HeapRb::<u8>::new(VSOCK_RX_BUFFER_SIZE);
        let (rx_producer, rx_consumer) = rb.split();
        Arc::new(Self {
            state: PiMutex::new(ConnectionData {
                state,
                local_addr,
                peer_addr,
                rx_producer,
                rx_consumer,
                rx_closed: false,
                tx_closed: false,
                rx_bytes: 0,
                tx_bytes: 0,
                dropped_bytes: 0,
            }),
            tx_wait_queue: WaitQueue::default(),
            rx_wakers: PiMutex::new(PollSet::new()),
            connect_wakers: PiMutex::new(PollSet::new()),
            _poll_lease: poll_lease,
        })
    }

    pub(crate) fn lock(&self) -> PiMutexGuard<'_, ConnectionData> {
        self.state.lock()
    }

    #[inline]
    pub fn wait_for_tx(&self) {
        self.tx_wait_queue
            .wait_timeout(core::time::Duration::from_millis(10));
    }

    #[inline]
    pub fn notify_tx(&self) {
        self.tx_wait_queue.notify_all();
    }

    pub fn register_rx_poll(&self, context: &mut core::task::Context<'_>) {
        // SAFETY: registration happens in task context and the caller repeats
        // its readiness check after publishing the waker.
        unsafe {
            self.rx_wakers
                .lock()
                .register(context.waker(), IoEvents::IN)
        };
    }

    pub fn register_connect_poll(&self, context: &mut core::task::Context<'_>) {
        // SAFETY: registration happens in task context and connection state is
        // checked again after the waker is published.
        unsafe {
            self.connect_wakers
                .lock()
                .register(context.waker(), IoEvents::OUT | IoEvents::ERR)
        };
    }

    pub fn wake_rx(&self) {
        // SAFETY: the state transition or RX publication is complete before
        // this task-context wake.
        unsafe {
            self.rx_wakers
                .lock()
                .wake(IoEvents::IN | IoEvents::RDHUP | IoEvents::HUP)
        };
    }

    pub fn wake_connect(&self) {
        // SAFETY: connected/closed state is published before this wake.
        unsafe {
            self.connect_wakers
                .lock()
                .wake(IoEvents::OUT | IoEvents::ERR)
        };
    }

    pub(crate) fn stats(&self) -> ConnectionStats {
        let state = self.lock();
        ConnectionStats {
            rx_bytes: state.rx_bytes,
            tx_bytes: state.tx_bytes,
            dropped_bytes: state.dropped_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionStats {
    pub(crate) rx_bytes: usize,
    pub(crate) tx_bytes: usize,
    pub(crate) dropped_bytes: usize,
}

impl ConnectionData {
    /// Get the free space in the receive buffer
    #[inline]
    pub fn rx_buffer_free(&self) -> usize {
        self.rx_producer.vacant_len()
    }

    /// Get the used space in the receive buffer
    #[inline]
    pub fn rx_buffer_used(&self) -> usize {
        self.rx_consumer.occupied_len()
    }

    /// push data into the receive buffer
    pub fn push_rx_data(&mut self, data: &[u8]) -> usize {
        let available = self.rx_buffer_free();
        let to_write = data.len().min(available);

        if to_write > 0 {
            let written = self.rx_producer.push_slice(&data[..to_write]);
            self.rx_bytes += written;

            if written < data.len() {
                let dropped = data.len() - written;
                self.dropped_bytes += dropped;
                info!(
                    "Vsock connection {:?} rx buffer full, dropped {} bytes",
                    (self.local_addr, self.peer_addr),
                    dropped
                );
            }
            written
        } else {
            self.dropped_bytes += data.len();
            info!(
                "Vsock connection {:?} rx buffer full, dropped {} bytes",
                (self.local_addr, self.peer_addr),
                data.len()
            );
            0
        }
    }

    #[inline]
    pub fn rx_slices(&self) -> (&[u8], &[u8]) {
        self.rx_consumer.as_slices()
    }

    #[inline]
    pub fn advance_rx_read(&mut self, count: usize) {
        unsafe {
            self.rx_consumer.advance_read_index(count);
        }
    }

    #[inline]
    pub fn add_tx_bytes(&mut self, count: usize) {
        self.tx_bytes += count;
    }

    #[inline]
    pub fn local_addr(&self) -> VsockAddr {
        self.local_addr
    }

    #[inline]
    pub fn peer_addr(&self) -> Option<VsockAddr> {
        self.peer_addr
    }

    #[inline]
    pub fn set_state(&mut self, state: ConnectionState) {
        self.state = state;
    }

    #[inline]
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    #[inline]
    pub fn rx_closed(&self) -> bool {
        self.rx_closed
    }

    #[inline]
    pub fn tx_closed(&self) -> bool {
        self.tx_closed
    }

    #[inline]
    pub fn set_rx_closed(&mut self, closed: bool) {
        self.rx_closed = closed;
    }

    #[inline]
    pub fn set_tx_closed(&mut self, closed: bool) {
        self.tx_closed = closed;
    }
}

/// A fixed-size accept queue
pub struct AcceptQueue {
    producer: ringbuf::HeapProd<VsockConnId>,
    consumer: ringbuf::HeapCons<VsockConnId>,
}

impl AcceptQueue {
    pub fn new() -> Self {
        let rb = HeapRb::<VsockConnId>::new(VSOCK_ACCEPT_QUEUE_SIZE);
        let (producer, consumer) = rb.split();
        Self { producer, consumer }
    }

    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    pub fn push(&mut self, conn_id: VsockConnId) -> AxResult<()> {
        match self.producer.try_push(conn_id) {
            Ok(_) => Ok(()),
            Err(_) => ax_bail!(ResourceBusy, "accept queue full"),
        }
    }

    pub fn pop(&mut self) -> Option<VsockConnId> {
        self.consumer.try_pop()
    }

    fn remove(&mut self, conn_id: VsockConnId) -> bool {
        let queued = self.consumer.occupied_len();
        let mut removed = false;
        for _ in 0..queued {
            let current = self
                .consumer
                .try_pop()
                .expect("queued count came from the same accept queue");
            if !removed && current == conn_id {
                removed = true;
            } else {
                self.producer
                    .try_push(current)
                    .expect("popping one entry reserves space for reinsertion");
            }
        }
        removed
    }
}

/// listen queue
pub struct ListenQueue {
    pub accept_queue: AcceptQueue,
    pub wakers: PollSet,
    pub local_addr: VsockAddr,
}

impl ListenQueue {
    pub fn new(local_addr: VsockAddr) -> Self {
        Self {
            accept_queue: AcceptQueue::new(),
            wakers: PollSet::new(),
            local_addr,
        }
    }

    pub fn wake(&mut self) {
        // Accept queue state is published before waking listeners.
        unsafe { self.wakers.wake(IoEvents::IN) };
    }

    pub fn register_poll(&mut self, context: &mut core::task::Context<'_>) {
        // Registration happens from vsock poll task context.
        unsafe { self.wakers.register(context.waker(), IoEvents::IN) };
    }
}

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

        let peer_addr = conn.lock().peer_addr.ok_or(AxError::NotFound)?;

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
    use alloc::boxed::Box;

    use ax_task::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

    use super::*;

    fn test_poll_lease() -> VsockPollLease {
        VsockPollLease::inactive_for_test()
    }

    #[test]
    fn tx_wait_capability_is_owned_outside_connection_state() {
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
        let _wait_without_state_guard = || connection.wait_for_tx();
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

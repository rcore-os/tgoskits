//! Per-connection vsock state and readiness capabilities.

use alloc::sync::Arc;

use ax_sync::{PiMutex, PiMutexGuard};
use axpoll::{IoEvents, PollSet};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};

use super::{VsockAddr, VsockPollLease};

pub const VSOCK_RX_BUFFER_SIZE: usize = 64 * 1024; // 64KB receive buffer

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
/// Poll registrations live outside mutable connection state so task-context
/// wake publication never runs while owning `state`.
pub struct Connection {
    state: PiMutex<ConnectionData>,
    rx_wakers: PiMutex<PollSet>,
    tx_wakers: PiMutex<PollSet>,
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
            rx_wakers: PiMutex::new(PollSet::new()),
            tx_wakers: PiMutex::new(PollSet::new()),
            connect_wakers: PiMutex::new(PollSet::new()),
            _poll_lease: poll_lease,
        })
    }

    pub(crate) fn lock(&self) -> PiMutexGuard<'_, ConnectionData> {
        self.state.lock()
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

    pub fn register_tx_poll(&self, context: &mut core::task::Context<'_>) {
        // SAFETY: registration happens in task context and the caller repeats
        // its transport-credit check after publishing the waker.
        unsafe {
            self.tx_wakers
                .lock()
                .register(context.waker(), IoEvents::OUT)
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

    pub fn wake_tx(&self) {
        // SAFETY: peer-credit or terminal connection state is published before
        // this task-context wake.
        unsafe { self.tx_wakers.lock().wake(IoEvents::OUT | IoEvents::ERR) };
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

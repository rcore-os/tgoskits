//! Adaptive vsock polling and task-context event publication.

use alloc::{string::ToString, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_sync::PiMutex;
use ax_task::WaitQueue;
use rdif_vsock::{VsockError, VsockEvent};

use super::VSOCK_DEVICE;
use crate::vsock::connection_manager::{ConnectionState, VSOCK_CONN_MANAGER, VSOCK_RX_BUFFER_SIZE};

const VSOCK_RX_TMPBUF_SIZE: usize = 0x1000; // 4KiB buffer for vsock receive

static POLL_TASK_STATE: PiMutex<PollTaskState> = PiMutex::new(PollTaskState::new());
static POLL_ACTIVE_USERS: AtomicUsize = AtomicUsize::new(0);
static POLL_TASK_WAIT: WaitQueue = WaitQueue::new();
static POLL_FREQUENCY: PollFrequencyController = PollFrequencyController::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollTaskPhase {
    Offline,
    Starting,
    Running,
}

struct PollTaskState {
    phase: PollTaskPhase,
}

impl PollTaskState {
    const fn new() -> Self {
        Self {
            phase: PollTaskPhase::Offline,
        }
    }
}

struct PollFrequencyController {
    consecutive_idle: AtomicU64,
}

impl PollFrequencyController {
    const fn new() -> Self {
        Self {
            consecutive_idle: AtomicU64::new(0),
        }
    }

    fn current_interval(&self) -> Duration {
        let idle = self.consecutive_idle.load(Ordering::Relaxed);
        let interval_us = match idle {
            0..=3 => 100,     //  3 ：100μs
            4..=10 => 500,    // 4-10 ：500μs
            11..=20 => 2_000, // 11-20 ：2ms
            _ => 10_000,      // 20+ ：10ms
        };
        Duration::from_micros(interval_us)
    }

    fn on_event(&self) {
        self.consecutive_idle.store(0, Ordering::Release);
    }

    fn on_idle(&self) {
        self.consecutive_idle.fetch_add(1, Ordering::Relaxed);
    }

    fn stats(&self) -> (u64, u64) {
        let idle = self.consecutive_idle.load(Ordering::Relaxed);
        let interval = self.current_interval().as_micros() as u64;
        (idle, interval)
    }
}

/// An active-user reference that keeps the permanent vsock poll worker awake.
///
/// The lease is move-only and releases exactly one reference when dropped.
/// Connections own their lease, so registry removal and the final connection
/// capability release define the worker lifetime without a separate counter
/// protocol at each call site.
pub(crate) struct VsockPollLease {
    active: bool,
}

impl VsockPollLease {
    #[cfg(test)]
    pub(crate) const fn inactive_for_test() -> Self {
        Self { active: false }
    }
}

impl Drop for VsockPollLease {
    fn drop(&mut self) {
        if self.active {
            release_vsock_poll();
            self.active = false;
        }
    }
}

pub(crate) fn start_vsock_poll() -> AxResult<VsockPollLease> {
    loop {
        let mut state = POLL_TASK_STATE.lock();
        match state.phase {
            PollTaskPhase::Running => {
                let active_users = POLL_ACTIVE_USERS
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                        current.checked_add(1)
                    })
                    .map_err(|_| AxError::ResourceBusy)?
                    + 1;
                drop(state);
                POLL_TASK_WAIT.notify_all();
                debug!("start_vsock_poll: ref_count -> {active_users}");
                return Ok(VsockPollLease { active: true });
            }
            PollTaskPhase::Starting => {
                drop(state);
                POLL_TASK_WAIT
                    .wait_until(|| POLL_TASK_STATE.lock().phase != PollTaskPhase::Starting);
            }
            PollTaskPhase::Offline => {
                state.phase = PollTaskPhase::Starting;
                POLL_ACTIVE_USERS.store(1, Ordering::Release);
                drop(state);
                break;
            }
        }
    }

    debug!("Starting vsock poll task");
    let spawn_result = crate::spawn_permanent_worker("vsock-poll".to_string(), vsock_poll_loop);
    let mut state = POLL_TASK_STATE.lock();
    if let Err(error) = spawn_result {
        state.phase = PollTaskPhase::Offline;
        POLL_ACTIVE_USERS.store(0, Ordering::Release);
        drop(state);
        POLL_TASK_WAIT.notify_all();
        warn!("Failed to start vsock poll task: {error}");
        return Err(AxError::BadState);
    }
    state.phase = PollTaskPhase::Running;
    drop(state);
    POLL_TASK_WAIT.notify_all();
    Ok(VsockPollLease { active: true })
}

/// Retains a poll-worker reference without starting or waiting for a worker.
///
/// Device events use this path before publishing a new incoming connection.
/// Because the event is already being handled by the poll worker, any state
/// other than an active `Running` worker means the listener lifetime ended and
/// the request must not become visible.
fn retain_running_vsock_poll() -> AxResult<VsockPollLease> {
    let state = POLL_TASK_STATE.lock();
    if state.phase != PollTaskPhase::Running {
        return Err(AxError::BadState);
    }
    let active_users = POLL_ACTIVE_USERS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            if current == 0 {
                None
            } else {
                current.checked_add(1)
            }
        })
        .map_err(|_| AxError::BadState)?
        + 1;
    drop(state);
    debug!("retain_running_vsock_poll: ref_count -> {active_users}");
    Ok(VsockPollLease { active: true })
}

/// Drops one active-user reference to the adaptive vsock poll task.
fn release_vsock_poll() {
    match POLL_ACTIVE_USERS.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        current.checked_sub(1)
    }) {
        Ok(previous) => debug!("release_vsock_poll: ref_count -> {}", previous - 1),
        Err(_) => warn!("vsock poll lease released but ref_count already 0"),
    }
}

fn vsock_poll_loop() {
    let mut worker = VsockPollWorker::new();
    loop {
        POLL_TASK_WAIT.wait_until(|| POLL_ACTIVE_USERS.load(Ordering::Acquire) != 0);
        if let Err(error) = worker.poll_interfaces_adaptive() {
            debug!("vsock poll iteration failed: {error}");
        }
    }
}

struct VsockPollWorker {
    pending_events: heapless::Deque<VsockEvent, VSOCK_PENDING_EVENT_CAPACITY>,
    rx_buffer: Vec<u8>,
}

impl VsockPollWorker {
    fn new() -> Self {
        Self {
            pending_events: heapless::Deque::new(),
            rx_buffer: vec![0; VSOCK_RX_TMPBUF_SIZE],
        }
    }

    fn poll_interfaces_adaptive(&mut self) -> AxResult<()> {
        let has_events = self.poll_vsock_interfaces()?;

        if has_events {
            POLL_FREQUENCY.on_event();
        } else {
            POLL_FREQUENCY.on_idle();
        }

        let interval = POLL_FREQUENCY.current_interval();

        let (idle_count, interval_us) = POLL_FREQUENCY.stats();
        if idle_count > 0 && idle_count % 10 == 0 {
            trace!("Poll frequency: idle_count={idle_count}, interval={interval_us}μs",);
        }
        ax_task::sleep(interval);
        Ok(())
    }
}

const VSOCK_EVENT_BUDGET: usize = 64;
const VSOCK_PENDING_RETRY_BUDGET: usize = 16;
const VSOCK_PENDING_EVENT_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventDisposition {
    Consumed,
    Retry,
}

impl VsockPollWorker {
    fn poll_vsock_interfaces(&mut self) -> AxResult<bool> {
        let mut made_progress = false;
        let retry_count = self.pending_events.len().min(VSOCK_PENDING_RETRY_BUDGET);
        for _ in 0..retry_count {
            let event = self
                .pending_events
                .pop_front()
                .expect("pending retry count came from queue length");
            match self.handle_vsock_event(event)? {
                EventDisposition::Consumed => made_progress = true,
                EventDisposition::Retry => {
                    self.pending_events
                        .push_back(event)
                        .expect("a popped pending event must fit when requeued");
                }
            }
        }

        let mut polled_events = 0;
        while retry_count + polled_events < VSOCK_EVENT_BUDGET
            && self.pending_events.len() < VSOCK_PENDING_EVENT_CAPACITY
        {
            let event = {
                let mut device = VSOCK_DEVICE.lock();
                let device = device.as_mut().ok_or(AxError::NotFound)?;
                match device.poll_event() {
                    Ok(event) => event,
                    Err(error) => {
                        info!("Failed to poll vsock event: {error:?}");
                        break;
                    }
                }
            };
            let Some(event) = event else {
                break;
            };
            polled_events += 1;
            match self.handle_vsock_event(event)? {
                EventDisposition::Consumed => made_progress = true,
                EventDisposition::Retry => {
                    self.pending_events
                        .push_back(event)
                        .expect("polling stops before the bounded pending queue is full");
                }
            }
        }
        Ok(made_progress || polled_events != 0)
    }

    fn handle_vsock_event(&mut self, event: VsockEvent) -> AxResult<EventDisposition> {
        debug!("Handling vsock event: {event:?}");
        match event {
            VsockEvent::ConnectionRequest(conn_id) => {
                let poll_lease = retain_running_vsock_poll()?;
                let incoming = VSOCK_CONN_MANAGER
                    .lock()
                    .on_connection_request(conn_id, poll_lease)?;
                if let Some(incoming) = incoming {
                    incoming.publish();
                }
            }
            VsockEvent::Received(conn_id, event_len) => {
                let Some(connection) = VSOCK_CONN_MANAGER.lock().get_connection(conn_id) else {
                    info!("Received data for unknown connection: {conn_id:?}");
                    return Ok(EventDisposition::Consumed);
                };
                let free_space = connection.lock().rx_buffer_free();
                if free_space == 0 {
                    return Ok(EventDisposition::Retry);
                }
                let max_read = free_space.min(event_len).min(self.rx_buffer.len());
                if max_read == 0 {
                    return Ok(EventDisposition::Consumed);
                }
                let read_len = {
                    let mut device = VSOCK_DEVICE.lock();
                    let device = device.as_mut().ok_or(AxError::NotFound)?;
                    match device.recv(conn_id, &mut self.rx_buffer[..max_read]) {
                        Ok(read_len) => read_len,
                        Err(VsockError::Retry) => return Ok(EventDisposition::Retry),
                        Err(error) => {
                            info!(
                                "Failed to receive vsock data: conn_id={conn_id:?}, \
                                 error={error:?}"
                            );
                            return Ok(EventDisposition::Consumed);
                        }
                    }
                };
                let (written, buffer_used) = {
                    let mut state = connection.lock();
                    let written = state.push_rx_data(&self.rx_buffer[..read_len]);
                    (written, state.rx_buffer_used())
                };
                if written != 0 {
                    connection.wake_rx();
                }
                trace!(
                    "Received {read_len} bytes for connection {conn_id:?} (written={written}, \
                     buffer_used={buffer_used}/{VSOCK_RX_BUFFER_SIZE})"
                );
            }
            VsockEvent::Disconnected(conn_id) => {
                if let Some(connection) = VSOCK_CONN_MANAGER.lock().get_connection(conn_id) {
                    {
                        let mut state = connection.lock();
                        state.set_state(ConnectionState::Closed);
                        state.set_rx_closed(true);
                        state.set_tx_closed(true);
                    }
                    connection.wake_rx();
                    connection.wake_connect();
                    connection.notify_tx();
                    trace!("Connection {conn_id:?} disconnected");
                }
            }
            VsockEvent::Connected(conn_id) => {
                if let Some(connection) = VSOCK_CONN_MANAGER.lock().get_connection(conn_id) {
                    connection.lock().set_state(ConnectionState::Connected);
                    connection.wake_connect();
                    trace!("Connection {conn_id:?} established");
                }
            }
            VsockEvent::CreditUpdate(conn_id) => {
                if let Some(connection) = VSOCK_CONN_MANAGER.lock().get_connection(conn_id) {
                    connection.notify_tx();
                    trace!("Connection {conn_id:?} tx wait queue notified");
                }
            }
            VsockEvent::Unknown => warn!("Received unknown vsock event"),
        }
        Ok(EventDisposition::Consumed)
    }
}

#[cfg(test)]
mod tests;

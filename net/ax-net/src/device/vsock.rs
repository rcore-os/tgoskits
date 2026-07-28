//! Vsock device polling glue.
//!
//! Vsock is driven outside the smoltcp IP path. This module owns the single
//! registered vsock interface, adapts its event stream into the connection
//! manager, and starts an adaptive poll task while vsock connections exist.
//!
//! # Polling Model
//!
//! The vsock device exposes connection and credit events rather than IP
//! packets. A reference-counted poll task runs only while stream transports are
//! active, backs off when no events are observed, and pushes data into the
//! vsock connection manager's byte rings.
//!
//! # Isolation From IP Stack
//!
//! This code must not acquire smoltcp service/socket locks. Vsock readiness is
//! handled through its own connection manager and socket transport layer.

use alloc::{string::ToString, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult, ax_bail};
use ax_sync::PiMutex;
use ax_task::WaitQueue;
use rdif_vsock::{Interface, VsockAddr, VsockConnId, VsockError, VsockEvent};

use crate::vsock::connection_manager::{ConnectionState, VSOCK_CONN_MANAGER, VSOCK_RX_BUFFER_SIZE};

pub type VsockDevice = alloc::boxed::Box<dyn Interface>;
pub type VsockDeviceList = alloc::vec::Vec<VsockDevice>;

// we need a global and static only one vsock device
static VSOCK_DEVICE: PiMutex<Option<VsockDevice>> = PiMutex::new(None);

const VSOCK_RX_TMPBUF_SIZE: usize = 0x1000; // 4KiB buffer for vsock receive

/// Registers the single vsock device used by the system.
pub fn register_vsock_device(dev: VsockDevice) -> AxResult {
    let mut guard = VSOCK_DEVICE.lock();
    if guard.is_some() {
        ax_bail!(AlreadyExists, "vsock device already registered");
    }
    *guard = Some(dev);
    drop(guard);
    Ok(())
}

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

pub fn vsock_listen(addr: VsockAddr) -> AxResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    dev.listen(addr.port).map_err(map_vsock_error)
}

fn map_vsock_error(e: VsockError) -> AxError {
    match e {
        VsockError::AlreadyExists => AxError::AlreadyExists,
        VsockError::Retry => AxError::WouldBlock,
        VsockError::NotConnected => AxError::NotConnected,
        VsockError::NotAvailable => AxError::NotFound,
        VsockError::NotSupported => AxError::Unsupported,
        VsockError::Other(_) => AxError::BadState,
    }
}

pub fn vsock_connect(conn_id: VsockConnId) -> AxResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    dev.connect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_send(conn_id: VsockConnId, buf: &[u8]) -> AxResult<usize> {
    let max_retries = 10; // Tests have shown that no more than two retries will be notified
    for _ in 0..max_retries {
        let result = {
            let mut guard = VSOCK_DEVICE.lock();
            let dev = guard.as_mut().ok_or(AxError::NotFound)?;
            dev.send(conn_id, buf)
        };
        match result {
            Ok(len) => return Ok(len),
            Err(VsockError::Retry) => {
                let manager = VSOCK_CONN_MANAGER.lock();
                if let Some(conn) = manager.get_connection(conn_id) {
                    drop(manager);
                    conn.wait_for_tx();
                };
            }
            Err(e) => return Err(map_vsock_error(e)),
        }
    }
    Err(map_vsock_error(VsockError::Retry))
}

pub fn vsock_disconnect(conn_id: VsockConnId) -> AxResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    dev.disconnect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_guest_cid() -> AxResult<u64> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    Ok(dev.guest_cid())
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicBool, AtomicUsize},
        task::{Context, Waker},
    };

    use ax_task::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};
    use rdif_vsock::DriverGeneric;

    use super::*;

    struct TestVsock {
        requested_rx: Arc<AtomicUsize>,
        poll_count: Arc<AtomicUsize>,
        always_poll_event: bool,
    }

    impl DriverGeneric for TestVsock {
        fn name(&self) -> &str {
            "test-vsock"
        }
    }

    impl Interface for TestVsock {
        fn guest_cid(&self) -> u64 {
            3
        }

        fn listen(&mut self, _port: u32) -> Result<(), VsockError> {
            Ok(())
        }

        fn connect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn send(&mut self, _id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError> {
            Ok(buf.len())
        }

        fn recv(&mut self, _id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError> {
            self.requested_rx.store(buf.len(), Ordering::Release);
            if let Some(first) = buf.first_mut() {
                *first = 0x5a;
                Ok(1)
            } else {
                Ok(0)
            }
        }

        fn recv_avail(&mut self, _id: VsockConnId) -> Result<usize, VsockError> {
            Ok(1)
        }

        fn disconnect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn abort(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
            if self.always_poll_event {
                self.poll_count.fetch_add(1, Ordering::AcqRel);
                Ok(Some(VsockEvent::Unknown))
            } else {
                Ok(None)
            }
        }
    }

    struct DeviceGateProbe {
        device_released: AtomicBool,
        manager_released: AtomicBool,
    }

    impl Wake for DeviceGateProbe {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.device_released
                .store(VSOCK_DEVICE.try_lock().is_some(), Ordering::Release);
            self.manager_released
                .store(VSOCK_CONN_MANAGER.try_lock().is_some(), Ordering::Release);
        }
    }

    #[test]
    fn received_event_releases_device_gate_before_waking_socket() {
        let system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime = crate::test_runtime::install(&system, cpu.as_mut());

        let conn_id = VsockConnId {
            peer_addr: VsockAddr { cid: 2, port: 3 },
            local_port: 4,
        };
        let connection = VSOCK_CONN_MANAGER
            .lock()
            .create_connection(
                conn_id,
                VsockAddr { cid: 3, port: 4 },
                Some(conn_id.peer_addr),
                ConnectionState::Connected,
                VsockPollLease::inactive_for_test(),
            )
            .unwrap();
        let probe = Arc::new(DeviceGateProbe {
            device_released: AtomicBool::new(false),
            manager_released: AtomicBool::new(false),
        });
        let waker = Waker::from(probe.clone());
        connection.register_rx_poll(&mut Context::from_waker(&waker));

        let requested_rx = Arc::new(AtomicUsize::new(0));
        *VSOCK_DEVICE.lock() = Some(Box::new(TestVsock {
            requested_rx: requested_rx.clone(),
            poll_count: Arc::new(AtomicUsize::new(0)),
            always_poll_event: false,
        }));
        let mut worker = VsockPollWorker::new();

        assert_eq!(
            worker
                .handle_vsock_event(VsockEvent::Received(conn_id, 1))
                .unwrap(),
            EventDisposition::Consumed
        );
        assert_eq!(requested_rx.load(Ordering::Acquire), 1);
        assert!(probe.device_released.load(Ordering::Acquire));
        assert!(probe.manager_released.load(Ordering::Acquire));
        assert_eq!(connection.lock().rx_buffer_used(), 1);

        *VSOCK_DEVICE.lock() = None;
        VSOCK_CONN_MANAGER
            .lock()
            .remove_connection_if(conn_id, &connection);
    }

    #[test]
    fn poll_iteration_has_a_fixed_event_budget() {
        let system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime = crate::test_runtime::install(&system, cpu.as_mut());

        let poll_count = Arc::new(AtomicUsize::new(0));
        *VSOCK_DEVICE.lock() = Some(Box::new(TestVsock {
            requested_rx: Arc::new(AtomicUsize::new(0)),
            poll_count: poll_count.clone(),
            always_poll_event: true,
        }));
        let mut worker = VsockPollWorker::new();

        assert!(worker.poll_vsock_interfaces().unwrap());
        assert_eq!(
            poll_count.load(Ordering::Acquire),
            VSOCK_EVENT_BUDGET,
            "an always-ready device must return to the scheduler after one budget"
        );

        *VSOCK_DEVICE.lock() = None;
    }
}

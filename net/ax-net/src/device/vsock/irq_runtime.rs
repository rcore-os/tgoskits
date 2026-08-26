//! Fixed-owner IRQ worker for one vsock device.

use alloc::{boxed::Box, format, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ax_sync::Mutex;
use ax_task::{CpuId, CpuSet, IrqWaitCell, IrqWorkerWaiter};
use rdif_vsock::{
    VsockConnId, VsockError, VsockEvent, VsockHardIrqResult, VsockPollIrqControl, VsockRearmResult,
};

use super::{VsockDevice, VsockDeviceInput};
use crate::{
    PinnedNetIrqAction, PinnedNetIrqError, PinnedNetIrqOutcome, PinnedNetIrqRegistrar,
    PinnedNetIrqRegistration,
    vsock::connection_manager::{
        Connection, ConnectionState, VSOCK_CONN_MANAGER, VSOCK_RX_BUFFER_SIZE,
    },
};

const COMMAND_WAIT: u8 = 0;
const COMMAND_START: u8 = 1;
const COMMAND_STOP: u8 = 2;
const COMMAND_QUARANTINE: u8 = 3;

const STATUS_PENDING: u8 = 0;
const STATUS_READY: u8 = 1;
const STATUS_FAILED: u8 = 2;

const VSOCK_RX_TMPBUF_SIZE: usize = 0x1000;
const VSOCK_EVENT_BUDGET: usize = 64;
const VSOCK_PENDING_RETRY_BUDGET: usize = 16;
const VSOCK_PENDING_EVENT_CAPACITY: usize = 256;

/// Vsock IRQ runtime initialization or lifecycle error.
#[derive(Debug, thiserror::Error)]
pub enum VsockRuntimeError {
    #[error("vsock device or IRQ topology is invalid")]
    InvalidTopology,
    #[error("vsock IRQ worker could not be pinned to CPU {0}")]
    WorkerAffinity(usize),
    #[error("vsock IRQ worker for CPU {cpu} could not be spawned: {source}")]
    WorkerSpawn {
        cpu: usize,
        #[source]
        source: ax_task::TaskError,
    },
    #[error("vsock IRQ worker initialization failed")]
    WorkerStartup,
    #[error("vsock IRQ registration failed: {0}")]
    IrqRegistration(#[from] PinnedNetIrqError),
}

/// Live device, IRQ registration, and fixed worker ownership.
pub(super) struct VsockIrqRuntime {
    registration: Option<Box<dyn PinnedNetIrqRegistration>>,
    worker: Option<ax_task::KernelThreadHandle>,
    control: Arc<VsockWorkerControl>,
    device: Arc<Mutex<VsockDevice>>,
}

impl VsockIrqRuntime {
    pub(super) fn start(
        input: VsockDeviceInput,
        registrar: &dyn PinnedNetIrqRegistrar,
        owner_cpu: usize,
        topology_len: usize,
    ) -> Result<Self, VsockRuntimeError> {
        if topology_len == 0 || owner_cpu >= topology_len {
            return Err(VsockRuntimeError::InvalidTopology);
        }

        let VsockDeviceInput {
            name,
            device,
            irq,
            endpoints,
        } = input;
        let (mut hard_irq, irq_control) = endpoints.into_parts();
        let device = Arc::new(Mutex::new(device));
        let control = Arc::new(VsockWorkerControl::new(owner_cpu));

        let mut affinity = CpuSet::empty(topology_len);
        if !affinity.insert(CpuId::new(owner_cpu as u32)) {
            return Err(VsockRuntimeError::InvalidTopology);
        }
        let worker_control = Arc::clone(&control);
        let worker_device = Arc::clone(&device);
        let worker = ax_task::ThreadBuilder::new(format!("vsock-irq-cpu{owner_cpu}"))
            .affinity(affinity)
            .spawn(move || vsock_worker_main(worker_device, irq_control, worker_control))
            .map_err(|source| VsockRuntimeError::WorkerSpawn {
                cpu: owner_cpu,
                source,
            })?;

        wait_status(&control.affinity_status);
        if control.affinity_status.load(Ordering::Acquire) != STATUS_READY {
            stop_worker(&control, worker, true);
            return Err(VsockRuntimeError::WorkerAffinity(owner_cpu));
        }

        let irq_control = Arc::clone(&control);
        let action = PinnedNetIrqAction::new(move || match hard_irq.handle_irq() {
            VsockHardIrqResult::Spurious => PinnedNetIrqOutcome::Unhandled,
            VsockHardIrqResult::Handled => PinnedNetIrqOutcome::Handled,
            VsockHardIrqResult::Schedule | VsockHardIrqResult::ProbeDeferred => {
                irq_control.schedule_irq();
                PinnedNetIrqOutcome::Wake
            }
        });
        let registration = match registrar.register(format!("{name}-vsock"), irq, owner_cpu, action)
        {
            Ok(registration) if registration.owner_cpu() == owner_cpu => registration,
            Ok(registration) => {
                let synchronized = release_registration(registration);
                stop_worker(&control, worker, synchronized);
                return Err(VsockRuntimeError::InvalidTopology);
            }
            Err(error) => {
                stop_worker(&control, worker, true);
                return Err(error.into());
            }
        };

        control.command.store(COMMAND_START, Ordering::Release);
        control.notify.notify_from_task();
        wait_status(&control.startup_status);
        if control.startup_status.load(Ordering::Acquire) != STATUS_READY {
            let synchronized = release_registration(registration);
            stop_worker(&control, worker, synchronized);
            return Err(VsockRuntimeError::WorkerStartup);
        }

        if let Err(error) = registration.enable() {
            let synchronized = release_registration(registration);
            stop_worker(&control, worker, synchronized);
            return Err(error.into());
        }

        Ok(Self {
            registration: Some(registration),
            worker: Some(worker),
            control,
            device,
        })
    }

    pub(super) fn device(&self) -> &Mutex<VsockDevice> {
        &self.device
    }

    pub(super) fn request_task_work(&self) {
        self.control.schedule_task();
    }
}

impl Drop for VsockIrqRuntime {
    fn drop(&mut self) {
        let synchronized = self
            .registration
            .take()
            .map(release_registration)
            .unwrap_or(true);
        if let Some(worker) = self.worker.take() {
            stop_worker(&self.control, worker, synchronized);
        }
        if !synchronized {
            let device = Arc::clone(&self.device);
            core::mem::forget(device);
        }
    }
}

struct VsockWorkerNotification {
    event: IrqWaitCell,
}

impl VsockWorkerNotification {
    const fn new() -> Self {
        Self {
            event: IrqWaitCell::new(),
        }
    }

    fn notify_from_irq(&self) {
        let _ = self.event.notify();
    }

    fn notify_from_task(&self) {
        let _ = self.event.notify_from_task();
    }

    fn wait(&self, waiter: &IrqWorkerWaiter) {
        waiter
            .wait(&self.event)
            .unwrap_or_else(|error| panic!("vsock IRQ worker notification failed: {error}"));
    }
}

struct VsockWorkerControl {
    owner_cpu: usize,
    command: AtomicU8,
    affinity_status: AtomicU8,
    startup_status: AtomicU8,
    scheduled: AtomicBool,
    notify: VsockWorkerNotification,
}

impl VsockWorkerControl {
    const fn new(owner_cpu: usize) -> Self {
        Self {
            owner_cpu,
            command: AtomicU8::new(COMMAND_WAIT),
            affinity_status: AtomicU8::new(STATUS_PENDING),
            startup_status: AtomicU8::new(STATUS_PENDING),
            scheduled: AtomicBool::new(false),
            notify: VsockWorkerNotification::new(),
        }
    }

    fn schedule_irq(&self) {
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            self.notify.notify_from_irq();
        }
    }

    fn schedule_task(&self) {
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            self.notify.notify_from_task();
        }
    }

    fn take_scheduled(&self) -> bool {
        self.scheduled.swap(false, Ordering::AcqRel)
    }
}

fn vsock_worker_main(
    device: Arc<Mutex<VsockDevice>>,
    mut irq_control: Box<dyn VsockPollIrqControl>,
    control: Arc<VsockWorkerControl>,
) {
    let current = ax_task::current_thread_handle()
        .unwrap_or_else(|error| panic!("vsock IRQ worker has no scheduler thread: {error}"));
    let waiter = IrqWorkerWaiter::new(current.wake_handle());
    if ax_hal::percpu::this_cpu_id() != control.owner_cpu {
        control
            .affinity_status
            .store(STATUS_FAILED, Ordering::Release);
        control.notify.notify_from_task();
        return;
    }
    control
        .affinity_status
        .store(STATUS_READY, Ordering::Release);
    control.notify.notify_from_task();

    while control.command.load(Ordering::Acquire) == COMMAND_WAIT {
        control.notify.wait(&waiter);
    }
    if stop_requested(&control) {
        if control.command.load(Ordering::Acquire) == COMMAND_QUARANTINE {
            core::mem::forget((device, irq_control));
        }
        return;
    }

    let mut worker = VsockEventWorker::new(device);
    let initialized = worker.process_irq_cycle(&mut *irq_control).is_ok();
    control.startup_status.store(
        if initialized {
            STATUS_READY
        } else {
            STATUS_FAILED
        },
        Ordering::Release,
    );
    control.notify.notify_from_task();
    if !initialized {
        wait_for_cleanup(&control, &waiter);
        release_worker_resources(worker, irq_control, &control);
        return;
    }

    loop {
        if stop_requested(&control) {
            release_worker_resources(worker, irq_control, &control);
            return;
        }
        if !control.take_scheduled() {
            control.notify.wait(&waiter);
            continue;
        }
        if let Err(error) = worker.process_irq_cycle(&mut *irq_control) {
            warn!("vsock IRQ worker cycle failed: {error}");
        }
    }
}

fn stop_requested(control: &VsockWorkerControl) -> bool {
    matches!(
        control.command.load(Ordering::Acquire),
        COMMAND_STOP | COMMAND_QUARANTINE
    )
}

fn wait_for_cleanup(control: &VsockWorkerControl, waiter: &IrqWorkerWaiter) {
    while !stop_requested(control) {
        control.notify.wait(waiter);
    }
}

fn release_worker_resources(
    worker: VsockEventWorker,
    mut irq_control: Box<dyn VsockPollIrqControl>,
    control: &VsockWorkerControl,
) {
    if control.command.load(Ordering::Acquire) == COMMAND_QUARANTINE {
        core::mem::forget((worker, irq_control));
        return;
    }
    if let Err(error) = irq_control.shutdown() {
        warn!("vsock IRQ control shutdown failed: {error}");
        core::mem::forget((worker, irq_control));
    }
}

fn wait_status(status: &AtomicU8) {
    while status.load(Ordering::Acquire) == STATUS_PENDING {
        crate::yield_network_thread();
    }
}

fn release_registration(registration: Box<dyn PinnedNetIrqRegistration>) -> bool {
    if registration.disable_and_synchronize().is_ok() {
        drop(registration);
        true
    } else {
        warn!("quarantining vsock IRQ registration after synchronization failure");
        core::mem::forget(registration);
        false
    }
}

fn stop_worker(
    control: &VsockWorkerControl,
    worker: ax_task::KernelThreadHandle,
    irq_synchronized: bool,
) {
    control.command.store(
        if irq_synchronized {
            COMMAND_STOP
        } else {
            COMMAND_QUARANTINE
        },
        Ordering::Release,
    );
    control.notify.notify_from_task();
    if let Err(error) = worker.join() {
        warn!("failed to join vsock IRQ worker: {error}");
    }
}

struct VsockEventWorker {
    device: Arc<Mutex<VsockDevice>>,
    pending_events: heapless::Deque<VsockEvent, VSOCK_PENDING_EVENT_CAPACITY>,
    rx_buffer: Vec<u8>,
}

impl VsockEventWorker {
    fn new(device: Arc<Mutex<VsockDevice>>) -> Self {
        Self {
            device,
            pending_events: heapless::Deque::new(),
            rx_buffer: vec![0; VSOCK_RX_TMPBUF_SIZE],
        }
    }

    fn process_irq_cycle(
        &mut self,
        irq_control: &mut dyn VsockPollIrqControl,
    ) -> Result<(), VsockError> {
        irq_control.quiesce()?;
        loop {
            match self.drain_events()? {
                DrainOutcome::More => crate::yield_network_thread(),
                DrainOutcome::Idle => match irq_control.rearm_and_check()? {
                    VsockRearmResult::Idle => return Ok(()),
                    VsockRearmResult::WorkPending => {}
                },
            }
        }
    }

    fn drain_events(&mut self) -> Result<DrainOutcome, VsockError> {
        let retry_count = self.pending_events.len().min(VSOCK_PENDING_RETRY_BUDGET);
        let mut consumed_retries = 0;
        for _ in 0..retry_count {
            let event = self
                .pending_events
                .pop_front()
                .expect("pending retry count came from queue length");
            match self.handle_event(event) {
                EventDisposition::Consumed => consumed_retries += 1,
                EventDisposition::Retry => self
                    .pending_events
                    .push_back(event)
                    .expect("a popped pending event must fit when requeued"),
            }
        }

        let mut polled_events = 0;
        while retry_count + polled_events < VSOCK_EVENT_BUDGET
            && self.pending_events.len() < VSOCK_PENDING_EVENT_CAPACITY
        {
            let event = match self.device.lock().poll_event() {
                Ok(event) => event,
                Err(error) => {
                    info!("failed to poll vsock event: {error:?}");
                    break;
                }
            };
            let Some(event) = event else {
                break;
            };
            polled_events += 1;
            match self.handle_event(event) {
                EventDisposition::Consumed => {}
                EventDisposition::Retry => self
                    .pending_events
                    .push_back(event)
                    .expect("bounded vsock pending event queue must have capacity"),
            }
        }

        let budget_exhausted = retry_count + polled_events == VSOCK_EVENT_BUDGET;
        let retry_progress_remains = consumed_retries != 0 && !self.pending_events.is_empty();
        Ok(if budget_exhausted || retry_progress_remains {
            DrainOutcome::More
        } else {
            DrainOutcome::Idle
        })
    }

    fn handle_event(&mut self, event: VsockEvent) -> EventDisposition {
        debug!("handling vsock event: {event:?}");
        match event {
            VsockEvent::ConnectionRequest(conn_id) => {
                match VSOCK_CONN_MANAGER.lock().on_connection_request(conn_id) {
                    Ok(Some(incoming)) => incoming.publish(),
                    Ok(None) => {}
                    Err(error) => {
                        info!("rejecting vsock connection request {conn_id:?}: {error}");
                        let _ = self.device.lock().abort(conn_id);
                    }
                }
            }
            VsockEvent::Received(conn_id, event_len) => {
                let Some(connection) = lookup_connection(conn_id) else {
                    info!("received data for unknown vsock connection: {conn_id:?}");
                    return EventDisposition::Consumed;
                };
                let free_space = connection.lock().rx_buffer_free();
                if free_space == 0 {
                    return EventDisposition::Retry;
                }
                let max_read = free_space.min(event_len).min(self.rx_buffer.len());
                if max_read == 0 {
                    return EventDisposition::Consumed;
                }
                let read_len = match self
                    .device
                    .lock()
                    .recv(conn_id, &mut self.rx_buffer[..max_read])
                {
                    Ok(read_len) => read_len,
                    Err(VsockError::Retry) => return EventDisposition::Retry,
                    Err(error) => {
                        info!("failed to receive vsock data for {conn_id:?}: {error:?}");
                        return EventDisposition::Consumed;
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
                    "received {read_len} vsock bytes for {conn_id:?} (written={written}, \
                     buffer_used={buffer_used}/{VSOCK_RX_BUFFER_SIZE})"
                );
                wake_tx_if_ready(conn_id, &connection);
            }
            VsockEvent::Disconnected(conn_id) => {
                if let Some(connection) = lookup_connection(conn_id) {
                    {
                        let mut state = connection.lock();
                        state.set_state(ConnectionState::Closed);
                        state.set_rx_closed(true);
                        state.set_tx_closed(true);
                    }
                    connection.wake_rx();
                    connection.wake_connect();
                    connection.wake_tx();
                }
            }
            VsockEvent::Connected(conn_id) => {
                if let Some(connection) = lookup_connection(conn_id) {
                    connection.lock().set_state(ConnectionState::Connected);
                    connection.wake_connect();
                    wake_tx_if_ready(conn_id, &connection);
                }
            }
            VsockEvent::CreditUpdate(conn_id) => {
                if let Some(connection) = lookup_connection(conn_id) {
                    wake_tx_if_ready(conn_id, &connection);
                }
            }
            VsockEvent::Unknown => warn!("received unknown vsock event"),
        }
        EventDisposition::Consumed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DrainOutcome {
    Idle,
    More,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventDisposition {
    Consumed,
    Retry,
}

fn lookup_connection(connection_id: VsockConnId) -> Option<Arc<Connection>> {
    VSOCK_CONN_MANAGER.lock().get_connection(connection_id)
}

fn wake_tx_if_ready(connection_id: VsockConnId, connection: &Connection) {
    if super::vsock_send_capacity(connection_id).is_ok_and(|capacity| capacity != 0) {
        connection.wake_tx();
    }
}

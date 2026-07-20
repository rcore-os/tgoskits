//! CPU-pinned Ethernet maintenance owner and mailbox-backed ax-net facade.

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc, vec, vec::Vec};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_net::{
    EthernetDriver, NetDeviceError, NetDeviceResult, NetRxBuffer, NetTxBuffer,
    WifiControl as AxWifiControl, WifiControlCommand, WifiControlCompletion, WifiControlGeneration,
    WifiControlResult,
};
use axpoll::{IoEvents, PollSet};
use rd_net::{
    ActiveNetQueues, EthernetIrqFault, Event as EthernetIrqEvent, Net, NetError, OwnerInitInput,
    OwnerInitPoll, OwnerInitSchedule, QueueActivationError, WifiCommand as DriverWifiCommand,
    WifiCommandProgress, WifiCommandResult as DriverWifiCommandResult, WifiCommandSchedule,
    WifiCommandStartError, WifiLinkPolicy,
};
use rdif_irq::{ContainmentCause, FaultContainment, IrqCapture, MaskedSource};
use thiserror::Error;

use crate::{
    maintenance::{
        DeviceMaintenanceHandle, LocalIrqWake, LocalIrqWakeError, MaintenanceCauses,
        MaintenanceClosed, MaintenanceError, MaintenanceIrqAction, MaintenancePublishResult,
        MaintenanceRegistrar, MaintenanceSession, MaintenanceState, MaintenanceThread,
        spawn_maintenance_domain,
    },
    task::WaitQueue,
};

const NET_OWNER_CPU: usize = 0;
const NET_BATCH_LIMIT: usize = 64;
const NET_PACKET_MAILBOX_CAPACITY: usize = 64;
const NET_WIFI_COMMAND_MAILBOX_CAPACITY: usize = 8;
const NET_RX_SPACE_CAUSE: MaintenanceCauses = MaintenanceCauses::from_bits(1 << 16);
const NET_WIFI_COMMAND_CAUSE: MaintenanceCauses = MaintenanceCauses::from_bits(1 << 17);

/// Failure before a hardware NIC can be published to ax-net.
#[derive(Debug, Error)]
pub(crate) enum NetActivationError {
    #[error(transparent)]
    Maintenance(#[from] MaintenanceError),
    #[error("network IRQ operation failed: {0:?}")]
    Irq(irq_framework::IrqError),
    #[error("interrupt-driven network device has no resolved IRQ binding")]
    MissingIrq,
    #[error("interrupt-driven network device did not provide an IRQ endpoint")]
    MissingIrqEndpoint,
    #[error("discovered network device exposed an armed IRQ source before owner binding")]
    DiscoveredSourceArmed,
    #[error(transparent)]
    QueueActivation(#[from] QueueActivationError),
    #[error("network device initialization failed on its maintenance owner: {0}")]
    DriverInitialization(NetError),
    #[error("network device IRQ source transition failed on its maintenance owner: {0}")]
    DriverIrq(NetError),
    #[error("network device returned an initialization schedule with no activation source")]
    InvalidInitSchedule,
}

impl From<irq_framework::IrqError> for NetActivationError {
    fn from(error: irq_framework::IrqError) -> Self {
        Self::Irq(error)
    }
}

#[derive(Clone, Copy, Debug)]
enum NetMaintenanceEvent {
    Irq {
        event: EthernetIrqEvent,
        masked: Option<MaskedSource>,
    },
    Fault {
        reason: EthernetIrqFault,
        masked: Option<MaskedSource>,
    },
}

struct PendingNetRearms {
    sources: [Option<MaskedSource>; NET_BATCH_LIMIT],
    len: usize,
}

impl PendingNetRearms {
    const fn new() -> Self {
        Self {
            sources: [None; NET_BATCH_LIMIT],
            len: 0,
        }
    }

    fn push(&mut self, source: MaskedSource) -> Result<(), MaintenanceError> {
        if self.sources[..self.len]
            .iter()
            .flatten()
            .any(|retained| *retained == source)
        {
            return Ok(());
        }
        if self.len == self.sources.len() {
            return Err(MaintenanceError::Irq(irq_framework::IrqError::Busy));
        }
        self.sources[self.len] = Some(source);
        self.len += 1;
        Ok(())
    }

    fn rearm_if_drained(
        &mut self,
        queues: &mut ActiveNetQueues,
        tx_irq_pending: bool,
        rx_irq_pending: bool,
        wifi_irq_activations: usize,
    ) -> Result<(), MaintenanceError> {
        if tx_irq_pending || rx_irq_pending || wifi_irq_activations != 0 {
            return Ok(());
        }
        while self.len != 0 {
            let source =
                self.sources[0].expect("non-empty rearm queue must retain its first source");
            queues
                .rearm_irq_source(source)
                .map_err(|_| MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
            self.sources.copy_within(1..self.len, 0);
            self.len -= 1;
            self.sources[self.len] = None;
        }
        Ok(())
    }
}

struct NetReady {
    name: String,
    mac: [u8; 6],
    tx_capacity: usize,
    link_policy: Option<WifiLinkPolicy>,
    supports_wifi_control: bool,
}

/// A published network facade and immutable policy established by its owner.
pub(crate) struct ActivatedNetDevice {
    pub(crate) driver: Box<dyn EthernetDriver>,
    pub(crate) link_policy: Option<WifiLinkPolicy>,
}

struct NetActivationSlot {
    result: ax_kspin::SpinNoIrq<Option<Result<NetReady, NetActivationError>>>,
    wait: WaitQueue,
}

impl NetActivationSlot {
    const fn new() -> Self {
        Self {
            result: ax_kspin::SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn publish(&self, result: Result<NetReady, NetActivationError>) {
        let mut slot = self.result.lock();
        if slot.is_some() {
            return;
        }
        *slot = Some(result);
        drop(slot);
        self.wait.notify_all();
    }

    fn publish_owner_failure(&self, error: MaintenanceError) {
        self.publish(Err(NetActivationError::Maintenance(error)));
    }

    fn wait_result(&self) -> Result<NetReady, NetActivationError> {
        self.wait
            .try_wait_until(|| self.result.lock().is_some())
            .map_err(MaintenanceError::from)?;
        self.result
            .lock()
            .take()
            .expect("network activation result disappeared after publication")
    }
}

struct NetRemote {
    maintenance: LazyInit<DeviceMaintenanceHandle<NetMaintenanceEvent>>,
    tx_ingress: ax_kspin::SpinNoIrq<VecDeque<Vec<u8>>>,
    rx_egress: ax_kspin::SpinNoIrq<VecDeque<Vec<u8>>>,
    wifi_commands: ax_kspin::SpinNoIrq<VecDeque<WifiCommandRequest>>,
    next_wifi_generation: AtomicU64,
    readiness: Arc<PollSet>,
    rx_continuation: AtomicBool,
    closed: AtomicBool,
}

impl NetRemote {
    fn new() -> Self {
        Self {
            maintenance: LazyInit::new(),
            tx_ingress: ax_kspin::SpinNoIrq::new(VecDeque::with_capacity(
                NET_PACKET_MAILBOX_CAPACITY,
            )),
            rx_egress: ax_kspin::SpinNoIrq::new(VecDeque::with_capacity(
                NET_PACKET_MAILBOX_CAPACITY,
            )),
            wifi_commands: ax_kspin::SpinNoIrq::new(VecDeque::with_capacity(
                NET_WIFI_COMMAND_MAILBOX_CAPACITY,
            )),
            next_wifi_generation: AtomicU64::new(1),
            readiness: Arc::new(PollSet::new()),
            rx_continuation: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    fn install_maintenance(&self, maintenance: DeviceMaintenanceHandle<NetMaintenanceEvent>) {
        self.maintenance.init_once(maintenance);
    }

    fn submit_packet(&self, packet: Vec<u8>) -> NetDeviceResult {
        if self.closed.load(Ordering::Acquire) {
            return Err(NetDeviceError::BadState);
        }
        let mut ingress = self.tx_ingress.lock();
        // Serialize the final admission decision with close(), which sets the
        // sticky flag before taking this same lock to drain accepted work.
        if self.closed.load(Ordering::Acquire) {
            return Err(NetDeviceError::BadState);
        }
        if ingress.len() == NET_PACKET_MAILBOX_CAPACITY {
            return Err(NetDeviceError::Again);
        }
        ingress.push_back(packet);
        drop(ingress);
        if self
            .maintenance
            .publish_cause(MaintenanceCauses::SUBMIT)
            .is_err()
        {
            // Publication failure closes or quarantines the maintenance
            // lifecycle. Keep the accepted packet in the remote-owned queue:
            // teardown is its sole owner, whereas removing the queue tail
            // could steal another producer's packet.
            self.closed.store(true, Ordering::Release);
            return Err(NetDeviceError::BadState);
        }
        Ok(())
    }

    fn receive_packet(&self) -> NetDeviceResult<Vec<u8>> {
        let packet = self
            .rx_egress
            .lock()
            .pop_front()
            .ok_or(NetDeviceError::Again)?;
        // Only an owner-published continuation proves that an acknowledged RX
        // event still has descriptors to consume. Releasing ordinary mailbox
        // space must not turn into an unqualified ring probe.
        if self.rx_continuation.load(Ordering::Acquire) {
            let _ = self.maintenance.publish_cause(NET_RX_SPACE_CAUSE);
        }
        Ok(packet)
    }

    fn submit_wifi_command(
        &self,
        command: WifiControlCommand,
    ) -> NetDeviceResult<WifiControlCompletion> {
        if self.closed.load(Ordering::Acquire) || ax_hal::irq::in_irq_context() {
            return Err(NetDeviceError::BadState);
        }
        let reply = Arc::new(WifiCommandReply::new());
        let command = map_wifi_command(command);
        let mut commands = self.wifi_commands.lock();
        // close() publishes the flag before acquiring this lock. Rechecking
        // here makes "accepted" and "drained by close" one atomic mailbox
        // transition, so no request can be stranded behind shutdown.
        if self.closed.load(Ordering::Acquire) {
            return Err(NetDeviceError::BadState);
        }
        if commands.len() == NET_WIFI_COMMAND_MAILBOX_CAPACITY {
            return Err(NetDeviceError::Again);
        }
        let generation = self
            .allocate_wifi_generation()
            .ok_or(NetDeviceError::BadState)?;
        reply.bind_generation(generation)?;
        let request = WifiCommandRequest::new(generation, command, Arc::clone(&reply));
        commands.push_back(request);
        drop(commands);

        if self
            .maintenance
            .publish_cause(NET_WIFI_COMMAND_CAUSE)
            .is_err()
        {
            self.closed.store(true, Ordering::Release);
            reply.publish(generation, Err(NetDeviceError::BadState));
        }
        reply.wait()
    }

    fn allocate_wifi_generation(&self) -> Option<WifiControlGeneration> {
        self.next_wifi_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .ok()
            .and_then(NonZeroU64::new)
            .map(WifiControlGeneration::new)
    }

    fn take_wifi_command(&self) -> Option<WifiCommandRequest> {
        self.wifi_commands.lock().pop_front()
    }

    fn has_wifi_commands(&self) -> bool {
        !self.wifi_commands.lock().is_empty()
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.rx_continuation.store(false, Ordering::Release);
        let commands = core::mem::take(&mut *self.wifi_commands.lock());
        // Dropping a request publishes its terminal owner-unavailable reply.
        // Do it after releasing the ingress lock so waiter wakeup cannot
        // re-enter this mailbox while it is borrowed.
        drop(commands);
    }

    fn publish_rx(&self, packet: Vec<u8>) -> Result<(), Vec<u8>> {
        let mut egress = self.rx_egress.lock();
        if egress.len() == NET_PACKET_MAILBOX_CAPACITY {
            return Err(packet);
        }
        egress.push_back(packet);
        drop(egress);
        // SAFETY: the CPU-pinned maintenance owner is ordinary task context;
        // packet bytes were published before waking protocol workers.
        unsafe {
            self.readiness.wake(IoEvents::IN);
        }
        Ok(())
    }
}

struct RuntimeTxBuffer {
    packet: Vec<u8>,
}

struct WifiCommandReply {
    generation: AtomicU64,
    result: ax_kspin::SpinNoIrq<Option<NetDeviceResult<WifiControlCompletion>>>,
    wait: WaitQueue,
}

impl WifiCommandReply {
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            result: ax_kspin::SpinNoIrq::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn bind_generation(&self, generation: WifiControlGeneration) -> NetDeviceResult {
        self.generation
            .compare_exchange(0, generation.get(), Ordering::Release, Ordering::Relaxed)
            .map(|_| ())
            .map_err(|_| NetDeviceError::BadState)
    }

    fn publish(
        &self,
        generation: WifiControlGeneration,
        result: NetDeviceResult<WifiControlResult>,
    ) {
        let result = if generation.get() == self.generation.load(Ordering::Acquire) {
            result.map(|result| WifiControlCompletion { generation, result })
        } else {
            Err(NetDeviceError::BadState)
        };
        let mut slot = self.result.lock();
        if slot.is_some() {
            return;
        }
        *slot = Some(result);
        drop(slot);
        self.wait.notify_all();
    }

    fn wait(&self) -> NetDeviceResult<WifiControlCompletion> {
        self.wait
            .try_wait_until(|| self.result.lock().is_some())
            .map_err(|_| NetDeviceError::BadState)?;
        self.result.lock().take().ok_or(NetDeviceError::BadState)?
    }
}

struct WifiCommandRequest {
    generation: WifiControlGeneration,
    command: Option<DriverWifiCommand>,
    reply: Arc<WifiCommandReply>,
    completed: bool,
}

impl WifiCommandRequest {
    fn new(
        generation: WifiControlGeneration,
        command: DriverWifiCommand,
        reply: Arc<WifiCommandReply>,
    ) -> Self {
        Self {
            generation,
            command: Some(command),
            reply,
            completed: false,
        }
    }

    fn take_command(&mut self) -> DriverWifiCommand {
        self.command
            .take()
            .expect("uncompleted Wi-Fi request must retain its command")
    }

    fn complete(mut self, result: NetDeviceResult<WifiControlResult>) {
        self.reply.publish(self.generation, result);
        self.completed = true;
    }
}

impl Drop for WifiCommandRequest {
    fn drop(&mut self) {
        if !self.completed {
            self.reply
                .publish(self.generation, Err(NetDeviceError::BadState));
        }
    }
}

struct ActiveWifiCommand {
    request: WifiCommandRequest,
    schedule: WifiCommandSchedule,
}

struct RuntimeWifiControl {
    remote: Arc<NetRemote>,
}

impl AxWifiControl for RuntimeWifiControl {
    fn reconfigure(&self, command: WifiControlCommand) -> NetDeviceResult<WifiControlCompletion> {
        self.remote.submit_wifi_command(command)
    }
}

impl NetTxBuffer for RuntimeTxBuffer {
    fn packet(&self) -> &[u8] {
        &self.packet
    }

    fn packet_mut(&mut self) -> &mut [u8] {
        &mut self.packet
    }

    fn packet_len(&self) -> usize {
        self.packet.len()
    }
}

struct RuntimeRxBuffer {
    packet: Vec<u8>,
}

impl NetRxBuffer for RuntimeRxBuffer {
    fn packet(&self) -> &[u8] {
        &self.packet
    }
}

struct RuntimeEthernetDriver {
    name: String,
    mac: [u8; 6],
    tx_capacity: usize,
    remote: Arc<NetRemote>,
    wifi_control: Option<Arc<RuntimeWifiControl>>,
    _maintenance_thread: MaintenanceThread,
}

impl EthernetDriver for RuntimeEthernetDriver {
    fn device_name(&self) -> &str {
        &self.name
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        Some(Arc::clone(&self.remote.readiness))
    }

    fn wifi_control(&self) -> Option<Arc<dyn AxWifiControl>> {
        self.wifi_control
            .as_ref()
            .map(|control| Arc::clone(control) as Arc<dyn AxWifiControl>)
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> NetDeviceResult<Box<dyn NetTxBuffer>> {
        if size > self.tx_capacity {
            return Err(NetDeviceError::InvalidParam);
        }
        Ok(Box::new(RuntimeTxBuffer {
            packet: vec![0; size],
        }))
    }

    fn recycle_tx_buffers(&mut self) -> NetDeviceResult {
        Ok(())
    }

    fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult {
        self.remote.submit_packet(tx_buf.packet().to_vec())
    }

    fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>> {
        self.remote
            .receive_packet()
            .map(|packet| Box::new(RuntimeRxBuffer { packet }) as Box<dyn NetRxBuffer>)
    }

    fn recycle_rx_buffer(&mut self, _rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult {
        Ok(())
    }
}

/// Moves a real NIC into one fixed-CPU maintenance owner and returns only its
/// software-mailbox facade to ax-net.
pub(crate) fn activate_net_device(
    net: Net,
    name: &'static str,
    irq: Option<irq_framework::IrqId>,
) -> Result<ActivatedNetDevice, NetActivationError> {
    let remote = Arc::new(NetRemote::new());
    let activation = Arc::new(NetActivationSlot::new());
    let owner_remote = Arc::clone(&remote);
    let owner_activation = Arc::clone(&activation);
    let failure_activation = Arc::clone(&activation);
    let thread = spawn_maintenance_domain::<NetMaintenanceEvent, _>(
        NET_OWNER_CPU,
        alloc::format!("net-maint/{name}"),
        move |registrar| {
            let result = run_net_owner(net, name, irq, owner_remote, owner_activation, registrar);
            if let Err(error) = result.as_ref() {
                failure_activation.publish_owner_failure(*error);
            }
            result
        },
    )?;
    let ready = activation.wait_result()?;
    let wifi_control = ready.supports_wifi_control.then(|| {
        Arc::new(RuntimeWifiControl {
            remote: Arc::clone(&remote),
        })
    });
    Ok(ActivatedNetDevice {
        link_policy: ready.link_policy,
        driver: Box::new(RuntimeEthernetDriver {
            name: ready.name,
            mac: ready.mac,
            tx_capacity: ready.tx_capacity,
            remote,
            wifi_control,
            _maintenance_thread: thread,
        }),
    })
}

fn run_net_owner(
    mut net: Net,
    name: &'static str,
    irq: Option<irq_framework::IrqId>,
    remote: Arc<NetRemote>,
    activation: Arc<NetActivationSlot>,
    registrar: MaintenanceRegistrar<NetMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    registrar.validate_owner()?;
    let maintenance = registrar.remote_handle();
    let registration = match prepare_net_irq_owner(&mut net, name, irq, &registrar) {
        Ok(registration) => registration,
        Err(error) => {
            let session = registrar.activate()?;
            activation.publish(Err(error));
            park_net_quarantine(remote, session, net);
        }
    };
    // Discovery is a typed source-masked state. Calling ready-only queue IRQ
    // control here would force drivers such as VirtIO through an invalid state
    // transition before their owner initialization has constructed queues.
    if net.is_irq_enabled() {
        let error = match registration.quench_line() {
            Ok(()) => NetActivationError::DiscoveredSourceArmed,
            Err(containment) => NetActivationError::Maintenance(containment),
        };
        let session = registrar.activate()?;
        activation.publish(Err(error));
        park_net_quarantine(remote, session, (net, registration));
    }
    let session = match registrar.activate() {
        Ok(session) => session,
        Err(error) => {
            if let Err(failure) = registration.close() {
                warn!("failed to close network IRQ after activation failure: {failure}");
            }
            remote.close();
            return Err(error);
        }
    };
    if let Err(error) = registration.enable() {
        activation.publish(Err(NetActivationError::Maintenance(error)));
        park_net_quarantine(remote, session, (net, registration));
    }
    if let Err(error) = drive_net_owner_init(&mut net, &session) {
        warn!("network device {name} owner initialization failed: {error}");
        activation.publish(Err(error));
        park_net_quarantine(remote, session, (net, registration));
    }
    // Queue construction publishes DMA ownership. Stop and drain the OS
    // callback before asking the now-ready driver to suppress queue sources.
    if let Err(error) = registration.disable() {
        activation.publish(Err(NetActivationError::Maintenance(error)));
        park_net_quarantine(remote, session, (net, registration));
    }
    if let Err(error) = registration.synchronize() {
        activation.publish(Err(NetActivationError::Maintenance(error)));
        park_net_quarantine(remote, session, (net, registration));
    }
    if let Err(error) = net.disable_irq() {
        let error = match registration.quench_line() {
            Ok(()) => NetActivationError::DriverIrq(error),
            Err(containment) => NetActivationError::Maintenance(containment),
        };
        activation.publish(Err(error));
        park_net_quarantine(remote, session, (net, registration));
    }
    let queues = match net.activate_queues() {
        Ok(queues) => queues,
        Err(failure) => {
            warn!(
                "network queue activation failed at {}: {:?}",
                failure.reason(),
                failure.source_error()
            );
            activation.publish(Err(NetActivationError::QueueActivation(failure.reason())));
            let quarantine = failure.into_quarantine();
            park_net_quarantine(remote, session, (quarantine, registration));
        }
    };
    let mut queues = queues;
    if queues.tx_queue_count() != 1 || queues.rx_queue_count() != 1 {
        activation.publish(Err(NetActivationError::QueueActivation(
            QueueActivationError::UnsupportedTopology,
        )));
        let quarantine = queues.into_quarantine();
        park_net_quarantine(remote, session, (quarantine, registration));
    }
    let enable_result = queues.enable_irq();
    if let Err(error) = enable_result {
        activation.publish(Err(NetActivationError::DriverIrq(error)));
        let quarantine = queues.into_quarantine();
        park_net_quarantine(remote, session, (quarantine, registration));
    }
    if let Err(error) = registration.enable() {
        let mask_result = queues.disable_irq();
        if let Err(mask_error) = mask_result {
            warn!(
                "network action re-enable failed and queue source rollback also failed: \
                 {mask_error}"
            );
            let _ = registration.quench_line();
        }
        activation.publish(Err(NetActivationError::Maintenance(error)));
        let quarantine = queues.into_quarantine();
        park_net_quarantine(remote, session, (quarantine, registration));
    }
    let ready = NetReady {
        name: String::from(name),
        mac: queues.mac_address(),
        tx_capacity: queues
            .tx_buf_size(0)
            .expect("validated single-queue topology must retain TX owner"),
        link_policy: queues.owner_link_policy(),
        supports_wifi_control: queues.supports_wifi_control(),
    };
    remote.install_maintenance(maintenance);
    activation.publish(Ok(ready));

    let result = net_owner_loop(&mut queues, &remote, &session);
    if let Err(error) = result {
        warn!("network maintenance owner entered contained shutdown: {error}");
    } else {
        warn!("network maintenance shutdown lacks a portable DMA quiesce proof");
    }
    remote.close();
    if session.state() != MaintenanceState::Quarantined {
        let mask_result = queues.disable_irq();
        if let Err(error) = mask_result {
            warn!("network maintenance owner could not mask device sources: {error}");
        }
    }
    // The portable Interface boundary cannot yet prove that DMA has stopped.
    // Retain every callback, queue, descriptor and buffer on this fixed stack;
    // releasing any subset would turn a recoverable device fault into DMA UAF.
    let quarantine = queues.into_quarantine();
    park_net_quarantine(remote, session, (quarantine, registration));
}

fn park_net_quarantine<R>(
    remote: Arc<NetRemote>,
    session: MaintenanceSession<NetMaintenanceEvent>,
    retained: R,
) -> ! {
    remote.close();
    let _retained = (remote, retained);
    session.quarantine_and_park();
}

fn drive_net_owner_init(
    net: &mut Net,
    session: &MaintenanceSession<NetMaintenanceEvent>,
) -> Result<(), NetActivationError> {
    let mut schedule = OwnerInitSchedule::run_again();
    loop {
        if schedule.run_again {
            let mut exhausted = true;
            for _ in 0..NET_BATCH_LIMIT {
                match poll_net_owner_init(net, OwnerInitInput::at(monotonic_now_ns()))? {
                    None => return Ok(()),
                    Some(next) if next.run_again => schedule = next,
                    Some(next) => {
                        schedule = next;
                        exhausted = false;
                        break;
                    }
                }
            }
            if exhausted && schedule.run_again {
                crate::task::yield_current_cpu().map_err(MaintenanceError::from)?;
                continue;
            }
        }

        wait_for_net_init_activation(session, schedule)?;
        // Initialization consumes one acknowledged event at a time. If that
        // event makes the interface Ready, later snapshots remain in the
        // maintenance mailbox for the normal owner loop instead of being
        // stranded in a local batch that can no longer be returned.
        let mut captured = None;
        let drain = session.drain_owner(1, |event| captured = Some(event))?;
        if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            return Err(NetActivationError::Maintenance(MaintenanceError::Irq(
                irq_framework::IrqError::Busy,
            )));
        }
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN) {
            return Err(NetActivationError::Maintenance(
                MaintenanceError::CloseIncomplete(session.state()),
            ));
        }

        let Some(event) = captured else {
            schedule = match poll_net_owner_init(net, OwnerInitInput::at(monotonic_now_ns()))? {
                None => return Ok(()),
                Some(schedule) => schedule,
            };
            continue;
        };

        let (event, masked) = match event {
            NetMaintenanceEvent::Irq { event, masked } => (event, masked),
            NetMaintenanceEvent::Fault { reason, masked } => {
                warn!("network initialization IRQ capture fault: {reason}; masked={masked:?}");
                return Err(NetActivationError::Maintenance(MaintenanceError::Irq(
                    irq_framework::IrqError::Controller,
                )));
            }
        };
        schedule = match poll_net_owner_init(
            net,
            OwnerInitInput::with_event(monotonic_now_ns(), event),
        )? {
            None => {
                if let Some(source) = masked {
                    net.rearm_irq_source(source)
                        .map_err(NetActivationError::DriverInitialization)?;
                }
                return Ok(());
            }
            Some(schedule) => schedule,
        };
        if let Some(source) = masked {
            net.rearm_irq_source(source)
                .map_err(NetActivationError::DriverInitialization)?;
        }
    }
}

fn poll_net_owner_init(
    net: &mut Net,
    input: OwnerInitInput,
) -> Result<Option<OwnerInitSchedule>, NetActivationError> {
    match net.poll_owner_init(input) {
        OwnerInitPoll::Ready => Ok(None),
        OwnerInitPoll::Pending(schedule) => Ok(Some(schedule)),
        OwnerInitPoll::Failed(error) => Err(NetActivationError::DriverInitialization(error)),
    }
}

fn wait_for_net_init_activation(
    session: &MaintenanceSession<NetMaintenanceEvent>,
    schedule: OwnerInitSchedule,
) -> Result<(), NetActivationError> {
    if schedule.run_again {
        return Ok(());
    }
    let schedule = schedule
        .validate()
        .map_err(|_| NetActivationError::InvalidInitSchedule)?;
    match (!schedule.irq_sources.is_empty(), schedule.wake_at_ns) {
        (true, Some(deadline_ns)) => {
            let _ = session.wait_for_pending_until(deadline_ns)?;
        }
        (true, None) => session.wait_for_pending()?,
        (false, Some(deadline_ns)) => {
            if monotonic_now_ns() < deadline_ns {
                let _ = session.wait_for_pending_until(deadline_ns)?;
            }
        }
        (false, None) => return Err(NetActivationError::InvalidInitSchedule),
    }
    Ok(())
}

fn monotonic_now_ns() -> u64 {
    ax_hal::time::monotonic_time_nanos()
}

fn prepare_net_irq_owner(
    net: &mut Net,
    name: &'static str,
    irq: Option<irq_framework::IrqId>,
    registrar: &MaintenanceRegistrar<NetMaintenanceEvent>,
) -> Result<MaintenanceIrqAction, NetActivationError> {
    let irq = irq.ok_or(NetActivationError::MissingIrq)?;
    let mut endpoint = net
        .take_irq_endpoint()
        .ok_or(NetActivationError::MissingIrqEndpoint)?;
    let wake = registrar.local_irq_wake()?;
    let owner_cpu = registrar.owner_cpu();
    let action = registrar.register_shared_disabled(
        alloc::format!("{name}/ethernet"),
        irq,
        move |context| net_irq_action(context.cpu.0, owner_cpu, &wake, &mut endpoint),
    )?;
    Ok(action)
}

fn net_irq_action(
    actual_cpu: usize,
    owner_cpu: usize,
    wake: &LocalIrqWake<NetMaintenanceEvent>,
    endpoint: &mut rd_net::IrqEndpoint,
) -> irq_framework::IrqReturn {
    if actual_cpu != owner_cpu {
        return contain_net_irq(endpoint, ContainmentCause::OwnerUnavailable);
    }
    match endpoint.capture_irq() {
        IrqCapture::Unhandled => irq_framework::IrqReturn::Unhandled,
        IrqCapture::Captured { event, masked } => match wake.publish_from_irq(
            MaintenanceCauses::IRQ,
            NetMaintenanceEvent::Irq { event, masked },
        ) {
            Ok(MaintenancePublishResult::Published) => irq_framework::IrqReturn::Wake,
            Ok(MaintenancePublishResult::Overflowed) => {
                contain_net_irq(endpoint, ContainmentCause::PublicationFull)
            }
            Err(error) => contain_net_irq(endpoint, containment_cause(error)),
        },
        IrqCapture::Fault {
            reason,
            containment,
        } => {
            let masked = match containment {
                FaultContainment::DeviceSourceMasked(source) => Some(source),
                FaultContainment::Uncontained => None,
            };
            let _ = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                NetMaintenanceEvent::Fault { reason, masked },
            );
            match containment {
                FaultContainment::DeviceSourceMasked(_) => {
                    irq_framework::IrqReturn::DisableActionAndWake
                }
                FaultContainment::Uncontained => irq_framework::IrqReturn::MaskLineAndWake,
            }
        }
    }
}

fn containment_cause(error: LocalIrqWakeError) -> ContainmentCause {
    match error {
        LocalIrqWakeError::Closed => ContainmentCause::PublicationClosed,
        LocalIrqWakeError::NotHardIrq
        | LocalIrqWakeError::WrongCpu { .. }
        | LocalIrqWakeError::OwnerIdentityMismatch
        | LocalIrqWakeError::OwnerPlacementMismatch { .. }
        | LocalIrqWakeError::OwnerUnavailable { .. } => ContainmentCause::OwnerUnavailable,
    }
}

fn contain_net_irq(
    endpoint: &mut rd_net::IrqEndpoint,
    cause: ContainmentCause,
) -> irq_framework::IrqReturn {
    match endpoint.contain(cause) {
        Ok(_) => irq_framework::IrqReturn::DisableActionAndWake,
        Err(_) => irq_framework::IrqReturn::MaskLineAndWake,
    }
}

fn net_owner_loop(
    queues: &mut ActiveNetQueues,
    remote: &NetRemote,
    session: &MaintenanceSession<NetMaintenanceEvent>,
) -> Result<(), MaintenanceError> {
    session.validate_owner()?;
    let tx_queue_id = queues
        .tx_queue_id(0)
        .ok_or(MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
    let rx_queue_id = queues
        .rx_queue_id(0)
        .ok_or(MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
    let mut pending = true;
    let mut tx_irq_pending = false;
    let mut rx_irq_pending = false;
    let mut active_wifi_command = None;
    let mut wifi_irq_activations = 0_usize;
    let mut pending_rearms = PendingNetRearms::new();
    loop {
        session.validate_owner()?;
        let now_ns = monotonic_now_ns();
        let queued_wifi_ready = active_wifi_command.is_none() && remote.has_wifi_commands();
        if !pending && !wifi_command_work_ready(&active_wifi_command, queued_wifi_ready, now_ns) {
            if let Some(deadline_ns) = wifi_command_deadline(&active_wifi_command) {
                let _ = session.wait_for_pending_until(deadline_ns)?;
            } else {
                session.wait_for_pending()?;
            }
        }
        let command_was_active = active_wifi_command.is_some();
        let mut events = [None; NET_BATCH_LIMIT];
        let mut count = 0;
        let drain = session.drain_owner(NET_BATCH_LIMIT, |event| {
            events[count] = Some(event);
            count += 1;
        })?;
        if session.state() == MaintenanceState::Quarantined {
            return Err(MaintenanceError::CloseIncomplete(
                MaintenanceState::Quarantined,
            ));
        }
        pending = drain.pending();
        if drain.causes().contains(MaintenanceCauses::SHUTDOWN) {
            return Ok(());
        }
        if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
            return Err(MaintenanceError::Irq(irq_framework::IrqError::Busy));
        }

        for event in events.iter().copied().flatten() {
            if let NetMaintenanceEvent::Fault { reason, masked } = event {
                warn!("network IRQ capture fault: {reason}; masked={masked:?}");
                return Err(MaintenanceError::Irq(irq_framework::IrqError::Controller));
            }
        }
        for event in events.iter().copied().flatten() {
            if let NetMaintenanceEvent::Irq { event, masked } = event {
                queues
                    .service_irq_event(event)
                    .map_err(|_| MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
                if event.tx_queue.contains(tx_queue_id) {
                    tx_irq_pending = true;
                }
                if event.rx_queue.contains(rx_queue_id) {
                    rx_irq_pending = true;
                    remote.rx_continuation.store(true, Ordering::Release);
                }
                if let Some(source) = masked {
                    pending_rearms.push(source)?;
                }
            }
        }

        if command_was_active {
            wifi_irq_activations = wifi_irq_activations.saturating_add(
                events
                    .iter()
                    .flatten()
                    .filter(|event| matches!(event, NetMaintenanceEvent::Irq { .. }))
                    .count(),
            );
        }
        pending |= service_wifi_commands(
            queues,
            remote,
            &mut active_wifi_command,
            &mut wifi_irq_activations,
            monotonic_now_ns(),
        )?;

        if tx_irq_pending {
            let reclaimed = queues
                .reclaim_tx(0, NET_BATCH_LIMIT)
                .map_err(|_| MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
            if reclaimed < NET_BATCH_LIMIT {
                tx_irq_pending = false;
            } else {
                // This is continuation of a captured completion snapshot, not
                // a timer or submit-side completion probe.
                pending = true;
            }
        }
        if rx_irq_pending {
            match service_rx(queues, remote)? {
                RxServiceProgress::Drained => {
                    rx_irq_pending = false;
                    remote.rx_continuation.store(false, Ordering::Release);
                }
                RxServiceProgress::BudgetExhausted => pending = true,
                RxServiceProgress::Backpressured => {}
            }
        }
        pending |= service_tx(queues, remote)?;
        pending_rearms.rearm_if_drained(
            queues,
            tx_irq_pending,
            rx_irq_pending,
            wifi_irq_activations,
        )?;
        if pending {
            crate::task::yield_current_cpu()?;
        }
    }
}

fn service_wifi_commands(
    queues: &mut ActiveNetQueues,
    remote: &NetRemote,
    active: &mut Option<ActiveWifiCommand>,
    irq_activations: &mut usize,
    now_ns: u64,
) -> Result<bool, MaintenanceError> {
    let mut transitions = 0;
    while transitions < NET_BATCH_LIMIT {
        if active.is_none() {
            let Some(mut request) = remote.take_wifi_command() else {
                return Ok(false);
            };
            let command = request.take_command();
            transitions += 1;
            match queues.start_wifi_command(command, now_ns) {
                Ok(WifiCommandProgress::Complete(result)) => {
                    request.complete(Ok(map_wifi_command_result(result)));
                    *irq_activations = 0;
                    continue;
                }
                Ok(WifiCommandProgress::Pending(schedule)) => {
                    *active = Some(ActiveWifiCommand {
                        request,
                        schedule: validate_wifi_command_schedule(schedule)?,
                    });
                }
                Ok(WifiCommandProgress::Failed(error)) => {
                    request.complete(Err(map_net_error(error)));
                    *irq_activations = 0;
                    continue;
                }
                Err(WifiCommandStartError::Unsupported(command)) => {
                    drop(command);
                    request.complete(Err(NetDeviceError::Unsupported));
                    *irq_activations = 0;
                    continue;
                }
                Err(WifiCommandStartError::Busy(command)) => {
                    drop(command);
                    request.complete(Err(NetDeviceError::Again));
                    *irq_activations = 0;
                    continue;
                }
            }
        }

        let schedule = active
            .as_ref()
            .expect("active Wi-Fi command was installed above")
            .schedule;
        let deadline_expired = schedule
            .wake_at_ns
            .is_some_and(|deadline_ns| now_ns >= deadline_ns);
        if !schedule.run_again && *irq_activations == 0 && !deadline_expired {
            return Ok(false);
        }
        if !schedule.run_again && *irq_activations > 0 {
            *irq_activations -= 1;
        }

        transitions += 1;
        match queues.poll_wifi_command(now_ns) {
            WifiCommandProgress::Complete(result) => {
                let command = active
                    .take()
                    .expect("completed Wi-Fi command must remain active");
                command
                    .request
                    .complete(Ok(map_wifi_command_result(result)));
                *irq_activations = 0;
            }
            WifiCommandProgress::Pending(schedule) => {
                active
                    .as_mut()
                    .expect("pending Wi-Fi command must remain active")
                    .schedule = validate_wifi_command_schedule(schedule)?;
            }
            WifiCommandProgress::Failed(error) => {
                let command = active
                    .take()
                    .expect("failed Wi-Fi command must remain active");
                command.request.complete(Err(map_net_error(error)));
                *irq_activations = 0;
            }
        }
    }

    let queued_wifi_ready = active.is_none() && remote.has_wifi_commands();
    Ok(wifi_command_work_ready_with_irq(
        active,
        queued_wifi_ready,
        *irq_activations,
        now_ns,
    ))
}

fn wifi_command_deadline(active: &Option<ActiveWifiCommand>) -> Option<u64> {
    active
        .as_ref()
        .and_then(|command| command.schedule.wake_at_ns)
}

fn wifi_command_ready(active: &Option<ActiveWifiCommand>, now_ns: u64) -> bool {
    active.as_ref().is_some_and(|command| {
        command.schedule.run_again
            || command
                .schedule
                .wake_at_ns
                .is_some_and(|deadline_ns| now_ns >= deadline_ns)
    })
}

fn wifi_command_work_ready(
    active: &Option<ActiveWifiCommand>,
    queued_command_ready: bool,
    now_ns: u64,
) -> bool {
    if active.is_some() {
        wifi_command_ready(active, now_ns)
    } else {
        queued_command_ready
    }
}

fn wifi_command_work_ready_with_irq(
    active: &Option<ActiveWifiCommand>,
    queued_command_ready: bool,
    irq_activations: usize,
    now_ns: u64,
) -> bool {
    wifi_command_work_ready(active, queued_command_ready, now_ns)
        || (active.is_some() && irq_activations > 0)
}

fn validate_wifi_command_schedule(
    schedule: WifiCommandSchedule,
) -> Result<WifiCommandSchedule, MaintenanceError> {
    schedule
        .validate()
        .map_err(|_| MaintenanceError::Irq(irq_framework::IrqError::Controller))
}

fn map_wifi_command(command: WifiControlCommand) -> DriverWifiCommand {
    match command {
        WifiControlCommand::JoinStation { ssid, passphrase } => {
            DriverWifiCommand::JoinStation { ssid, passphrase }
        }
        WifiControlCommand::StartAccessPoint { ssid, channel } => {
            DriverWifiCommand::StartAccessPoint { ssid, channel }
        }
    }
}

fn map_wifi_command_result(result: DriverWifiCommandResult) -> WifiControlResult {
    match result {
        DriverWifiCommandResult::StationConnected => WifiControlResult::StationConnected,
        DriverWifiCommandResult::AccessPointStarted => WifiControlResult::AccessPointStarted,
    }
}

fn map_net_error(error: NetError) -> NetDeviceError {
    match error {
        NetError::NotSupported => NetDeviceError::Unsupported,
        NetError::Retry => NetDeviceError::Again,
        NetError::NoMemory => NetDeviceError::NoMemory,
        NetError::LinkDown => NetDeviceError::BadState,
        NetError::Other(_) => NetDeviceError::Io,
    }
}

enum RxServiceProgress {
    Drained,
    BudgetExhausted,
    Backpressured,
}

fn service_rx(
    queues: &mut ActiveNetQueues,
    remote: &NetRemote,
) -> Result<RxServiceProgress, MaintenanceError> {
    let mut processed = 0;
    while processed < NET_BATCH_LIMIT {
        if remote.rx_egress.lock().len() == NET_PACKET_MAILBOX_CAPACITY {
            return Ok(RxServiceProgress::Backpressured);
        }
        let packet = queues
            .receive(0, |packet| packet.to_vec())
            .map_err(|_| MaintenanceError::Irq(irq_framework::IrqError::Controller))?;
        let Some(packet) = packet else {
            return Ok(RxServiceProgress::Drained);
        };
        if remote.publish_rx(packet).is_err() {
            return Err(MaintenanceError::Irq(irq_framework::IrqError::Busy));
        }
        processed += 1;
    }
    Ok(RxServiceProgress::BudgetExhausted)
}

fn service_tx(queues: &mut ActiveNetQueues, remote: &NetRemote) -> Result<bool, MaintenanceError> {
    let mut processed = 0;
    while processed < NET_BATCH_LIMIT {
        let Some(packet) = remote.tx_ingress.lock().pop_front() else {
            return Ok(false);
        };
        let packet_len = packet.len();
        let prepared = queues.prepare_send(0, packet_len, |buffer| {
            buffer.copy_from_slice(&packet);
        });
        let (_, mut pending) = match prepared {
            Ok(prepared) => prepared,
            Err(NetError::Retry) => {
                remote.tx_ingress.lock().push_front(packet);
                if queues.tx_has_inflight(0) {
                    return Ok(false);
                }
                return Err(MaintenanceError::Irq(irq_framework::IrqError::Controller));
            }
            Err(_) => {
                processed += 1;
                continue;
            }
        };
        match pending.try_submit() {
            Ok(()) => {}
            Err(NetError::Retry) => {
                drop(pending);
                remote.tx_ingress.lock().push_front(packet);
                if queues.tx_has_inflight(0) {
                    return Ok(false);
                }
                return Err(MaintenanceError::Irq(irq_framework::IrqError::Controller));
            }
            Err(_) => {}
        }
        processed += 1;
    }
    Ok(!remote.tx_ingress.lock().is_empty())
}

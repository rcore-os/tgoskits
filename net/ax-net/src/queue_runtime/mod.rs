//! Queue-level, fixed-CPU network poll runtime.
//!
//! Every physical IRQ source is assigned to an affinity domain.  A domain's
//! hard callbacks and queue processing run on one owner CPU; only move-only DMA
//! tokens cross the SPSC boundary to the single protocol executor.

mod executor;
mod spsc;
mod state;
#[cfg(test)]
mod tests;

use alloc::{boxed::Box, collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use ax_sync::SpinLock;
use ax_task::WaitQueue;
use irq_framework::IrqId;
use rd_net::{
    NetError, NetHardIrqEndpoint, NetHardIrqResult, NetIrqSourceId, PreparedNetDevice,
    WifiLinkPolicy, WifiTransaction,
};

pub use self::state::NetQueueStats;
use self::{executor::*, spsc::*, state::PollGroupState};
use crate::device::{EthernetFramePort, EthernetFramePortList};

const QUEUE_BUDGET: usize = 64;
const CPU_ROUND_BUDGET: usize = 256;
const WIFI_CONTROL_QUEUE_CAPACITY: usize = 8;

const STATE_IDLE: u8 = 0;
const STATE_SCHEDULED: u8 = 1;
const STATE_POLLING: u8 = 2;
const STATE_DISABLED: u8 = 3;
const STATE_MASK: u8 = 0x0f;
const STATE_MISSED: u8 = 0x80;

const COMMAND_WAIT: u8 = 0;
const COMMAND_START: u8 = 1;
const COMMAND_STOP: u8 = 2;
const COMMAND_QUARANTINE: u8 = 3;

const STATUS_PENDING: u8 = 0;
const STATUS_READY: u8 = 1;
const STATUS_FAILED: u8 = 2;

struct WifiCommandCompletion {
    result: SpinLock<Option<Result<(), NetError>>>,
    wait: WaitQueue,
}

impl WifiCommandCompletion {
    fn new() -> Self {
        Self {
            result: SpinLock::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: Result<(), NetError>) {
        *self.result.lock_irqsave() = Some(result);
        self.wait.notify_all(true);
    }

    fn wait(&self) -> Result<(), NetError> {
        self.wait
            .wait_until(|| self.result.lock_irqsave().is_some());
        self.result
            .lock_irqsave()
            .take()
            .expect("Wi-Fi completion was published without a result")
    }
}

struct WifiControlRequest {
    transaction: WifiTransaction,
    completion: Arc<WifiCommandCompletion>,
}

struct WifiControlQueue {
    requests: SpinLock<VecDeque<WifiControlRequest>>,
    stopped: AtomicBool,
}

impl WifiControlQueue {
    fn new() -> Self {
        Self {
            requests: SpinLock::new(VecDeque::with_capacity(WIFI_CONTROL_QUEUE_CAPACITY)),
            stopped: AtomicBool::new(false),
        }
    }

    fn submit(
        &self,
        transaction: WifiTransaction,
        notify: &ax_task::IrqNotify,
    ) -> Result<(), NetError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(NetError::Stopped);
        }
        let completion = Arc::new(WifiCommandCompletion::new());
        {
            let mut requests = self.requests.lock_irqsave();
            if self.stopped.load(Ordering::Acquire) {
                return Err(NetError::Stopped);
            }
            if requests.len() == WIFI_CONTROL_QUEUE_CAPACITY {
                return Err(NetError::Retry);
            }
            requests.push_back(WifiControlRequest {
                transaction,
                completion: Arc::clone(&completion),
            });
        }
        notify.notify();
        completion.wait()
    }

    fn try_pop(&self) -> Option<WifiControlRequest> {
        self.requests.lock_irqsave().pop_front()
    }

    fn has_pending(&self) -> bool {
        !self.requests.lock_irqsave().is_empty()
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let pending = core::mem::take(&mut *self.requests.lock_irqsave());
        for request in pending {
            request.completion.complete(Err(NetError::Stopped));
        }
    }
}

#[derive(Clone)]
pub(crate) struct WifiRuntimeHandle {
    device_index: usize,
    owner_cpu: usize,
    queue: Arc<WifiControlQueue>,
    notify: Arc<ax_task::IrqNotify>,
}

impl WifiRuntimeHandle {
    pub(crate) const fn device_index(&self) -> usize {
        self.device_index
    }

    pub(crate) const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    pub(crate) fn submit(&self, transaction: WifiTransaction) -> Result<(), NetError> {
        self.queue.submit(transaction, &self.notify)
    }
}

/// Runtime initialization or lifecycle error.
#[derive(Debug, thiserror::Error)]
pub enum NetworkRuntimeError {
    #[error("network device parts are inconsistent with their IRQ bindings")]
    InvalidTopology,
    #[error("network queue executor could not be pinned to CPU {0}")]
    WorkerAffinity(usize),
    #[error("network queue initialization failed: {0}")]
    QueueInit(NetError),
    #[error("network IRQ registration failed: {0}")]
    IrqRegistration(#[from] PinnedNetIrqError),
    #[error("network DMA setup failed: {0}")]
    Device(#[from] NetError),
    #[error("secure Wi-Fi startup entropy failed: {0}")]
    StartupEntropy(#[from] crate::NetError),
}

/// Resolved driver source-id to physical IRQ mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedNetIrqSource {
    pub source_id: NetIrqSourceId,
    pub irq: IrqId,
}

/// One prepared device and its complete, resolved IRQ source map.
pub struct NetworkDeviceInput {
    pub name: String,
    pub device: PreparedNetDevice,
    pub irq_sources: Vec<ResolvedNetIrqSource>,
    pub tx_queue_discipline: TxQueueDiscipline,
}

/// Protocol-side transmit queue policy for one network device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxQueueDiscipline {
    /// Submit directly to the device and return `Again` when it is busy.
    NoQueue,
    /// Retain frames in submission order while the device is busy.
    Fifo { max_frames: NonZeroUsize },
}

/// Result of a bounded hard-IRQ callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinnedNetIrqOutcome {
    Unhandled,
    Handled,
    Wake,
}

/// Move-only callback installed by the OS IRQ adapter.
pub struct PinnedNetIrqAction {
    handler: Box<dyn FnMut() -> PinnedNetIrqOutcome + Send>,
}

impl PinnedNetIrqAction {
    pub fn new(handler: impl FnMut() -> PinnedNetIrqOutcome + Send + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    pub fn run(&mut self) -> PinnedNetIrqOutcome {
        (self.handler)()
    }
}

/// OS-specific fixed-affinity IRQ registration error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PinnedNetIrqError {
    #[error("invalid network IRQ or owner CPU")]
    Invalid,
    #[error("network IRQ affinity conflicts with an existing shared action")]
    AffinityConflict,
    #[error("fixed network IRQ routing is unsupported")]
    Unsupported,
    #[error("network IRQ operation failed")]
    Other,
}

/// Move-only registration lease.  It is created disabled.
pub trait PinnedNetIrqRegistration: Send + 'static {
    fn owner_cpu(&self) -> usize;
    fn enable(&self) -> Result<(), PinnedNetIrqError>;
    fn disable_and_synchronize(&self) -> Result<(), PinnedNetIrqError>;
}

/// OS adapter that accepts fixed affinity only.
pub trait PinnedNetIrqRegistrar: Sync {
    fn register(
        &self,
        name: String,
        irq: IrqId,
        owner_cpu: usize,
        action: PinnedNetIrqAction,
    ) -> Result<Box<dyn PinnedNetIrqRegistration>, PinnedNetIrqError>;
}

struct EndpointToRegister {
    name: String,
    irq: IrqId,
    owner_cpu: usize,
    endpoint: NetHardIrqEndpoint,
    shared: Arc<PollGroupState>,
}

/// Live queue runtime.  Dropping it masks IRQs before stopping executors.
pub struct NetworkQueueRuntime {
    registrations: Vec<Box<dyn PinnedNetIrqRegistration>>,
    executors: Vec<ExecutorLease>,
    group_states: Vec<Arc<PollGroupState>>,
    _controls: Vec<Box<dyn rd_net::NetControlEndpoint>>,
    wifi_handles: Vec<WifiRuntimeHandle>,
    initial_wifi_policies: Vec<(usize, WifiLinkPolicy)>,
    protocol_owner_cpu: usize,
}

impl NetworkQueueRuntime {
    pub fn protocol_owner_cpu(&self) -> usize {
        self.protocol_owner_cpu
    }

    pub fn stats(&self) -> Vec<NetQueueStats> {
        self.group_states
            .iter()
            .map(|state| state.stats.snapshot(state.owner_cpu))
            .collect()
    }

    pub(crate) fn wifi_handle(&self, device_index: usize) -> Option<WifiRuntimeHandle> {
        self.wifi_handles
            .iter()
            .find(|handle| handle.device_index() == device_index)
            .cloned()
    }

    pub(crate) fn initial_wifi_policy(&self, device_index: usize) -> Option<WifiLinkPolicy> {
        self.initial_wifi_policies
            .iter()
            .find_map(|(index, policy)| (*index == device_index).then_some(*policy))
    }
}

impl Drop for NetworkQueueRuntime {
    fn drop(&mut self) {
        for handle in self.wifi_handles.iter().rev() {
            handle.queue.stop();
            handle.notify.notify();
        }
        let registrations = core::mem::take(&mut self.registrations);
        let irq_synchronized = release_registrations(registrations);
        let runtime_side_resources = (
            core::mem::take(&mut self.group_states),
            core::mem::take(&mut self._controls),
            core::mem::take(&mut self.wifi_handles),
        );
        stop_executors(&self.executors, irq_synchronized);
        release_runtime_side_resources(runtime_side_resources, irq_synchronized);
    }
}

/// Builder for an all-at-once network queue runtime.
pub struct NetworkRuntimeBuilder<'a> {
    devices: Vec<NetworkDeviceInput>,
    registrar: &'a dyn PinnedNetIrqRegistrar,
    online_cpus: usize,
}

impl<'a> NetworkRuntimeBuilder<'a> {
    pub fn new(
        devices: Vec<NetworkDeviceInput>,
        registrar: &'a dyn PinnedNetIrqRegistrar,
        online_cpus: usize,
    ) -> Self {
        Self {
            devices,
            registrar,
            online_cpus,
        }
    }

    pub fn build(
        self,
    ) -> Result<(NetworkQueueRuntime, EthernetFramePortList), NetworkRuntimeError> {
        if self.online_cpus == 0 {
            // No owner context exists in which a driver can prove DMA has
            // stopped, so prepared device backing must not be dropped.
            release_or_quarantine(self.devices, false);
            return Err(NetworkRuntimeError::InvalidTopology);
        }

        let group_irq_sets = match validate_and_collect_irq_sets(&self.devices) {
            Ok(sets) => sets,
            Err(error) => {
                // The portable parts may already contain hardware-visible
                // descriptor rings, but no owner CPU exists for lifecycle
                // control yet. Preserve them as an early-init quarantine.
                core::mem::forget(self.devices);
                return Err(error);
            }
        };
        let group_owners = assign_affinity_domains(&group_irq_sets, self.online_cpus);
        let mut groups_by_cpu = (0..self.online_cpus)
            .map(|_| Vec::new())
            .collect::<Vec<Vec<QueueGroupExecutor>>>();
        let mut wifi_by_cpu = (0..self.online_cpus)
            .map(|_| Vec::new())
            .collect::<Vec<Vec<WifiExecutorSlot>>>();
        let cpu_notifies = (0..self.online_cpus)
            .map(|_| Arc::new(ax_task::IrqNotify::new()))
            .collect::<Vec<_>>();
        let mut endpoints = Vec::new();
        let mut ports = Vec::with_capacity(self.devices.len());
        let mut controls = Vec::new();
        let mut port_macs = Vec::new();
        let mut wifi_handles = Vec::new();
        let mut startup_transactions = Vec::new();
        let mut group_states = Vec::new();
        let mut flat_group = 0;

        for (device_index, input) in self.devices.into_iter().enumerate() {
            let port_name = input.name.clone();
            let PreparedNetDevice {
                info,
                control,
                wifi_control,
                poll_groups,
            } = input.device;
            let mut protocol_groups = Vec::with_capacity(poll_groups.len());
            let mut checksum_capabilities = None;
            let mut wifi_target = None;
            for mut group in poll_groups {
                checksum_capabilities = Some(checksum_capabilities.map_or(
                    group.tx.checksum_capabilities(),
                    |current: rd_net::TxChecksumCapabilities| {
                        current.intersection(group.tx.checksum_capabilities())
                    },
                ));
                let owner_cpu = group_owners[flat_group];
                let owner_group_index = groups_by_cpu[owner_cpu].len();
                let shared = Arc::new(PollGroupState::new(
                    owner_cpu,
                    Arc::clone(&cpu_notifies[owner_cpu]),
                ));
                let rx_capacity = group.rx.capacity();
                let (rx_ready_tx, rx_ready_rx) = spsc_ring(rx_capacity);
                let (rx_recycle_tx, rx_recycle_rx) = spsc_ring(rx_capacity);
                let (tx_ready_tx, tx_ready_rx) = spsc_ring(group.tx.capacity());
                let (tx_free_tx, tx_free_rx) = spsc_ring(group.tx.capacity());
                let rx_recycler = Arc::new(RxRecycler::new(
                    rx_recycle_tx,
                    Arc::clone(&shared),
                    rx_capacity,
                ));

                for endpoint in group.irq_endpoints.drain(..) {
                    let irq = resolve_endpoint_irq(&input.irq_sources, endpoint.source_id())
                        .expect("network IRQ topology was validated before ownership transfer");
                    endpoints.push(EndpointToRegister {
                        name: format!(
                            "{}-g{}-s{}",
                            input.name,
                            group.id.get(),
                            endpoint.source_id().get()
                        ),
                        irq,
                        owner_cpu,
                        endpoint,
                        shared: Arc::clone(&shared),
                    });
                }

                protocol_groups.push(ProtocolGroupPort {
                    rx_ready: rx_ready_rx,
                    rx_recycler: Arc::clone(&rx_recycler),
                    tx_ready: tx_ready_tx,
                    tx_free: tx_free_rx,
                    tx_spares: Vec::with_capacity(group.tx.capacity()),
                    shared: Arc::clone(&shared),
                });
                groups_by_cpu[owner_cpu].push(QueueGroupExecutor {
                    group,
                    rx_ready: rx_ready_tx,
                    rx_recycle: rx_recycle_rx,
                    rx_recycler,
                    rx_spares: Vec::with_capacity(rx_capacity.max(QUEUE_BUDGET)),
                    tx_ready: tx_ready_rx,
                    tx_free: tx_free_tx,
                    pending_rx: None,
                    pending_rx_refill: VecDeque::with_capacity(rx_capacity),
                    pending_tx: None,
                    pending_tx_free: None,
                    retry_at: None,
                    shared: Arc::clone(&shared),
                });
                wifi_target.get_or_insert((owner_cpu, owner_group_index));
                group_states.push(shared);
                flat_group += 1;
            }
            if let Some(wifi_control) = wifi_control {
                let (owner_cpu, group_index) =
                    wifi_target.ok_or(NetworkRuntimeError::InvalidTopology)?;
                let queue = Arc::new(WifiControlQueue::new());
                let handle = WifiRuntimeHandle {
                    device_index,
                    owner_cpu,
                    queue: Arc::clone(&queue),
                    notify: Arc::clone(&cpu_notifies[owner_cpu]),
                };
                if let Some(transaction) = wifi_control.startup_transaction() {
                    startup_transactions.push((handle.clone(), transaction));
                }
                wifi_by_cpu[owner_cpu].push(WifiExecutorSlot {
                    group_index,
                    control: wifi_control,
                    queue,
                    active: None,
                });
                wifi_handles.push(handle);
            }
            controls.push(control);
            let port_mac = Arc::new(SpinLock::new(info.mac_address));
            port_macs.push(Arc::clone(&port_mac));
            ports.push(Box::new(QueueFramePort {
                name: port_name,
                mac: port_mac,
                groups: protocol_groups,
                tx_queue_discipline: input.tx_queue_discipline,
                pending_tx: VecDeque::new(),
                next_rx: 0,
                next_tx: 0,
                checksum_capabilities: checksum_capabilities
                    .unwrap_or(rd_net::TxChecksumCapabilities::NONE),
            }) as Box<dyn EthernetFramePort>);
        }

        let mut executors = Vec::new();
        for (owner_cpu, (groups, wifi)) in groups_by_cpu.into_iter().zip(wifi_by_cpu).enumerate() {
            if groups.is_empty() {
                continue;
            }
            let control = Arc::new(ExecutorControl {
                owner_cpu,
                command: AtomicU8::new(COMMAND_WAIT),
                affinity_status: AtomicU8::new(STATUS_PENDING),
                startup_status: AtomicU8::new(STATUS_PENDING),
                startup_error: SpinLock::new(None),
                notify: Arc::clone(&cpu_notifies[owner_cpu]),
            });
            let task_control = Arc::clone(&control);
            let task = ax_task::spawn_with_name(
                move || queue_executor_main(groups, wifi, task_control),
                format!("net-queue-cpu{owner_cpu}"),
            );
            executors.push(ExecutorLease { control, task });
        }
        for executor in &executors {
            wait_status(&executor.control.affinity_status);
            if executor.control.affinity_status.load(Ordering::Acquire) != STATUS_READY {
                stop_executors(&executors, true);
                return Err(NetworkRuntimeError::WorkerAffinity(
                    executor.control.owner_cpu,
                ));
            }
        }

        let mut registrations = Vec::new();
        let mut endpoint_iter = endpoints.into_iter();
        while let Some(mut endpoint) = endpoint_iter.next() {
            let shared = Arc::clone(&endpoint.shared);
            let owner_cpu = endpoint.owner_cpu;
            let action = PinnedNetIrqAction::new(move || match endpoint.endpoint.handle_irq() {
                NetHardIrqResult::Spurious => {
                    shared.stats.spurious.fetch_add(1, Ordering::Relaxed);
                    PinnedNetIrqOutcome::Unhandled
                }
                NetHardIrqResult::Schedule(_snapshot) => {
                    shared.schedule_irq();
                    PinnedNetIrqOutcome::Wake
                }
                NetHardIrqResult::ProbeDeferred => {
                    shared.stats.probe_deferred.fetch_add(1, Ordering::Relaxed);
                    shared.schedule_irq();
                    PinnedNetIrqOutcome::Wake
                }
            });
            let registration =
                match self
                    .registrar
                    .register(endpoint.name, endpoint.irq, owner_cpu, action)
                {
                    Ok(registration) if registration.owner_cpu() == owner_cpu => registration,
                    Ok(registration) => {
                        registrations.push(registration);
                        let irq_synchronized = release_registrations(registrations);
                        stop_executors(&executors, irq_synchronized);
                        release_runtime_side_resources(
                            (
                                controls,
                                ports,
                                port_macs,
                                wifi_handles,
                                startup_transactions,
                                group_states,
                                cpu_notifies,
                                endpoint_iter,
                            ),
                            irq_synchronized,
                        );
                        return Err(NetworkRuntimeError::InvalidTopology);
                    }
                    Err(error) => {
                        let irq_synchronized = release_registrations(registrations);
                        stop_executors(&executors, irq_synchronized);
                        release_runtime_side_resources(
                            (
                                controls,
                                ports,
                                port_macs,
                                wifi_handles,
                                startup_transactions,
                                group_states,
                                cpu_notifies,
                                endpoint_iter,
                            ),
                            irq_synchronized,
                        );
                        return Err(error.into());
                    }
                };
            registrations.push(registration);
        }
        drop(endpoint_iter);

        for registration in &registrations {
            if let Err(error) = registration.enable() {
                let irq_synchronized = release_registrations(registrations);
                stop_executors(&executors, irq_synchronized);
                release_runtime_side_resources(
                    (
                        controls,
                        ports,
                        port_macs,
                        wifi_handles,
                        startup_transactions,
                        group_states,
                        cpu_notifies,
                    ),
                    irq_synchronized,
                );
                return Err(error.into());
            }
        }

        for executor in &executors {
            executor
                .control
                .command
                .store(COMMAND_START, Ordering::Release);
            executor.control.notify.notify();
        }
        for executor in &executors {
            wait_status(&executor.control.startup_status);
            if executor.control.startup_status.load(Ordering::Acquire) != STATUS_READY {
                let error = executor
                    .control
                    .startup_error
                    .lock_irqsave()
                    .take()
                    .unwrap_or(NetError::InvalidParts);
                let irq_synchronized = release_registrations(registrations);
                stop_executors(&executors, irq_synchronized);
                release_runtime_side_resources(
                    (
                        controls,
                        ports,
                        port_macs,
                        wifi_handles,
                        startup_transactions,
                        group_states,
                        cpu_notifies,
                    ),
                    irq_synchronized,
                );
                return Err(NetworkRuntimeError::QueueInit(error));
            }
        }
        let protocol_owner_cpu = select_protocol_owner(&group_owners, self.online_cpus);
        let mut runtime = NetworkQueueRuntime {
            registrations,
            executors,
            group_states,
            _controls: controls,
            wifi_handles,
            initial_wifi_policies: Vec::new(),
            protocol_owner_cpu,
        };
        for (handle, transaction) in startup_transactions {
            let transaction =
                prepare_startup_transaction(transaction, super::next_wifi_connection_entropy)?;
            let policy = transaction.link_policy();
            handle.submit(transaction)?;
            if let Some(policy) = policy {
                runtime
                    .initial_wifi_policies
                    .push((handle.device_index(), policy));
            }
        }
        for (control, mac) in runtime._controls.iter_mut().zip(port_macs) {
            let address = control.mac_address()?;
            *mac.lock_irqsave() = address;
        }
        Ok((runtime, ports))
    }
}

fn prepare_startup_transaction(
    mut transaction: WifiTransaction,
    next_entropy: impl FnOnce() -> Result<[u8; 32], crate::NetError>,
) -> Result<WifiTransaction, crate::NetError> {
    if transaction.needs_connect_entropy() {
        transaction.provide_connect_entropy(next_entropy()?);
        log::info!("[wifi] secure startup connection entropy prepared");
    }
    Ok(transaction)
}

fn validate_and_collect_irq_sets(
    devices: &[NetworkDeviceInput],
) -> Result<Vec<Vec<IrqId>>, NetworkRuntimeError> {
    let mut sets = Vec::new();
    for input in devices {
        if input.device.poll_groups.is_empty() || input.irq_sources.is_empty() {
            return Err(NetworkRuntimeError::InvalidTopology);
        }
        for group in &input.device.poll_groups {
            if group.irq_endpoints.is_empty() {
                return Err(NetworkRuntimeError::InvalidTopology);
            }
            let mut irqs = Vec::new();
            for endpoint in &group.irq_endpoints {
                let irq = resolve_endpoint_irq(&input.irq_sources, endpoint.source_id())?;
                if !irqs.contains(&irq) {
                    irqs.push(irq);
                }
            }
            sets.push(irqs);
        }
        for source in &input.irq_sources {
            let used = input.device.poll_groups.iter().any(|group| {
                group
                    .irq_endpoints
                    .iter()
                    .any(|endpoint| endpoint.source_id() == source.source_id)
            });
            if !used {
                return Err(NetworkRuntimeError::InvalidTopology);
            }
        }
    }
    Ok(sets)
}

fn resolve_endpoint_irq(
    sources: &[ResolvedNetIrqSource],
    source_id: NetIrqSourceId,
) -> Result<IrqId, NetworkRuntimeError> {
    let mut matches = sources
        .iter()
        .filter(|source| source.source_id == source_id)
        .map(|source| source.irq);
    let irq = matches.next().ok_or(NetworkRuntimeError::InvalidTopology)?;
    if matches.next().is_some() {
        return Err(NetworkRuntimeError::InvalidTopology);
    }
    Ok(irq)
}

fn assign_affinity_domains(irq_sets: &[Vec<IrqId>], cpu_count: usize) -> Vec<usize> {
    let mut parents = (0..irq_sets.len()).collect::<Vec<_>>();
    for left in 0..irq_sets.len() {
        for right in (left + 1)..irq_sets.len() {
            if irq_sets[left]
                .iter()
                .any(|irq| irq_sets[right].contains(irq))
            {
                union(&mut parents, left, right);
            }
        }
    }
    let mut roots = Vec::new();
    let mut owners = Vec::with_capacity(irq_sets.len());
    for index in 0..irq_sets.len() {
        let root = find(&mut parents, index);
        let domain_index = match roots.iter().position(|candidate| *candidate == root) {
            Some(index) => index,
            None => {
                roots.push(root);
                roots.len() - 1
            }
        };
        owners.push(domain_index % cpu_count);
    }
    owners
}

fn find(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        let grandparent = parents[parents[index]];
        parents[index] = grandparent;
        index = grandparent;
    }
    index
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left_root = find(parents, left);
    let right_root = find(parents, right);
    if left_root != right_root {
        let (first, second) = if left_root < right_root {
            (left_root, right_root)
        } else {
            (right_root, left_root)
        };
        parents[second] = first;
    }
}

fn select_protocol_owner(group_owners: &[usize], cpu_count: usize) -> usize {
    let mut load = vec![0usize; cpu_count];
    for &owner in group_owners {
        load[owner] += 1;
    }
    load.iter()
        .enumerate()
        .min_by_key(|(cpu, groups)| (**groups, *cpu))
        .map(|(cpu, _)| cpu)
        .unwrap_or(0)
}

fn wait_status(status: &AtomicU8) {
    while status.load(Ordering::Acquire) == STATUS_PENDING {
        ax_task::yield_now();
    }
}

fn disable_registrations(registrations: &[Box<dyn PinnedNetIrqRegistration>]) -> bool {
    let mut synchronized = true;
    for registration in registrations.iter().rev() {
        if registration.disable_and_synchronize().is_err() {
            synchronized = false;
        }
    }
    synchronized
}

fn release_registrations(registrations: Vec<Box<dyn PinnedNetIrqRegistration>>) -> bool {
    let synchronized = disable_registrations(&registrations);
    if synchronized {
        drop(registrations);
    } else {
        log::warn!(
            "quarantining {} network IRQ registrations because callback synchronization failed",
            registrations.len()
        );
        core::mem::forget(registrations);
    }
    synchronized
}

fn release_runtime_side_resources<T>(resource: T, irq_synchronized: bool) {
    release_or_quarantine(resource, irq_synchronized);
}

fn stop_executors(executors: &[ExecutorLease], irq_synchronized: bool) {
    for executor in executors.iter().rev() {
        executor.stop(irq_synchronized);
    }
    for executor in executors.iter().rev() {
        executor.task.join();
    }
}

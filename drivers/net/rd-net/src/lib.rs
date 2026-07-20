#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    vec::Vec,
};
use core::{
    alloc::Layout,
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use dma_api::{ContiguousBuffer, ContiguousBufferPool, DeviceDma, DmaDirection, DmaOp};
pub use rdif_eth::{IrqEndpoint as InterfaceIrqEndpoint, *};

mod legacy;

use legacy::LegacyNetDevice;

fn other_error(msg: &'static str) -> NetError {
    NetError::Other(Box::new(KError::Unknown(msg)))
}

pub struct Net {
    owner: Box<dyn NetDeviceOwner>,
    dma_op: &'static dyn DmaOp,
}

impl DriverGeneric for Net {
    fn name(&self) -> &str {
        self.owner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Net {
    /// Wraps a legacy split-queue interface in one aggregate owner.
    ///
    /// New stateful drivers should implement [`NetDeviceOwner`] and use
    /// [`Self::new_owned`] so controller and queue state never need shared
    /// locks.
    pub fn new(interface: impl Interface, dma_op: &'static dyn DmaOp) -> Self {
        Self {
            owner: Box::new(LegacyNetDevice::new(Box::new(interface))),
            dma_op,
        }
    }

    /// Creates a runtime wrapper around one move-only device owner.
    pub fn new_owned(owner: impl NetDeviceOwner, dma_op: &'static dyn DmaOp) -> Self {
        Self {
            owner: Box::new(owner),
            dma_op,
        }
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.owner.mac_address()
    }

    /// Advances the interface's bounded owner-side initialization state.
    pub fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        self.owner.poll_owner_init(input)
    }

    /// Returns immutable link policy established by owner initialization.
    pub fn owner_link_policy(&self) -> Option<WifiLinkPolicy> {
        self.owner.owner_link_policy()
    }

    /// Reports whether the portable interface accepts wireless owner commands.
    pub fn supports_wifi_control(&self) -> bool {
        self.owner.supports_wifi_control()
    }

    /// Transfers one owned wireless command to the portable owner state.
    pub fn start_wifi_command(
        &mut self,
        command: WifiCommand,
        now_ns: u64,
    ) -> Result<WifiCommandProgress, WifiCommandStartError> {
        self.owner.start_wifi_command(command, now_ns)
    }

    /// Advances the accepted wireless command after its declared activation.
    pub fn poll_wifi_command(&mut self, now_ns: u64) -> WifiCommandProgress {
        self.owner.poll_wifi_command(now_ns)
    }

    pub fn enable_irq(&mut self) -> Result<(), NetError> {
        self.owner.enable_irq()
    }

    pub fn disable_irq(&mut self) -> Result<(), NetError> {
        self.owner.disable_irq()
    }

    pub fn is_irq_enabled(&self) -> bool {
        self.owner.is_irq_enabled()
    }

    /// Consumes the disabled controller and constructs both DMA queues as one
    /// activation transaction.
    ///
    /// A failed transaction retains the controller and every partially built
    /// queue in [`QueueActivationFailure`]. The OS must either prove hardware
    /// quiescence before releasing that value or move it into a named
    /// quarantine; dropping individual partial queues can free DMA memory that
    /// the device still owns.
    pub fn activate_queues(self) -> Result<ActiveNetQueues, QueueActivationFailure> {
        // Allocate the failure-retention container before queue construction
        // can publish DMA ownership to hardware. No error path below needs to
        // allocate in order to preserve a partially activated device.
        let mut owner = Box::new(NetQueueOwner::new(self));
        if owner.net.owner.is_irq_enabled() {
            return Err(QueueActivationFailure::new(
                QueueActivationError::InterruptsEnabled,
                None,
                owner,
            ));
        }

        let queue_set = match owner.net.owner.activate_queue_set() {
            Ok(queue_set) => queue_set,
            Err(error) => {
                return Err(QueueActivationFailure::new(
                    QueueActivationError::DeviceActivation,
                    Some(error),
                    owner,
                ));
            }
        };
        (owner.resources.tx_raw, owner.resources.rx_raw) = queue_set.into_parts();

        if let Err((reason, error)) = owner.build_runtime_queues() {
            return Err(QueueActivationFailure::new(reason, Some(error), owner));
        }

        Ok(ActiveNetQueues { owner: Some(owner) })
    }

    /// Moves the destructive IRQ endpoint out of the control interface.
    pub fn take_irq_endpoint(&mut self) -> Option<IrqEndpoint> {
        self.owner
            .take_irq_endpoint()
            .map(|endpoint| IrqEndpoint { endpoint })
    }

    /// Services one already-acknowledged event on the queue owner thread.
    pub fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        self.owner.service_irq_event(event)
    }

    /// Rearms a generation-checked source after owner-thread service.
    pub fn rearm_irq_source(&mut self, source: MaskedSource) -> Result<(), NetError> {
        self.owner.rearm_irq_source(source)
    }
}

/// Stage at which transactional queue activation stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueueActivationError {
    /// Device sources were not masked before queue construction.
    #[error("network queue activation requires device IRQ sources to be disabled")]
    InterruptsEnabled,
    /// The aggregate device owner rejected queue publication.
    #[error("network device owner could not publish its queue set")]
    DeviceActivation,
    /// The driver could not create its TX queue.
    #[error("network driver did not provide a TX queue")]
    TxUnavailable,
    /// The TX queue advertised an invalid DMA configuration.
    #[error("network TX queue configuration is invalid")]
    TxConfiguration,
    /// The driver could not create its RX queue.
    #[error("network driver did not provide an RX queue")]
    RxUnavailable,
    /// The RX queue advertised an invalid DMA configuration.
    #[error("network RX queue configuration is invalid")]
    RxConfiguration,
    /// RX buffer admission failed after the device may have observed DMA.
    #[error("network RX queue prefill failed after partial activation")]
    RxPrefill,
    /// The current runtime facade cannot publish this queue topology as one
    /// blocking network device.
    #[error("network runtime does not support the activated queue topology")]
    UnsupportedTopology,
}

/// Fully activated controller and all of its owner-thread queues.
///
/// Controller and queue operations remain methods on this one linear value.
/// No API returns independently callable driver queue objects, so a stateful
/// device never needs `Arc<Mutex<_>>` to reunite its own split parts.
#[must_use = "service, explicitly quarantine, or retain the active DMA owner"]
pub struct ActiveNetQueues {
    owner: Option<Box<NetQueueOwner>>,
}

impl ActiveNetQueues {
    /// Returns the device name retained by the aggregate owner.
    pub fn name(&self) -> &str {
        self.owner_ref().net.name()
    }

    /// Returns the device MAC address.
    pub fn mac_address(&self) -> [u8; 6] {
        self.owner_ref().net.mac_address()
    }

    /// Returns immutable link policy established during owner initialization.
    pub fn owner_link_policy(&self) -> Option<WifiLinkPolicy> {
        self.owner_ref().net.owner_link_policy()
    }

    /// Reports whether wireless owner commands are supported.
    pub fn supports_wifi_control(&self) -> bool {
        self.owner_ref().net.supports_wifi_control()
    }

    /// Transfers one wireless command into the aggregate owner.
    pub fn start_wifi_command(
        &mut self,
        command: WifiCommand,
        now_ns: u64,
    ) -> Result<WifiCommandProgress, WifiCommandStartError> {
        self.owner_mut().net.start_wifi_command(command, now_ns)
    }

    /// Advances the accepted wireless command once.
    pub fn poll_wifi_command(&mut self, now_ns: u64) -> WifiCommandProgress {
        self.owner_mut().net.poll_wifi_command(now_ns)
    }

    /// Enables the exact device interrupt sources.
    pub fn enable_irq(&mut self) -> Result<(), NetError> {
        self.owner_mut().net.enable_irq()
    }

    /// Masks the exact device interrupt sources.
    pub fn disable_irq(&mut self) -> Result<(), NetError> {
        self.owner_mut().net.disable_irq()
    }

    /// Services one stable event on the maintenance owner.
    pub fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        self.owner_mut().net.service_irq_event(event)
    }

    /// Rearms one generation-checked source after all obligations drain.
    pub fn rearm_irq_source(&mut self, source: MaskedSource) -> Result<(), NetError> {
        self.owner_mut().net.rearm_irq_source(source)
    }

    /// Returns the number of transmit queue authorities.
    pub fn tx_queue_count(&self) -> usize {
        self.owner_ref().resources.tx_ready.len()
    }

    /// Returns the number of receive queue authorities.
    pub fn rx_queue_count(&self) -> usize {
        self.owner_ref().resources.rx_ready.len()
    }

    /// Returns the device-local transmit queue identifier.
    pub fn tx_queue_id(&self, index: usize) -> Option<usize> {
        self.owner_ref()
            .resources
            .tx_ready
            .get(index)
            .map(TxQueue::id)
    }

    /// Returns the device-local receive queue identifier.
    pub fn rx_queue_id(&self, index: usize) -> Option<usize> {
        self.owner_ref()
            .resources
            .rx_ready
            .get(index)
            .map(RxQueue::id)
    }

    /// Returns the maximum packet size of one transmit queue.
    pub fn tx_buf_size(&self, index: usize) -> Option<usize> {
        self.owner_ref()
            .resources
            .tx_ready
            .get(index)
            .map(TxQueue::buf_size)
    }

    /// Reports whether one transmit queue still owns hardware buffers.
    pub fn tx_has_inflight(&self, index: usize) -> bool {
        self.owner_ref()
            .resources
            .tx_ready
            .get(index)
            .is_some_and(TxQueue::has_inflight)
    }

    /// Reclaims at most `limit` completions from one transmit queue.
    pub fn reclaim_tx(&mut self, index: usize, limit: usize) -> Result<usize, NetError> {
        let (net, queue) = tx_parts(self.owner_mut(), index)?;
        queue.reclaim_bounded(net, limit)
    }

    /// Allocates and fills one transmit buffer owned by this aggregate.
    pub fn prepare_send<R>(
        &mut self,
        index: usize,
        len: usize,
        fill: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<(R, TxPending<'_>), NetError> {
        let (ret, bus_addr, buffer) = self
            .owner_mut()
            .resources
            .tx_ready
            .get_mut(index)
            .ok_or_else(|| other_error("unknown tx queue index"))?
            .prepare_buffer(len, fill)?;
        Ok((
            ret,
            TxPending {
                queues: self,
                queue_index: index,
                len,
                bus_addr,
                buff: Some(buffer),
            },
        ))
    }

    /// Receives one packet from a selected queue and returns copied output.
    pub fn receive<R>(
        &mut self,
        index: usize,
        consume: impl FnOnce(&[u8]) -> R,
    ) -> Result<Option<R>, NetError> {
        let Some(packet) = self.try_receive(index)? else {
            return Ok(None);
        };
        Ok(Some(packet.consume(consume)))
    }

    /// Borrows one received packet until it is consumed or recycled by Drop.
    pub fn try_receive(&mut self, index: usize) -> Result<Option<RxPacket<'_>>, NetError> {
        let packet = {
            let (net, queue) = rx_parts(self.owner_mut(), index)?;
            queue.retry_staged_returns(net)?;
            queue.reclaim_packet(net)?
        };
        Ok(packet.map(|(buff, len)| RxPacket {
            queues: self,
            queue_index: index,
            len,
            buff: Some(buff),
        }))
    }

    /// Converts this live DMA owner into a named quarantine value.
    pub fn into_quarantine(mut self) -> QuarantinedNetQueues {
        QuarantinedNetQueues::new(
            self.owner
                .take()
                .expect("active queue owner missing before quarantine"),
            NetQueueQuarantineReason::ActiveOwnerDropped,
        )
    }

    fn owner_ref(&self) -> &NetQueueOwner {
        self.owner
            .as_deref()
            .expect("active queue owner missing before quarantine")
    }

    fn owner_mut(&mut self) -> &mut NetQueueOwner {
        self.owner
            .as_deref_mut()
            .expect("active queue owner missing before quarantine")
    }

    fn recycle_rx(&mut self, index: usize, buffer: ContiguousBuffer) {
        let Ok((net, queue)) = rx_parts(self.owner_mut(), index) else {
            return;
        };
        queue.recycle_or_stage(net, buffer);
    }
}

fn tx_parts(owner: &mut NetQueueOwner, index: usize) -> Result<(&mut Net, &mut TxQueue), NetError> {
    let queue = owner
        .resources
        .tx_ready
        .get_mut(index)
        .ok_or_else(|| other_error("unknown tx queue index"))?;
    Ok((&mut owner.net, queue))
}

fn rx_parts(owner: &mut NetQueueOwner, index: usize) -> Result<(&mut Net, &mut RxQueue), NetError> {
    let queue = owner
        .resources
        .rx_ready
        .get_mut(index)
        .ok_or_else(|| other_error("unknown rx queue index"))?;
    Ok((&mut owner.net, queue))
}

impl Drop for ActiveNetQueues {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            quarantine_net_queues(owner, NetQueueQuarantineReason::ActiveOwnerDropped);
        }
    }
}

#[derive(Default)]
struct QueueActivationResources {
    tx_raw: Vec<TxQueueOwner>,
    tx_ready: Vec<TxQueue>,
    rx_raw: Vec<RxQueueOwner>,
    rx_ready: Vec<RxQueue>,
}

struct NetQueueOwner {
    net: Net,
    resources: QueueActivationResources,
    quarantine_next: AtomicPtr<Self>,
    quarantine_reason: NetQueueQuarantineReason,
}

impl NetQueueOwner {
    fn new(net: Net) -> Self {
        Self {
            net,
            resources: QueueActivationResources::default(),
            quarantine_next: AtomicPtr::new(ptr::null_mut()),
            quarantine_reason: NetQueueQuarantineReason::ActiveOwnerDropped,
        }
    }

    fn build_runtime_queues(&mut self) -> Result<(), (QueueActivationError, NetError)> {
        self.build_tx_queues()?;
        self.build_rx_queues()?;
        self.prefill_rx_queues()
    }

    fn build_tx_queues(&mut self) -> Result<(), (QueueActivationError, NetError)> {
        while let Some(token) = self.resources.tx_raw.first() {
            let config = token.config();
            let pool = make_pool(self.net.dma_op, config, DmaDirection::ToDevice)
                .map_err(|error| (QueueActivationError::TxConfiguration, error))?;
            let token = self.resources.tx_raw.remove(0);
            self.resources.tx_ready.push(TxQueue {
                token,
                pool,
                inflight: BTreeMap::new(),
                config,
            });
        }
        Ok(())
    }

    fn build_rx_queues(&mut self) -> Result<(), (QueueActivationError, NetError)> {
        while let Some(token) = self.resources.rx_raw.first() {
            let config = token.config();
            let pool = make_pool(self.net.dma_op, config, DmaDirection::FromDevice)
                .map_err(|error| (QueueActivationError::RxConfiguration, error))?;
            let token = self.resources.rx_raw.remove(0);
            self.resources.rx_ready.push(RxQueue {
                token,
                pool,
                inflight: BTreeMap::new(),
                staged_returns: VecDeque::new(),
                return_fault: None,
                config,
            });
        }
        Ok(())
    }

    fn prefill_rx_queues(&mut self) -> Result<(), (QueueActivationError, NetError)> {
        let NetQueueOwner { net, resources, .. } = self;
        for queue in &mut resources.rx_ready {
            queue
                .prefill(net)
                .map_err(|error| (QueueActivationError::RxPrefill, error))?;
        }
        Ok(())
    }
}

/// Failed queue transaction that retains every potentially hardware-owned
/// resource.
#[must_use = "quiesce the device or retain this complete value in quarantine"]
pub struct QueueActivationFailure {
    reason: QueueActivationError,
    source: Option<NetError>,
    owner: Option<Box<NetQueueOwner>>,
}

impl QueueActivationFailure {
    fn new(
        reason: QueueActivationError,
        source: Option<NetError>,
        owner: Box<NetQueueOwner>,
    ) -> Self {
        Self {
            reason,
            source,
            owner: Some(owner),
        }
    }

    /// Returns the stable activation stage for OS diagnostics.
    pub const fn reason(&self) -> QueueActivationError {
        self.reason
    }

    /// Returns the lower-level failure retained with the quarantine owner.
    pub fn source_error(&self) -> Option<&NetError> {
        self.source.as_ref()
    }

    /// Converts the failed transaction into a named quarantine owner.
    pub fn into_quarantine(mut self) -> QuarantinedNetQueues {
        QuarantinedNetQueues::new(
            self.owner
                .take()
                .expect("failed activation owner missing before quarantine"),
            NetQueueQuarantineReason::Activation(self.reason),
        )
    }
}

impl Drop for QueueActivationFailure {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            quarantine_net_queues(owner, NetQueueQuarantineReason::Activation(self.reason));
        }
    }
}

/// Reason a complete network queue transaction entered quarantine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetQueueQuarantineReason {
    /// Queue activation failed after hardware ownership may have been
    /// published.
    Activation(QueueActivationError),
    /// A live queue owner was dropped without a DMA-quiescence proof.
    ActiveOwnerDropped,
}

/// Named owner for a controller and queues that may remain DMA-visible.
///
/// Dropping this value does not run driver or DMA destructors. It links the
/// already allocated owner into a process-lifetime registry, keeping the
/// ownership diagnosable without allocating in the failure path.
#[must_use = "retain this owner in the runtime quarantine or let Drop register it"]
pub struct QuarantinedNetQueues {
    owner: Option<Box<NetQueueOwner>>,
    reason: NetQueueQuarantineReason,
}

impl QuarantinedNetQueues {
    fn new(owner: Box<NetQueueOwner>, reason: NetQueueQuarantineReason) -> Self {
        Self {
            owner: Some(owner),
            reason,
        }
    }

    /// Returns why the queue transaction could not be destroyed safely.
    pub const fn reason(&self) -> NetQueueQuarantineReason {
        self.reason
    }
}

impl Drop for QuarantinedNetQueues {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            quarantine_net_queues(owner, self.reason);
        }
    }
}

static QUARANTINED_NET_QUEUES: AtomicPtr<NetQueueOwner> = AtomicPtr::new(ptr::null_mut());
static QUARANTINED_NET_QUEUE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Returns the number of complete network queue owners retained by the
/// process-lifetime fallback quarantine.
pub fn quarantined_net_queue_count() -> usize {
    QUARANTINED_NET_QUEUE_COUNT.load(Ordering::Acquire)
}

fn quarantine_net_queues(mut owner: Box<NetQueueOwner>, reason: NetQueueQuarantineReason) {
    owner.quarantine_reason = reason;
    let owner = Box::into_raw(owner);
    let mut head = QUARANTINED_NET_QUEUES.load(Ordering::Acquire);
    loop {
        // SAFETY: `owner` was produced by `Box::into_raw` above and remains
        // exclusively owned by this publication loop. Once linked, the
        // process-lifetime registry never removes or frees it.
        unsafe { (*owner).quarantine_next.store(head, Ordering::Relaxed) };
        match QUARANTINED_NET_QUEUES.compare_exchange_weak(
            head,
            owner,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                QUARANTINED_NET_QUEUE_COUNT.fetch_add(1, Ordering::AcqRel);
                return;
            }
            Err(observed) => head = observed,
        }
    }
}

fn make_pool(
    dma_op: &'static dyn DmaOp,
    config: QueueConfig,
    direction: DmaDirection,
) -> Result<ContiguousBufferPool, NetError> {
    let layout = Layout::from_size_align(config.buf_size, config.align.max(1))
        .map_err(|_| other_error("invalid queue layout"))?;
    let dma = DeviceDma::new_legacy(config.dma_mask, dma_op);
    Ok(dma.contiguous_buffer_pool(layout, direction, config.ring_size))
}

pub struct IrqEndpoint {
    endpoint: rdif_eth::BIrqEndpoint,
}

impl IrqEndpoint {
    /// Captures one stable event without touching queue state or task wakers.
    pub fn capture_irq(&mut self) -> IrqCapture<Event, EthernetIrqFault> {
        self.endpoint.capture()
    }

    /// Contains the exact device source after runtime publication fails.
    pub fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, EthernetIrqFault> {
        self.endpoint.contain(cause)
    }
}

pub struct TxQueue {
    token: TxQueueOwner,
    pool: ContiguousBufferPool,
    inflight: BTreeMap<u64, ContiguousBuffer>,
    config: QueueConfig,
}

impl TxQueue {
    fn capacity(&self) -> usize {
        self.config.ring_size.saturating_sub(1)
    }

    fn reclaim_bounded(&mut self, net: &mut Net, limit: usize) -> Result<usize, NetError> {
        let mut reclaimed = 0;
        while reclaimed < limit {
            let Some(bus_addr) = net.owner.reclaim_tx(&self.token)? else {
                break;
            };
            let Some(buff) = self.inflight.remove(&bus_addr) else {
                return Err(other_error("reclaimed unknown tx buffer"));
            };
            drop(buff);
            reclaimed += 1;
        }
        Ok(reclaimed)
    }

    pub fn id(&self) -> usize {
        self.token.id()
    }

    pub fn buf_size(&self) -> usize {
        self.config.buf_size
    }

    /// Reports whether a hardware-owned TX buffer can make a future IRQ
    /// release a staged submission.
    pub fn has_inflight(&self) -> bool {
        !self.inflight.is_empty()
    }

    fn prepare_buffer<R>(
        &mut self,
        len: usize,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<(R, u64, ContiguousBuffer), NetError> {
        if len > self.config.buf_size {
            return Err(other_error("tx packet too large"));
        }
        if self.inflight.len() >= self.capacity() {
            return Err(NetError::Retry);
        }

        let mut buff = self.pool.alloc()?;
        let bus_addr = buff.dma_addr().as_u64();
        let ret = buff.write_with_cpu(len, f);
        Ok((ret, bus_addr, buff))
    }
}

pub struct TxPending<'a> {
    queues: &'a mut ActiveNetQueues,
    queue_index: usize,
    len: usize,
    bus_addr: u64,
    buff: Option<ContiguousBuffer>,
}

impl TxPending<'_> {
    pub fn bus_addr(&self) -> u64 {
        self.bus_addr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn try_submit(&mut self) -> Result<(), NetError> {
        let buff = self
            .buff
            .as_ref()
            .expect("tx pending buffer should exist until submit succeeds");
        let memory_mode = self
            .queues
            .owner_ref()
            .resources
            .tx_ready
            .get(self.queue_index)
            .ok_or_else(|| other_error("unknown tx queue index"))?
            .config
            .memory_mode;
        if memory_mode.requires_runtime_dma_sync() {
            buff.prepare_for_device(0, self.len);
        }
        let result = {
            let (net, queue) = tx_parts(self.queues.owner_mut(), self.queue_index)?;
            net.owner.submit_tx(
                &queue.token,
                DmaBuffer {
                    virt: buff.as_ptr(),
                    bus_addr: self.bus_addr,
                    len: self.len,
                },
            )
        };
        if let Err(error) = result {
            if memory_mode.requires_runtime_dma_sync() {
                buff.complete_for_cpu(0, self.len);
            }
            return Err(error);
        }
        let buff = self
            .buff
            .take()
            .expect("tx pending buffer should exist until submit succeeds");
        self.queues
            .owner_mut()
            .resources
            .tx_ready
            .get_mut(self.queue_index)
            .ok_or_else(|| other_error("unknown tx queue index"))?
            .inflight
            .insert(self.bus_addr, buff);
        Ok(())
    }
}

pub struct RxQueue {
    token: RxQueueOwner,
    pool: ContiguousBufferPool,
    inflight: BTreeMap<u64, ContiguousBuffer>,
    staged_returns: VecDeque<ContiguousBuffer>,
    return_fault: Option<NetError>,
    config: QueueConfig,
}

impl RxQueue {
    fn capacity(&self) -> usize {
        self.config.ring_size.saturating_sub(1)
    }

    fn prefill(&mut self, net: &mut Net) -> Result<(), NetError> {
        while self.inflight.len() + self.staged_returns.len() < self.capacity() {
            let buff = self.pool.alloc()?;
            if let Err(rejected) = self.submit_buffer(net, buff) {
                let RejectedRxBuffer { error, buffer } = rejected;
                self.staged_returns.push_back(buffer);
                return Err(error);
            }
        }
        Ok(())
    }

    fn submit_buffer(
        &mut self,
        net: &mut Net,
        buff: ContiguousBuffer,
    ) -> Result<(), RejectedRxBuffer> {
        let bus_addr = buff.dma_addr().as_u64();
        let len = self.config.buf_size.min(buff.len());
        if self.config.memory_mode.requires_runtime_dma_sync() {
            buff.prepare_for_device(0, len);
        }
        let result = net.owner.submit_rx(
            &self.token,
            DmaBuffer {
                virt: buff.as_ptr(),
                bus_addr,
                len,
            },
        );
        if let Err(error) = result {
            if self.config.memory_mode.requires_runtime_dma_sync() {
                buff.complete_for_cpu(0, len);
            }
            return Err(RejectedRxBuffer {
                error,
                buffer: buff,
            });
        }
        self.inflight.insert(bus_addr, buff);
        Ok(())
    }

    fn recycle_or_stage(&mut self, net: &mut Net, buff: ContiguousBuffer) {
        if self.return_fault.is_some() {
            self.staged_returns.push_back(buff);
            return;
        }
        if let Err(rejected) = self.submit_buffer(net, buff) {
            let RejectedRxBuffer { error, buffer } = rejected;
            self.staged_returns.push_back(buffer);
            if !matches!(error, NetError::Retry) {
                self.return_fault = Some(error);
            }
        }
    }

    fn retry_staged_returns(&mut self, net: &mut Net) -> Result<(), NetError> {
        if let Some(error) = self.return_fault.take() {
            return Err(error);
        }
        let retry_count = self.staged_returns.len();
        for _ in 0..retry_count {
            let buffer = self
                .staged_returns
                .pop_front()
                .expect("staged RX return count changed without publication");
            if let Err(rejected) = self.submit_buffer(net, buffer) {
                let RejectedRxBuffer { error, buffer } = rejected;
                self.staged_returns.push_front(buffer);
                return Err(error);
            }
        }
        Ok(())
    }

    fn reclaim_packet(
        &mut self,
        net: &mut Net,
    ) -> Result<Option<(ContiguousBuffer, usize)>, NetError> {
        let Some((bus_addr, len)) = net.owner.reclaim_rx(&self.token)? else {
            return Ok(None);
        };
        let Some(buff) = self.inflight.remove(&bus_addr) else {
            return Err(other_error("reclaimed unknown rx buffer"));
        };
        let packet_len = len.min(self.config.buf_size).min(buff.len());
        if self.config.memory_mode.requires_runtime_dma_sync() {
            buff.complete_for_cpu(0, packet_len);
        }
        Ok(Some((buff, packet_len)))
    }

    pub fn id(&self) -> usize {
        self.token.id()
    }

    pub fn buf_size(&self) -> usize {
        self.config.buf_size
    }
}

pub struct RxPacket<'a> {
    queues: &'a mut ActiveNetQueues,
    queue_index: usize,
    len: usize,
    buff: Option<ContiguousBuffer>,
}

impl RxPacket<'_> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn consume<R>(mut self, f: impl FnOnce(&[u8]) -> R) -> R {
        let buff = self.buff.as_ref().expect("rx packet buffer should exist");
        let ret = buff.read_with_cpu(self.len, f);
        if let Some(buff) = self.buff.take() {
            self.queues.recycle_rx(self.queue_index, buff);
        }
        ret
    }
}

impl Drop for RxPacket<'_> {
    fn drop(&mut self) {
        if let Some(buff) = self.buff.take() {
            self.queues.recycle_rx(self.queue_index, buff);
        }
    }
}

struct RejectedRxBuffer {
    error: NetError,
    buffer: ContiguousBuffer,
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::{
        any::Any,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use ax_kspin_test_runtime as _;
    use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle};
    use rdif_eth::{
        ContainmentCause, DriverGeneric, EthernetIrqFault, Event, IRxQueue, ITxQueue, IdList,
        Interface, IrqCapture, MaskedSource,
    };

    use super::*;

    struct TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<DmaAllocHandle> {
            panic!("test should not allocate contiguous DMA")
        }

        unsafe fn dealloc_contiguous(&self, _handle: DmaAllocHandle) {
            panic!("test should not deallocate contiguous DMA")
        }

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<DmaAllocHandle> {
            panic!("test should not allocate coherent DMA")
        }

        unsafe fn dealloc_coherent(&self, _handle: DmaAllocHandle) {
            panic!("test should not deallocate coherent DMA")
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            panic!("test should not map streaming DMA")
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {
            panic!("test should not unmap streaming DMA")
        }
    }

    struct TestInterface {
        service_calls: Arc<AtomicUsize>,
        owned_irq_endpoint: Option<rdif_eth::BIrqEndpoint>,
    }

    impl DriverGeneric for TestInterface {
        fn name(&self) -> &str {
            "test-net"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl Interface for TestInterface {
        fn mac_address(&self) -> [u8; 6] {
            [0x02, 0, 0, 0, 0, 1]
        }

        fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
            panic!("test should not create TX queue")
        }

        fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
            panic!("test should not create RX queue")
        }

        fn enable_irq(&mut self) -> Result<(), NetError> {
            Ok(())
        }

        fn disable_irq(&mut self) -> Result<(), NetError> {
            Ok(())
        }

        fn is_irq_enabled(&self) -> bool {
            false
        }

        fn take_irq_endpoint(&mut self) -> Option<rdif_eth::BIrqEndpoint> {
            self.owned_irq_endpoint.take()
        }

        fn service_irq_event(&mut self, _event: Event) -> Result<(), NetError> {
            self.service_calls.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    struct OwnedTestIrqEndpoint {
        irq_events: Event,
        capture_calls: Arc<AtomicUsize>,
    }

    struct DropTrackedTxQueue(Arc<AtomicUsize>);

    impl Drop for DropTrackedTxQueue {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl ITxQueue for DropTrackedTxQueue {
        fn id(&self) -> usize {
            0
        }

        fn config(&self) -> QueueConfig {
            QueueConfig {
                dma_mask: u64::MAX,
                align: 3,
                buf_size: 64,
                ring_size: 2,
                memory_mode: QueueMemoryMode::DirectDma,
            }
        }

        fn submit(&mut self, _buffer: DmaBuffer) -> Result<(), NetError> {
            unreachable!("activation retention test does not submit packets")
        }

        fn reclaim(&mut self) -> Option<u64> {
            None
        }
    }

    struct MissingRxInterface {
        tx_drops: Arc<AtomicUsize>,
    }

    impl DriverGeneric for MissingRxInterface {
        fn name(&self) -> &str {
            "missing-rx"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl Interface for MissingRxInterface {
        fn mac_address(&self) -> [u8; 6] {
            [0; 6]
        }

        fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
            Some(Box::new(DropTrackedTxQueue(Arc::clone(&self.tx_drops))))
        }

        fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
            None
        }

        fn enable_irq(&mut self) -> Result<(), NetError> {
            Ok(())
        }

        fn disable_irq(&mut self) -> Result<(), NetError> {
            Ok(())
        }

        fn is_irq_enabled(&self) -> bool {
            false
        }
    }

    impl rdif_eth::IrqEndpoint for OwnedTestIrqEndpoint {
        type Event = Event;
        type Fault = EthernetIrqFault;

        fn capture(&mut self) -> IrqCapture<Event, EthernetIrqFault> {
            self.capture_calls.fetch_add(1, Ordering::AcqRel);
            IrqCapture::Captured {
                event: self.irq_events,
                masked: None,
            }
        }

        fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, EthernetIrqFault> {
            MaskedSource::try_new(1, 1).map_err(|_| EthernetIrqFault::Containment)
        }
    }

    #[test]
    fn dropped_failed_queue_transaction_quarantines_the_raw_tx_queue() {
        static DMA: TestDma = TestDma;
        let tx_drops = Arc::new(AtomicUsize::new(0));
        let failure = Net::new(
            MissingRxInterface {
                tx_drops: Arc::clone(&tx_drops),
            },
            &DMA,
        )
        .activate_queues()
        .err()
        .expect("missing RX queue must fail activation");

        assert_eq!(failure.reason(), QueueActivationError::DeviceActivation);
        assert_eq!(tx_drops.load(Ordering::Acquire), 0);
        drop(failure);
        assert_eq!(tx_drops.load(Ordering::Acquire), 0);
    }

    #[test]
    fn irq_endpoint_only_captures_and_owner_service_is_synchronous() {
        static DMA: TestDma = TestDma;
        let mut rx = IdList::none();
        rx.insert(3);
        let mut tx = IdList::none();
        tx.insert(5);
        let service_calls = Arc::new(AtomicUsize::new(0));
        let capture_calls = Arc::new(AtomicUsize::new(0));
        let mut net = Net::new(
            TestInterface {
                service_calls: Arc::clone(&service_calls),
                owned_irq_endpoint: Some(Box::new(OwnedTestIrqEndpoint {
                    irq_events: Event {
                        tx_queue: tx,
                        rx_queue: rx,
                        device_status: 0x55,
                    },
                    capture_calls: Arc::clone(&capture_calls),
                })),
            },
            &DMA,
        );
        let mut irq = net.take_irq_endpoint().unwrap();
        let IrqCapture::Captured { event, masked } = irq.capture_irq() else {
            panic!("test endpoint must capture one event")
        };

        assert!(event.rx_queue.contains(3));
        assert!(event.tx_queue.contains(5));
        assert_eq!(masked, None);
        assert_eq!(capture_calls.load(Ordering::Acquire), 1);
        assert_eq!(service_calls.load(Ordering::Acquire), 0);

        net.service_irq_event(event).unwrap();
        assert_eq!(service_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn irq_capture_requires_owned_endpoint() {
        static DMA: TestDma = TestDma;
        let service_calls = Arc::new(AtomicUsize::new(0));
        let mut net = Net::new(
            TestInterface {
                service_calls,
                owned_irq_endpoint: None,
            },
            &DMA,
        );

        assert!(net.take_irq_endpoint().is_none());
    }

    #[test]
    fn irq_capture_uses_owned_endpoint() {
        static DMA: TestDma = TestDma;
        let mut rx = IdList::none();
        rx.insert(1);
        let mut tx = IdList::none();
        tx.insert(2);
        let service_calls = Arc::new(AtomicUsize::new(0));
        let capture_calls = Arc::new(AtomicUsize::new(0));
        let mut net = Net::new(
            TestInterface {
                service_calls: Arc::clone(&service_calls),
                owned_irq_endpoint: Some(Box::new(OwnedTestIrqEndpoint {
                    irq_events: Event {
                        tx_queue: tx,
                        rx_queue: rx,
                        device_status: 0xaa,
                    },
                    capture_calls: Arc::clone(&capture_calls),
                })),
            },
            &DMA,
        );

        let mut irq = net.take_irq_endpoint().unwrap();
        let IrqCapture::Captured { event, .. } = irq.capture_irq() else {
            panic!("test endpoint must capture one event")
        };

        assert!(event.rx_queue.contains(1));
        assert!(event.tx_queue.contains(2));
        assert_eq!(capture_calls.load(Ordering::Acquire), 1);
        assert_eq!(service_calls.load(Ordering::Acquire), 0);
    }
}

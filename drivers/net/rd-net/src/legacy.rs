//! Compatibility adapter for split queue interfaces.
//!
//! New stateful devices should implement [`rdif_eth::NetDeviceOwner`]
//! directly. This adapter keeps existing independently allocated queue
//! endpoints inside one aggregate owner so upper layers use the same ownership
//! shape while drivers are migrated incrementally.

use alloc::{boxed::Box, collections::BTreeMap};

use rdif_eth::{
    ActiveQueueSet, BIrqEndpoint, DmaBuffer, DriverGeneric, Event, IRxQueue, ITxQueue, Interface,
    MaskedSource, NetDeviceOwner, NetError, OwnerInitInput, OwnerInitPoll, QueueOwnerError,
    RxQueueOwner, TxQueueOwner, WifiCommand, WifiCommandProgress, WifiCommandStartError,
    WifiLinkPolicy,
};

pub(crate) struct LegacyNetDevice {
    interface: Box<dyn Interface>,
    tx: BTreeMap<usize, Box<dyn ITxQueue>>,
    rx: BTreeMap<usize, Box<dyn IRxQueue>>,
    activated: bool,
}

impl LegacyNetDevice {
    pub(crate) fn new(interface: Box<dyn Interface>) -> Self {
        Self {
            interface,
            tx: BTreeMap::new(),
            rx: BTreeMap::new(),
            activated: false,
        }
    }

    fn activate_split_queues(&mut self) -> Result<ActiveQueueSet, NetError> {
        if self.activated {
            return Err(NetError::Retry);
        }
        self.activated = true;

        let tx = self
            .interface
            .create_tx_queue()
            .ok_or_else(|| queue_owner_error(QueueOwnerError::MissingTx))?;
        let tx_owner = TxQueueOwner::new(tx.id(), tx.config()).map_err(queue_owner_error)?;
        self.tx.insert(tx_owner.id(), tx);

        let rx = self
            .interface
            .create_rx_queue()
            .ok_or_else(|| queue_owner_error(QueueOwnerError::MissingRx))?;
        let rx_owner = RxQueueOwner::new(rx.id(), rx.config()).map_err(queue_owner_error)?;
        self.rx.insert(rx_owner.id(), rx);

        ActiveQueueSet::new(alloc::vec![tx_owner], alloc::vec![rx_owner]).map_err(queue_owner_error)
    }

    fn tx_mut(&mut self, owner: &TxQueueOwner) -> Result<&mut dyn ITxQueue, NetError> {
        match self.tx.get_mut(&owner.id()) {
            Some(queue) if queue.config() == owner.config() => Ok(queue.as_mut()),
            _ => Err(NetError::NotSupported),
        }
    }

    fn rx_mut(&mut self, owner: &RxQueueOwner) -> Result<&mut dyn IRxQueue, NetError> {
        match self.rx.get_mut(&owner.id()) {
            Some(queue) if queue.config() == owner.config() => Ok(queue.as_mut()),
            _ => Err(NetError::NotSupported),
        }
    }
}

impl DriverGeneric for LegacyNetDevice {
    fn name(&self) -> &str {
        self.interface.name()
    }
}

impl NetDeviceOwner for LegacyNetDevice {
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        self.interface.poll_owner_init(input)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.interface.mac_address()
    }

    fn activate_queue_set(&mut self) -> Result<ActiveQueueSet, NetError> {
        self.activate_split_queues()
    }

    fn submit_tx(&mut self, queue: &TxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError> {
        self.tx_mut(queue)?.submit(buffer)
    }

    fn reclaim_tx(&mut self, queue: &TxQueueOwner) -> Result<Option<u64>, NetError> {
        Ok(self.tx_mut(queue)?.reclaim())
    }

    fn submit_rx(&mut self, queue: &RxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError> {
        self.rx_mut(queue)?.submit(buffer)
    }

    fn reclaim_rx(&mut self, queue: &RxQueueOwner) -> Result<Option<(u64, usize)>, NetError> {
        Ok(self.rx_mut(queue)?.reclaim())
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        self.interface.enable_irq()
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        self.interface.disable_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.interface.is_irq_enabled()
    }

    fn take_irq_endpoint(&mut self) -> Option<BIrqEndpoint> {
        self.interface.take_irq_endpoint()
    }

    fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        self.interface.service_irq_event(event)
    }

    fn rearm_irq_source(&mut self, source: MaskedSource) -> Result<(), NetError> {
        self.interface.rearm_irq_source(source)
    }

    fn owner_link_policy(&self) -> Option<WifiLinkPolicy> {
        self.interface.owner_link_policy()
    }

    fn supports_wifi_control(&self) -> bool {
        self.interface.supports_wifi_control()
    }

    fn start_wifi_command(
        &mut self,
        command: WifiCommand,
        now_ns: u64,
    ) -> Result<WifiCommandProgress, WifiCommandStartError> {
        self.interface.start_wifi_command(command, now_ns)
    }

    fn poll_wifi_command(&mut self, now_ns: u64) -> WifiCommandProgress {
        self.interface.poll_wifi_command(now_ns)
    }
}

fn queue_owner_error(error: QueueOwnerError) -> NetError {
    NetError::Other(Box::new(error))
}

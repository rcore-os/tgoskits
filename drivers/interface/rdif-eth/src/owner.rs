//! Move-only network-device maintenance ownership.
//!
//! This boundary keeps controller state and every hardware queue under one
//! mutable owner. The OS may move the owner into its selected maintenance
//! thread, but it cannot split queue objects into independently callable
//! shared handles.

use alloc::vec::Vec;

use crate::{
    BIrqEndpoint, DmaBuffer, DriverGeneric, Event, MaskedSource, NetError, OwnerInitInput,
    OwnerInitPoll, QueueConfig, WifiCommand, WifiCommandProgress, WifiCommandStartError,
    WifiLinkPolicy,
};

const MAX_QUEUE_ID: usize = 63;

/// Invalid queue ownership metadata returned during device activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueueOwnerError {
    /// A queue identifier cannot be represented by the portable IRQ event mask.
    #[error("network queue id {0} exceeds the portable maximum of 63")]
    IdOutOfRange(usize),
    /// Two transmit ownership tokens name the same hardware queue.
    #[error("duplicate transmit queue owner for id {0}")]
    DuplicateTx(usize),
    /// Two receive ownership tokens name the same hardware queue.
    #[error("duplicate receive queue owner for id {0}")]
    DuplicateRx(usize),
    /// A hardware device did not publish any transmit queue owner.
    #[error("network device published no transmit queue owner")]
    MissingTx,
    /// A hardware device did not publish any receive queue owner.
    #[error("network device published no receive queue owner")]
    MissingRx,
}

/// Exclusive authority for one transmit hardware queue.
///
/// The token is intentionally neither `Clone` nor `Copy`. It stays beside the
/// aggregate device owner and is presented back to that owner for every queue
/// operation.
#[derive(Debug)]
pub struct TxQueueOwner {
    id: usize,
    config: QueueConfig,
}

impl TxQueueOwner {
    /// Creates one transmit queue authority.
    ///
    /// # Errors
    ///
    /// Returns [`QueueOwnerError::IdOutOfRange`] when `id` cannot be encoded in
    /// [`crate::Event`].
    pub fn new(id: usize, config: QueueConfig) -> Result<Self, QueueOwnerError> {
        validate_queue_id(id)?;
        Ok(Self { id, config })
    }

    /// Returns the device-local hardware queue identifier.
    pub const fn id(&self) -> usize {
        self.id
    }

    /// Returns the immutable DMA and ring contract for this queue.
    pub const fn config(&self) -> QueueConfig {
        self.config
    }
}

/// Exclusive authority for one receive hardware queue.
///
/// Like [`TxQueueOwner`], this value is move-only and has no queue methods of
/// its own. Hardware access always goes through the aggregate
/// [`NetDeviceOwner`].
#[derive(Debug)]
pub struct RxQueueOwner {
    id: usize,
    config: QueueConfig,
}

impl RxQueueOwner {
    /// Creates one receive queue authority.
    ///
    /// # Errors
    ///
    /// Returns [`QueueOwnerError::IdOutOfRange`] when `id` cannot be encoded in
    /// [`crate::Event`].
    pub fn new(id: usize, config: QueueConfig) -> Result<Self, QueueOwnerError> {
        validate_queue_id(id)?;
        Ok(Self { id, config })
    }

    /// Returns the device-local hardware queue identifier.
    pub const fn id(&self) -> usize {
        self.id
    }

    /// Returns the immutable DMA and ring contract for this queue.
    pub const fn config(&self) -> QueueConfig {
        self.config
    }
}

/// All queue authorities published by one controller activation.
///
/// Multi-queue devices publish one distinct token for each queue. Splitting a
/// token does not split controller ownership: the same [`NetDeviceOwner`]
/// remains the only object allowed to access registers or advance queue state.
#[must_use = "queue ownership must remain beside its aggregate device owner"]
#[derive(Debug)]
pub struct ActiveQueueSet {
    tx: Vec<TxQueueOwner>,
    rx: Vec<RxQueueOwner>,
}

impl ActiveQueueSet {
    /// Validates and constructs a complete hardware queue ownership set.
    ///
    /// # Errors
    ///
    /// Returns a typed error for empty directions, duplicate identifiers, or
    /// identifiers that cannot be represented by the portable event mask.
    pub fn new(tx: Vec<TxQueueOwner>, rx: Vec<RxQueueOwner>) -> Result<Self, QueueOwnerError> {
        if tx.is_empty() {
            return Err(QueueOwnerError::MissingTx);
        }
        if rx.is_empty() {
            return Err(QueueOwnerError::MissingRx);
        }
        validate_unique_tx(&tx)?;
        validate_unique_rx(&rx)?;
        Ok(Self { tx, rx })
    }

    /// Constructs the common one-TX/one-RX topology.
    pub fn single(tx_config: QueueConfig, rx_config: QueueConfig) -> Result<Self, QueueOwnerError> {
        Self::new(
            alloc::vec![TxQueueOwner::new(0, tx_config)?],
            alloc::vec![RxQueueOwner::new(0, rx_config)?],
        )
    }

    /// Transfers all individual authorities to the runtime owner bundle.
    pub fn into_parts(self) -> (Vec<TxQueueOwner>, Vec<RxQueueOwner>) {
        (self.tx, self.rx)
    }
}

/// Aggregate portable owner for one stateful network device.
///
/// Exactly one CPU-pinned maintenance thread owns this value. IRQ capture is
/// moved separately into the registered callback, while controller service,
/// queue submission/reclaim, initialization, and recovery all require
/// `&mut self`. Portable drivers must not store a task, waker, scheduler
/// object, or an OS lock in order to implement this trait.
pub trait NetDeviceOwner: DriverGeneric {
    /// Advances discovery-to-ready state on the final maintenance owner.
    fn poll_owner_init(&mut self, _input: OwnerInitInput) -> OwnerInitPoll {
        OwnerInitPoll::Ready
    }

    /// Returns the device's six-byte MAC address.
    fn mac_address(&self) -> [u8; 6];

    /// Publishes all hardware queue ownership after initialization is ready.
    ///
    /// A successful implementation may be called exactly once. Queue state
    /// remains inside `self`; the returned values are linear authorities, not
    /// independently callable queue objects.
    fn activate_queue_set(&mut self) -> Result<ActiveQueueSet, NetError>;

    /// Submits one packet through the named transmit queue.
    fn submit_tx(&mut self, queue: &TxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError>;

    /// Reclaims one completed transmit buffer from the named queue.
    fn reclaim_tx(&mut self, queue: &TxQueueOwner) -> Result<Option<u64>, NetError>;

    /// Supplies one empty packet buffer to the named receive queue.
    fn submit_rx(&mut self, queue: &RxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError>;

    /// Reclaims one completed receive packet from the named queue.
    fn reclaim_rx(&mut self, queue: &RxQueueOwner) -> Result<Option<(u64, usize)>, NetError>;

    /// Enables the exact device interrupt sources owned by this interface.
    fn enable_irq(&mut self) -> Result<(), NetError>;

    /// Masks the exact device interrupt sources owned by this interface.
    fn disable_irq(&mut self) -> Result<(), NetError>;

    /// Reports whether the exact device sources are currently enabled.
    fn is_irq_enabled(&self) -> bool;

    /// Moves the destructive IRQ endpoint into the registered callback.
    fn take_irq_endpoint(&mut self) -> Option<BIrqEndpoint> {
        None
    }

    /// Advances owner state using one acknowledged stable IRQ event.
    fn service_irq_event(&mut self, _event: Event) -> Result<(), NetError> {
        Ok(())
    }

    /// Rearms an exact generation-checked source after owner service.
    fn rearm_irq_source(&mut self, _source: MaskedSource) -> Result<(), NetError> {
        Err(NetError::NotSupported)
    }

    /// Returns immutable link policy established before publication.
    fn owner_link_policy(&self) -> Option<WifiLinkPolicy> {
        None
    }

    /// Reports whether this owner accepts wireless control commands.
    fn supports_wifi_control(&self) -> bool {
        false
    }

    /// Transfers one wireless command into the owner state machine.
    fn start_wifi_command(
        &mut self,
        command: WifiCommand,
        _now_ns: u64,
    ) -> Result<WifiCommandProgress, WifiCommandStartError> {
        Err(WifiCommandStartError::Unsupported(command))
    }

    /// Advances one accepted wireless command after its declared activation.
    fn poll_wifi_command(&mut self, _now_ns: u64) -> WifiCommandProgress {
        WifiCommandProgress::Failed(NetError::NotSupported)
    }
}

fn validate_queue_id(id: usize) -> Result<(), QueueOwnerError> {
    if id > MAX_QUEUE_ID {
        Err(QueueOwnerError::IdOutOfRange(id))
    } else {
        Ok(())
    }
}

fn validate_unique_tx(queues: &[TxQueueOwner]) -> Result<(), QueueOwnerError> {
    let mut seen = 0_u64;
    for queue in queues {
        validate_queue_id(queue.id)?;
        let bit = 1_u64 << queue.id;
        if seen & bit != 0 {
            return Err(QueueOwnerError::DuplicateTx(queue.id));
        }
        seen |= bit;
    }
    Ok(())
}

fn validate_unique_rx(queues: &[RxQueueOwner]) -> Result<(), QueueOwnerError> {
    let mut seen = 0_u64;
    for queue in queues {
        validate_queue_id(queue.id)?;
        let bit = 1_u64 << queue.id;
        if seen & bit != 0 {
            return Err(QueueOwnerError::DuplicateRx(queue.id));
        }
        seen |= bit;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueueMemoryMode;

    const fn queue_config() -> QueueConfig {
        QueueConfig {
            dma_mask: u64::MAX,
            align: 8,
            buf_size: 2048,
            ring_size: 16,
            memory_mode: QueueMemoryMode::DirectDma,
        }
    }

    #[test]
    fn active_set_rejects_duplicate_queue_authority() {
        let result = ActiveQueueSet::new(
            alloc::vec![
                TxQueueOwner::new(4, queue_config()).unwrap(),
                TxQueueOwner::new(4, queue_config()).unwrap(),
            ],
            alloc::vec![RxQueueOwner::new(0, queue_config()).unwrap()],
        );

        assert!(matches!(result, Err(QueueOwnerError::DuplicateTx(4))));
    }

    #[test]
    fn active_set_keeps_each_multi_queue_token_distinct() {
        let set = ActiveQueueSet::new(
            alloc::vec![
                TxQueueOwner::new(1, queue_config()).unwrap(),
                TxQueueOwner::new(2, queue_config()).unwrap(),
            ],
            alloc::vec![RxQueueOwner::new(3, queue_config()).unwrap()],
        )
        .unwrap();
        let (tx, rx) = set.into_parts();

        assert_eq!(tx.iter().map(TxQueueOwner::id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(rx.iter().map(RxQueueOwner::id).collect::<Vec<_>>(), [3]);
    }
}

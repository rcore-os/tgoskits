use rdif_eth::{
    DmaBuffer, IRxQueue, ITxQueue, NetError, NetQueueId, QueueConfig, RxCompletion, SubmitError,
};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

const QUEUE_ID: NetQueueId = NetQueueId::new(0);

pub(crate) struct QueueOwnerPorts {
    pub(crate) tx_submit: HeapCons<DmaBuffer>,
    pub(crate) tx_complete: HeapProd<DmaBuffer>,
    pub(crate) rx_submit: HeapCons<DmaBuffer>,
    pub(crate) rx_complete: HeapProd<RxCompletion>,
    pub(crate) rx_frame_size: usize,
}

pub(crate) struct AicTxQueue {
    config: QueueConfig,
    submit: HeapProd<DmaBuffer>,
    complete: HeapCons<DmaBuffer>,
}

pub(crate) struct AicRxQueue {
    config: QueueConfig,
    submit: HeapProd<DmaBuffer>,
    complete: HeapCons<RxCompletion>,
}

pub(crate) fn queue_parts(config: QueueConfig) -> (AicTxQueue, AicRxQueue, QueueOwnerPorts) {
    let tx_submit = HeapRb::new(config.ring_size);
    let tx_complete = HeapRb::new(config.ring_size);
    let rx_submit = HeapRb::new(config.ring_size);
    let rx_complete = HeapRb::new(config.ring_size);
    let (tx_submit_prod, tx_submit_cons) = tx_submit.split();
    let (tx_complete_prod, tx_complete_cons) = tx_complete.split();
    let (rx_submit_prod, rx_submit_cons) = rx_submit.split();
    let (rx_complete_prod, rx_complete_cons) = rx_complete.split();
    (
        AicTxQueue {
            config,
            submit: tx_submit_prod,
            complete: tx_complete_cons,
        },
        AicRxQueue {
            config,
            submit: rx_submit_prod,
            complete: rx_complete_cons,
        },
        QueueOwnerPorts {
            tx_submit: tx_submit_cons,
            tx_complete: tx_complete_prod,
            rx_submit: rx_submit_cons,
            rx_complete: rx_complete_prod,
            rx_frame_size: config.buf_size,
        },
    )
}

impl ITxQueue for AicTxQueue {
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        self.config
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if buffer.capacity() < self.config.buf_size {
            return Err(SubmitError::new(buffer, NetError::InvalidParts));
        }
        self.submit
            .try_push(buffer)
            .map_err(|buffer| SubmitError::new(buffer, NetError::Retry))
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        self.complete.try_pop()
    }
}

impl IRxQueue for AicRxQueue {
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        self.config
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if buffer.capacity() < self.config.buf_size {
            return Err(SubmitError::new(buffer, NetError::InvalidParts));
        }
        self.submit
            .try_push(buffer)
            .map_err(|buffer| SubmitError::new(buffer, NetError::Retry))
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        self.complete.try_pop()
    }
}

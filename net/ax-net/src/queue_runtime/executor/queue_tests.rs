use alloc::{boxed::Box, vec};
use std::sync::Mutex;

use rd_net::{
    FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSourceId, NetPollGroupId,
    NetPollGroupParts, NetPollIrqControl, NetQueueId, NetQueuePairParts, QueueConfig, SubmitError,
    TxNotify,
    dma_api::{DeviceDma, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDomainId},
};

use super::*;
use crate::queue_runtime::{spsc_ring, tests::TEST_DMA};

type Trace = Arc<Mutex<Vec<&'static str>>>;

fn queue_config() -> QueueConfig {
    QueueConfig {
        ring_size: 3,
        buf_size: 2048,
        align: 64,
        dma_mask: u64::MAX,
    }
}

struct TestTx(Trace);

impl ITxQueue for TestTx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        queue_config()
    }
    fn submit(&mut self, _buffer: DmaBuffer) -> Result<(), SubmitError> {
        self.0.lock().unwrap().push("tx");
        Ok(())
    }
    fn flush(&mut self) {
        self.0.lock().unwrap().push("flush");
    }
    fn reclaim(&mut self) -> Option<DmaBuffer> {
        None
    }
}

struct TestRx {
    trace: Trace,
    completions: VecDeque<RxCompletion>,
    initial: usize,
    reclaimed: usize,
    replacements: Vec<DmaBuffer>,
}

impl IRxQueue for TestRx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        queue_config()
    }
    fn submit(&mut self, mut buffer: DmaBuffer) -> Result<(), SubmitError> {
        if self.initial > 0 {
            self.initial -= 1;
            buffer.write_with_cpu(|packet| packet.fill(self.initial as u8));
            self.completions.push_back(RxCompletion {
                buffer,
                packet_len: 60,
            });
            return Ok(());
        }
        // Model a software queue whose owner cannot accept more buffers
        // until both completion slots have been consumed.
        if self.reclaimed < 2 {
            self.trace.lock().unwrap().push("retry");
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        self.trace.lock().unwrap().push("refill");
        self.replacements.push(buffer);
        Ok(())
    }
    fn reclaim(&mut self) -> Option<RxCompletion> {
        let completion = self.completions.pop_front()?;
        self.reclaimed += 1;
        self.trace.lock().unwrap().push("rx");
        Some(completion)
    }
}

struct TestIrq;
impl NetHardIrqHandler for TestIrq {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        NetHardIrqResult::Spurious
    }
}
impl NetPollIrqControl for TestIrq {
    fn quiesce(&mut self) -> Result<(), NetError> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), NetError> {
        Ok(())
    }
    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        Ok(NetRearmResult::Idle)
    }
}

struct TestDevice(Trace);
impl rd_net::DriverGeneric for TestDevice {
    fn name(&self) -> &str {
        "test"
    }
}
impl NetDevice for TestDevice {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("test", [0; 6]),
            control: Box::new(FixedNetControl::new([0; 6])),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(0),
                queues: NetQueuePairParts {
                    tx: Box::new(TestTx(Arc::clone(&self.0))),
                    rx: Box::new(TestRx {
                        trace: self.0,
                        completions: VecDeque::new(),
                        initial: 2,
                        reclaimed: 0,
                        replacements: Vec::new(),
                    }),
                },
                irq_control: Box::new(TestIrq),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(0),
                    Box::new(TestIrq),
                )],
            }],
        })
    }
}

#[test]
fn rx_refill_retry_drains_completions_and_preserves_tx_flush() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let mut device = rd_net::prepare_device(Box::new(TestDevice(Arc::clone(&trace))), dma).unwrap();
    let mut group = device.poll_groups.pop().unwrap();
    group.rx.initial_refill(2).unwrap();
    let (rx_ready, mut received) = spsc_ring(1);
    let (recycle, rx_recycle) = spsc_ring(2);
    let (mut transmit, tx_ready) = spsc_ring(2);
    let (tx_free, _free) = spsc_ring(2);
    let shared = Arc::new(PollGroupState::new(0, Arc::new(ax_task::IrqNotify::new())));
    shared.activate(false);
    let buffer = group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let mut executor = QueueGroupExecutor {
        group,
        rx_ready,
        rx_recycle,
        rx_recycler: Arc::new(RxRecycler::new(recycle, Arc::clone(&shared), 2)),
        rx_spares: Vec::new(),
        tx_ready,
        tx_free,
        pending_rx: None,
        pending_rx_refill: VecDeque::with_capacity(2),
        pending_tx: None,
        pending_tx_free: None,
        retry_at: None,
        shared,
    };

    assert!(matches!(executor.poll(2), GroupPollOutcome::More(2)));
    assert!(matches!(executor.poll(256), GroupPollOutcome::More(_)));
    assert_eq!(
        &*trace.lock().unwrap(),
        &["tx", "flush", "rx", "retry", "rx"]
    );
    assert!(
        received.pop().is_none(),
        "RX escaped before replacement was accepted"
    );
    assert_eq!(executor.pending_rx_refill.len(), 2);

    assert!(matches!(executor.poll(256), GroupPollOutcome::Blocked(_)));
    let first = received.pop().unwrap();
    first
        .buffer
        .read_with_cpu(60, |packet| assert_eq!(packet, &[1; 60]));
    assert!(executor.pending_rx.is_some());
    assert!(matches!(executor.poll(256), GroupPollOutcome::Idle(_)));
    let second = received.pop().unwrap();
    second
        .buffer
        .read_with_cpu(60, |packet| assert_eq!(packet, &[0; 60]));
    assert!(executor.pending_rx.is_none());
    assert!(executor.pending_rx_refill.is_empty());
    assert_eq!(
        trace
            .lock()
            .unwrap()
            .iter()
            .filter(|&&event| event == "refill")
            .count(),
        2
    );
}

//! Queue-level AIC8800 network device parts.
//!
//! The complete device is consumed once into a controller endpoint, one poll
//! group, move-only RX/TX queues, and an owner-CPU wireless control endpoint.
//! No AIC background task, periodic poller, global callback, or complete-device
//! handle survives this split.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering, fence};

use ax_sync::SpinLock;
use rd_net::{
    DmaBuffer, IRxQueue, ITxQueue, KError, NetControlEndpoint, NetDevice, NetDeviceInfo,
    NetDeviceParts, NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult,
    NetIrqSnapshot, NetIrqSourceId, NetOwnerStartup, NetPollGroupId, NetPollGroupParts,
    NetPollIrqControl, NetQueueId, NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion,
    SubmitError, WifiControl, WifiOperation, WifiTransaction,
};
use rdif_eth::DriverGeneric;
use sdio_host::{SdioHost, SdioIrqSource, SdioIrqStatus};

use crate::{
    common::ChipVariant,
    fdrv::{
        core::bus::{BusState, WifiBus},
        thread::{ap, rx, tx},
        wifi::api::{WifiClient, WifiConfig},
    },
};

const DEVICE_NAME: &str = "aic8800-wifi";
const GROUP_ID: NetPollGroupId = NetPollGroupId::new(0);
const QUEUE_ID: NetQueueId = NetQueueId::new(0);
const IRQ_SOURCE_ID: NetIrqSourceId = NetIrqSourceId::new(0);
const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const MAX_TX_QUEUE_LEN: usize = 128;

fn net_err(error: crate::fdrv::wifi::api::WifiError) -> NetError {
    NetError::Other(Box::new(error))
}

fn startup_err(message: &'static str) -> NetError {
    NetError::Other(Box::new(KError::Unknown(message)))
}

enum AicInitState {
    Pending,
    Ready(Arc<WifiBus>),
    Failed,
}

struct AicDeferredState {
    init: SpinLock<AicInitState>,
    irq_pending: AtomicBool,
}

impl AicDeferredState {
    fn new() -> Self {
        Self {
            init: SpinLock::new(AicInitState::Pending),
            irq_pending: AtomicBool::new(false),
        }
    }

    fn bus(&self) -> Result<Arc<WifiBus>, NetError> {
        match &*self.init.lock_irqsave() {
            AicInitState::Ready(bus) => Ok(Arc::clone(bus)),
            AicInitState::Pending => Err(NetError::Retry),
            AicInitState::Failed => Err(NetError::Stopped),
        }
    }

    fn publish_ready(&self, bus: Arc<WifiBus>) -> Result<(), NetError> {
        let mut init = self.init.lock_irqsave();
        if !matches!(*init, AicInitState::Pending) {
            return Err(NetError::InvalidParts);
        }
        if self.irq_pending.swap(false, Ordering::AcqRel) {
            bus.rx.irq_pending.store(true, Ordering::Release);
        }
        *init = AicInitState::Ready(bus);
        Ok(())
    }

    fn publish_failed(&self) {
        *self.init.lock_irqsave() = AicInitState::Failed;
    }
}

/// AIC8800 device consumed by [`NetDevice::into_parts`].
pub struct AicWifiNetDev<H: SdioHost + 'static> {
    sdio: H,
    chip: ChipVariant,
    fallback_mac: [u8; 6],
    irq_source: Box<dyn SdioIrqSource>,
    startup: Option<WifiTransaction>,
}

impl<H: SdioHost + 'static> AicWifiNetDev<H> {
    pub fn new(
        sdio: H,
        chip: ChipVariant,
        fallback_mac: [u8; 6],
        irq_source: Box<dyn SdioIrqSource>,
    ) -> Self {
        Self {
            sdio,
            chip,
            fallback_mac,
            irq_source,
            startup: None,
        }
    }

    /// Selects the board startup transaction. It is executed by the fixed-CPU
    /// network queue runtime after IRQ registration, never during probe.
    pub fn with_startup_transaction(mut self, transaction: WifiTransaction) -> Self {
        self.startup = Some(transaction);
        self
    }
}

impl<H: SdioHost + 'static> DriverGeneric for AicWifiNetDev<H> {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

struct AicNetControl {
    state: Arc<AicDeferredState>,
    fallback_mac: [u8; 6],
}

impl NetControlEndpoint for AicNetControl {
    fn mac_address(&mut self) -> Result<[u8; 6], NetError> {
        let bus = self.state.bus()?;
        Ok(bus.conn.sta_mac.lock().unwrap_or(self.fallback_mac))
    }
}

struct AicWifiControl {
    state: Arc<AicDeferredState>,
    chip: ChipVariant,
    startup: Option<WifiTransaction>,
}

impl WifiControl for AicWifiControl {
    fn execute(&mut self, operation: &WifiOperation) -> Result<(), NetError> {
        let mut client = WifiClient::new(self.state.bus()?);
        match operation {
            WifiOperation::Connect { ssid, password } => {
                client.lmac_configure(self.chip, 6000).map_err(net_err)?;
                let config = if password.is_empty() {
                    WifiConfig::open(ssid)
                } else {
                    WifiConfig::wpa2_psk(ssid, password)
                };
                client.connect(&config, 15000).map_err(net_err)
            }
            WifiOperation::Disconnect => client.disconnect().map_err(net_err),
            WifiOperation::StartOpenAccessPoint { ssid, channel } => client
                .start_ap_open(self.chip, ssid, *channel, 6000)
                .map(|_| ())
                .map_err(net_err),
        }
    }

    fn startup_transaction(&self) -> Option<WifiTransaction> {
        self.startup.clone()
    }
}

struct AicOwnerStartup<H: SdioHost + 'static> {
    sdio: Option<H>,
    chip: ChipVariant,
    state: Arc<AicDeferredState>,
}

impl<H: SdioHost + 'static> NetOwnerStartup for AicOwnerStartup<H> {
    fn initialize(&mut self) -> Result<(), NetError> {
        let mut sdio = self.sdio.take().ok_or(NetError::Stopped)?;
        if let Err(error) = crate::fw::firmware_init(&mut sdio, self.chip) {
            log::error!("[aic8800] owner-CPU firmware init failed: {error:?}");
            self.state.publish_failed();
            return Err(startup_err("AIC8800 firmware initialization failed"));
        }
        log::info!("[aic8800] firmware loaded on queue owner CPU");

        let bus = match crate::fdrv::init(sdio, self.chip) {
            Ok(bus) => bus,
            Err(error) => {
                log::error!("[aic8800] owner-CPU FDRV init failed: {error}");
                self.state.publish_failed();
                return Err(startup_err("AIC8800 FDRV initialization failed"));
            }
        };
        self.state.publish_ready(bus)
    }
}

struct AicHardIrq {
    state: Arc<AicDeferredState>,
    source: Box<dyn SdioIrqSource>,
}

impl NetHardIrqHandler for AicHardIrq {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        match self.source.handle_irq() {
            SdioIrqStatus::Spurious => NetHardIrqResult::Spurious,
            SdioIrqStatus::CardPending => {
                if let Ok(bus) = self.state.bus() {
                    bus.rx.irq_pending.store(true, Ordering::Release);
                    NetHardIrqResult::Schedule(NetIrqSnapshot::RX)
                } else {
                    self.state.irq_pending.store(true, Ordering::Release);
                    NetHardIrqResult::ProbeDeferred
                }
            }
        }
    }
}

struct AicPollIrqControl {
    state: Arc<AicDeferredState>,
}

impl AicPollIrqControl {
    fn has_work(bus: &WifiBus) -> bool {
        bus.rx.irq_pending.load(Ordering::Acquire)
            || !bus.rx.data_queue.lock().is_empty()
            || !bus.tx.completed.lock().is_empty()
            || tx::has_pending_work(bus)
            || !bus.ap.assoc_queue.lock().is_empty()
            || !bus.ap.sta_del_queue.lock().is_empty()
    }
}

impl NetPollIrqControl for AicPollIrqControl {
    fn quiesce(&mut self) -> Result<(), NetError> {
        let bus = self.state.bus()?;
        if *bus.state.lock() == BusState::Down {
            return Err(NetError::Stopped);
        }
        bus.transport.mask_card_irq();
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        let bus = self
            .state
            .bus()
            .map_err(|_| NetError::DmaShutdownUnconfirmed)?;
        // SDIO transfers are synchronous host transactions. Queue DmaBuffer
        // tokens are retained only as CPU-owned staging objects and are never
        // published as controller DMA descriptors. Releasing them is safe only
        // after both the variant-specific card source and controller signal are
        // disabled on this owner CPU.
        bus.shutdown().map_err(|_| NetError::DmaShutdownUnconfirmed)
    }

    fn rearm_and_check(&mut self) -> Result<NetRearmResult, NetError> {
        let bus = self.state.bus()?;
        if *bus.state.lock() == BusState::Down {
            return Err(NetError::Stopped);
        }
        if Self::has_work(&bus) {
            return Ok(NetRearmResult::WorkPending(NetIrqSnapshot::all_queue_work()));
        }

        bus.transport.enable_irq();
        let controller_pending = bus.transport.rearm_and_check_card_irq();
        fence(Ordering::SeqCst);
        if controller_pending || Self::has_work(&bus) {
            if controller_pending {
                bus.rx.irq_pending.store(true, Ordering::Release);
            }
            bus.transport.mask_card_irq();
            Ok(NetRearmResult::WorkPending(NetIrqSnapshot::all_queue_work()))
        } else {
            Ok(NetRearmResult::Idle)
        }
    }
}

impl<H: SdioHost + 'static> NetDevice for AicWifiNetDev<H> {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let AicWifiNetDev {
            sdio,
            chip,
            fallback_mac,
            irq_source,
            startup,
        } = *self;
        let state = Arc::new(AicDeferredState::new());
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new(DEVICE_NAME, fallback_mac),
            control: Box::new(AicNetControl {
                state: Arc::clone(&state),
                fallback_mac,
            }),
            wifi_control: Some(Box::new(AicWifiControl {
                state: Arc::clone(&state),
                chip,
                startup,
            })),
            poll_groups: alloc::vec![NetPollGroupParts {
                id: GROUP_ID,
                queues: NetQueuePairParts {
                    tx: Box::new(AicTxQueue {
                        state: Arc::clone(&state),
                    }),
                    rx: Box::new(AicRxQueue {
                        state: Arc::clone(&state),
                        rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
                    }),
                },
                irq_control: Box::new(AicPollIrqControl {
                    state: Arc::clone(&state),
                }),
                owner_startup: Some(Box::new(AicOwnerStartup {
                    sdio: Some(sdio),
                    chip,
                    state: Arc::clone(&state),
                })),
                irq_endpoints: alloc::vec![NetHardIrqEndpoint::new(
                    IRQ_SOURCE_ID,
                    Box::new(AicHardIrq {
                        state,
                        source: irq_source,
                    }),
                )],
            }],
        })
    }
}

fn aic_queue_config() -> QueueConfig {
    QueueConfig {
        dma_mask: u64::MAX,
        align: 1,
        buf_size: BUFFER_SIZE,
        ring_size: QUEUE_SIZE,
    }
}

struct AicTxQueue {
    state: Arc<AicDeferredState>,
}

impl ITxQueue for AicTxQueue {
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        aic_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        let bus = match self.state.bus() {
            Ok(bus) => bus,
            Err(error) => return Err(SubmitError::new(buffer, error)),
        };
        if bus.conn.vif_idx.load(Ordering::Acquire) == 0xFF
            || bus.tx.pktcnt.load(Ordering::Acquire) >= MAX_TX_QUEUE_LEN as u32
        {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        let frame = buffer.read_with_cpu(buffer.len(), |packet| packet.to_vec());
        tx::enqueue_data_frame(&bus, frame, buffer)
            .map_err(|(_, buffer)| SubmitError::new(buffer, NetError::Retry))
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        let bus = self.state.bus().ok()?;
        let _ = tx::tx_process(&bus);
        tx::reclaim_completed(&bus)
    }
}

struct AicRxQueue {
    state: Arc<AicDeferredState>,
    rx_buffers: VecDeque<DmaBuffer>,
}

impl IRxQueue for AicRxQueue {
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        aic_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if self.rx_buffers.len() >= QUEUE_SIZE - 1 {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        self.rx_buffers.push_back(buffer);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        let bus = self.state.bus().ok()?;
        if bus.rx.data_queue.lock().is_empty() {
            let _ = rx::process_rx_frames(&bus, 64);
            let _ = ap::process_pending(&bus, 8);
            let _ = tx::tx_process(&bus);
        }
        let frame = bus.rx.data_queue.lock().pop_front()?;
        let mut buffer = self.rx_buffers.pop_front()?;
        let packet_len = frame.len().min(buffer.capacity());
        buffer.write_with_cpu(|target| target[..packet_len].copy_from_slice(&frame[..packet_len]));
        Some(RxCompletion { buffer, packet_len })
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use ax_sync::SpinLock;
    use sdio_host::{SdioCardIrq, SdioHost, SdioIrqSource, error::SdioError};

    use super::*;
    use crate::{common::SDIOWIFI_INTR_ENABLE_REG_V3, fdrv::core::sdio_transport::SdioTransport};

    struct RecordingCardIrq;

    impl SdioCardIrq for RecordingCardIrq {
        fn mask_card_irq(&self) {}

        fn rearm_and_check_card_irq(&self) -> bool {
            false
        }
    }

    struct RecordingHost {
        writes: Arc<SpinLock<Vec<(u8, u32, u8)>>>,
        disable_count: Arc<AtomicUsize>,
        fail_write: bool,
    }

    impl SdioHost for RecordingHost {
        fn init(&mut self) -> Result<(), SdioError> {
            Ok(())
        }

        fn mmio_base(&self) -> usize {
            0
        }

        fn read_byte(&self, _func: u8, _addr: u32) -> Result<u8, SdioError> {
            Ok(0)
        }

        fn write_byte(&self, func: u8, addr: u32, value: u8) -> Result<(), SdioError> {
            self.writes.lock_irqsave().push((func, addr, value));
            if self.fail_write {
                Err(SdioError::IoError)
            } else {
                Ok(())
            }
        }

        fn write_byte_read(&self, _func: u8, _addr: u32, value: u8) -> Result<u8, SdioError> {
            Ok(value)
        }

        fn read_fifo(&self, _func: u8, _addr: u32, _buf: &mut [u8]) -> Result<(), SdioError> {
            Ok(())
        }

        fn read_fifo_inc(&self, _func: u8, _addr: u32, _buf: &mut [u8]) -> Result<(), SdioError> {
            Ok(())
        }

        fn write_fifo(&self, _func: u8, _addr: u32, _buf: &[u8]) -> Result<(), SdioError> {
            Ok(())
        }

        fn write_fifo_inc(&self, _func: u8, _addr: u32, _buf: &[u8]) -> Result<(), SdioError> {
            Ok(())
        }

        fn set_block_size(&self, _func: u8, _size: u16) -> Result<(), SdioError> {
            Ok(())
        }

        fn set_clock(&self, _hz: u32) -> Result<(), SdioError> {
            Ok(())
        }

        fn enable_func(&self, _func: u8) -> Result<(), SdioError> {
            Ok(())
        }

        fn vendor_device_id(&self) -> (u16, u16) {
            (0, 0)
        }

        fn enable_irq(&self) {}

        fn disable_irq(&self) {
            self.disable_count.fetch_add(1, Ordering::Relaxed);
        }

        fn card_irq_ctrl(&self) -> Option<Arc<dyn SdioCardIrq>> {
            Some(Arc::new(RecordingCardIrq))
        }

        fn take_irq_source(&mut self) -> Option<alloc::boxed::Box<dyn SdioIrqSource>> {
            None
        }
    }

    #[test]
    fn shutdown_disarms_the_v3_card_source_and_controller_signal() {
        let writes = Arc::new(SpinLock::new(Vec::new()));
        let disable_count = Arc::new(AtomicUsize::new(0));
        let host = RecordingHost {
            writes: Arc::clone(&writes),
            disable_count: Arc::clone(&disable_count),
            fail_write: false,
        };
        let transport = SdioTransport::new(host, ChipVariant::Aic8800D80).unwrap();
        let bus = WifiBus::new(transport);
        *bus.state.lock() = BusState::Up;
        let state = Arc::new(AicDeferredState::new());
        state.publish_ready(Arc::clone(&bus)).unwrap();
        let mut control = AicPollIrqControl { state };

        control.shutdown().unwrap();

        assert_eq!(
            *writes.lock_irqsave(),
            alloc::vec![(1, SDIOWIFI_INTR_ENABLE_REG_V3, 0)]
        );
        assert_eq!(disable_count.load(Ordering::Relaxed), 1);
        assert_eq!(*bus.state.lock(), BusState::Down);
    }

    #[test]
    fn shutdown_write_failure_still_masks_the_controller_and_fails_closed() {
        let writes = Arc::new(SpinLock::new(Vec::new()));
        let disable_count = Arc::new(AtomicUsize::new(0));
        let host = RecordingHost {
            writes: Arc::clone(&writes),
            disable_count: Arc::clone(&disable_count),
            fail_write: true,
        };
        let transport = SdioTransport::new(host, ChipVariant::Aic8800D80).unwrap();
        let bus = WifiBus::new(transport);
        *bus.state.lock() = BusState::Up;
        let state = Arc::new(AicDeferredState::new());
        state.publish_ready(Arc::clone(&bus)).unwrap();
        let mut control = AicPollIrqControl { state };

        assert!(matches!(
            control.shutdown(),
            Err(NetError::DmaShutdownUnconfirmed)
        ));
        assert_eq!(
            *writes.lock_irqsave(),
            alloc::vec![(1, SDIOWIFI_INTR_ENABLE_REG_V3, 0)]
        );
        assert_eq!(disable_count.load(Ordering::Relaxed), 1);
        assert_eq!(*bus.state.lock(), BusState::Down);
    }
}

//! Queue-level AIC8800 network device parts.
//!
//! The complete device is consumed once into a controller endpoint, one poll
//! group, move-only RX/TX queues, and an owner-CPU wireless control endpoint.
//! No AIC background task, periodic poller, global callback, or complete-device
//! handle survives this split.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};
use core::sync::atomic::{Ordering, fence};

use rd_net::{
    DmaBuffer, IRxQueue, ITxQueue, NetControlEndpoint, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot,
    NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetPollIrqControl, NetQueueId,
    NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion, SubmitError, WifiControl,
    WifiOperation, WifiTransaction,
};
use rdif_eth::DriverGeneric;
use sdio_host::{SdioIrqSource, SdioIrqStatus};

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

/// AIC8800 device consumed by [`NetDevice::into_parts`].
pub struct AicWifiNetDev {
    bus: Arc<WifiBus>,
    chip: ChipVariant,
    fallback_mac: [u8; 6],
    irq_source: Box<dyn SdioIrqSource>,
    startup: Option<WifiTransaction>,
}

impl AicWifiNetDev {
    pub fn new(
        bus: Arc<WifiBus>,
        chip: ChipVariant,
        fallback_mac: [u8; 6],
        irq_source: Box<dyn SdioIrqSource>,
    ) -> Self {
        Self {
            bus,
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

    fn current_mac(&self) -> [u8; 6] {
        self.bus.conn.sta_mac.lock().unwrap_or(self.fallback_mac)
    }
}

impl DriverGeneric for AicWifiNetDev {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

struct AicNetControl {
    bus: Arc<WifiBus>,
    fallback_mac: [u8; 6],
}

impl NetControlEndpoint for AicNetControl {
    fn mac_address(&mut self) -> Result<[u8; 6], NetError> {
        Ok(self.bus.conn.sta_mac.lock().unwrap_or(self.fallback_mac))
    }
}

struct AicWifiControl {
    client: WifiClient,
    chip: ChipVariant,
    startup: Option<WifiTransaction>,
}

impl WifiControl for AicWifiControl {
    fn execute(&mut self, operation: &WifiOperation) -> Result<(), NetError> {
        match operation {
            WifiOperation::Connect { ssid, password } => {
                self.client
                    .lmac_configure(self.chip, 6000)
                    .map_err(net_err)?;
                let config = if password.is_empty() {
                    WifiConfig::open(ssid)
                } else {
                    WifiConfig::wpa2_psk(ssid, password)
                };
                self.client.connect(&config, 15000).map_err(net_err)
            }
            WifiOperation::Disconnect => self.client.disconnect().map_err(net_err),
            WifiOperation::StartOpenAccessPoint { ssid, channel } => self
                .client
                .start_ap_open(self.chip, ssid, *channel, 6000)
                .map(|_| ())
                .map_err(net_err),
        }
    }

    fn startup_transaction(&self) -> Option<WifiTransaction> {
        self.startup.clone()
    }
}

struct AicHardIrq {
    bus: Arc<WifiBus>,
    source: Box<dyn SdioIrqSource>,
}

impl NetHardIrqHandler for AicHardIrq {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        match self.source.handle_irq() {
            SdioIrqStatus::Spurious => NetHardIrqResult::Spurious,
            SdioIrqStatus::CardPending => {
                self.bus.rx.irq_pending.store(true, Ordering::Release);
                NetHardIrqResult::Schedule(NetIrqSnapshot::RX)
            }
        }
    }
}

struct AicPollIrqControl {
    bus: Arc<WifiBus>,
}

impl AicPollIrqControl {
    fn has_work(&self) -> bool {
        self.bus.rx.irq_pending.load(Ordering::Acquire)
            || !self.bus.rx.data_queue.lock().is_empty()
            || !self.bus.tx.completed.lock().is_empty()
            || tx::has_pending_work(&self.bus)
            || !self.bus.ap.assoc_queue.lock().is_empty()
            || !self.bus.ap.sta_del_queue.lock().is_empty()
    }
}

impl NetPollIrqControl for AicPollIrqControl {
    fn quiesce(&mut self) -> Result<(), NetError> {
        if *self.bus.state.lock() == BusState::Down {
            return Err(NetError::Stopped);
        }
        self.bus.transport.mask_card_irq();
        Ok(())
    }

    fn rearm_and_check(&mut self) -> Result<NetRearmResult, NetError> {
        if *self.bus.state.lock() == BusState::Down {
            return Err(NetError::Stopped);
        }
        if self.has_work() {
            return Ok(NetRearmResult::WorkPending(NetIrqSnapshot::all_queue_work()));
        }

        self.bus.transport.enable_irq();
        let controller_pending = self.bus.transport.rearm_and_check_card_irq();
        fence(Ordering::SeqCst);
        if controller_pending || self.has_work() {
            if controller_pending {
                self.bus.rx.irq_pending.store(true, Ordering::Release);
            }
            self.bus.transport.mask_card_irq();
            Ok(NetRearmResult::WorkPending(NetIrqSnapshot::all_queue_work()))
        } else {
            Ok(NetRearmResult::Idle)
        }
    }
}

impl NetDevice for AicWifiNetDev {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let mac = self.current_mac();
        let bus = self.bus;
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new(DEVICE_NAME, mac),
            control: Box::new(AicNetControl {
                bus: Arc::clone(&bus),
                fallback_mac: self.fallback_mac,
            }),
            wifi_control: Some(Box::new(AicWifiControl {
                client: WifiClient::new(Arc::clone(&bus)),
                chip: self.chip,
                startup: self.startup,
            })),
            poll_groups: alloc::vec![NetPollGroupParts {
                id: GROUP_ID,
                queues: NetQueuePairParts {
                    tx: Box::new(AicTxQueue {
                        bus: Arc::clone(&bus),
                    }),
                    rx: Box::new(AicRxQueue {
                        bus: Arc::clone(&bus),
                        rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
                    }),
                },
                irq_control: Box::new(AicPollIrqControl {
                    bus: Arc::clone(&bus),
                }),
                irq_endpoints: alloc::vec![NetHardIrqEndpoint::new(
                    IRQ_SOURCE_ID,
                    Box::new(AicHardIrq {
                        bus,
                        source: self.irq_source,
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
    bus: Arc<WifiBus>,
}

impl ITxQueue for AicTxQueue {
    fn id(&self) -> NetQueueId {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        aic_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if self.bus.conn.vif_idx.load(Ordering::Acquire) == 0xFF
            || self.bus.tx.pktcnt.load(Ordering::Acquire) >= MAX_TX_QUEUE_LEN as u32
        {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        let frame = buffer.read_with_cpu(buffer.len(), |packet| packet.to_vec());
        tx::enqueue_data_frame(&self.bus, frame, buffer)
            .map_err(|(_, buffer)| SubmitError::new(buffer, NetError::Retry))
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        let _ = tx::tx_process(&self.bus);
        tx::reclaim_completed(&self.bus)
    }
}

struct AicRxQueue {
    bus: Arc<WifiBus>,
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
        if self.bus.rx.data_queue.lock().is_empty() {
            let _ = rx::process_rx_frames(&self.bus, 64);
            let _ = ap::process_pending(&self.bus, 8);
            let _ = tx::tx_process(&self.bus);
        }
        let frame = self.bus.rx.data_queue.lock().pop_front()?;
        let mut buffer = self.rx_buffers.pop_front()?;
        let packet_len = frame.len().min(buffer.capacity());
        buffer.write_with_cpu(|target| target[..packet_len].copy_from_slice(&frame[..packet_len]));
        Some(RxCompletion { buffer, packet_len })
    }
}

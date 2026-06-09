//! AIC8800 Wi-Fi network device implementing the `rdif-eth` `Interface`.
//!
//! This module bridges the `WifiBus` TX/RX queues to the upstream network
//! stack via the `rd_net::Interface` + `ITxQueue`/`IRxQueue` traits. The
//! device is copy-based (frames are memcpy'd between the runtime's DMA buffers
//! and the SDIO TX/RX paths), modelled on the `fxmac` net driver.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig};
use rdif_eth::DriverGeneric;

use crate::fdrv::{core::bus::WifiBus, thread::tx};

const DEVICE_NAME: &str = "aic8800-wifi";
const QUEUE_ID: usize = 0;
const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const MAX_TX_QUEUE_LEN: usize = 128;

/// AIC8800 Wi-Fi network device.
///
/// Wraps a shared `WifiBus` and implements `rd_net::Interface` so the upstream
/// `RdNetDriver` adapter can drive Ethernet-level TX/RX.
pub struct AicWifiNetDev {
    bus: Arc<WifiBus>,
    mac: [u8; 6],
    tx_created: bool,
    rx_created: bool,
    irq_enabled: bool,
}

// SAFETY: WifiBus internals are protected by atomics and SpinNoIrq locks.
unsafe impl Send for AicWifiNetDev {}
unsafe impl Sync for AicWifiNetDev {}

impl AicWifiNetDev {
    pub fn new(bus: Arc<WifiBus>, mac: [u8; 6]) -> Self {
        Self {
            bus,
            mac,
            tx_created: false,
            rx_created: false,
            irq_enabled: false,
        }
    }
}

impl DriverGeneric for AicWifiNetDev {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl Interface for AicWifiNetDev {
    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(AicTxQueue {
            bus: self.bus.clone(),
            tx_done: VecDeque::with_capacity(QUEUE_SIZE),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(AicRxQueue {
            bus: self.bus.clone(),
            rx_buffers: VecDeque::with_capacity(QUEUE_SIZE),
        }))
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut event = Event::none();
        event.tx_queue.insert(QUEUE_ID);
        event.rx_queue.insert(QUEUE_ID);
        event
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

/// TX queue: copies the outgoing Ethernet frame out of the runtime buffer and
/// enqueues it to the Wi-Fi TX thread, which builds the HostDesc and sends it
/// over SDIO.
struct AicTxQueue {
    bus: Arc<WifiBus>,
    tx_done: VecDeque<u64>,
}

impl ITxQueue for AicTxQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        aic_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        // Backpressure: station not associated, or the TX queue is full.
        if self.bus.conn.vif_idx.load(Ordering::Acquire) == 0xFF
            || self.bus.tx.pktcnt.load(Ordering::Acquire) >= MAX_TX_QUEUE_LEN as u32
        {
            return Err(NetError::Retry);
        }
        let packet = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        let eth_frame: Vec<u8> = packet.to_vec();
        tx::enqueue_data_frame(&self.bus, eth_frame).map_err(|_| NetError::Retry)?;
        self.tx_done.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.tx_done.pop_front()
    }
}

/// RX queue: holds a pool of empty runtime buffers; on `reclaim` it drains one
/// received Ethernet frame from the Wi-Fi RX queue and copies it into a pooled
/// buffer.
struct AicRxQueue {
    bus: Arc<WifiBus>,
    rx_buffers: VecDeque<RuntimeBuffer>,
}

#[derive(Clone, Copy)]
struct RuntimeBuffer {
    virt: usize,
    bus_addr: u64,
    len: usize,
}

impl From<DmaBuffer> for RuntimeBuffer {
    fn from(buffer: DmaBuffer) -> Self {
        Self {
            virt: buffer.virt.as_ptr() as usize,
            bus_addr: buffer.bus_addr,
            len: buffer.len,
        }
    }
}

impl IRxQueue for AicRxQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn config(&self) -> QueueConfig {
        aic_queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.rx_buffers.push_back(buffer.into());
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        if self.rx_buffers.is_empty() {
            return None;
        }
        let frame = self.bus.rx.data_queue.lock().pop_front()?;
        let buffer = self.rx_buffers.pop_front()?;
        let len = core::cmp::min(frame.len(), buffer.len);
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer.virt as *mut u8, len);
        }
        Some((buffer.bus_addr, len))
    }
}

use ax_sync::Mutex;

/// 全局暂存：WiFi 网络设备（由集成层取出并通过 `register_net` 注册）
static PENDING_NET_DEV: Mutex<Option<AicWifiNetDev>> = Mutex::new(None);

/// 创建 WiFi 网络设备并存入全局，等待集成层取出注册。
pub fn store_wifi_net_device(bus: Arc<WifiBus>, mac: [u8; 6]) {
    let dev = AicWifiNetDev::new(bus, mac);
    *PENDING_NET_DEV.lock() = Some(dev);
    log::debug!("[aic8800] Wi-Fi net device stored");
}

/// 取出暂存的 WiFi 网络设备（一次性，取后清空）。
pub fn take_wifi_net_device() -> Option<AicWifiNetDev> {
    PENDING_NET_DEV.lock().take()
}

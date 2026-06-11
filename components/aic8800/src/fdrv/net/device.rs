//! AIC8800 Wi-Fi network device implementing the `rdif-eth` `Interface`.
//!
//! This module bridges the `WifiBus` TX/RX queues to the upstream network
//! stack via the `rd_net::Interface` + `ITxQueue`/`IRxQueue` traits. The
//! device is copy-based (frames are memcpy'd between the runtime's DMA buffers
//! and the SDIO TX/RX paths), modelled on the `fxmac` net driver.
//!
//! The same device object also carries the Wi-Fi *control plane* by
//! implementing [`rd_net::WifiControl`] (STA connect, SoftAP start, RX wake,
//! link policy). Data plane and control plane share one `Arc<WifiBus>`, so a
//! single object is both an [`Interface`] and a [`WifiControl`] — the upper
//! layers drive it through the generic net device model, with no Wi-Fi-specific
//! device type or registration path.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

use rd_net::{
    DmaBuffer, Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig, WifiControl,
    WifiLinkPolicy,
};
use rdif_eth::DriverGeneric;

use crate::{
    common::ChipVariant,
    fdrv::{
        core::bus::WifiBus, thread::rx::register_rx_data_callback, thread::tx,
        wifi::api::WifiClient, wifi::api::WifiConfig,
    },
};

const DEVICE_NAME: &str = "aic8800-wifi";
const QUEUE_ID: usize = 0;
const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const MAX_TX_QUEUE_LEN: usize = 128;

fn net_err(e: crate::fdrv::wifi::api::WifiError) -> NetError {
    NetError::Other(Box::new(e))
}

/// AIC8800 Wi-Fi network device.
///
/// Wraps a shared `WifiBus` and implements both `rd_net::Interface` (data
/// plane: Ethernet-level TX/RX) and `rd_net::WifiControl` (control plane:
/// STA/SoftAP control, RX wake, link policy). The control plane operates the
/// same `WifiBus` as the queues, so one object serves both roles.
pub struct AicWifiNetDev {
    bus: Arc<WifiBus>,
    client: WifiClient,
    chip: ChipVariant,
    mac: [u8; 6],
    link_policy: Option<WifiLinkPolicy>,
    tx_created: bool,
    rx_created: bool,
    irq_enabled: bool,
}

// SAFETY: WifiBus internals are protected by atomics and SpinNoIrq locks.
unsafe impl Send for AicWifiNetDev {}
unsafe impl Sync for AicWifiNetDev {}

impl AicWifiNetDev {
    pub fn new(bus: Arc<WifiBus>, chip: ChipVariant, mac: [u8; 6]) -> Self {
        Self {
            client: WifiClient::new(Arc::clone(&bus)),
            bus,
            chip,
            mac,
            link_policy: None,
            tx_created: false,
            rx_created: false,
            irq_enabled: false,
        }
    }

    /// Sets the link policy this device reports via
    /// [`WifiControl::link_policy`] (e.g. a board's SoftAP static IP + DHCP
    /// lease). Returns `self` for builder-style construction.
    pub fn with_link_policy(mut self, policy: WifiLinkPolicy) -> Self {
        self.link_policy = Some(policy);
        self
    }
}

impl DriverGeneric for AicWifiNetDev {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl WifiControl for AicWifiNetDev {
    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), NetError> {
        // STA mode needs LMAC configured first (SoftAP's start_ap_open does its
        // own configuration).
        self.client
            .lmac_configure(self.chip, 6000)
            .map_err(net_err)?;

        let config = if password.is_empty() {
            WifiConfig::open(ssid)
        } else {
            WifiConfig::wpa2_psk(ssid, password)
        };

        let mut last_err = None;
        for attempt in 0..2 {
            if attempt > 0 {
                log::info!("[aic8800] retrying connect (attempt {})...", attempt + 1);
                crate::runtime::runtime().sleep_ms(3000);
            }
            match self.client.connect(&config, 15000) {
                Ok(()) => {
                    log::info!("[aic8800] connected to '{}'", ssid);
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("[aic8800] connect attempt {} failed: {:?}", attempt + 1, e);
                    last_err = Some(e);
                }
            }
        }
        Err(net_err(last_err.unwrap_or(
            crate::fdrv::wifi::api::WifiError::OperationFailed("connect failed".into()),
        )))
    }

    fn disconnect(&mut self) -> Result<(), NetError> {
        self.client.disconnect().map_err(net_err)?;
        log::info!("[aic8800] disconnected");
        Ok(())
    }

    fn start_ap_open(&mut self, ssid: &[u8], channel: u8) -> Result<(), NetError> {
        let cfm = self
            .client
            .start_ap_open(self.chip, ssid, channel, 6000)
            .map_err(net_err)?;
        log::info!("[aic8800] AP started, APM_START_CFM={:02x?}", cfm);
        Ok(())
    }

    fn set_rx_wake(&mut self, wake: fn()) {
        register_rx_data_callback(wake);
    }

    fn link_policy(&self) -> Option<WifiLinkPolicy> {
        self.link_policy
    }
}

impl Interface for AicWifiNetDev {
    fn mac_address(&self) -> [u8; 6] {
        // Prefer the live MAC the firmware reported on the bus; fall back to the
        // value captured at construction.
        self.bus.conn.sta_mac.lock().unwrap_or(self.mac)
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

    fn wifi_control(&mut self) -> Option<&mut dyn WifiControl> {
        Some(self)
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

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};

use descriptor::{RING_END, RxDesc, TxDesc};
use dma_api::{DeviceDma, DmaOp};
use log::info;
use mmio_api::{Mmio, MmioAddr, MmioOp};
use queue::{QueueStart, QueueStartState, Rtl8125RxQueue, Rtl8125TxQueue};
use rdif_eth::{Event, IRxQueue, ITxQueue, Interface};
use registers::*;
use spin::Mutex;

mod descriptor;
mod hw;
mod queue;
mod registers;

const DRIVER_NAME: &str = "realtek-rtl8125";
const QUEUE_ID0: usize = 0;
const QUEUE_SIZE: usize = 256;
const RX_QUEUE_CONFIG_SIZE: usize = QUEUE_SIZE + 1;
const RX_START_THRESHOLD: usize = QUEUE_SIZE;
const MAX_PACKET: usize = 2048;
const RX_BUF_SIZE: usize = 2048;
const DMA_ALIGN: usize = 256;
const DMA_CACHE_LINE_SIZE: usize = 64;
const RX_DESC_PER_CACHE_LINE: usize = DMA_CACHE_LINE_SIZE / core::mem::size_of::<RxDesc>();
const RX_DEFERRED_REFILL_CAPACITY: usize = QUEUE_SIZE;
const LINK_DOWN_DROP_LOG_INTERVAL: u64 = 64;
const EARLY_PACKET_LOG_COUNT: u64 = 8;
const TX_SUBMIT_LOG_INTERVAL: u64 = 16;
const TX_RECLAIM_LOG_INTERVAL: u64 = 64;
const RX_RECLAIM_LOG_INTERVAL: u64 = 64;
const RX_IDLE_LOG_INTERVAL: u64 = 262_144;
const RX_OVERFLOW_REARM_IDLE_POLLS: u64 = 2048;
const TX_LINK_SAMPLE_INTERVAL: u64 = 64;
const OCP_STD_PHY_BASE: u32 = 0xa400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVersion {
    Rtl8125A,
    Rtl8125B,
    Unknown(u16),
}

#[derive(Debug, Clone, Copy)]
pub struct Rtl8125Status {
    pub phy_status: u8,
    pub chip_cmd: u8,
    pub mcu: u8,
    pub intr_status: u32,
    pub intr_mask: u32,
    pub rx_config: u32,
    pub tx_config: u32,
    pub cplus_cmd: u16,
    pub rx_desc_base: u64,
}

impl Rtl8125Status {
    pub const fn link_up(&self) -> bool {
        phy_status_link_up(self.phy_status)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("unsupported PCI id {vendor:#06x}:{device:#06x}")]
    UnsupportedPciId { vendor: u16, device: u16 },
    #[error("MMIO map failed")]
    MmioMap(#[from] mmio_api::MapError),
    #[error("DMA allocation failed")]
    Dma(#[from] dma_api::DmaError),
    #[error("invalid MAC address")]
    InvalidMacAddress,
    #[error("hardware reset timed out")]
    ResetTimeout,
    #[error("wait for {operation} timed out")]
    HardwareTimeout { operation: &'static str },
    #[error("invalid OCP register address {reg:#x}")]
    InvalidOcpAddress { reg: u32 },
}

pub type Result<T> = core::result::Result<T, Error>;

pub struct Rtl8125 {
    regs: Regs,
    _mmio: Mmio,
    dma: DeviceDma,
    mac: [u8; 6],
    chip: ChipVersion,
    tx_created: bool,
    rx_created: bool,
    phy_ocp_base: u32,
    queue_start: QueueStart,
}

impl Rtl8125 {
    pub fn check_vid_did(vendor: u16, device: u16) -> bool {
        vendor == VENDOR_ID && device == DEVICE_ID_RTL8125
    }

    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size.max(RTL8125_REGS_SIZE))?;
        let regs = Regs::new(mmio.as_nonnull_ptr());
        let dma = DeviceDma::new(dma_mask, dma_op);
        let xid = rtl8125_xid(regs);
        let chip = chip_version(xid);

        let mut dev = Self {
            regs,
            _mmio: mmio,
            dma,
            mac: [0; 6],
            chip,
            tx_created: false,
            rx_created: false,
            phy_ocp_base: OCP_STD_PHY_BASE,
            queue_start: Arc::new(Mutex::new(QueueStartState::default())),
        };
        dev.init()?;
        info!(
            "RTL8125 device initialized: chip={:?}, xid={:#x}, \
             mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, status={:?}",
            dev.chip,
            xid,
            dev.mac[0],
            dev.mac[1],
            dev.mac[2],
            dev.mac[3],
            dev.mac[4],
            dev.mac[5],
            dev.status(),
        );
        Ok(dev)
    }

    pub fn init(&mut self) -> Result<()> {
        self.disable_irq();
        self.ack_events(u32::MAX);
        self.reset()?;
        self.hw_init_8125()?;

        self.mac = self.read_mac_address()?;
        self.set_mac_address(self.mac);
        self.regs.configure_cplus(self.dma.dma_mask());
        self.regs.write_default_rx_config();
        self.regs.write_default_tx_config();
        self.regs.write_rx_max_size(RX_BUF_SIZE as u16 + 1);
        self.regs.disable_interrupt_mitigation();
        self.hw_start_8125()?;
        self.hw_phy_config()?;
        Ok(())
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    pub fn chip_version(&self) -> ChipVersion {
        self.chip
    }

    pub fn poll_link(&self) -> bool {
        self.status().link_up()
    }

    pub fn status(&self) -> Rtl8125Status {
        read_status(self.regs)
    }

    fn read_mac_address(&self) -> Result<[u8; 6]> {
        let mac = self.regs.read_backup_mac();
        if is_valid_mac(mac) {
            return Ok(mac);
        }

        let mac = self.regs.read_mac();
        if is_valid_mac(mac) {
            return Ok(mac);
        }

        Err(Error::InvalidMacAddress)
    }

    fn set_mac_address(&self, mac: [u8; 6]) {
        self.regs.unlock_config();
        self.regs.write_mac(mac);
        self.regs.lock_config();
    }

    fn reset(&self) -> Result<()> {
        self.regs.request_reset();
        for _ in 0..100_000 {
            if !self.regs.reset_pending() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::ResetTimeout)
    }
}

impl rdif_eth::DriverGeneric for Rtl8125 {
    fn name(&self) -> &str {
        DRIVER_NAME
    }
}

impl Interface for Rtl8125 {
    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }

        let mut desc = self
            .dma
            .coherent_array_zero_with_align::<TxDesc>(QUEUE_SIZE, DMA_ALIGN)
            .ok()?;
        desc.set(
            QUEUE_SIZE - 1,
            TxDesc {
                opts1: RING_END,
                opts2: 0,
                addr: 0,
            },
        );

        {
            let mut start = self.queue_start.lock();
            start.tx_base = Some(desc.dma_addr().as_u64());
        }
        self.tx_created = true;
        self.maybe_start_queues();

        Some(queue::boxed_tx(Rtl8125TxQueue {
            regs: self.regs,
            desc,
            dma_mask: self.dma.dma_mask(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            link_up: None,
            link_down_drops: 0,
            submitted: 0,
            reclaimed: 0,
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }

        let desc = self
            .dma
            .coherent_array_zero_with_align::<RxDesc>(QUEUE_SIZE, DMA_ALIGN)
            .ok()?;

        {
            let mut start = self.queue_start.lock();
            start.rx_base = Some(desc.dma_addr().as_u64());
        }
        self.rx_created = true;
        self.maybe_start_queues();

        Some(queue::boxed_rx(Rtl8125RxQueue {
            regs: self.regs,
            desc,
            dma_mask: self.dma.dma_mask(),
            start: self.queue_start.clone(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            idle_polls: 0,
            last_rx_rearm_idle: 0,
            submitted: 0,
            reclaimed: 0,
            rx_errors: 0,
            deferred_refill: VecDeque::with_capacity(RX_DEFERRED_REFILL_CAPACITY),
        }))
    }

    fn enable_irq(&mut self) {
        self.ack_events(u32::MAX);
        self.regs.write_interrupt_mask(DEFAULT_IRQ_MASK);
    }

    fn disable_irq(&mut self) {
        self.regs.write_interrupt_mask(0);
    }

    fn is_irq_enabled(&self) -> bool {
        self.regs.read_interrupt_mask() != 0
    }

    fn handle_irq(&mut self) -> Event {
        let status = self.regs.read_interrupt_status();
        if status == 0 || status == u32::MAX {
            return Event::none();
        }

        self.ack_events(status);

        let mut event = Event::none();
        if irq_has_tx_event(status) {
            event.tx_queue.insert(QUEUE_ID0);
        }
        if irq_has_rx_event(status) {
            event.rx_queue.insert(QUEUE_ID0);
        }
        if irq_has_link_change(status) {
            info!("RTL8125 irq link change: status={:?}", self.status());
        }
        event
    }
}

fn rtl8125_xid(regs: Regs) -> u16 {
    ((regs.read_tx_config() >> 20) & 0x0fcf) as u16
}

fn read_status(regs: Regs) -> Rtl8125Status {
    Rtl8125Status {
        phy_status: regs.read_phy_status(),
        chip_cmd: regs.read_chip_cmd(),
        mcu: regs.read_mcu(),
        intr_status: regs.read_interrupt_status(),
        intr_mask: regs.read_interrupt_mask(),
        rx_config: regs.read_rx_config(),
        tx_config: regs.read_tx_config(),
        cplus_cmd: regs.read_cplus_cmd(),
        rx_desc_base: regs.read_rx_desc_base(),
    }
}

pub(crate) fn set_rx_mode(regs: Regs) {
    regs.set_multicast_filter_all();
    regs.set_rx_accept_mode();
}

fn chip_version(xid: u16) -> ChipVersion {
    if xid & 0x07cf == 0x0641 {
        ChipVersion::Rtl8125B
    } else if xid & 0x07cf == 0x0609 {
        ChipVersion::Rtl8125A
    } else {
        ChipVersion::Unknown(xid)
    }
}

fn is_valid_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [0xff; 6] && mac[0] & 1 == 0
}

const _: () = {
    assert!(size_of::<TxDesc>() == 16);
    assert!(size_of::<RxDesc>() == 16);
};

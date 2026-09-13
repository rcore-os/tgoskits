#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_sync::SpinLock as Mutex;
use descriptor::{RING_END, RxDesc, TxDesc};
use dma_api::DeviceDma;
use log::info;
use mmio_api::{Mmio, MmioAddr, MmioOp};
use queue::{QueueStart, QueueStartState, Rtl8125RxQueue, Rtl8125TxQueue};
use rdif_eth::{
    FixedNetControl, NetDevice, NetDeviceInfo, NetDeviceParts, NetError, NetHardIrqEndpoint,
    NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot, NetIrqSourceId, NetPollGroupId,
    NetPollGroupParts, NetPollIrqControl, NetQueueId, NetQueuePairParts, NetRearmResult,
};
use registers::*;

mod descriptor;
mod hw;
mod queue;
mod registers;

const DRIVER_NAME: &str = "realtek-rtl8125";
const QUEUE_ID0: NetQueueId = NetQueueId::new(0);
const GROUP_ID0: NetPollGroupId = NetPollGroupId::new(0);
const IRQ_SOURCE0: NetIrqSourceId = NetIrqSourceId::new(0);
const QUEUE_SIZE: usize = 256;
const RX_QUEUE_CONFIG_SIZE: usize = QUEUE_SIZE + 1;
const RX_START_THRESHOLD: usize = QUEUE_SIZE;
const MAX_PACKET: usize = 2048;
const RX_BUF_SIZE: usize = 2048;
const DMA_ALIGN: usize = 256;
const LINK_DOWN_DROP_LOG_INTERVAL: u64 = 64;
const TX_SUBMIT_LOG_INTERVAL: u64 = 16;
const TX_RECLAIM_LOG_INTERVAL: u64 = 64;
const RX_RECLAIM_LOG_INTERVAL: u64 = 64;
const RX_IDLE_LOG_INTERVAL: u64 = 262_144;
const RX_OVERFLOW_REARM_IDLE_POLLS: u64 = 2048;
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
    phy_ocp_base: u32,
    queue_start: QueueStart,
    link_up: Arc<AtomicBool>,
}

impl Rtl8125 {
    pub fn check_vid_did(vendor: u16, device: u16) -> bool {
        vendor == VENDOR_ID && device == DEVICE_ID_RTL8125
    }

    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma: DeviceDma,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size.max(RTL8125_REGS_SIZE))?;
        let regs = Regs::new(mmio.as_nonnull_ptr());
        let xid = rtl8125_xid(regs);
        let chip = chip_version(xid);

        let mut dev = Self {
            regs,
            _mmio: mmio,
            dma,
            mac: [0; 6],
            chip,
            phy_ocp_base: OCP_STD_PHY_BASE,
            queue_start: Arc::new(Mutex::new(QueueStartState::default())),
            link_up: Arc::new(AtomicBool::new(false)),
        };
        dev.init()?;
        dev.link_up.store(dev.regs.link_up(), Ordering::Release);
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
        self.regs.write_interrupt_mask(0);
        self.ack_events(u32::MAX);
        self.reset()?;
        self.hw_init_8125()?;

        self.mac = self.read_mac_address()?;
        self.set_mac_address(self.mac);
        self.regs
            .configure_cplus(self.dma.info().constraints().addr_mask);
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

impl NetDevice for Rtl8125 {
    fn into_parts(self: Box<Self>) -> core::result::Result<NetDeviceParts, NetError> {
        let Rtl8125 {
            regs,
            _mmio,
            dma,
            mac,
            chip: _,
            phy_ocp_base: _,
            queue_start,
            link_up,
        } = *self;

        let mut tx_desc = dma
            .coherent_array_zero_with_align::<TxDesc>(QUEUE_SIZE, DMA_ALIGN)
            .map_err(NetError::from)?;
        tx_desc.set_cpu(
            QUEUE_SIZE - 1,
            TxDesc {
                opts1: RING_END,
                opts2: 0,
                addr: 0,
            },
        );

        {
            // SAFETY: queue servicing excludes local re-entry and this raw
            // lock serializes concurrent queue state across CPUs.
            let mut start = unsafe { queue_start.lock_raw() };
            start.tx_base = Some(tx_desc.dma_addr().as_u64());
        }

        let tx = Rtl8125TxQueue {
            regs,
            desc: tx_desc,
            dma_mask: dma.info().constraints().addr_mask,
            buffers: core::array::from_fn(|_| None),
            next_submit: 0,
            next_reclaim: 0,
            link_up: Arc::clone(&link_up),
            link_down_drops: 0,
            submitted: 0,
            reclaimed: 0,
            notification: queue::TxNotificationState::default(),
        };

        let rx_desc = dma
            .coherent_array_zero_with_align::<RxDesc>(QUEUE_SIZE, DMA_ALIGN)
            .map_err(NetError::from)?;

        {
            // SAFETY: see the matching queue-start acquisition above.
            let mut start = unsafe { queue_start.lock_raw() };
            start.rx_base = Some(rx_desc.dma_addr().as_u64());
        }

        let rx = Rtl8125RxQueue {
            regs,
            desc: rx_desc,
            dma_mask: dma.info().constraints().addr_mask,
            start: queue_start.clone(),
            buffers: core::array::from_fn(|_| None),
            next_submit: 0,
            next_reclaim: 0,
            idle_polls: 0,
            last_rx_rearm_idle: 0,
            submitted: 0,
            reclaimed: 0,
            rx_errors: 0,
        };

        Ok(NetDeviceParts {
            info: NetDeviceInfo::new(DRIVER_NAME, mac),
            control: Box::new(FixedNetControl::new(mac)),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: GROUP_ID0,
                queues: NetQueuePairParts {
                    tx: queue::boxed_tx(tx),
                    rx: queue::boxed_rx(rx),
                },
                irq_control: Box::new(Rtl8125IrqControl {
                    regs,
                    _mmio,
                    queue_start,
                    link_up: Arc::clone(&link_up),
                }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    IRQ_SOURCE0,
                    Box::new(Rtl8125IrqHandler { regs, link_up }),
                )],
            }],
        })
    }
}

struct Rtl8125IrqControl {
    regs: Regs,
    _mmio: Mmio,
    queue_start: QueueStart,
    link_up: Arc<AtomicBool>,
}

impl NetPollIrqControl for Rtl8125IrqControl {
    fn quiesce(&mut self) -> core::result::Result<(), NetError> {
        self.regs.write_interrupt_mask(0);
        self.regs.commit();
        Ok(())
    }

    fn shutdown(&mut self) -> core::result::Result<(), NetError> {
        self.regs.write_interrupt_mask(0);
        self.regs.disable_tx_rx();
        self.regs.commit();
        self.regs.request_reset();
        for _ in 0..100_000 {
            if !self.regs.reset_pending() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(NetError::DmaShutdownUnconfirmed)
    }

    fn rearm_and_check(
        &mut self,
        _now_nanos: u64,
    ) -> core::result::Result<NetRearmResult, NetError> {
        let started = {
            // SAFETY: the owner CPU is the only task-context mutator after
            // publication; this read only checks initialization completion.
            unsafe { self.queue_start.lock_raw() }.started
        };
        if !started {
            return Err(NetError::InvalidParts);
        }

        let before_status = self.regs.read_interrupt_status();
        let before = rtl8125_irq_snapshot(before_status);
        self.regs.write_interrupt_status(u32::MAX);
        self.regs.write_interrupt_mask(DEFAULT_IRQ_MASK);
        self.regs.commit();
        let after_status = self.regs.read_interrupt_status();
        if irq_has_link_change(before_status | after_status) {
            self.link_up.store(self.regs.link_up(), Ordering::Release);
        }
        let pending = before.union(rtl8125_irq_snapshot(after_status));
        if pending == NetIrqSnapshot::empty() {
            Ok(NetRearmResult::Idle)
        } else {
            self.regs.write_interrupt_mask(0);
            self.regs.write_interrupt_status(after_status);
            self.regs.commit();
            Ok(NetRearmResult::WorkPending(pending))
        }
    }
}

struct Rtl8125IrqHandler {
    regs: Regs,
    link_up: Arc<AtomicBool>,
}

impl NetHardIrqHandler for Rtl8125IrqHandler {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        let status = self.regs.read_interrupt_status();
        let snapshot = rtl8125_irq_snapshot(status);
        if snapshot == NetIrqSnapshot::empty() {
            return NetHardIrqResult::Spurious;
        }

        if irq_has_link_change(status) {
            self.link_up.store(self.regs.link_up(), Ordering::Release);
        }

        self.regs.write_interrupt_mask(0);
        self.regs.write_interrupt_status(status);
        self.regs.commit();
        NetHardIrqResult::Schedule(snapshot)
    }
}

fn rtl8125_irq_snapshot(status: u32) -> NetIrqSnapshot {
    if status == 0 || status == u32::MAX {
        return NetIrqSnapshot::empty();
    }

    let mut snapshot = NetIrqSnapshot::empty();
    if irq_has_tx_event(status) {
        snapshot = snapshot.union(NetIrqSnapshot::TX);
    }
    if irq_has_rx_event(status) {
        snapshot = snapshot.union(NetIrqSnapshot::RX);
    }
    if irq_has_link_change(status) {
        snapshot = snapshot.union(NetIrqSnapshot::ERROR);
    }
    snapshot
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

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::mem::size_of;

use descriptor::{RING_END, RxDesc, TxDesc};
use dma_api::{DArray, DeviceDma, DmaDirection, DmaOp};
use log::info;
use mmio_api::{Mmio, MmioAddr, MmioOp};
use rdif_eth::{Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig};
use registers::*;

mod descriptor;
mod registers;

const DRIVER_NAME: &str = "realtek-rtl8125";
const QUEUE_ID0: usize = 0;
const QUEUE_SIZE: usize = 256;
const MAX_PACKET: usize = 2048;
const RX_BUF_SIZE: usize = 2048;
const DMA_ALIGN: usize = 256;
const RTL8125_REGS_SIZE: usize = 0x100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVersion {
    Rtl8125A,
    Rtl8125B,
    Unknown(u16),
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
}

pub type Result<T> = core::result::Result<T, Error>;

pub struct Rtl8125 {
    regs: Regs,
    _mmio: Mmio,
    dma: DeviceDma,
    mac: [u8; 6],
    chip: ChipVersion,
    irq_enabled: bool,
    tx_created: bool,
    rx_created: bool,
    tx_desc_base: Option<u64>,
    rx_desc_base: Option<u64>,
    started: bool,
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
            irq_enabled: false,
            tx_created: false,
            rx_created: false,
            tx_desc_base: None,
            rx_desc_base: None,
            started: false,
        };
        dev.init()?;
        info!(
            "RTL8125 device initialized: chip={:?}, xid={:#x}, \
             mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            dev.chip, xid, dev.mac[0], dev.mac[1], dev.mac[2], dev.mac[3], dev.mac[4], dev.mac[5]
        );
        Ok(dev)
    }

    pub fn init(&mut self) -> Result<()> {
        self.disable_irq();
        self.ack_events(u32::MAX);
        self.reset();
        self.hw_init_8125();

        self.mac = self.read_mac_address()?;
        let mut cp_cmd = (self.regs.read16(CPLUS_CMD) & CPCMD_MASK) | PCIMULRW;
        if self.dma.dma_mask() > u32::MAX as u64 {
            cp_cmd |= PCIDAC;
        }
        self.regs.write16(CPLUS_CMD, cp_cmd);
        self.regs
            .write32(RX_CONFIG, RX_FETCH_DFLT_8125 | RX_DMA_BURST);
        self.regs.write32(TX_CONFIG, TX_DMA_BURST | INTER_FRAME_GAP);
        self.regs.write16(RX_MAX_SIZE, RX_BUF_SIZE as u16 + 1);
        self.regs.write16(INTR_MITIGATE, 0);
        self.hw_start_8125();
        Ok(())
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    pub fn chip_version(&self) -> ChipVersion {
        self.chip
    }

    pub fn poll_link(&self) -> bool {
        self.regs.read8(PHY_STATUS) & LINK_STATUS != 0
    }

    fn read_mac_address(&self) -> Result<[u8; 6]> {
        let mac = self.regs.read_mac(MAC0_BKP);
        if is_valid_mac(mac) {
            return Ok(mac);
        }

        let mac = self.regs.read_mac(MAC0);
        if is_valid_mac(mac) {
            return Ok(mac);
        }

        Err(Error::InvalidMacAddress)
    }

    fn reset(&self) {
        self.regs
            .write8(CHIP_CMD, self.regs.read8(CHIP_CMD) | CMD_RESET);
        for _ in 0..100_000 {
            if self.regs.read8(CHIP_CMD) & CMD_RESET == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }

    fn hw_init_8125(&self) {
        self.regs.write8(
            CHIP_CMD,
            self.regs.read8(CHIP_CMD) & !(CMD_TX_ENB | CMD_RX_ENB),
        );
        spin_delay(10_000);
        self.regs.write8(MCU, self.regs.read8(MCU) & !NOW_IS_OOB);

        self.mac_ocp_modify(0xe8de, 1 << 14, 0);
        self.wait_link_list_ready();
        self.mac_ocp_write(0xc0aa, 0x07d0);
        self.mac_ocp_write(0xc0a6, 0x0150);
        self.mac_ocp_write(0xc01e, 0x5555);
        self.wait_link_list_ready();
    }

    fn hw_start_8125(&self) {
        for offset in (0x0a00..0x0b00).step_by(4) {
            self.regs.write32(offset, 0);
        }

        match self.chip {
            ChipVersion::Rtl8125A => self.ephy_init(&RTL8125A_EPHY),
            ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => self.ephy_init(&RTL8125B_EPHY),
        }

        self.hw_start_8125_common();
    }

    fn hw_start_8125_common(&self) {
        self.regs.write16(0x0382, 0x221b);
        self.regs.write8(0x4500, 0);
        self.regs.write16(0x4800, 0);
        self.mac_ocp_modify(0xd40a, 0x0010, 0);
        self.regs.write8(CONFIG1, self.regs.read8(CONFIG1) & !0x10);
        self.mac_ocp_write(0xc140, 0xffff);
        self.mac_ocp_write(0xc142, 0xffff);
        self.mac_ocp_modify(0xd3e2, 0x0fff, 0x03a9);
        self.mac_ocp_modify(0xd3e4, 0x00ff, 0);
        self.mac_ocp_modify(0xe860, 0, 0x0080);
        self.mac_ocp_modify(0xeb58, 0x0001, 0);

        if self.chip == ChipVersion::Rtl8125B {
            self.mac_ocp_modify(0xe614, 0x0700, 0x0200);
            self.mac_ocp_modify(0xe63e, 0x0c30, 0);
        } else {
            self.mac_ocp_modify(0xe614, 0x0700, 0x0400);
            self.mac_ocp_modify(0xe63e, 0x0c30, 0x0020);
        }

        self.mac_ocp_modify(0xc0b4, 0, 0x000c);
        self.mac_ocp_modify(0xeb6a, 0x00ff, 0x0033);
        self.mac_ocp_modify(0xeb50, 0x03e0, 0x0040);
        self.mac_ocp_modify(0xe056, 0x00f0, 0x0030);
        self.mac_ocp_modify(0xe040, 0x1000, 0);
        self.mac_ocp_modify(0xea1c, 0x0003, 0x0001);
        self.mac_ocp_modify(0xe0c0, 0x4f0f, 0x4403);
        self.mac_ocp_modify(0xe052, 0x0080, 0x0068);
        self.mac_ocp_modify(0xd430, 0x0fff, 0x047f);
        self.mac_ocp_modify(0xea1c, 0x0004, 0);
        self.mac_ocp_modify(0xeb54, 0, 0x0001);
        spin_delay(100);
        self.mac_ocp_modify(0xeb54, 0x0001, 0);
        self.regs
            .write16(0x1880, self.regs.read16(0x1880) & !0x0030);
        self.mac_ocp_write(0xe098, 0xc302);
        self.mac_ocp_modify(0xe040, 0, 0x0003);
    }

    fn maybe_start_queues(&mut self) {
        if self.started {
            return;
        }
        let (Some(tx_base), Some(rx_base)) = (self.tx_desc_base, self.rx_desc_base) else {
            return;
        };

        self.regs.write8(CFG9346, CFG9346_UNLOCK);
        self.regs
            .write32(TX_DESC_START_ADDR_HIGH, (tx_base >> 32) as u32);
        self.regs.write32(TX_DESC_START_ADDR_LOW, tx_base as u32);
        self.regs.write32(RX_DESC_ADDR_HIGH, (rx_base >> 32) as u32);
        self.regs.write32(RX_DESC_ADDR_LOW, rx_base as u32);
        self.regs.write8(CFG9346, CFG9346_LOCK);

        let dma_mask = self.dma.dma_mask();
        info!("RTL8125 queue DMA bases: tx={tx_base:#x}, rx={rx_base:#x}, mask={dma_mask:#x}");
        self.regs.write8(CHIP_CMD, CMD_TX_ENB | CMD_RX_ENB);
        self.regs
            .write32(RX_CONFIG, RX_FETCH_DFLT_8125 | RX_DMA_BURST);
        self.regs.write32(TX_CONFIG, TX_DMA_BURST | INTER_FRAME_GAP);
        self.set_rx_mode();
        self.regs.commit();
        self.started = true;
    }

    fn set_rx_mode(&self) {
        let rx_mode = ACCEPT_BROADCAST | ACCEPT_MULTICAST | ACCEPT_MY_PHYS;
        let rx_config = self.regs.read32(RX_CONFIG);
        self.regs
            .write32(RX_CONFIG, (rx_config & !RX_CONFIG_ACCEPT_OK_MASK) | rx_mode);
    }

    fn ack_events(&self, bits: u32) {
        self.regs.write32(INTR_STATUS_8125, bits);
    }

    fn mac_ocp_write(&self, reg: u32, data: u16) {
        if reg & 0xffff_0001 != 0 {
            return;
        }
        self.regs
            .write32(OCPDR, 0x8000_0000 | (reg << 15) | u32::from(data));
    }

    fn mac_ocp_read(&self, reg: u32) -> u16 {
        if reg & 0xffff_0001 != 0 {
            return 0;
        }
        self.regs.write32(OCPDR, reg << 15);
        self.regs.read32(OCPDR) as u16
    }

    fn mac_ocp_modify(&self, reg: u32, mask: u16, set: u16) {
        let data = self.mac_ocp_read(reg);
        self.mac_ocp_write(reg, (data & !mask) | set);
    }

    fn ephy_read(&self, reg: u32) -> u16 {
        self.regs.write32(EPHYAR, (reg & 0x1f) << 16);
        for _ in 0..1_000 {
            if self.regs.read32(EPHYAR) & 0x8000_0000 != 0 {
                return self.regs.read32(EPHYAR) as u16;
            }
            core::hint::spin_loop();
        }
        u16::MAX
    }

    fn ephy_write(&self, reg: u32, value: u16) {
        self.regs
            .write32(EPHYAR, 0x8000_0000 | (reg & 0x1f) << 16 | u32::from(value));
        for _ in 0..1_000 {
            if self.regs.read32(EPHYAR) & 0x8000_0000 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        spin_delay(1_000);
    }

    fn ephy_init(&self, entries: &[EphyInfo]) {
        for entry in entries {
            let value = (self.ephy_read(entry.offset.into()) & !entry.mask) | entry.bits;
            self.ephy_write(entry.offset.into(), value);
        }
    }

    fn wait_link_list_ready(&self) {
        for _ in 0..4_200 {
            if self.regs.read8(MCU) & 0x02 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
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
            .array_zero_with_align::<TxDesc>(QUEUE_SIZE, DMA_ALIGN, DmaDirection::Bidirectional)
            .ok()?;
        desc.set(
            QUEUE_SIZE - 1,
            TxDesc {
                opts1: RING_END,
                opts2: 0,
                addr: 0,
            },
        );

        self.tx_desc_base = Some(desc.dma_addr().as_u64());
        self.tx_created = true;
        self.maybe_start_queues();

        Some(Box::new(Rtl8125TxQueue {
            regs: self.regs,
            desc,
            dma_mask: self.dma.dma_mask(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }

        let desc = self
            .dma
            .array_zero_with_align::<RxDesc>(QUEUE_SIZE, DMA_ALIGN, DmaDirection::Bidirectional)
            .ok()?;

        self.rx_desc_base = Some(desc.dma_addr().as_u64());
        self.rx_created = true;
        self.maybe_start_queues();

        Some(Box::new(Rtl8125RxQueue {
            desc,
            dma_mask: self.dma.dma_mask(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
        }))
    }

    fn enable_irq(&mut self) {
        self.ack_events(u32::MAX);
        self.regs.write32(INTR_MASK_8125, DEFAULT_IRQ_MASK);
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.regs.write32(INTR_MASK_8125, 0);
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let status = self.regs.read32(INTR_STATUS_8125);
        if status == 0 || status == u32::MAX {
            return Event::none();
        }

        self.ack_events(status);

        let mut event = Event::none();
        if status & (TX_OK | TX_ERR | TX_DESC_UNAVAIL) != 0 {
            event.tx_queue.insert(QUEUE_ID0);
        }
        if status & (RX_OK | RX_ERR | RX_FIFO_OVER | RX_OVERFLOW) != 0 {
            event.rx_queue.insert(QUEUE_ID0);
        }
        event
    }
}

struct Rtl8125TxQueue {
    regs: Regs,
    desc: DArray<TxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
}

impl ITxQueue for Rtl8125TxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: DMA_ALIGN,
            buf_size: MAX_PACKET,
            ring_size: QUEUE_SIZE,
        }
    }

    fn submit(&mut self, bus_addr: u64, len: usize) -> core::result::Result<(), NetError> {
        if len > MAX_PACKET {
            return Err(NetError::NotSupported);
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if next == self.next_reclaim && self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        self.desc.set(idx, TxDesc::new(bus_addr, len, ring_end));
        self.bus_addrs[idx] = Some(bus_addr);
        self.next_submit = next;
        self.regs.write16(TX_POLL_8125, 1);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let idx = self.next_reclaim;
        self.bus_addrs[idx]?;
        let desc = self.desc.read(idx)?;
        if desc.is_owned_by_hw() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx].take()
    }
}

struct Rtl8125RxQueue {
    desc: DArray<RxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
}

impl IRxQueue for Rtl8125RxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: DMA_ALIGN,
            buf_size: RX_BUF_SIZE,
            ring_size: QUEUE_SIZE,
        }
    }

    fn submit(&mut self, bus_addr: u64, len: usize) -> core::result::Result<(), NetError> {
        if len < RX_BUF_SIZE {
            return Err(NetError::NotSupported);
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if next == self.next_reclaim && self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        self.desc
            .set(idx, RxDesc::new(bus_addr, RX_BUF_SIZE, ring_end));
        self.bus_addrs[idx] = Some(bus_addr);
        self.next_submit = next;
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let idx = self.next_reclaim;
        let bus_addr = self.bus_addrs[idx]?;
        let desc = self.desc.read(idx)?;
        if desc.is_owned_by_hw() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx] = None;

        if desc.has_error() || !desc.is_whole_packet() {
            return Some((bus_addr, 0));
        }
        Some((bus_addr, desc.packet_len()))
    }
}

fn rtl8125_xid(regs: Regs) -> u16 {
    ((regs.read32(TX_CONFIG) >> 20) & 0x0fcf) as u16
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

fn spin_delay(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

#[derive(Clone, Copy)]
struct EphyInfo {
    offset: u8,
    mask: u16,
    bits: u16,
}

const RTL8125A_EPHY: [EphyInfo; 12] = [
    EphyInfo {
        offset: 0x04,
        mask: 0xffff,
        bits: 0xd000,
    },
    EphyInfo {
        offset: 0x0a,
        mask: 0xffff,
        bits: 0x8653,
    },
    EphyInfo {
        offset: 0x23,
        mask: 0xffff,
        bits: 0xab66,
    },
    EphyInfo {
        offset: 0x20,
        mask: 0xffff,
        bits: 0x9455,
    },
    EphyInfo {
        offset: 0x21,
        mask: 0xffff,
        bits: 0x99ff,
    },
    EphyInfo {
        offset: 0x29,
        mask: 0xffff,
        bits: 0xfe04,
    },
    EphyInfo {
        offset: 0x44,
        mask: 0xffff,
        bits: 0xd000,
    },
    EphyInfo {
        offset: 0x4a,
        mask: 0xffff,
        bits: 0x8653,
    },
    EphyInfo {
        offset: 0x63,
        mask: 0xffff,
        bits: 0xab66,
    },
    EphyInfo {
        offset: 0x60,
        mask: 0xffff,
        bits: 0x9455,
    },
    EphyInfo {
        offset: 0x61,
        mask: 0xffff,
        bits: 0x99ff,
    },
    EphyInfo {
        offset: 0x69,
        mask: 0xffff,
        bits: 0xfe04,
    },
];

const RTL8125B_EPHY: [EphyInfo; 6] = [
    EphyInfo {
        offset: 0x0b,
        mask: 0xffff,
        bits: 0xa908,
    },
    EphyInfo {
        offset: 0x1e,
        mask: 0xffff,
        bits: 0x20eb,
    },
    EphyInfo {
        offset: 0x4b,
        mask: 0xffff,
        bits: 0xa908,
    },
    EphyInfo {
        offset: 0x5e,
        mask: 0xffff,
        bits: 0x20eb,
    },
    EphyInfo {
        offset: 0x22,
        mask: 0x0030,
        bits: 0x0020,
    },
    EphyInfo {
        offset: 0x62,
        mask: 0x0030,
        bits: 0x0020,
    },
];

const _: () = {
    assert!(size_of::<TxDesc>() == 16);
    assert!(size_of::<RxDesc>() == 16);
};

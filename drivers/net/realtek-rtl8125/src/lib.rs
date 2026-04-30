#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{Ordering as AtomicOrdering, fence};

use descriptor::{RING_END, RxDesc, TxDesc};
use dma_api::{DArray, DeviceDma, DmaDirection, DmaOp};
use log::{info, warn};
use mmio_api::{Mmio, MmioAddr, MmioOp};
use rdif_eth::{Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig};
use registers::*;
use spin::Mutex;

mod descriptor;
mod registers;

const DRIVER_NAME: &str = "realtek-rtl8125";
const QUEUE_ID0: usize = 0;
const QUEUE_SIZE: usize = 256;
const RX_QUEUE_CONFIG_SIZE: usize = QUEUE_SIZE + 1;
const RX_START_THRESHOLD: usize = QUEUE_SIZE;
const MAX_PACKET: usize = 2048;
const RX_BUF_SIZE: usize = 2048;
const DMA_ALIGN: usize = 256;
const OCP_STD_PHY_BASE: u32 = 0xa400;
const EEE_TXIDLE_TIMER_VALUE: u16 = 1500 + 14 + 0x20;
const LINK_DOWN_DROP_LOG_INTERVAL: u64 = 64;
const EARLY_PACKET_LOG_COUNT: u64 = 8;
const TX_SUBMIT_LOG_INTERVAL: u64 = 16;
const TX_RECLAIM_LOG_INTERVAL: u64 = 64;
const RX_RECLAIM_LOG_INTERVAL: u64 = 64;
const TX_LINK_SAMPLE_INTERVAL: u64 = 64;

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

type QueueStart = Arc<Mutex<QueueStartState>>;

#[derive(Default)]
struct QueueStartState {
    tx_base: Option<u64>,
    rx_base: Option<u64>,
    rx_ready: bool,
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

    fn hw_init_8125(&self) -> Result<()> {
        self.enable_rxdv_gate();
        self.regs.disable_tx_rx();
        spin_delay(10_000);
        self.regs.clear_now_is_oob();

        self.mac_ocp_modify(0xe8de, 1 << 14, 0)?;
        self.wait_link_list_ready();
        self.mac_ocp_write(0xc0aa, 0x07d0)?;
        self.mac_ocp_write(0xc0a6, 0x0150)?;
        self.mac_ocp_write(0xc01e, 0x5555)?;
        self.wait_link_list_ready();
        Ok(())
    }

    fn hw_start_8125(&mut self) -> Result<()> {
        for offset in (0x0a00..0x0b00).step_by(4) {
            self.regs.write_vendor_u32(offset, 0);
        }

        self.set_aspm_clkreq(false)?;
        match self.chip {
            ChipVersion::Rtl8125A => self.ephy_init(&RTL8125A_EPHY)?,
            ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => self.ephy_init(&RTL8125B_EPHY)?,
        }

        self.hw_start_8125_common()
    }

    fn hw_start_8125_common(&self) -> Result<()> {
        self.regs.clear_ready_to_l23();
        self.regs.write_vendor_u16(0x0382, 0x221b);
        self.regs.write_vendor_u8(0x4500, 0);
        self.regs.write_vendor_u16(0x4800, 0);
        self.mac_ocp_modify(0xd40a, 0x0010, 0)?;
        self.regs.clear_speed_down();
        self.mac_ocp_write(0xc140, 0xffff)?;
        self.mac_ocp_write(0xc142, 0xffff)?;
        self.mac_ocp_modify(0xd3e2, 0x0fff, 0x03a9)?;
        self.mac_ocp_modify(0xd3e4, 0x00ff, 0)?;
        self.mac_ocp_modify(0xe860, 0, 0x0080)?;
        self.mac_ocp_modify(0xeb58, 0x0001, 0)?;

        if self.chip == ChipVersion::Rtl8125B {
            self.mac_ocp_modify(0xe614, 0x0700, 0x0200)?;
            self.mac_ocp_modify(0xe63e, 0x0c30, 0)?;
        } else {
            self.mac_ocp_modify(0xe614, 0x0700, 0x0400)?;
            self.mac_ocp_modify(0xe63e, 0x0c30, 0x0020)?;
        }

        self.mac_ocp_modify(0xc0b4, 0, 0x000c)?;
        self.mac_ocp_modify(0xeb6a, 0x00ff, 0x0033)?;
        self.mac_ocp_modify(0xeb50, 0x03e0, 0x0040)?;
        self.mac_ocp_modify(0xe056, 0x00f0, 0x0030)?;
        self.mac_ocp_modify(0xe040, 0x1000, 0)?;
        self.mac_ocp_modify(0xea1c, 0x0003, 0x0001)?;
        self.mac_ocp_modify(0xe0c0, 0x4f0f, 0x4403)?;
        self.mac_ocp_modify(0xe052, 0x0080, 0x0068)?;
        self.mac_ocp_modify(0xd430, 0x0fff, 0x047f)?;
        self.mac_ocp_modify(0xea1c, 0x0004, 0)?;
        self.mac_ocp_modify(0xeb54, 0, 0x0001)?;
        spin_delay(100);
        self.mac_ocp_modify(0xeb54, 0x0001, 0)?;
        self.regs.clear_vendor_u16_bits(0x1880, 0x0030);
        self.mac_ocp_write(0xe098, 0xc302)?;
        self.wait_mac_ocp_e00e_low();
        self.config_eee_mac()?;
        self.disable_rxdv_gate();
        Ok(())
    }

    fn maybe_start_queues(&mut self) {
        try_start_queues(self.regs, self.dma.dma_mask(), &self.queue_start);
    }

    fn enable_rxdv_gate(&self) {
        self.regs.enable_rxdv_gate();
        spin_delay(2_000);
        self.wait_rxtx_empty();
    }

    fn disable_rxdv_gate(&self) {
        self.regs.disable_rxdv_gate();
    }

    fn wait_rxtx_empty(&self) {
        for _ in 0..4_200 {
            if self.regs.rxtx_empty() {
                return;
            }
            core::hint::spin_loop();
        }
        warn!("RTL8125: timed out waiting for RX/TX FIFO empty");
    }

    fn wait_mac_ocp_e00e_low(&self) {
        for _ in 0..10 {
            match self.mac_ocp_read(0xe00e) {
                Ok(value) if value & (1 << 13) == 0 => return,
                Ok(_) => spin_delay(1_000),
                Err(err) => {
                    warn!("RTL8125: failed to read MAC OCP 0xe00e: {err:?}");
                    return;
                }
            }
        }
        warn!("RTL8125: timed out waiting for MAC OCP 0xe00e bit 13 to clear");
    }

    fn set_aspm_clkreq(&self, enable: bool) -> Result<()> {
        if enable {
            self.regs.set_aspm_clkreq(true);
            self.mac_ocp_modify(0xe094, 0xff00, 0)?;
            self.mac_ocp_modify(0xe092, 0x00ff, 1 << 2)?;
        } else {
            self.mac_ocp_modify(0xe092, 0x00ff, 0)?;
            self.regs.set_aspm_clkreq(false);
        }
        spin_delay(100);
        Ok(())
    }

    fn config_eee_mac(&self) -> Result<()> {
        if self.chip == ChipVersion::Rtl8125B {
            self.regs.write_eee_txidle_timer(EEE_TXIDLE_TIMER_VALUE);
        } else {
            self.mac_ocp_modify(0xeb62, 0, (1 << 2) | (1 << 1))?;
        }
        self.mac_ocp_modify(0xe040, 0, (1 << 1) | 1)
    }

    fn ack_events(&self, bits: u32) {
        self.regs.write_interrupt_status(bits);
    }

    fn mac_ocp_write(&self, reg: u32, data: u16) -> Result<()> {
        validate_ocp_reg(reg)?;
        self.regs.start_mac_ocp_write(reg, data);
        Ok(())
    }

    fn mac_ocp_read(&self, reg: u32) -> Result<u16> {
        validate_ocp_reg(reg)?;
        self.regs.start_mac_ocp_read(reg);
        Ok(self.regs.read_mac_ocp_data())
    }

    fn mac_ocp_modify(&self, reg: u32, mask: u16, set: u16) -> Result<()> {
        let data = self.mac_ocp_read(reg)?;
        self.mac_ocp_write(reg, (data & !mask) | set)
    }

    fn ephy_read(&self, reg: u32) -> Result<u16> {
        self.regs.start_ephy_read(reg);
        for _ in 0..1_000 {
            if self.regs.ephy_ready() {
                return Ok(self.regs.read_ephy_data());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "EPHY read",
        })
    }

    fn ephy_write(&self, reg: u32, value: u16) -> Result<()> {
        self.regs.start_ephy_write(reg, value);
        for _ in 0..1_000 {
            if !self.regs.ephy_ready() {
                spin_delay(1_000);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "EPHY write",
        })
    }

    fn ephy_init(&self, entries: &[EphyInfo]) -> Result<()> {
        for entry in entries {
            let value = (self.ephy_read(entry.offset.into())? & !entry.mask) | entry.bits;
            self.ephy_write(entry.offset.into(), value)?;
        }
        Ok(())
    }

    fn phy_ocp_write(&self, reg: u32, data: u16) -> Result<()> {
        validate_ocp_reg(reg)?;
        self.regs.start_phy_ocp_write(reg, data);
        for _ in 0..1_000 {
            if !self.regs.phy_ocp_busy() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "PHY OCP write",
        })
    }

    fn phy_ocp_read(&self, reg: u32) -> Result<u16> {
        validate_ocp_reg(reg)?;
        self.regs.start_phy_ocp_read(reg);
        for _ in 0..1_000 {
            if self.regs.phy_ocp_busy() {
                return Ok(self.regs.read_phy_ocp_data());
            }
            core::hint::spin_loop();
        }
        Err(Error::HardwareTimeout {
            operation: "PHY OCP read",
        })
    }

    fn phy_reg_addr(&self, reg: u32) -> u32 {
        let reg = if self.phy_ocp_base == OCP_STD_PHY_BASE {
            reg
        } else {
            reg.saturating_sub(0x10)
        };
        self.phy_ocp_base + reg * 2
    }

    fn phy_write(&mut self, reg: u32, value: u16) -> Result<()> {
        if reg == 0x1f {
            self.phy_ocp_base = if value == 0 {
                OCP_STD_PHY_BASE
            } else {
                u32::from(value) << 4
            };
            return Ok(());
        }

        self.phy_ocp_write(self.phy_reg_addr(reg), value)
    }

    fn phy_read(&self, reg: u32) -> Result<u16> {
        if reg == 0x1f {
            return Ok(if self.phy_ocp_base == OCP_STD_PHY_BASE {
                0
            } else {
                (self.phy_ocp_base >> 4) as u16
            });
        }

        self.phy_ocp_read(self.phy_reg_addr(reg))
    }

    fn phy_modify(&mut self, reg: u32, mask: u16, set: u16) -> Result<()> {
        let data = self.phy_read(reg)?;
        self.phy_write(reg, (data & !mask) | set)
    }

    fn phy_write_paged(&mut self, page: u16, reg: u32, value: u16) -> Result<()> {
        let old_page = self.phy_read(0x1f)?;
        self.phy_write(0x1f, page)?;
        self.phy_write(reg, value)?;
        self.phy_write(0x1f, old_page)
    }

    fn phy_modify_paged(&mut self, page: u16, reg: u32, mask: u16, set: u16) -> Result<()> {
        let old_page = self.phy_read(0x1f)?;
        self.phy_write(0x1f, page)?;
        self.phy_modify(reg, mask, set)?;
        self.phy_write(0x1f, old_page)
    }

    fn phy_param(&mut self, param: u16, mask: u16, set: u16) -> Result<()> {
        let old_page = self.phy_read(0x1f)?;
        self.phy_write(0x1f, 0x0a43)?;
        self.phy_write(0x13, param)?;
        self.phy_modify(0x14, mask, set)?;
        self.phy_write(0x1f, old_page)
    }

    fn config_eee_phy_8125a(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0a43, 0x11, 0, 1 << 4)?;
        self.phy_modify_paged(0x0a4a, 0x11, 0, 1 << 9)?;
        self.phy_modify_paged(0x0a42, 0x14, 0, 1 << 7)?;
        self.phy_modify_paged(0x0a6d, 0x12, 1, 0)?;
        self.phy_modify_paged(0x0a6d, 0x14, 1 << 4, 0)
    }

    fn config_eee_phy_8125b(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0a6d, 0x12, 1, 0)?;
        self.phy_modify_paged(0x0a6d, 0x14, 1 << 4, 0)?;
        self.phy_modify_paged(0x0a42, 0x14, 1 << 7, 0)?;
        self.phy_modify_paged(0x0a4a, 0x11, 1 << 9, 0)
    }

    fn hw_phy_config(&mut self) -> Result<()> {
        match self.chip {
            ChipVersion::Rtl8125A => self.hw_phy_config_8125a(),
            ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => self.hw_phy_config_8125b(),
        }
    }

    fn hw_phy_config_8125a(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0ad4, 0x17, 0, 0x0010)?;
        self.phy_modify_paged(0x0ad1, 0x13, 0x03ff, 0x03ff)?;
        self.phy_modify_paged(0x0ad3, 0x11, 0x003f, 0x0006)?;
        self.phy_modify_paged(0x0ac0, 0x14, 0x1100, 0)?;
        self.phy_modify_paged(0x0acc, 0x10, 0x0003, 0x0002)?;
        self.phy_modify_paged(0x0ad4, 0x10, 0x00e7, 0x0044)?;
        self.phy_modify_paged(0x0ac1, 0x12, 0x0080, 0)?;
        self.phy_modify_paged(0x0ac8, 0x10, 0x0300, 0)?;
        self.phy_modify_paged(0x0ac5, 0x17, 0x0007, 0x0002)?;
        self.phy_write_paged(0x0ad4, 0x16, 0x00a8)?;
        self.phy_write_paged(0x0ac5, 0x16, 0x01ff)?;
        self.phy_modify_paged(0x0ac8, 0x15, 0x00f0, 0x0030)?;

        self.phy_write(0x1f, 0x0b87)?;
        self.phy_write(0x16, 0x80a2)?;
        self.phy_write(0x17, 0x0153)?;
        self.phy_write(0x16, 0x809c)?;
        self.phy_write(0x17, 0x0153)?;
        self.phy_write(0x1f, 0)?;

        self.phy_param(0x8257, 0xffff, 0x020f)?;
        self.phy_param(0x80ea, 0xffff, 0x7843)?;
        self.phy_modify_paged(0x0d06, 0x14, 0, 0x2000)?;
        self.phy_param(0x81a2, 0, 0x0100)?;
        self.phy_modify_paged(0x0b54, 0x16, 0xff00, 0xdb00)?;
        self.phy_modify_paged(0x0a45, 0x12, 0x0001, 0)?;
        self.phy_modify_paged(0x0a5d, 0x12, 0, 0x0020)?;
        self.phy_modify_paged(0x0ad4, 0x17, 0x0010, 0)?;
        self.phy_modify_paged(0x0a86, 0x15, 0x0001, 0)?;
        self.phy_modify_paged(0x0a44, 0x11, 0, 1 << 11)?;
        self.config_eee_phy_8125a()
    }

    fn hw_phy_config_8125b(&mut self) -> Result<()> {
        self.phy_modify_paged(0x0a44, 0x11, 0, 0x0800)?;
        self.phy_modify_paged(0x0ac4, 0x13, 0x00f0, 0x0090)?;
        self.phy_modify_paged(0x0ad3, 0x10, 0x0003, 0x0001)?;

        self.phy_write(0x1f, 0x0b87)?;
        self.phy_write(0x16, 0x80f5)?;
        self.phy_write(0x17, 0x760e)?;
        self.phy_write(0x16, 0x8107)?;
        self.phy_write(0x17, 0x360e)?;
        self.phy_write(0x16, 0x8551)?;
        self.phy_modify(0x17, 0xff00, 0x0800)?;
        self.phy_write(0x1f, 0)?;

        self.phy_modify_paged(0x0bf0, 0x10, 0xe000, 0xa000)?;
        self.phy_modify_paged(0x0bf4, 0x13, 0x0f00, 0x0300)?;
        for param in [
            0x8044, 0x804a, 0x8050, 0x8056, 0x805c, 0x8062, 0x8068, 0x806e, 0x8074, 0x807a,
        ] {
            self.phy_param(param, 0xffff, 0x2417)?;
        }
        self.phy_modify_paged(0x0a4c, 0x15, 0, 0x0040)?;
        self.phy_modify_paged(0x0bf8, 0x12, 0xe000, 0xa000)?;
        self.phy_modify_paged(0x0a5b, 0x12, 1 << 15, 0)?;
        self.config_eee_phy_8125b()
    }

    fn wait_link_list_ready(&self) {
        for _ in 0..4_200 {
            if self.regs.link_list_ready() {
                return;
            }
            core::hint::spin_loop();
        }
        warn!("RTL8125: timed out waiting for link-list FIFO ready");
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

        {
            let mut start = self.queue_start.lock();
            start.tx_base = Some(desc.dma_addr().as_u64());
        }
        self.tx_created = true;
        self.maybe_start_queues();

        Some(Box::new(Rtl8125TxQueue {
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
            .array_zero_with_align::<RxDesc>(QUEUE_SIZE, DMA_ALIGN, DmaDirection::Bidirectional)
            .ok()?;

        {
            let mut start = self.queue_start.lock();
            start.rx_base = Some(desc.dma_addr().as_u64());
        }
        self.rx_created = true;
        self.maybe_start_queues();

        Some(Box::new(Rtl8125RxQueue {
            regs: self.regs,
            desc,
            dma_mask: self.dma.dma_mask(),
            start: self.queue_start.clone(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
            submitted: 0,
            reclaimed: 0,
            rx_errors: 0,
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

struct Rtl8125TxQueue {
    regs: Regs,
    desc: DArray<TxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
    link_up: Option<bool>,
    link_down_drops: u64,
    submitted: u64,
    reclaimed: u64,
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

        if !self.observe_link_before_tx(len) {
            self.link_down_drops = self.link_down_drops.saturating_add(1);
            return Err(NetError::Retry);
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = TxDesc::new_cpu_owned(bus_addr, len, ring_end);
        self.desc.set(idx, desc);
        release_dma_descriptor();
        self.desc.set(idx, desc.release_to_hw());
        self.bus_addrs[idx] = Some(bus_addr);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        self.regs.poll_tx();
        if self.submitted <= EARLY_PACKET_LOG_COUNT
            || self.submitted.is_multiple_of(TX_SUBMIT_LOG_INTERVAL)
        {
            info!(
                "RTL8125 tx submitted: idx={idx}, len={len}, submitted={}, reclaimed={}, \
                 status={:?}",
                self.submitted,
                self.reclaimed,
                read_status(self.regs),
            );
        }
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
        let bus_addr = self.bus_addrs[idx].take()?;
        self.reclaimed = self.reclaimed.saturating_add(1);
        if self.reclaimed <= EARLY_PACKET_LOG_COUNT
            || self.reclaimed.is_multiple_of(TX_RECLAIM_LOG_INTERVAL)
        {
            info!(
                "RTL8125 tx reclaimed: idx={idx}, len={}, submitted={}, reclaimed={}, status={:?}",
                desc.len(),
                self.submitted,
                self.reclaimed,
                read_status(self.regs),
            );
        }
        Some(bus_addr)
    }
}

impl Rtl8125TxQueue {
    fn observe_link_before_tx(&mut self, len: usize) -> bool {
        let must_sample = self.link_up != Some(true)
            || self.submitted == 0
            || self.submitted.is_multiple_of(TX_LINK_SAMPLE_INTERVAL);
        if !must_sample {
            return true;
        }

        let link_up = self.regs.link_up();
        let changed = self.link_up.replace(link_up) != Some(link_up);

        if link_up {
            if changed {
                let status = read_status(self.regs);
                info!("RTL8125 tx link up before submit: len={len}, status={status:?}");
            }
        } else if changed
            || self.link_down_drops == 0
            || self
                .link_down_drops
                .is_multiple_of(LINK_DOWN_DROP_LOG_INTERVAL)
        {
            let status = read_status(self.regs);
            warn!(
                "RTL8125 tx link down before submit: len={len}, dropped_tx={}, status={status:?}",
                self.link_down_drops
            );
        }

        link_up
    }
}

struct Rtl8125RxQueue {
    regs: Regs,
    desc: DArray<RxDesc>,
    dma_mask: u64,
    start: QueueStart,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
    submitted: usize,
    reclaimed: u64,
    rx_errors: u64,
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
            ring_size: RX_QUEUE_CONFIG_SIZE,
        }
    }

    fn submit(&mut self, bus_addr: u64, len: usize) -> core::result::Result<(), NetError> {
        if len < RX_BUF_SIZE {
            return Err(NetError::NotSupported);
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = RxDesc::new_cpu_owned(bus_addr, RX_BUF_SIZE, ring_end);
        self.desc.set(idx, desc);
        release_dma_descriptor();
        self.desc.set(idx, desc.release_to_hw());
        self.bus_addrs[idx] = Some(bus_addr);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        if self.submitted >= RX_START_THRESHOLD {
            let was_ready = {
                let mut start = self.start.lock();
                let was_ready = start.rx_ready;
                start.rx_ready = true;
                was_ready
            };
            if !was_ready {
                let last_opts1 = self.desc.read(QUEUE_SIZE - 1).map_or(0, |desc| desc.opts1);
                info!(
                    "RTL8125 rx ring ready: submitted={}, last_desc_opts1={:#x}",
                    self.submitted, last_opts1
                );
            }
            try_start_queues(self.regs, self.dma_mask, &self.start);
        }
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let idx = self.next_reclaim;
        let bus_addr = self.bus_addrs[idx]?;
        let desc = self.desc.read(idx)?;
        if desc.is_owned_by_hw() {
            return None;
        }
        acquire_dma_descriptor();
        let desc = self.desc.read(idx)?;

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx] = None;

        if desc.has_error() || !desc.is_whole_packet() {
            self.rx_errors = self.rx_errors.saturating_add(1);
            warn!(
                "RTL8125 rx error: idx={idx}, opts1={:#x}, submitted={}, reclaimed={}, errors={}, \
                 status={:?}",
                desc.opts1,
                self.submitted,
                self.reclaimed,
                self.rx_errors,
                read_status(self.regs),
            );
            return Some((bus_addr, 0));
        }
        let len = desc.packet_len();
        self.reclaimed = self.reclaimed.saturating_add(1);
        if self.reclaimed <= EARLY_PACKET_LOG_COUNT
            || self.reclaimed.is_multiple_of(RX_RECLAIM_LOG_INTERVAL)
        {
            info!(
                "RTL8125 rx packet: idx={idx}, len={len}, submitted={}, reclaimed={}, status={:?}",
                self.submitted,
                self.reclaimed,
                read_status(self.regs),
            );
        }
        Some((bus_addr, len))
    }
}

fn release_dma_descriptor() {
    fence(AtomicOrdering::Release);
}

fn acquire_dma_descriptor() {
    fence(AtomicOrdering::Acquire);
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
    }
}

fn try_start_queues(regs: Regs, dma_mask: u64, start: &QueueStart) {
    let (tx_base, rx_base) = {
        let mut start = start.lock();
        if start.started || !start.rx_ready {
            return;
        }
        let (Some(tx_base), Some(rx_base)) = (start.tx_base, start.rx_base) else {
            return;
        };
        start.started = true;
        (tx_base, rx_base)
    };

    regs.unlock_config();
    regs.write_tx_desc_base(tx_base);
    regs.write_rx_desc_base(rx_base);
    regs.lock_config();

    info!("RTL8125 queue DMA bases: tx={tx_base:#x}, rx={rx_base:#x}, mask={dma_mask:#x}");
    regs.enable_tx_rx();
    regs.write_default_rx_config();
    regs.write_default_tx_config();
    regs.write_interrupt_status(u32::MAX);
    set_rx_mode(regs);
    regs.write_interrupt_mask(DEFAULT_IRQ_MASK);
    regs.commit();
    info!("RTL8125 queues started: status={:?}", read_status(regs));
}

fn set_rx_mode(regs: Regs) {
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

fn validate_ocp_reg(reg: u32) -> Result<()> {
    if reg & 0xffff_0001 == 0 {
        Ok(())
    } else {
        Err(Error::InvalidOcpAddress { reg })
    }
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

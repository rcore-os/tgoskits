use core::ptr::NonNull;

pub const VENDOR_ID: u16 = 0x10ec;
pub const DEVICE_ID_RTL8125: u16 = 0x8125;

pub const MAC0: usize = 0x0000;
pub const MAC4: usize = 0x0004;
pub const MAR0: usize = 0x0008;
pub const TX_DESC_START_ADDR_LOW: usize = 0x0020;
pub const TX_DESC_START_ADDR_HIGH: usize = 0x0024;
pub const INTR_MASK_8125: usize = 0x0038;
pub const INTR_STATUS_8125: usize = 0x003c;
pub const TX_CONFIG: usize = 0x0040;
pub const RX_CONFIG: usize = 0x0044;
pub const CFG9346: usize = 0x0050;
pub const CONFIG1: usize = 0x0052;
pub const CONFIG2: usize = 0x0053;
pub const CONFIG3: usize = 0x0054;
pub const CONFIG5: usize = 0x0056;
pub const PHY_STATUS: usize = 0x006c;
pub const EPHYAR: usize = 0x0080;
pub const TX_POLL_8125: usize = 0x0090;
pub const OCPDR: usize = 0x00b0;
pub const GPHY_OCP: usize = 0x00b8;
pub const MCU: usize = 0x00d3;
pub const RX_MAX_SIZE: usize = 0x00da;
pub const CPLUS_CMD: usize = 0x00e0;
pub const RX_DESC_ADDR_LOW: usize = 0x00e4;
pub const RX_DESC_ADDR_HIGH: usize = 0x00e8;
pub const INTR_MITIGATE: usize = 0x00e2;
pub const MISC: usize = 0x00f0;
pub const MAC0_BKP: usize = 0x19e0;
pub const EEE_TXIDLE_TIMER_8125: usize = 0x6048;

pub const CMD_RESET: u8 = 0x10;
pub const CMD_RX_ENB: u8 = 0x08;
pub const CMD_TX_ENB: u8 = 0x04;

pub const CFG9346_LOCK: u8 = 0x00;
pub const CFG9346_UNLOCK: u8 = 0xc0;

pub const LINK_STATUS: u8 = 0x02;
pub const RDY_TO_L23: u8 = 1 << 1;
pub const ASPM_EN: u8 = 1 << 0;
pub const CLK_REQ_EN: u8 = 1 << 7;
pub const NOW_IS_OOB: u8 = 0x80;
pub const TX_EMPTY: u8 = 1 << 5;
pub const RX_EMPTY: u8 = 1 << 4;
pub const RXTX_EMPTY: u8 = TX_EMPTY | RX_EMPTY;
pub const RXDV_GATED_EN: u32 = 1 << 19;

pub const RX_DMA_BURST: u32 = 7 << 8;
pub const RX_FETCH_DFLT_8125: u32 = 8 << 27;
pub const ACCEPT_BROADCAST: u32 = 0x08;
pub const ACCEPT_MULTICAST: u32 = 0x04;
pub const ACCEPT_MY_PHYS: u32 = 0x02;
pub const RX_CONFIG_ACCEPT_OK_MASK: u32 = 0x0f;

pub const TX_DMA_BURST: u32 = 7 << 8;
pub const INTER_FRAME_GAP: u32 = 3 << 24;

pub const PCIMULRW: u16 = 1 << 3;
pub const PCIDAC: u16 = 1 << 4;
pub const CPCMD_MASK: u16 = (1 << 13) | (1 << 6) | (1 << 5) | 0x3;

pub const TX_DESC_UNAVAIL: u32 = 0x0080;
pub const RX_FIFO_OVER: u32 = 0x0040;
pub const LINK_CHG: u32 = 0x0020;
pub const RX_OVERFLOW: u32 = 0x0010;
pub const TX_ERR: u32 = 0x0008;
pub const TX_OK: u32 = 0x0004;
pub const RX_ERR: u32 = 0x0002;
pub const RX_OK: u32 = 0x0001;

pub const DEFAULT_IRQ_MASK: u32 = LINK_CHG | RX_OVERFLOW | TX_ERR | TX_OK | RX_ERR | RX_OK;

#[derive(Clone, Copy)]
pub struct Regs {
    base: NonNull<u8>,
}

unsafe impl Send for Regs {}
unsafe impl Sync for Regs {}

impl Regs {
    pub fn new(base: NonNull<u8>) -> Self {
        Self { base }
    }

    #[inline]
    pub fn read8(&self, offset: usize) -> u8 {
        unsafe { self.base.as_ptr().add(offset).cast::<u8>().read_volatile() }
    }

    #[inline]
    pub fn write8(&self, offset: usize, value: u8) {
        unsafe {
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u8>()
                .write_volatile(value);
        }
    }

    #[inline]
    pub fn read16(&self, offset: usize) -> u16 {
        unsafe { self.base.as_ptr().add(offset).cast::<u16>().read_volatile() }
    }

    #[inline]
    pub fn write16(&self, offset: usize, value: u16) {
        unsafe {
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u16>()
                .write_volatile(value);
        }
    }

    #[inline]
    pub fn read32(&self, offset: usize) -> u32 {
        unsafe { self.base.as_ptr().add(offset).cast::<u32>().read_volatile() }
    }

    #[inline]
    pub fn write32(&self, offset: usize, value: u32) {
        unsafe {
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value);
        }
    }

    pub fn commit(&self) {
        let _ = self.read8(super::registers::CHIP_CMD);
    }

    pub fn read_mac(&self, offset: usize) -> [u8; 6] {
        [
            self.read8(offset),
            self.read8(offset + 1),
            self.read8(offset + 2),
            self.read8(offset + 3),
            self.read8(offset + 4),
            self.read8(offset + 5),
        ]
    }
}

pub const CHIP_CMD: usize = 0x0037;

use alloc::sync::Arc;
use core::time::Duration;

use mmio_api::Mmio;

use crate::Error;

pub(crate) const BAR_SIZE: usize = 0x4000;
const MAC_BASE: usize = 0x2000;
pub(crate) const MAC_CONFIG: usize = MAC_BASE;
pub(crate) const PACKET_FILTER: usize = MAC_BASE + 8;
pub(crate) const MAC_VERSION: usize = MAC_BASE + 0x110;
pub(crate) const HW_FEATURE1: usize = MAC_BASE + 0x120;
pub(crate) const MAC_IRQ_ENABLE: usize = MAC_BASE + 0xb4;
pub(crate) const MDIO_ADDRESS: usize = MAC_BASE + 0x200;
pub(crate) const MDIO_DATA: usize = MAC_BASE + 0x204;
pub(crate) const MAC_ADDRESS_HIGH: usize = MAC_BASE + 0x300;
pub(crate) const MAC_ADDRESS_LOW: usize = MAC_BASE + 0x304;
pub(crate) const RX_QUEUE_CONTROL: usize = MAC_BASE + 0xa0;
pub(crate) const MTL_RX_DMA_MAP: usize = MAC_BASE + 0xc30;
pub(crate) const MTL_TX_OPERATION: usize = MAC_BASE + 0xd00;
pub(crate) const MTL_RX_OPERATION: usize = MAC_BASE + 0xd30;
pub(crate) const DMA_MODE: usize = MAC_BASE + 0x1000;
pub(crate) const DMA_BUS_MODE: usize = MAC_BASE + 0x1004;
pub(crate) const CHANNEL_CONTROL: usize = MAC_BASE + 0x1100;
pub(crate) const TX_CONTROL: usize = MAC_BASE + 0x1104;
pub(crate) const RX_CONTROL: usize = MAC_BASE + 0x1108;
pub(crate) const TX_BASE_HIGH: usize = MAC_BASE + 0x1110;
pub(crate) const TX_BASE_LOW: usize = MAC_BASE + 0x1114;
pub(crate) const RX_BASE_HIGH: usize = MAC_BASE + 0x1118;
pub(crate) const RX_BASE_LOW: usize = MAC_BASE + 0x111c;
pub(crate) const TX_TAIL: usize = MAC_BASE + 0x1120;
pub(crate) const RX_TAIL: usize = MAC_BASE + 0x1128;
pub(crate) const TX_RING_LENGTH: usize = MAC_BASE + 0x112c;
pub(crate) const RX_RING_LENGTH: usize = MAC_BASE + 0x1130;
pub(crate) const IRQ_ENABLE: usize = MAC_BASE + 0x1134;
pub(crate) const IRQ_STATUS: usize = MAC_BASE + 0x1160;

/// Every endpoint retains the mapping. Register ownership is split between
/// the control endpoint (filter), queue owner (DMA), and IRQ endpoint (status).
#[derive(Clone)]
pub(crate) struct Registers(pub(crate) Arc<Mmio>);

impl Registers {
    pub(crate) fn read(&self, offset: usize) -> u32 {
        u32::from_le(self.0.read(offset))
    }

    pub(crate) fn write(&self, offset: usize, value: u32) {
        self.0.write(offset, value.to_le());
    }

    pub(crate) fn update(&self, offset: usize, clear: u32, set: u32) {
        self.write(offset, (self.read(offset) & !clear) | set);
    }

    pub(crate) fn commit(&self) {
        mbarrier::mb();
        let _ = self.read(IRQ_ENABLE);
    }

    pub(crate) fn wait_clear(
        &self,
        offset: usize,
        mask: u32,
        delay: fn(Duration),
        operation: &'static str,
    ) -> Result<(), Error> {
        for _ in 0..2000 {
            if self.read(offset) & mask == 0 {
                return Ok(());
            }
            delay(Duration::from_micros(10));
        }
        Err(Error::Timeout { operation })
    }

    pub(crate) fn mdio_read(&self, register: u8, delay: fn(Duration)) -> Result<u16, Error> {
        self.wait_clear(MDIO_ADDRESS, 1, delay, "MDIO idle")?;
        // The integrated PHY is at address zero. CSR clock is 25 MHz.
        self.write(
            MDIO_ADDRESS,
            u32::from(register) << 16 | 2 << 8 | 3 << 2 | 1,
        );
        self.wait_clear(MDIO_ADDRESS, 1, delay, "MDIO read")?;
        Ok(self.read(MDIO_DATA) as u16)
    }

    pub(crate) fn mdio_write(
        &self,
        register: u8,
        value: u16,
        delay: fn(Duration),
    ) -> Result<(), Error> {
        self.wait_clear(MDIO_ADDRESS, 1, delay, "MDIO idle")?;
        self.write(MDIO_DATA, u32::from(value));
        self.write(
            MDIO_ADDRESS,
            u32::from(register) << 16 | 2 << 8 | 1 << 2 | 1,
        );
        self.wait_clear(MDIO_ADDRESS, 1, delay, "MDIO write")
    }

    pub(crate) fn phy_ext_read(&self, register: u16, delay: fn(Duration)) -> Result<u16, Error> {
        self.mdio_write(0x1e, register, delay)?;
        self.mdio_read(0x1f, delay)
    }

    pub(crate) fn phy_ext_write(
        &self,
        register: u16,
        value: u16,
        delay: fn(Duration),
    ) -> Result<(), Error> {
        self.mdio_write(0x1e, register, delay)?;
        self.mdio_write(0x1f, value, delay)
    }

    pub(crate) fn reset_dma(&self, delay: fn(Duration)) -> Result<(), Error> {
        self.write(IRQ_ENABLE, 0);
        self.update(TX_CONTROL, 1, 0);
        self.update(RX_CONTROL, 1, 0);
        self.write(MAC_CONFIG, 0);
        self.write(DMA_MODE, 1);
        self.wait_clear(DMA_MODE, 1, delay, "DMA reset")
    }
}

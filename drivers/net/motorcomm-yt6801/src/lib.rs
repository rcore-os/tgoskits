#![no_std]

//! Motorcomm YT6801 Ethernet controller with one owned RX/TX poll group.

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec};
use core::time::Duration;

use dma_api::DeviceDma;
use mmio_api::{MmioAddr, MmioOp};
use queue::{Ring, RxQueue, TxQueue};
use rdif_eth::{
    NetControlEndpoint, NetDevice, NetDeviceInfo, NetDeviceParts, NetError, NetHardIrqEndpoint,
    NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot, NetIrqSourceId, NetPollGroupId,
    NetPollGroupParts, NetPollIrqControl, NetQueuePairParts, NetRearmResult, NetRxMode,
};
use registers::*;

mod queue;
mod registers;

const NAME: &str = "motorcomm-yt6801";
const RING_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const DMA_ALIGN: usize = 128;
const DESCRIPTOR_WORDS: usize = 4;
const DESCRIPTOR_BYTES: usize = 16;
const OWN: u32 = 1 << 31;
const RX_EVENT: u32 = 1 << 6 | 1 << 7 | 1 << 8;
const TX_EVENT: u32 = 1;
const BUS_ERROR: u32 = 1 << 12;

/// Initialization failures leave all driver-owned DMA engines stopped.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MMIO mapping failed: {0}")]
    Mmio(#[from] mmio_api::MapError),
    #[error("BAR0 does not contain the complete YT6801 register window")]
    InvalidBar,
    #[error("hardware timeout during {operation}")]
    Timeout { operation: &'static str },
    #[error("eFuse contains no valid MAC address")]
    InvalidMac,
    #[error("unsupported integrated PHY {0:#010x}")]
    UnsupportedPhy(u32),
    #[error("unsupported DMA address width")]
    InvalidDmaWidth,
    #[error("initial Ethernet link did not become ready")]
    LinkTimeout,
}

/// Prepared controller; DMA rings are allocated when the runtime takes it.
pub struct Yt6801 {
    regs: Registers,
    dma: DeviceDma,
    mac: [u8; 6],
    irq_mask: u32,
}

impl rdif_eth::DriverGeneric for Yt6801 {
    fn name(&self) -> &str {
        NAME
    }
}

impl Yt6801 {
    /// Maps and initializes BAR0. `delay` is used only for bounded startup waits.
    pub fn new(
        bar: impl Into<MmioAddr>,
        size: usize,
        dma: DeviceDma,
        mmio_op: &'static dyn MmioOp,
        delay: fn(Duration),
    ) -> Result<Self, Error> {
        let bar = bar.into();
        if size < BAR_SIZE
            || !bar.as_usize().is_multiple_of(core::mem::align_of::<u32>())
            || bar.as_usize().checked_add(size).is_none()
        {
            return Err(Error::InvalidBar);
        }
        mmio_api::init(mmio_op);
        let regs = Registers(Arc::new(mmio_api::ioremap(bar, size)?));
        regs.update(0x152c, 1 << 31, 0);
        regs.update(0x152c, 0, 1 << 31);
        regs.update(0x1004, 0, 1);
        delay(Duration::from_millis(5));
        // Disable out-of-band wake: otherwise DMA IRQs do not reach the host.
        regs.write(0x1010, 1);
        regs.write(0x1100, 0xffff_0000);
        regs.write(0x1108, 200 << 16 | 200);
        let mac = read_mac(&regs, delay)?;
        initialize_phy(&regs, delay)?;
        regs.reset_dma(delay)?;
        let feature = regs.read(HW_FEATURE1);
        let width = match (feature >> 14) & 3 {
            0 => 32,
            1 => 40,
            2 => 48,
            _ => return Err(Error::InvalidDmaWidth),
        };
        let mut constraints = dma.info().constraints();
        constraints.addr_mask &= (1u64 << width) - 1;
        let dma = dma.with_constraints(constraints);
        let version = regs.read(MAC_VERSION);
        let summaries = if version & 0xff >= 0x41 {
            1 << 14 | 1 << 15
        } else {
            1 << 15 | 1 << 16
        };
        let irq_mask = summaries | RX_EVENT | TX_EVENT | BUS_ERROR;
        configure_mac(&regs, mac, feature, delay)?;
        log::info!(
            "YT6801 initialized: MAC={mac:02x?}, version={version:#x}, features={feature:#x}, \
             DMA={width} bits, IRQ mask={irq_mask:#x}"
        );
        Ok(Self {
            regs,
            dma,
            mac,
            irq_mask,
        })
    }

    /// Returns the hardware-constrained capability for runtime buffer pools.
    pub fn dma(&self) -> DeviceDma {
        self.dma.clone()
    }
}

fn read_mac(regs: &Registers, delay: fn(Duration)) -> Result<[u8; 6], Error> {
    let mut low = None;
    let mut high = None;
    for patch in 0..39u16 {
        let mut bytes = [0u8; 6];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let address = 18 + patch * 6 + index as u16;
            regs.write(0x1500, u32::from(address) << 8 | 5);
            let mut done = false;
            for _ in 0..2000 {
                let value = regs.read(0x1504);
                if value & 2 != 0 {
                    *byte = (value >> 24) as u8;
                    done = true;
                    break;
                }
                delay(Duration::from_micros(10));
            }
            if !done {
                return Err(Error::Timeout {
                    operation: "eFuse read",
                });
            }
        }
        let address = u16::from_le_bytes([bytes[0], bytes[1]]);
        let value = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
        match address {
            0 => break,
            0x1520 => low = Some(value),
            0x1524 => high = Some(value),
            _ => {}
        }
        if low.is_some() && high.is_some() {
            break;
        }
    }
    decode_mac(
        low.ok_or(Error::InvalidMac)?,
        high.ok_or(Error::InvalidMac)?,
    )
}

fn decode_mac(low: u32, high: u32) -> Result<[u8; 6], Error> {
    let low = low.to_le_bytes();
    let high = high.to_le_bytes();
    let mac = [high[1], high[0], low[3], low[2], low[1], low[0]];
    if mac == [0; 6] || mac == [u8::MAX; 6] || mac[0] & 1 != 0 {
        return Err(Error::InvalidMac);
    }
    Ok(mac)
}

fn initialize_phy(regs: &Registers, delay: fn(Duration)) -> Result<(), Error> {
    let id = u32::from(regs.mdio_read(2, delay)?) << 16 | u32::from(regs.mdio_read(3, delay)?);
    if id != 0x4f51_e91a {
        return Err(Error::UnsupportedPhy(id));
    }
    let page = regs.phy_ext_read(0xa000, delay)?;
    regs.phy_ext_write(0xa000, page & !2, delay)?;
    // The integrated YT8531S uses GMII and needs its analog DAC clock enabled.
    let afe = regs.phy_ext_read(0x12, delay)?;
    regs.phy_ext_write(0x12, afe | 1 << 13, delay)?;
    let control = regs.mdio_read(0, delay)?;
    regs.mdio_write(
        0,
        (control & !(1 << 11 | 1 << 10)) | 1 << 12 | 1 << 9,
        delay,
    )?;
    for _ in 0..100 {
        // BMSR link is latched low; the second read reports the current state.
        let _ = regs.mdio_read(1, delay)?;
        if regs.mdio_read(1, delay)? & 4 != 0 {
            log::info!(
                "YT6801 PHY link ready: status={:#x}",
                regs.mdio_read(0x11, delay)?
            );
            return Ok(());
        }
        delay(Duration::from_millis(50));
    }
    Err(Error::LinkTimeout)
}

fn configure_mac(
    regs: &Registers,
    mac: [u8; 6],
    feature: u32,
    delay: fn(Duration),
) -> Result<(), Error> {
    let phy = regs.mdio_read(0x11, delay)?;
    let speed = match (phy >> 14) & 3 {
        0 => 1 << 15,
        1 => 1 << 15 | 1 << 14,
        _ => 0,
    };
    let duplex = if phy & (1 << 13) != 0 { 1 << 13 } else { 0 };
    regs.write(MAC_CONFIG, speed | duplex);
    regs.write(
        MAC_ADDRESS_LOW,
        u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]),
    );
    regs.write(
        MAC_ADDRESS_HIGH,
        u32::from(mac[4]) | u32::from(mac[5]) << 8 | 1 << 31,
    );
    regs.write(PACKET_FILTER, 1 << 4);
    regs.write(MAC_IRQ_ENABLE, 0);
    // Mask statistics sources independently from the DMA channel IRQ.
    for offset in [0x270c, 0x2710, 0x2800] {
        regs.write(offset, u32::MAX);
    }
    let tx_fifo = (128u32 << ((feature >> 6) & 0x1f).min(8)).max(256);
    let rx_fifo = (128u32 << (feature & 0x1f).min(8)).max(256);
    regs.write(
        MTL_TX_OPERATION,
        ((tx_fifo / 256 - 1) << 16) | 1 << 3 | 1 << 1,
    );
    regs.write(MTL_RX_OPERATION, ((rx_fifo / 256 - 1) << 20) | 1 << 5);
    regs.write(RX_QUEUE_CONTROL, 2);
    regs.write(MTL_RX_DMA_MAP, 0);
    regs.write(DMA_BUS_MODE, 1 << 24 | 1 << 16 | 1 << 14 | 1 << 11 | 0x1e);
    // INTx uses level signaling. The reset default emits pulses intended for
    // message-signaled delivery and can strand a receive completion.
    regs.write(DMA_MODE, 1 << 16 | 1 << 19);
    regs.write(CHANNEL_CONTROL, 1 << 16);
    regs.write(TX_CONTROL, 32 << 16 | 1 << 4);
    regs.write(RX_CONTROL, 32 << 16 | (BUFFER_SIZE as u32) << 1);
    regs.write(IRQ_ENABLE, 0);
    regs.write(IRQ_STATUS, u32::MAX);
    regs.commit();
    Ok(())
}

impl NetDevice for Yt6801 {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let Self {
            regs,
            dma,
            mac,
            irq_mask,
        } = *self;
        // Allocate both rings before publishing either address to hardware.
        let tx_ring = Ring::new(&dma)?;
        let rx_ring = Ring::new(&dma)?;
        for (high, low, base) in [
            (TX_BASE_HIGH, TX_BASE_LOW, tx_ring.base()),
            (RX_BASE_HIGH, RX_BASE_LOW, rx_ring.base()),
        ] {
            regs.write(high, (base >> 32) as u32);
            regs.write(low, base as u32);
        }
        regs.write(TX_RING_LENGTH, RING_SIZE as u32 - 1);
        regs.write(RX_RING_LENGTH, RING_SIZE as u32 - 1);
        regs.write(TX_TAIL, tx_ring.base() as u32);
        regs.update(MAC_CONFIG, 0, 3);
        regs.update(TX_CONTROL, 0, 1);
        regs.commit();
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new(NAME, mac),
            control: Box::new(Control {
                regs: regs.clone(),
                mac,
            }),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(0),
                queues: NetQueuePairParts {
                    tx: Box::new(TxQueue::new(regs.clone(), tx_ring)),
                    rx: Box::new(RxQueue::new(regs.clone(), rx_ring)),
                },
                irq_control: Box::new(IrqControl {
                    regs: regs.clone(),
                    irq_mask,
                }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(0),
                    Box::new(IrqHandler { regs, irq_mask }),
                )],
            }],
        })
    }
}

struct Control {
    regs: Registers,
    mac: [u8; 6],
}

impl NetControlEndpoint for Control {
    fn mac_address(&mut self) -> Result<[u8; 6], NetError> {
        Ok(self.mac)
    }

    fn set_rx_mode(&mut self, mode: NetRxMode) -> Result<(), NetError> {
        let all = matches!(mode, NetRxMode::AllUnicast | NetRxMode::Promiscuous);
        self.regs.write(PACKET_FILTER, 1 << 4 | u32::from(all));
        self.regs.commit();
        Ok(())
    }
}

struct IrqControl {
    regs: Registers,
    irq_mask: u32,
}

impl NetPollIrqControl for IrqControl {
    fn quiesce(&mut self) -> Result<(), NetError> {
        self.regs.write(0x1100, 0xffff_0000);
        self.regs.write(IRQ_ENABLE, 0);
        self.regs.commit();
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        self.quiesce()?;
        self.regs.update(TX_CONTROL, 1, 0);
        self.regs.update(RX_CONTROL, 1, 0);
        self.regs.update(MAC_CONFIG, 3, 0);
        self.regs.write(DMA_MODE, 1);
        self.regs.commit();
        for _ in 0..100_000 {
            if self.regs.read(DMA_MODE) & 1 == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(NetError::DmaShutdownUnconfirmed)
    }

    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        let before = self.regs.read(IRQ_STATUS);
        self.regs.write(IRQ_STATUS, before);
        self.regs.write(IRQ_ENABLE, self.irq_mask);
        self.regs.write(0x1100, 0xf000_0000);
        self.regs.commit();
        let after = self.regs.read(IRQ_STATUS);
        let pending = irq_snapshot(before | after);
        if pending == NetIrqSnapshot::empty() {
            Ok(NetRearmResult::Idle)
        } else {
            self.quiesce()?;
            self.regs.write(IRQ_STATUS, after);
            Ok(NetRearmResult::WorkPending(pending))
        }
    }
}

struct IrqHandler {
    regs: Registers,
    irq_mask: u32,
}

impl NetHardIrqHandler for IrqHandler {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        let status = self.regs.read(IRQ_STATUS) & self.irq_mask;
        let pending = irq_snapshot(status);
        if pending == NetIrqSnapshot::empty() {
            return NetHardIrqResult::Spurious;
        }
        self.regs.write(0x1100, 0xffff_0000);
        self.regs.write(IRQ_ENABLE, 0);
        self.regs.write(IRQ_STATUS, status);
        self.regs.commit();
        NetHardIrqResult::Schedule(pending)
    }
}

fn irq_snapshot(status: u32) -> NetIrqSnapshot {
    if status == u32::MAX {
        return NetIrqSnapshot::empty();
    }
    let mut snapshot = NetIrqSnapshot::empty();
    if status & RX_EVENT != 0 {
        snapshot = snapshot.union(NetIrqSnapshot::RX);
    }
    if status & TX_EVENT != 0 {
        snapshot = snapshot.union(NetIrqSnapshot::TX);
    }
    if status & BUS_ERROR != 0 {
        snapshot = snapshot.union(NetIrqSnapshot::ERROR);
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::{Error, decode_mac};

    #[test]
    fn mac_identity_uses_efuse_network_order_and_rejects_group_addresses() {
        assert_eq!(
            decode_mac(0x2030_4050, 0x0000_0210).unwrap(),
            [0x02, 0x10, 0x20, 0x30, 0x40, 0x50]
        );
        assert!(matches!(
            decode_mac(0x2030_4050, 0x0310),
            Err(Error::InvalidMac)
        ));
    }
}

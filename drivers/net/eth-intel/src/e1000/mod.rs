extern crate alloc;

use alloc::boxed::Box;
use core::mem::size_of;

use dma_api::{CoherentArray, DeviceDma, DmaOp};
use mmio_api::{Mmio, MmioAddr, MmioOp};
use rdif_eth::{DmaBuffer, Event, IRxQueue, ITxQueue, Interface, NetError, QueueConfig};

use crate::err::{Error, Result};

mod descriptor;
mod registers;

use descriptor::{RxDesc, TxDesc};
use registers::*;

const QUEUE_SIZE: usize = 256;
const QUEUE_ID0: usize = 0;
const MAX_PACKET: usize = 2048;

pub struct E1000 {
    regs: Regs,
    _mmio: Mmio,
    dma: DeviceDma,
    mac: [u8; 6],
    irq_enabled: bool,
    tx_created: bool,
    rx_created: bool,
}

impl E1000 {
    pub fn check_vid_did(vid: u16, did: u16) -> bool {
        vid == 0x8086 && [0x100e, 0x100f].contains(&did)
    }

    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma_mask: u64,
        dma_op: &'static dyn DmaOp,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size)?;
        let regs = Regs::new(mmio.as_nonnull_ptr());
        let dma = DeviceDma::new(dma_mask, dma_op);

        regs.reset();
        regs.disable_all_irq();

        // CTRL.SLU: set link up in software for basic bring-up.
        regs.write(CTRL, regs.read(CTRL) | (1 << 6));

        let mac = regs.mac_addr();

        Ok(Self {
            regs,
            _mmio: mmio,
            dma,
            mac,
            irq_enabled: false,
            tx_created: false,
            rx_created: false,
        })
    }
}

impl rdif_eth::DriverGeneric for E1000 {
    fn name(&self) -> &str {
        "eth-intel-e1000"
    }
}

impl Interface for E1000 {
    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }

        let desc = self
            .dma
            .coherent_array_zero_with_align::<TxDesc>(QUEUE_SIZE, 16)
            .ok()?;

        let desc_base = desc.dma_addr().as_u64();

        self.regs.write(TDBAL, desc_base as u32);
        self.regs.write(TDBAH, (desc_base >> 32) as u32);
        self.regs
            .write(TDLEN, (QUEUE_SIZE * size_of::<TxDesc>()) as u32);
        self.regs.write(TDH, 0);
        self.regs.write(TDT, 0);

        // TCTL.EN + TCTL.PSP + CT + COLD, typical minimal values.
        self.regs
            .write(TCTL, (1 << 1) | (1 << 3) | (0x10 << 4) | (0x40 << 12));
        self.regs.write(TIPG, 10 | (8 << 10) | (6 << 20));

        let queue = E1000TxQueue {
            regs: self.regs,
            desc,
            dma_mask: self.dma.dma_mask(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
        };

        self.tx_created = true;
        Some(Box::new(queue))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }

        let desc = self
            .dma
            .coherent_array_zero_with_align::<RxDesc>(QUEUE_SIZE, 16)
            .ok()?;

        let desc_base = desc.dma_addr().as_u64();

        self.regs.write(RDBAL, desc_base as u32);
        self.regs.write(RDBAH, (desc_base >> 32) as u32);
        self.regs
            .write(RDLEN, (QUEUE_SIZE * size_of::<RxDesc>()) as u32);
        self.regs.write(RDH, 0);
        self.regs.write(RDT, 0);

        // RCTL.EN + BAM + SECRC (2048-byte buffer mode).
        self.regs.write(RCTL, (1 << 1) | (1 << 15) | (1 << 26));

        let queue = E1000RxQueue {
            regs: self.regs,
            desc,
            dma_mask: self.dma.dma_mask(),
            bus_addrs: [None; QUEUE_SIZE],
            next_submit: 0,
            next_reclaim: 0,
        };

        self.rx_created = true;
        Some(Box::new(queue))
    }

    fn enable_irq(&mut self) {
        self.regs.enable_default_irq();
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.regs.disable_all_irq();
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut ev = Event::none();
        let icr = self.regs.read(ICR);

        if icr & (1 << 0) != 0 {
            ev.tx_queue.insert(QUEUE_ID0);
        }
        if icr & (1 << 7) != 0 {
            ev.rx_queue.insert(QUEUE_ID0);
        }

        ev
    }
}

struct E1000TxQueue {
    regs: Regs,
    desc: CoherentArray<TxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
}

impl ITxQueue for E1000TxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: 16,
            buf_size: MAX_PACKET,
            ring_size: QUEUE_SIZE,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len > MAX_PACKET {
            return Err(NetError::Other(Box::new(Error::InvalidArgument(
                "tx packet too large",
            ))));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        let hw_head = self.regs.read(TDH) as usize;

        if next == hw_head {
            return Err(NetError::Retry);
        }

        self.desc
            .set_cpu(idx, TxDesc::new(buffer.bus_addr, buffer.len as u16));
        self.bus_addrs[idx] = Some(buffer.bus_addr);
        self.next_submit = next;
        self.regs.write(TDT, next as u32);

        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let idx = self.next_reclaim;
        let desc = self.desc.read_cpu(idx)?;
        if !desc.is_done() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx].take()
    }
}

struct E1000RxQueue {
    regs: Regs,
    desc: CoherentArray<RxDesc>,
    dma_mask: u64,
    bus_addrs: [Option<u64>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
}

impl IRxQueue for E1000RxQueue {
    fn id(&self) -> usize {
        QUEUE_ID0
    }

    fn config(&self) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: 16,
            buf_size: MAX_PACKET,
            ring_size: QUEUE_SIZE,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len > MAX_PACKET {
            return Err(NetError::Other(Box::new(Error::InvalidArgument(
                "rx buffer too large",
            ))));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        let hw_head = self.regs.read(RDH) as usize;

        if next == hw_head {
            return Err(NetError::Retry);
        }

        self.desc.set_cpu(idx, RxDesc::new(buffer.bus_addr));
        self.bus_addrs[idx] = Some(buffer.bus_addr);
        self.next_submit = next;
        self.regs.write(RDT, next as u32);

        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let idx = self.next_reclaim;
        let desc = self.desc.read_cpu(idx)?;
        if !desc.is_done() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.bus_addrs[idx]
            .take()
            .map(|bus_addr| (bus_addr, desc.length as usize))
    }
}

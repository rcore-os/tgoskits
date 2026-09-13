extern crate alloc;

use alloc::{boxed::Box, vec};
use core::mem::size_of;

use dma_api::{CoherentArray, DeviceDma};
use mmio_api::{Mmio, MmioAddr, MmioOp};
use rdif_eth::{
    DmaBuffer, FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot,
    NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetPollIrqControl, NetQueueId,
    NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion, SubmitError,
};

use crate::err::{Error, Result};

mod descriptor;
mod registers;

use descriptor::{RxDesc, TxDesc};
use registers::*;

const QUEUE_SIZE: usize = 256;
const QUEUE_ID0: NetQueueId = NetQueueId::new(0);
const GROUP_ID0: NetPollGroupId = NetPollGroupId::new(0);
const IRQ_SOURCE0: NetIrqSourceId = NetIrqSourceId::new(0);
const MAX_PACKET: usize = 2048;

pub struct E1000 {
    regs: Regs,
    _mmio: Mmio,
    dma: DeviceDma,
    mac: [u8; 6],
}

impl E1000 {
    pub fn check_vid_did(vid: u16, did: u16) -> bool {
        vid == 0x8086 && [0x100e, 0x100f].contains(&did)
    }

    pub fn new(
        bar_addr: impl Into<MmioAddr>,
        bar_size: usize,
        dma: DeviceDma,
        mmio_op: &'static dyn MmioOp,
    ) -> Result<Self> {
        mmio_api::init(mmio_op);
        let mmio = mmio_api::ioremap(bar_addr.into(), bar_size)?;
        let regs = Regs::new(mmio.as_nonnull_ptr());
        if !regs.reset() {
            return Err(Error::Timeout);
        }
        regs.disable_all_irq();

        // CTRL.SLU: set link up in software for basic bring-up.
        regs.write(CTRL, regs.read(CTRL) | (1 << 6));

        let mac = regs.mac_addr();

        Ok(Self {
            regs,
            _mmio: mmio,
            dma,
            mac,
        })
    }
}

impl rdif_eth::DriverGeneric for E1000 {
    fn name(&self) -> &str {
        "eth-intel-e1000"
    }
}

impl NetDevice for E1000 {
    fn into_parts(self: Box<Self>) -> core::result::Result<NetDeviceParts, NetError> {
        let E1000 {
            regs,
            _mmio,
            dma,
            mac,
        } = *self;

        let tx_desc = dma
            .coherent_array_zero_with_align::<TxDesc>(QUEUE_SIZE, 16)
            .map_err(NetError::from)?;

        let desc_base = tx_desc.dma_addr().as_u64();

        regs.write(TDBAL, desc_base as u32);
        regs.write(TDBAH, (desc_base >> 32) as u32);
        regs.write(TDLEN, (QUEUE_SIZE * size_of::<TxDesc>()) as u32);
        regs.write(TDH, 0);
        regs.write(TDT, 0);

        // TCTL.EN + TCTL.PSP + CT + COLD, typical minimal values.
        regs.write(TCTL, (1 << 1) | (1 << 3) | (0x10 << 4) | (0x40 << 12));
        regs.write(TIPG, 10 | (8 << 10) | (6 << 20));

        let tx = E1000TxQueue {
            regs,
            desc: tx_desc,
            dma_mask: dma.info().constraints().addr_mask,
            buffers: core::array::from_fn(|_| None),
            next_submit: 0,
            next_reclaim: 0,
        };

        let rx_desc = dma
            .coherent_array_zero_with_align::<RxDesc>(QUEUE_SIZE, 16)
            .map_err(NetError::from)?;

        let desc_base = rx_desc.dma_addr().as_u64();

        regs.write(RDBAL, desc_base as u32);
        regs.write(RDBAH, (desc_base >> 32) as u32);
        regs.write(RDLEN, (QUEUE_SIZE * size_of::<RxDesc>()) as u32);
        regs.write(RDH, 0);
        regs.write(RDT, 0);

        // RCTL.EN + BAM + SECRC (2048-byte buffer mode).
        regs.write(RCTL, (1 << 1) | (1 << 15) | (1 << 26));

        let rx = E1000RxQueue {
            regs,
            desc: rx_desc,
            dma_mask: dma.info().constraints().addr_mask,
            buffers: core::array::from_fn(|_| None),
            next_submit: 0,
            next_reclaim: 0,
        };

        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("eth-intel-e1000", mac),
            control: Box::new(FixedNetControl::new(mac)),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: GROUP_ID0,
                queues: NetQueuePairParts {
                    tx: Box::new(tx),
                    rx: Box::new(rx),
                },
                irq_control: Box::new(E1000IrqControl { regs, _mmio }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    IRQ_SOURCE0,
                    Box::new(E1000IrqHandler { regs }),
                )],
            }],
        })
    }
}

struct E1000IrqControl {
    regs: Regs,
    _mmio: Mmio,
}

impl NetPollIrqControl for E1000IrqControl {
    fn quiesce(&mut self) -> core::result::Result<(), NetError> {
        self.regs.disable_all_irq();
        Ok(())
    }

    fn shutdown(&mut self) -> core::result::Result<(), NetError> {
        self.regs.disable_all_irq();
        self.regs
            .reset()
            .then_some(())
            .ok_or(NetError::DmaShutdownUnconfirmed)
    }

    fn rearm_and_check(
        &mut self,
        _now_nanos: u64,
    ) -> core::result::Result<NetRearmResult, NetError> {
        let before = e1000_irq_snapshot(self.regs.read(ICR));
        self.regs.enable_default_irq();
        let after = e1000_irq_snapshot(self.regs.read(ICR));
        let pending = before.union(after);
        if pending == NetIrqSnapshot::empty() {
            Ok(NetRearmResult::Idle)
        } else {
            self.regs.disable_all_irq();
            Ok(NetRearmResult::WorkPending(pending))
        }
    }
}

struct E1000IrqHandler {
    regs: Regs,
}

impl NetHardIrqHandler for E1000IrqHandler {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        let snapshot = e1000_irq_snapshot(self.regs.read(ICR));
        if snapshot == NetIrqSnapshot::empty() {
            return NetHardIrqResult::Spurious;
        }
        self.regs.disable_all_irq();
        NetHardIrqResult::Schedule(snapshot)
    }
}

fn e1000_irq_snapshot(icr: u32) -> NetIrqSnapshot {
    let mut snapshot = NetIrqSnapshot::empty();
    if icr & (1 << 0) != 0 {
        snapshot = snapshot.union(NetIrqSnapshot::TX);
    }
    if icr & (1 << 7) != 0 {
        snapshot = snapshot.union(NetIrqSnapshot::RX);
    }
    snapshot
}

struct E1000TxQueue {
    regs: Regs,
    desc: CoherentArray<TxDesc>,
    dma_mask: u64,
    buffers: [Option<DmaBuffer>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
}

impl ITxQueue for E1000TxQueue {
    fn id(&self) -> NetQueueId {
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

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), SubmitError> {
        if buffer.len() > MAX_PACKET {
            return Err(SubmitError::new(
                buffer,
                NetError::Other(Box::new(Error::InvalidArgument("tx packet too large"))),
            ));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        let hw_head = self.regs.read(TDH) as usize;

        if next == hw_head {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }

        self.desc
            .set_cpu(idx, TxDesc::new(buffer.bus_addr(), buffer.len() as u16));
        self.buffers[idx] = Some(buffer);
        self.next_submit = next;
        self.regs.write(TDT, next as u32);

        Ok(())
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        let idx = self.next_reclaim;
        let desc = self.desc.read_cpu(idx)?;
        if !desc.is_done() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.buffers[idx].take()
    }
}

struct E1000RxQueue {
    regs: Regs,
    desc: CoherentArray<RxDesc>,
    dma_mask: u64,
    buffers: [Option<DmaBuffer>; QUEUE_SIZE],
    next_submit: usize,
    next_reclaim: usize,
}

impl IRxQueue for E1000RxQueue {
    fn id(&self) -> NetQueueId {
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

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), SubmitError> {
        if buffer.len() > MAX_PACKET {
            return Err(SubmitError::new(
                buffer,
                NetError::Other(Box::new(Error::InvalidArgument("rx buffer too large"))),
            ));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        let hw_head = self.regs.read(RDH) as usize;

        if next == hw_head {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }

        self.desc.set_cpu(idx, RxDesc::new(buffer.bus_addr()));
        self.buffers[idx] = Some(buffer);
        self.next_submit = next;
        self.regs.write(RDT, next as u32);

        Ok(())
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        let idx = self.next_reclaim;
        let desc = self.desc.read_cpu(idx)?;
        if !desc.is_done() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        self.buffers[idx].take().map(|buffer| RxCompletion {
            buffer,
            packet_len: desc.length as usize,
        })
    }
}

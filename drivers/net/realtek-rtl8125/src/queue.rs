use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{Ordering as AtomicOrdering, fence};

use dma_api::CoherentArray;
use log::{info, warn};
use mmio_api::Mmio;
use rdif_eth::{DmaBuffer, IRxQueue, ITxQueue, NetError, QueueConfig, QueueMemoryMode};

use crate::{
    DMA_ALIGN, EARLY_PACKET_LOG_COUNT, LINK_DOWN_DROP_LOG_INTERVAL, MAX_PACKET, QUEUE_ID0,
    QUEUE_SIZE, RX_BUF_SIZE, RX_QUEUE_CONFIG_SIZE, RX_RECLAIM_LOG_INTERVAL, RX_START_THRESHOLD,
    TX_LINK_SAMPLE_INTERVAL, TX_RECLAIM_LOG_INTERVAL, TX_SUBMIT_LOG_INTERVAL,
    descriptor::{RxDesc, TxDesc},
    registers::{Rtl8125RxRegs, Rtl8125TxRegs},
};

pub(crate) struct Rtl8125TxQueue {
    pub(crate) regs: Rtl8125TxRegs,
    pub(crate) desc: CoherentArray<TxDesc>,
    pub(crate) dma_mask: u64,
    pub(crate) bus_addrs: [Option<u64>; QUEUE_SIZE],
    pub(crate) next_submit: usize,
    pub(crate) next_reclaim: usize,
    pub(crate) link_up: Option<bool>,
    pub(crate) link_down_drops: u64,
    pub(crate) submitted: u64,
    pub(crate) reclaimed: u64,
    pub(crate) _mapping: Arc<Mmio>,
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
            memory_mode: QueueMemoryMode::DirectDma,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len > MAX_PACKET {
            return Err(NetError::NotSupported);
        }
        if !self.observe_link_before_tx(buffer.len) {
            self.link_down_drops = self.link_down_drops.saturating_add(1);
            return Err(NetError::Retry);
        }

        let index = self.next_submit;
        if self.bus_addrs[index].is_some() {
            return Err(NetError::Retry);
        }
        let next = (index + 1) % QUEUE_SIZE;
        let descriptor =
            TxDesc::new_cpu_owned(buffer.bus_addr, buffer.len, index == QUEUE_SIZE - 1);
        self.desc.set_cpu(index, descriptor);
        release_dma_descriptor();
        self.desc.set_cpu(index, descriptor.release_to_hw());
        self.bus_addrs[index] = Some(buffer.bus_addr);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        self.regs.poll_tx();
        if self.submitted <= EARLY_PACKET_LOG_COUNT
            || self.submitted.is_multiple_of(TX_SUBMIT_LOG_INTERVAL)
        {
            info!(
                "RTL8125 tx submitted: index={index}, len={}, submitted={}, reclaimed={}",
                buffer.len, self.submitted, self.reclaimed,
            );
        }
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let index = self.next_reclaim;
        self.bus_addrs[index]?;
        let descriptor = self.desc.read_cpu(index)?;
        if descriptor.is_owned_by_hw() {
            return None;
        }

        self.next_reclaim = (index + 1) % QUEUE_SIZE;
        let bus_addr = self.bus_addrs[index].take()?;
        self.reclaimed = self.reclaimed.saturating_add(1);
        if self.reclaimed <= EARLY_PACKET_LOG_COUNT
            || self.reclaimed.is_multiple_of(TX_RECLAIM_LOG_INTERVAL)
        {
            info!(
                "RTL8125 tx reclaimed: index={index}, len={}, submitted={}, reclaimed={}",
                descriptor.len(),
                self.submitted,
                self.reclaimed,
            );
        }
        Some(bus_addr)
    }
}

impl Rtl8125TxQueue {
    fn observe_link_before_tx(&mut self, packet_len: usize) -> bool {
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
                info!("RTL8125 link became ready before TX len={packet_len}");
            }
        } else if changed
            || self.link_down_drops == 0
            || self
                .link_down_drops
                .is_multiple_of(LINK_DOWN_DROP_LOG_INTERVAL)
        {
            warn!(
                "RTL8125 link down before TX len={packet_len}, dropped_tx={}",
                self.link_down_drops
            );
        }
        link_up
    }
}

pub(crate) struct Rtl8125RxQueue {
    pub(crate) regs: Rtl8125RxRegs,
    pub(crate) desc: CoherentArray<RxDesc>,
    pub(crate) dma_mask: u64,
    pub(crate) bus_addrs: [Option<u64>; QUEUE_SIZE],
    pub(crate) next_submit: usize,
    pub(crate) next_reclaim: usize,
    pub(crate) primed: usize,
    pub(crate) started: bool,
    pub(crate) reclaimed: u64,
    pub(crate) rx_errors: u64,
    pub(crate) _mapping: Arc<Mmio>,
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
            memory_mode: QueueMemoryMode::DirectDma,
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len < RX_BUF_SIZE {
            return Err(NetError::NotSupported);
        }

        let index = self.next_submit;
        if self.bus_addrs[index].is_some() {
            return Err(NetError::Retry);
        }
        let next = (index + 1) % QUEUE_SIZE;
        let descriptor =
            RxDesc::new_cpu_owned(buffer.bus_addr, RX_BUF_SIZE, index == QUEUE_SIZE - 1);
        self.desc.set_cpu(index, descriptor);
        release_dma_descriptor();
        self.desc.set_cpu(index, descriptor.release_to_hw());
        self.bus_addrs[index] = Some(buffer.bus_addr);
        self.next_submit = next;

        if !self.started {
            self.primed = self.primed.saturating_add(1);
            if self.primed == RX_START_THRESHOLD {
                self.regs.start_queues();
                self.started = true;
                info!("RTL8125 RX ring primed; owner enabled TX/RX queues");
            }
        }
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let index = self.next_reclaim;
        let bus_addr = self.bus_addrs[index]?;
        let descriptor = self.desc.read_cpu(index)?;
        if descriptor.is_owned_by_hw() {
            return None;
        }
        acquire_dma_descriptor();
        let descriptor = self.desc.read_cpu(index)?;

        self.next_reclaim = (index + 1) % QUEUE_SIZE;
        self.bus_addrs[index] = None;
        if descriptor.has_error() || !descriptor.is_whole_packet() {
            self.rx_errors = self.rx_errors.saturating_add(1);
            warn!(
                "RTL8125 RX descriptor error: index={index}, opts1={:#x}, errors={}",
                descriptor.opts1, self.rx_errors,
            );
            return Some((bus_addr, 0));
        }

        let packet_len = descriptor.packet_len();
        self.reclaimed = self.reclaimed.saturating_add(1);
        if self.reclaimed.is_multiple_of(RX_RECLAIM_LOG_INTERVAL) {
            info!(
                "RTL8125 RX packet: index={index}, len={packet_len}, reclaimed={}",
                self.reclaimed,
            );
        }
        Some((bus_addr, packet_len))
    }
}

pub(crate) fn release_dma_descriptor() {
    fence(AtomicOrdering::Release);
}

fn acquire_dma_descriptor() {
    fence(AtomicOrdering::Acquire);
}

pub(crate) fn boxed_tx(queue: Rtl8125TxQueue) -> Box<dyn ITxQueue> {
    Box::new(queue)
}

pub(crate) fn boxed_rx(queue: Rtl8125RxQueue) -> Box<dyn IRxQueue> {
    Box::new(queue)
}

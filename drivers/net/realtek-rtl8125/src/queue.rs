use alloc::{boxed::Box, collections::VecDeque, sync::Arc};
use core::sync::atomic::{Ordering as AtomicOrdering, fence};

use dma_api::CoherentArray;
use log::{debug, info, warn};
use rdif_eth::{DmaBuffer, IRxQueue, ITxQueue, NetError, QueueConfig};
use spin::Mutex;

use crate::{
    DMA_ALIGN, EARLY_PACKET_LOG_COUNT, LINK_DOWN_DROP_LOG_INTERVAL, MAX_PACKET, QUEUE_ID0,
    QUEUE_SIZE, RX_BUF_SIZE, RX_DESC_PER_CACHE_LINE, RX_IDLE_LOG_INTERVAL,
    RX_OVERFLOW_REARM_IDLE_POLLS, RX_QUEUE_CONFIG_SIZE, RX_RECLAIM_LOG_INTERVAL,
    RX_START_THRESHOLD, TX_LINK_SAMPLE_INTERVAL, TX_RECLAIM_LOG_INTERVAL, TX_SUBMIT_LOG_INTERVAL,
    descriptor::{RxDesc, TxDesc},
    read_status,
    registers::{DEFAULT_IRQ_MASK, Regs, irq_has_rx_overflow},
    set_rx_mode,
};

pub(crate) type QueueStart = Arc<Mutex<QueueStartState>>;

#[derive(Default)]
pub(crate) struct QueueStartState {
    pub(crate) tx_base: Option<u64>,
    pub(crate) rx_base: Option<u64>,
    pub(crate) rx_ready: bool,
    pub(crate) started: bool,
}

pub(crate) struct Rtl8125TxQueue {
    pub(crate) regs: Regs,
    pub(crate) desc: CoherentArray<TxDesc>,
    pub(crate) dma_mask: u64,
    pub(crate) bus_addrs: [Option<u64>; QUEUE_SIZE],
    pub(crate) next_submit: usize,
    pub(crate) next_reclaim: usize,
    pub(crate) link_up: Option<bool>,
    pub(crate) link_down_drops: u64,
    pub(crate) submitted: u64,
    pub(crate) reclaimed: u64,
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

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len > MAX_PACKET {
            return Err(NetError::NotSupported);
        }

        if !self.observe_link_before_tx(buffer.len) {
            self.link_down_drops = self.link_down_drops.saturating_add(1);
            return Err(NetError::Retry);
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = TxDesc::new_cpu_owned(buffer.bus_addr, buffer.len, ring_end);
        self.desc.set_cpu(idx, desc);
        release_dma_descriptor();
        self.desc.set_cpu(idx, desc.release_to_hw());
        self.bus_addrs[idx] = Some(buffer.bus_addr);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        self.regs.poll_tx();
        if self.submitted <= EARLY_PACKET_LOG_COUNT
            || self.submitted.is_multiple_of(TX_SUBMIT_LOG_INTERVAL)
        {
            info!(
                "RTL8125 tx submitted: idx={idx}, len={}, submitted={}, reclaimed={}, status={:?}",
                buffer.len,
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
        let desc = self.desc.read_cpu(idx)?;
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

pub(crate) struct Rtl8125RxQueue {
    pub(crate) regs: Regs,
    pub(crate) desc: CoherentArray<RxDesc>,
    pub(crate) dma_mask: u64,
    pub(crate) start: QueueStart,
    pub(crate) bus_addrs: [Option<u64>; QUEUE_SIZE],
    pub(crate) next_submit: usize,
    pub(crate) next_reclaim: usize,
    pub(crate) idle_polls: u64,
    pub(crate) last_rx_rearm_idle: u64,
    pub(crate) submitted: usize,
    pub(crate) reclaimed: u64,
    pub(crate) rx_errors: u64,
    pub(crate) deferred_refill: VecDeque<u64>,
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

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), NetError> {
        if buffer.len < RX_BUF_SIZE {
            return Err(NetError::NotSupported);
        }

        self.flush_deferred_refill();
        if self.submitted >= RX_START_THRESHOLD {
            self.deferred_refill.push_back(buffer.bus_addr);
            self.flush_deferred_refill();
            return Ok(());
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = RxDesc::new_cpu_owned(buffer.bus_addr, RX_BUF_SIZE, ring_end);
        self.desc.set_cpu(idx, desc);
        release_dma_descriptor();
        self.desc.set_cpu(idx, desc.release_to_hw());
        self.bus_addrs[idx] = Some(buffer.bus_addr);
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
                let last_opts1 = self
                    .desc
                    .read_cpu(QUEUE_SIZE - 1)
                    .map_or(0, |desc| desc.opts1);
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
        let desc = self.desc.read_cpu(idx)?;
        if desc.is_owned_by_hw() {
            self.idle_polls = self.idle_polls.saturating_add(1);
            let status = read_status(self.regs);
            if irq_has_rx_overflow(status.intr_status)
                && self.idle_polls.saturating_sub(self.last_rx_rearm_idle)
                    >= RX_OVERFLOW_REARM_IDLE_POLLS
            {
                self.last_rx_rearm_idle = self.idle_polls;
                warn!(
                    "RTL8125 rx overflow rearm: idx={idx}, opts1={:#x}, submitted={}, \
                     reclaimed={}, status={status:?}",
                    desc.opts1, self.submitted, self.reclaimed
                );
                self.regs.write_interrupt_status(status.intr_status);
                set_rx_mode(self.regs);
                self.regs.enable_tx_rx();
                self.regs.commit();
            }
            if self.idle_polls.is_multiple_of(RX_IDLE_LOG_INTERVAL) {
                debug!(
                    "RTL8125 rx idle: idx={idx}, opts1={:#x}, submitted={}, reclaimed={}, \
                     status={:?}",
                    desc.opts1, self.submitted, self.reclaimed, status,
                );
            }
            return None;
        }
        acquire_dma_descriptor();
        let desc = self.desc.read_cpu(idx)?;
        self.idle_polls = 0;
        self.last_rx_rearm_idle = 0;

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
        if self.reclaimed.is_multiple_of(RX_RECLAIM_LOG_INTERVAL) {
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

impl Rtl8125RxQueue {
    fn flush_deferred_refill(&mut self) {
        while self.deferred_refill.len() >= RX_DESC_PER_CACHE_LINE {
            let Some(bus_addr) = self.deferred_refill.pop_front() else {
                break;
            };
            if let Err(err) = self.submit_deferred_buffer(bus_addr) {
                warn!("RTL8125 rx deferred refill failed: {err:?}");
                self.deferred_refill.push_front(bus_addr);
                break;
            }
        }
    }

    fn submit_deferred_buffer(&mut self, bus_addr: u64) -> core::result::Result<(), NetError> {
        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.bus_addrs[idx].is_some() {
            return Err(NetError::Retry);
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = RxDesc::new_cpu_owned(bus_addr, RX_BUF_SIZE, ring_end);
        self.desc.set_cpu(idx, desc);
        release_dma_descriptor();
        self.desc.set_cpu(idx, desc.release_to_hw());
        self.bus_addrs[idx] = Some(bus_addr);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        Ok(())
    }
}

pub(crate) fn release_dma_descriptor() {
    fence(AtomicOrdering::Release);
}

fn acquire_dma_descriptor() {
    fence(AtomicOrdering::Acquire);
}

pub(crate) fn try_start_queues(regs: Regs, dma_mask: u64, start: &QueueStart) {
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
    regs.write_rx_max_size(RX_BUF_SIZE as u16 + 1);
    regs.enable_tx_rx();
    regs.write_default_rx_config_8125b();
    regs.write_default_tx_config();
    regs.write_interrupt_status(u32::MAX);
    set_rx_mode(regs);
    regs.write_interrupt_mask(DEFAULT_IRQ_MASK);
    regs.commit();
    info!("RTL8125 queues started: status={:?}", read_status(regs));
}

pub(crate) fn boxed_tx(queue: Rtl8125TxQueue) -> Box<dyn ITxQueue> {
    Box::new(queue)
}

pub(crate) fn boxed_rx(queue: Rtl8125RxQueue) -> Box<dyn IRxQueue> {
    Box::new(queue)
}

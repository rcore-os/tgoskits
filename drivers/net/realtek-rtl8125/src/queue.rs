use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering as AtomicOrdering, fence};

use ax_sync::SpinLock as Mutex;
use dma_api::CoherentArray;
use log::{Level, debug, info, warn};
use mbarrier::wmb;
use rdif_eth::{
    DmaBuffer, IRxQueue, ITxQueue, NetError, NetQueueId, QueueConfig, RxCompletion, SubmitError,
    TxChecksumCapabilities, TxNotify, TxSubmitOptions,
};

use crate::{
    DMA_ALIGN, LINK_DOWN_DROP_LOG_INTERVAL, MAX_PACKET, QUEUE_ID0, QUEUE_SIZE, RX_BUF_SIZE,
    RX_IDLE_LOG_INTERVAL, RX_OVERFLOW_REARM_IDLE_POLLS, RX_QUEUE_CONFIG_SIZE,
    RX_RECLAIM_LOG_INTERVAL, RX_START_THRESHOLD, TX_RECLAIM_LOG_INTERVAL, TX_SUBMIT_LOG_INTERVAL,
    descriptor::{RxDesc, TxDesc},
    read_status,
    registers::{Regs, irq_has_rx_overflow},
    set_rx_mode,
};

pub(crate) type QueueStart = Arc<Mutex<QueueStartState>>;

#[derive(Default)]
pub(crate) struct TxNotificationState {
    pending: bool,
}

impl TxNotificationState {
    fn descriptor_submitted(&mut self, notify: TxNotify) -> bool {
        match notify {
            TxNotify::Immediate => {
                self.pending = false;
                true
            }
            TxNotify::Deferred => {
                self.pending = true;
                false
            }
        }
    }

    fn take_pending(&mut self) -> bool {
        core::mem::take(&mut self.pending)
    }
}

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
    pub(crate) buffers: [Option<DmaBuffer>; QUEUE_SIZE],
    pub(crate) next_submit: usize,
    pub(crate) next_reclaim: usize,
    pub(crate) link_up: Arc<AtomicBool>,
    pub(crate) link_down_drops: u64,
    pub(crate) submitted: u64,
    pub(crate) reclaimed: u64,
    pub(crate) notification: TxNotificationState,
}

impl ITxQueue for Rtl8125TxQueue {
    fn id(&self) -> NetQueueId {
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

    fn checksum_capabilities(&self) -> TxChecksumCapabilities {
        TxChecksumCapabilities::TCP_UDP
    }

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), SubmitError> {
        self.submit_buffer(buffer, TxSubmitOptions::default())
    }

    fn submit_with_options(
        &mut self,
        buffer: DmaBuffer,
        options: TxSubmitOptions,
    ) -> core::result::Result<(), SubmitError> {
        self.submit_buffer(buffer, options)
    }

    fn flush(&mut self) {
        if self.notification.take_pending() {
            self.notify_device();
        }
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        let idx = self.next_reclaim;
        self.buffers[idx].as_ref()?;
        let desc = self.desc.read_cpu(idx)?;
        if desc.is_owned_by_hw() {
            return None;
        }

        self.next_reclaim = (idx + 1) % QUEUE_SIZE;
        let buffer = self.buffers[idx].take()?;
        self.reclaimed = self.reclaimed.saturating_add(1);
        if let Some(level) = packet_progress_log_level(self.reclaimed, TX_RECLAIM_LOG_INTERVAL) {
            log::log!(
                level,
                "RTL8125 tx reclaimed: idx={idx}, len={}, submitted={}, reclaimed={}, status={:?}",
                desc.len(),
                self.submitted,
                self.reclaimed,
                read_status(self.regs),
            );
        }
        Some(buffer)
    }
}

impl Rtl8125TxQueue {
    fn submit_buffer(
        &mut self,
        buffer: DmaBuffer,
        options: TxSubmitOptions,
    ) -> core::result::Result<(), SubmitError> {
        if buffer.len() > MAX_PACKET {
            return Err(SubmitError::new(buffer, NetError::NotSupported));
        }

        if !self.observe_link_before_tx(buffer.len()) {
            self.link_down_drops = self.link_down_drops.saturating_add(1);
            return Err(SubmitError::new(buffer, NetError::LinkDown));
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.buffers[idx].is_some() {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let len = buffer.len();
        let Some(desc) = TxDesc::new_cpu_owned(buffer.bus_addr(), len, ring_end, options.checksum)
        else {
            return Err(SubmitError::new(buffer, NetError::NotSupported));
        };
        self.desc.set_cpu(idx, desc);
        release_dma_descriptor();
        self.desc.set_cpu(idx, desc.release_to_hw());
        self.buffers[idx] = Some(buffer);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        if self.notification.descriptor_submitted(options.notify) {
            self.notify_device();
        }
        if let Some(level) = packet_progress_log_level(self.submitted, TX_SUBMIT_LOG_INTERVAL) {
            log::log!(
                level,
                "RTL8125 tx submitted: idx={idx}, len={}, submitted={}, reclaimed={}, status={:?}",
                len,
                self.submitted,
                self.reclaimed,
                read_status(self.regs),
            );
        }
        Ok(())
    }

    fn notify_device(&self) {
        // Coherent DMA removes cache-maintenance requirements, but descriptor
        // ownership still has to reach the device before the MMIO doorbell.
        wmb();
        self.regs.poll_tx();
    }

    fn observe_link_before_tx(&mut self, len: usize) -> bool {
        if self.link_up.load(AtomicOrdering::Acquire) {
            return true;
        }

        let link_up = self.regs.link_up();
        let changed = self.link_up.swap(link_up, AtomicOrdering::AcqRel) != link_up;

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
    pub(crate) buffers: [Option<DmaBuffer>; QUEUE_SIZE],
    pub(crate) next_submit: usize,
    pub(crate) next_reclaim: usize,
    pub(crate) idle_polls: u64,
    pub(crate) last_rx_rearm_idle: u64,
    pub(crate) submitted: usize,
    pub(crate) reclaimed: u64,
    pub(crate) rx_errors: u64,
}

impl IRxQueue for Rtl8125RxQueue {
    fn id(&self) -> NetQueueId {
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

    fn submit(&mut self, buffer: DmaBuffer) -> core::result::Result<(), SubmitError> {
        if buffer.len() < RX_BUF_SIZE {
            return Err(SubmitError::new(buffer, NetError::NotSupported));
        }

        if self.submitted >= RX_START_THRESHOLD {
            return self.submit_buffer(buffer);
        }

        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.buffers[idx].is_some() {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = RxDesc::new_cpu_owned(buffer.bus_addr(), RX_BUF_SIZE, ring_end);
        self.desc.set_cpu(idx, desc);
        release_dma_descriptor();
        self.desc.set_cpu(idx, desc.release_to_hw());
        self.buffers[idx] = Some(buffer);
        self.next_submit = next;
        self.submitted = self.submitted.saturating_add(1);
        if self.submitted >= RX_START_THRESHOLD {
            let was_ready = {
                // SAFETY: queue completion runs with local re-entry excluded;
                // the raw lock serializes concurrent queue state.
                let mut start = unsafe { self.start.lock_raw() };
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

    fn reclaim(&mut self) -> Option<RxCompletion> {
        let idx = self.next_reclaim;
        self.buffers[idx].as_ref()?;
        let desc = self.desc.read_cpu(idx)?;
        if desc.is_owned_by_hw() {
            self.idle_polls = self.idle_polls.saturating_add(1);
            if self.idle_polls.saturating_sub(self.last_rx_rearm_idle)
                >= RX_OVERFLOW_REARM_IDLE_POLLS
                && irq_has_rx_overflow(self.regs.read_interrupt_status())
            {
                self.last_rx_rearm_idle = self.idle_polls;
                let status = read_status(self.regs);
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
            if self.idle_polls.is_multiple_of(RX_IDLE_LOG_INTERVAL)
                && log::log_enabled!(Level::Debug)
            {
                let status = read_status(self.regs);
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
        let buffer = self.buffers[idx].take()?;

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
            return Some(RxCompletion {
                buffer,
                packet_len: 0,
            });
        }
        let len = desc.packet_len();
        self.reclaimed = self.reclaimed.saturating_add(1);
        if let Some(level) = packet_progress_log_level(self.reclaimed, RX_RECLAIM_LOG_INTERVAL) {
            log::log!(
                level,
                "RTL8125 rx packet: idx={idx}, len={len}, submitted={}, reclaimed={}, status={:?}",
                self.submitted,
                self.reclaimed,
                read_status(self.regs),
            );
        }
        Some(RxCompletion {
            buffer,
            packet_len: len,
        })
    }
}

fn packet_progress_log_level(sequence: u64, debug_interval: u64) -> Option<Level> {
    if sequence.is_multiple_of(debug_interval) {
        Some(Level::Debug)
    } else {
        None
    }
}

impl Rtl8125RxQueue {
    fn submit_buffer(&mut self, buffer: DmaBuffer) -> core::result::Result<(), SubmitError> {
        let idx = self.next_submit;
        let next = (idx + 1) % QUEUE_SIZE;
        if self.buffers[idx].is_some() {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }

        let ring_end = idx == QUEUE_SIZE - 1;
        let desc = RxDesc::new_cpu_owned(buffer.bus_addr(), RX_BUF_SIZE, ring_end);
        self.desc.set_cpu(idx, desc);
        release_dma_descriptor();
        self.desc.set_cpu(idx, desc.release_to_hw());
        self.buffers[idx] = Some(buffer);
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
        // SAFETY: queue initialization is serialized before publication.
        let mut start = unsafe { start.lock_raw() };
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
    regs.write_interrupt_mask(0);
    regs.commit();
    info!("RTL8125 queues started: status={:?}", read_status(regs));
}

pub(crate) fn boxed_tx(queue: Rtl8125TxQueue) -> Box<dyn ITxQueue> {
    Box::new(queue)
}

pub(crate) fn boxed_rx(queue: Rtl8125RxQueue) -> Box<dyn IRxQueue> {
    Box::new(queue)
}

#[cfg(test)]
mod tests {
    use rdif_eth::TxNotify;

    use super::{TxNotificationState, packet_progress_log_level};

    #[test]
    fn deferred_descriptors_share_one_device_notification() {
        let mut notification = TxNotificationState::default();

        assert!(!notification.descriptor_submitted(TxNotify::Deferred));
        assert!(!notification.descriptor_submitted(TxNotify::Deferred));
        assert!(notification.take_pending());
        assert!(!notification.take_pending());
        assert!(notification.descriptor_submitted(TxNotify::Immediate));
        assert!(!notification.take_pending());
    }

    #[test]
    fn periodic_packet_progress_is_debug_only() {
        assert_eq!(packet_progress_log_level(1, 16), None);
        assert_eq!(packet_progress_log_level(8, 16), None);
        assert_eq!(packet_progress_log_level(9, 16), None);
        assert_eq!(packet_progress_log_level(16, 16), Some(log::Level::Debug));
        assert_eq!(packet_progress_log_level(64, 64), Some(log::Level::Debug));
    }
}

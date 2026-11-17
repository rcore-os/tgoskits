use core::array;

use dma_api::{CoherentArray, DeviceDma};
use rdif_eth::{
    DmaBuffer, IRxQueue, ITxQueue, NetError, NetQueueId, QueueConfig, RxCompletion, SubmitError,
};

use crate::{
    BUFFER_SIZE, DESCRIPTOR_BYTES, DESCRIPTOR_WORDS, DMA_ALIGN, OWN, RING_SIZE,
    registers::{RX_CONTROL, RX_TAIL, Registers, TX_TAIL},
};

pub(crate) struct Ring {
    words: CoherentArray<u32>,
    buffers: [Option<DmaBuffer>; RING_SIZE],
    next_submit: usize,
    next_reclaim: usize,
    dma_mask: u64,
}

impl Ring {
    pub(crate) fn new(dma: &DeviceDma) -> Result<Self, NetError> {
        let words = dma
            .coherent_array_zero_with_align(RING_SIZE * DESCRIPTOR_WORDS, DMA_ALIGN)
            .map_err(NetError::from)?;
        if (words.dma_addr().as_u64() as u32)
            .checked_add((RING_SIZE * DESCRIPTOR_BYTES) as u32)
            .is_none()
        {
            return Err(NetError::InvalidParts);
        }
        Ok(Self {
            words,
            buffers: array::from_fn(|_| None),
            next_submit: 0,
            next_reclaim: 0,
            dma_mask: dma.info().constraints().addr_mask,
        })
    }

    pub(crate) fn base(&self) -> u64 {
        self.words.dma_addr().as_u64()
    }

    fn config(&self, rx: bool) -> QueueConfig {
        QueueConfig {
            dma_mask: self.dma_mask,
            align: DMA_ALIGN,
            buf_size: BUFFER_SIZE,
            ring_size: RING_SIZE + usize::from(rx),
        }
    }

    fn publish(&mut self, buffer: DmaBuffer, length: u32, control: u32) {
        let index = self.next_submit;
        let address = buffer.bus_addr();
        self.words.set_cpu(index * 4, (address as u32).to_le());
        self.words
            .set_cpu(index * 4 + 1, ((address >> 32) as u32).to_le());
        self.words.set_cpu(index * 4 + 2, length.to_le());
        self.buffers[index] = Some(buffer);
        // Publish all address, length and packet writes before giving DMA OWN.
        mbarrier::wmb();
        self.words.set_cpu(index * 4 + 3, (control | OWN).to_le());
        mbarrier::wmb();
        self.next_submit = (index + 1) % RING_SIZE;
    }

    fn completed(&self) -> Option<u32> {
        self.buffers[self.next_reclaim].as_ref()?;
        let status = u32::from_le(self.words.read_cpu(self.next_reclaim * 4 + 3)?);
        if status & OWN != 0 {
            return None;
        }
        // DMA publishes status after packet writes. CPU may consume only now.
        mbarrier::rmb();
        Some(status)
    }

    fn take_completed(&mut self) -> Option<DmaBuffer> {
        let index = self.next_reclaim;
        let buffer = self.buffers[index].take()?;
        self.next_reclaim = (index + 1) % RING_SIZE;
        Some(buffer)
    }

    fn has_room(&self) -> bool {
        self.buffers[self.next_submit].is_none()
    }

    fn address_fits(&self, buffer: &DmaBuffer, length: usize) -> bool {
        buffer
            .bus_addr()
            .checked_add(length.saturating_sub(1) as u64)
            .is_some_and(|last| last & !self.dma_mask == 0)
    }
}

pub(crate) struct TxQueue {
    regs: Registers,
    ring: Ring,
}

impl TxQueue {
    pub(crate) fn new(regs: Registers, ring: Ring) -> Self {
        Self { regs, ring }
    }
}

impl ITxQueue for TxQueue {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        self.ring.config(false)
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        let length = buffer.len();
        if length == 0 || length > 1518 || !self.ring.address_fits(&buffer, length) {
            return Err(SubmitError::new(buffer, NetError::NotSupported));
        }
        if !self.ring.has_room() {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        buffer.prepare_for_device();
        self.ring.publish(
            buffer,
            length as u32 | 1 << 31,
            length as u32 | 1 << 29 | 1 << 28,
        );
        self.regs.write(
            TX_TAIL,
            (self.ring.base() + (self.ring.next_submit * DESCRIPTOR_BYTES) as u64) as u32,
        );
        self.regs.commit();
        Ok(())
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        self.ring.completed()?;
        self.ring.take_completed()
    }
}

pub(crate) struct RxQueue {
    regs: Registers,
    ring: Ring,
    posted: usize,
}

impl RxQueue {
    pub(crate) fn new(regs: Registers, ring: Ring) -> Self {
        Self {
            regs,
            ring,
            posted: 0,
        }
    }
}

impl IRxQueue for RxQueue {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        self.ring.config(true)
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if buffer.len() < BUFFER_SIZE || !self.ring.address_fits(&buffer, BUFFER_SIZE) {
            return Err(SubmitError::new(buffer, NetError::NotSupported));
        }
        if !self.ring.has_room() {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        buffer.prepare_for_device();
        self.ring.publish(buffer, 0, 1 << 24 | 1 << 30);
        if self.posted < RING_SIZE {
            self.posted += 1;
            if self.posted != RING_SIZE {
                return Ok(());
            }
            self.regs.write(
                RX_TAIL,
                (self.ring.base() + (RING_SIZE * DESCRIPTOR_BYTES) as u64) as u32,
            );
            self.regs.update(RX_CONTROL, 0, 1);
            log::info!("YT6801 RX ring started at {:#x}", self.ring.base());
        } else {
            self.regs.write(
                RX_TAIL,
                (self.ring.base() + (self.ring.next_submit * DESCRIPTOR_BYTES) as u64) as u32,
            );
        }
        self.regs.commit();
        Ok(())
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        let status = self.ring.completed()?;
        let buffer = self.ring.take_completed()?;
        let length = (status & 0x7fff) as usize;
        let valid = status & (1 << 29 | 1 << 28 | 1 << 15) == (1 << 29 | 1 << 28)
            && (4..=BUFFER_SIZE).contains(&length);
        let packet_len = if valid { length - 4 } else { 0 };
        buffer.complete_for_cpu(packet_len);
        Some(RxCompletion { buffer, packet_len })
    }
}

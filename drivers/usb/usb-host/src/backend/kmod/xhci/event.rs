use alloc::vec::Vec;

use dma_api::{DArray, DmaDirection};
use mbarrier::mb;
use xhci::ring::trb::event::Allowed;

use super::ring::{Ring, TRBS_PER_SEGMENT};
use crate::{err::*, osal::Kernel};

#[repr(C)]
pub struct EventRingSte {
    pub addr: u64,
    pub size: u16,
    _reserved: [u8; 6],
}

pub struct EventRing {
    segments: Vec<Ring>,
    segment_index: usize,
    trb_index: usize,
    cycle: bool,
    pub ste: DArray<EventRingSte>,
}

unsafe impl Send for EventRing {}
unsafe impl Sync for EventRing {}

const EVENT_RING_SEGMENTS: usize = 2;

impl EventRing {
    pub fn new(max_segments: usize, dma: &Kernel) -> Result<Self> {
        let segment_count = EVENT_RING_SEGMENTS.min(max_segments.max(1));
        let mut segments = Vec::with_capacity(segment_count);
        for _ in 0..segment_count {
            segments.push(Ring::new_segment(false, DmaDirection::Bidirectional, dma)?);
        }

        let mut ste = dma
            .array_zero_with_align(segment_count, 64, DmaDirection::Bidirectional)
            .map_err(|_| USBError::NoMemory)?;

        for (index, segment) in segments.iter().enumerate() {
            ste.set(
                index,
                EventRingSte {
                    addr: segment.trbs.dma_addr().as_u64(),
                    size: TRBS_PER_SEGMENT as _,
                    _reserved: [0; 6],
                },
            );
        }

        Ok(Self {
            segments,
            segment_index: 0,
            trb_index: 0,
            cycle: true,
            ste,
        })
    }

    pub fn next(&mut self) -> Option<Allowed> {
        let data = self.current_data();

        let allowed = Allowed::try_from(data.to_raw()).ok()?;

        if self.cycle != allowed.cycle_bit() {
            return None;
        }
        mb();
        self.inc_deque();
        Some(allowed)
    }

    pub fn has_pending_event(&mut self) -> bool {
        let data = self.current_data();
        let Ok(allowed) = Allowed::try_from(data.to_raw()) else {
            return false;
        };
        self.cycle == allowed.cycle_bit()
    }

    pub fn erdp(&self) -> u64 {
        self.current_segment().trb_bus_addr(self.trb_index).raw() & 0xFFFF_FFFF_FFFF_FFF0
    }

    pub fn erst_dequeue_pointer(&self) -> u64 {
        self.erdp() | (self.segment_index as u64 & 0x7)
    }

    pub fn segment_index(&self) -> u8 {
        (self.segment_index & 0x7) as u8
    }

    pub fn erstba(&self) -> u64 {
        self.ste.dma_addr().as_u64()
    }

    pub fn len(&self) -> usize {
        self.ste.len()
    }

    pub fn info(&self) -> EventRingInfo {
        EventRingInfo {
            erstz: self.len() as _,
            erdp: self.erst_dequeue_pointer(),
            erstba: self.erstba(),
        }
    }

    fn current_segment(&self) -> &Ring {
        &self.segments[self.segment_index]
    }

    fn current_data(&self) -> super::ring::TrbData {
        self.current_segment()
            .trbs
            .read(self.trb_index)
            .expect("event ring TRB index out of bounds")
    }

    fn inc_deque(&mut self) {
        self.trb_index += 1;
        if self.trb_index >= TRBS_PER_SEGMENT {
            self.trb_index = 0;
            self.segment_index += 1;
            if self.segment_index >= self.segments.len() {
                self.segment_index = 0;
                self.cycle = !self.cycle;
            }
        }
    }
}

pub struct EventRingInfo {
    pub erstz: u16,
    pub erdp: u64,
    pub erstba: u64,
}

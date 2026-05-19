use alloc::vec::Vec;

use dma_api::{DArray, DmaDirection};
use mbarrier::mb;
use xhci::ring::trb::{Link, command, transfer};

use crate::{
    BusAddr,
    err::*,
    osal::Kernel,
    queue::{Finished, TWaiter},
};

const TRB_LEN: usize = 4;
pub(crate) const TRB_SIZE: usize = size_of::<TrbData>();
pub(crate) const TRBS_PER_SEGMENT: usize = 256;
const DEFAULT_RING_PAGES: usize = 2;

#[derive(Clone)]
#[repr(transparent)]
pub struct TrbData([u32; TRB_LEN]);

impl TrbData {
    pub fn to_raw(&self) -> [u32; TRB_LEN] {
        self.0
    }
}

impl From<command::Allowed> for TrbData {
    fn from(value: command::Allowed) -> Self {
        let raw = value.into_raw();
        Self(raw)
    }
}

impl From<transfer::Allowed> for TrbData {
    fn from(value: transfer::Allowed) -> Self {
        let raw = value.into_raw();
        Self(raw)
    }
}

pub struct Ring {
    link: bool,
    pub trbs: DArray<TrbData>,
    pub i: usize,
    pub cycle: bool,
}

unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    pub fn new_with_len(
        len: usize,
        link: bool,
        direction: DmaDirection,
        dma: &Kernel,
    ) -> core::result::Result<Self, HostError> {
        let trbs = dma.array_zero_with_align(len, dma.page_size(), direction)?;

        Ok(Self {
            link,
            trbs,
            i: 0,
            cycle: link,
        })
    }

    pub fn new(link: bool, direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        Self::new_with_pages(DEFAULT_RING_PAGES, link, direction, dma)
    }

    pub fn new_with_pages(
        pages: usize,
        link: bool,
        direction: DmaDirection,
        dma: &Kernel,
    ) -> Result<Self> {
        let len = (dma.page_size() * pages.max(1)) / TRB_SIZE;
        Ok(Self::new_with_len(len, link, direction, dma)?)
    }

    pub fn new_segment(link: bool, direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        Ok(Self::new_with_len(TRBS_PER_SEGMENT, link, direction, dma)?)
    }

    pub fn len(&self) -> usize {
        self.trbs.len()
    }

    pub fn bus_addr(&self) -> BusAddr {
        self.trbs.dma_addr().as_u64().into()
    }

    pub fn enque_command(&mut self, mut trb: command::Allowed) -> BusAddr {
        if self.cycle {
            trb.set_cycle_bit();
        } else {
            trb.clear_cycle_bit();
        }
        let addr = self.enque_trb(trb.into());
        trace!("[CMD] >> {trb:X?} @{addr:X?}");
        addr
    }

    pub fn enque_transfer(&mut self, mut trb: transfer::Allowed) -> BusAddr {
        if self.cycle {
            trb.set_cycle_bit();
        } else {
            trb.clear_cycle_bit();
        }
        let addr = self.enque_trb(trb.into());
        trace!("[Transfer] >> {trb:X?} @{addr:X?}");
        addr
    }

    fn enque_transfer_deferred_cycle(
        &mut self,
        mut trb: transfer::Allowed,
    ) -> (BusAddr, usize, transfer::Allowed) {
        let mut visible_trb = trb;
        if self.cycle {
            visible_trb.set_cycle_bit();
            trb.clear_cycle_bit();
        } else {
            visible_trb.clear_cycle_bit();
            trb.set_cycle_bit();
        }
        let index = self.i;
        let addr = self.enque_trb(trb.into());
        trace!("[Transfer] >> deferred first {visible_trb:X?} @{addr:X?}");
        (addr, index, visible_trb)
    }

    fn set_transfer_trb(&mut self, index: usize, trb: transfer::Allowed) {
        self.trbs.set(index, trb.into());
    }

    pub fn enque_trb(&mut self, trb: TrbData) -> BusAddr {
        self.trbs.set(self.i, trb);
        let addr = self.trb_bus_addr(self.i);
        self.next_index();
        addr
    }

    fn next_index(&mut self) -> usize {
        self.i += 1;
        let len = self.len();

        // link模式下，最后一个是Link
        if self.link && self.i >= len - 1 {
            self.i = 0;
            trace!("link!");
            let address = self.trb_bus_addr(0);
            let mut link = Link::new();
            link.set_ring_segment_pointer(address.into())
                .set_toggle_cycle();

            if self.cycle {
                link.set_cycle_bit();
            } else {
                link.clear_cycle_bit();
            }
            let trb = command::Allowed::Link(link);

            self.trbs.set(len - 1, trb.into());

            self.cycle = !self.cycle;
        } else if self.i >= len {
            self.i = 0;
        }

        self.i
    }

    pub fn trb_bus_addr(&self, i: usize) -> BusAddr {
        let base = self.bus_addr().raw();
        (base + (i * size_of::<TrbData>()) as u64).into()
    }

    pub fn trb_bus_addr_list(&self) -> impl Iterator<Item = BusAddr> + '_ {
        (0..self.len()).map(move |i| self.trb_bus_addr(i))
    }
}

pub struct SendRing<R> {
    ring: Ring,
    finished: Finished<R>,
}

impl<R> SendRing<R> {
    pub fn new(direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        let ring = Ring::new(true, direction, dma)?;
        let finished = Finished::new(ring.trb_bus_addr_list());
        Ok(Self { ring, finished })
    }

    pub fn new_with_pages(pages: usize, direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        let ring = Ring::new_with_pages(pages, true, direction, dma)?;
        let finished = Finished::new(ring.trb_bus_addr_list());
        Ok(Self { ring, finished })
    }

    pub fn enque_command(&mut self, trb: command::Allowed) -> BusAddr {
        let addr = self.ring.enque_command(trb);
        self.finished.clear_finished(addr);
        addr
    }

    pub fn enque_transfer(&mut self, trb: transfer::Allowed) -> BusAddr {
        let addr = self.ring.enque_transfer(trb);
        self.finished.clear_finished(addr);
        addr
    }

    pub fn enqueue_transfer_td(&mut self, trbs: &mut [transfer::Allowed]) -> Vec<BusAddr> {
        let mut addrs = Vec::with_capacity(trbs.len());
        let Some((first, rest)) = trbs.split_first_mut() else {
            return addrs;
        };

        let (first_addr, first_index, visible_first) =
            self.ring.enque_transfer_deferred_cycle(*first);
        self.finished.clear_finished(first_addr);
        addrs.push(first_addr);

        for trb in rest {
            let addr = self.ring.enque_transfer(*trb);
            self.finished.clear_finished(addr);
            addrs.push(addr);
        }

        mb();
        self.ring.set_transfer_trb(first_index, visible_first);
        addrs
    }

    pub fn take_finished_future(&self, addr: BusAddr) -> TWaiter<R> {
        self.finished.take_waiter(addr)
    }

    pub fn finished_handle(&self) -> Finished<R> {
        self.finished.clone()
    }

    pub fn get_finished(&self, addr: BusAddr) -> Option<R> {
        self.finished.get_finished(addr)
    }

    pub fn register_cx(&self, addr: BusAddr, cx: &mut core::task::Context<'_>) {
        self.finished.register_cx(addr, cx);
    }

    pub fn bus_addr(&self) -> BusAddr {
        self.ring.bus_addr()
    }

    pub fn usable_capacity(&self) -> usize {
        self.ring.len().saturating_sub(1)
    }

    pub fn cycle(&self) -> bool {
        self.ring.cycle
    }
}

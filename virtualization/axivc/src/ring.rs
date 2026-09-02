use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::{IVC_CELL_SIZE, IVC_RING_CAPACITY};

/// Direction of a one-way IVC ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum IvcRingDirection {
    /// Cells sent by the channel publisher and received by the subscriber.
    PublisherToSubscriber = 1,
    /// Cells sent by the subscriber and received by the publisher.
    SubscriberToPublisher = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IvcCellError {
    Full,
}

/// Single-producer, single-consumer opaque-cell ring.
#[repr(C, align(64))]
pub(crate) struct IvcRing {
    direction: AtomicU32,
    capacity: AtomicU32,
    cell_size: AtomicU32,
    head: AtomicU32,
    tail: AtomicU32,
    reserved: [AtomicU32; 3],
    cells: [IvcCell; IVC_RING_CAPACITY],
}

// SAFETY: Endpoint attachment guarantees exactly one producer and one consumer
// for this ring. The producer exclusively writes an unpublished cell before
// releasing `tail`. The consumer acquires `tail`, exclusively reads that cell,
// and releases `head` only after copying it. The producer acquires `head`
// before reusing a cell, so accesses through each cell's UnsafeCell cannot race.
unsafe impl Sync for IvcRing {}

impl IvcRing {
    pub(crate) fn initialize(&self, direction: IvcRingDirection) {
        self.direction.store(direction as u32, Ordering::Relaxed);
        self.capacity
            .store(IVC_RING_CAPACITY as u32, Ordering::Relaxed);
        self.cell_size
            .store(IVC_CELL_SIZE as u32, Ordering::Relaxed);
        self.head.store(0, Ordering::Relaxed);
        for cell in &self.cells {
            cell.clear();
        }
        self.tail.store(0, Ordering::Release);
    }

    pub(crate) fn layout_matches(&self, direction: IvcRingDirection) -> bool {
        self.direction.load(Ordering::Relaxed) == direction as u32
            && self.capacity.load(Ordering::Relaxed) == IVC_RING_CAPACITY as u32
            && self.cell_size.load(Ordering::Relaxed) == IVC_CELL_SIZE as u32
    }

    pub(crate) fn try_push_cell(&self, cell: &[u8; IVC_CELL_SIZE]) -> Result<(), IvcCellError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) as usize >= IVC_RING_CAPACITY {
            return Err(IvcCellError::Full);
        }

        let cell_index = tail as usize % IVC_RING_CAPACITY;
        self.cells[cell_index].write(cell);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub(crate) fn try_peek_cell(&self, output: &mut [u8; IVC_CELL_SIZE]) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return false;
        }

        let cell_index = head as usize % IVC_RING_CAPACITY;
        self.cells[cell_index].read(output);
        true
    }

    pub(crate) fn pop_cell(&self) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        debug_assert_ne!(head, tail, "a cell must be peeked before it is popped");
        self.head.store(head.wrapping_add(1), Ordering::Release);
    }
}

/// One fixed-size opaque ring cell.
#[repr(C, align(64))]
struct IvcCell {
    bytes: UnsafeCell<[u8; IVC_CELL_SIZE]>,
}

impl IvcCell {
    fn clear(&self) {
        unsafe {
            // Initialization occurs before the region is published to a peer.
            self.bytes.get().write([0; IVC_CELL_SIZE]);
        }
    }

    fn write(&self, cell: &[u8; IVC_CELL_SIZE]) {
        unsafe {
            // This producer owns the cell until `tail` publishes it.
            self.bytes.get().write(*cell);
        }
    }

    fn read(&self, output: &mut [u8; IVC_CELL_SIZE]) {
        unsafe {
            // Acquire of `tail` makes the producer's complete cell visible;
            // `head` is not released until this copy has completed.
            output.copy_from_slice(&*self.bytes.get());
        }
    }
}

#[cfg(test)]
pub(crate) fn new_ring_for_test() -> IvcRing {
    IvcRing {
        direction: AtomicU32::new(0),
        capacity: AtomicU32::new(0),
        cell_size: AtomicU32::new(0),
        head: AtomicU32::new(0),
        tail: AtomicU32::new(0),
        reserved: [const { AtomicU32::new(0) }; 3],
        cells: [const {
            IvcCell {
                bytes: UnsafeCell::new([0; IVC_CELL_SIZE]),
            }
        }; IVC_RING_CAPACITY],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_cells_are_fifo_and_full_cells_are_not_overwritten() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);

        for value in 0..IVC_RING_CAPACITY {
            ring.try_push_cell(&[value as u8; IVC_CELL_SIZE]).unwrap();
        }
        assert_eq!(
            ring.try_push_cell(&[0xff; IVC_CELL_SIZE]),
            Err(IvcCellError::Full)
        );

        for value in 0..IVC_RING_CAPACITY {
            let mut cell = [0u8; IVC_CELL_SIZE];
            assert!(ring.try_peek_cell(&mut cell));
            assert_eq!(cell, [value as u8; IVC_CELL_SIZE]);
            ring.pop_cell();
        }
        let mut cell = [0u8; IVC_CELL_SIZE];
        assert!(!ring.try_peek_cell(&mut cell));
    }
}

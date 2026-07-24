//! Common split-virtqueue correctness tests (plan section 13.1).
//!
//! These tests drive `VirtioQueue` through a mock guest-memory accessor whose
//! `translate_and_get_limit` returns real host pointers into a backing buffer,
//! so the trait's default `read_obj`/`write_obj`/`read_buffer`/`write_buffer`
//! operate on the same memory the test sets up and later inspects.

use std::sync::Arc;

use ax_memory_addr::PhysAddr;
use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{VirtioError, VirtioQueue, constants::*};
use axvm_types::GuestPhysAddr;

/// Mock guest memory: a flat backing buffer where guest physical address `gpa`
/// maps to `buf[gpa..]`. The accessor returns real host pointers, so the trait's
/// default volatile read/write helpers work and the test can verify state by
/// reading `buf` directly.
#[derive(Clone)]
struct MockMem {
    buf: std::vec::Vec<u8>,
}

impl MockMem {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0u8; len],
        }
    }

    /// Write bytes at a guest offset via the accessor (so setup goes through the
    /// same path the device uses, no `&mut` needed through the shared `Arc`).
    fn put(&self, off: usize, bytes: &[u8]) {
        self.write_buffer(GuestPhysAddr::from(off), bytes).unwrap();
    }
}

impl GuestMemoryAccessor for MockMem {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        let off = guest_addr.as_usize();
        if off < self.buf.len() {
            let host = self.buf.as_ptr() as usize + off;
            Some((PhysAddr::from(host), self.buf.len() - off))
        } else {
            None
        }
    }
}

/// A fully-wired queue plus its guest-memory layout offsets.
struct Fixture {
    mem: Arc<MockMem>,
    queue: VirtioQueue<MockMem>,
    size: u16,
    desc_base: usize,
    avail_base: usize,
    used_base: usize,
}

/// Layout (all bases 16-byte aligned to keep volatile accesses aligned):
/// desc table | avail ring | used ring. A non-zero base is used because the
/// queue treats guest address `0` as the "unconfigured" sentinel.
fn layout(size: u16) -> (usize, usize, usize, usize) {
    let desc = 0x1000usize;
    let desc_size = size as usize * 16;
    let avail = round_up(desc + desc_size, 16);
    let avail_size = 4 + size as usize * 2 + 2;
    let used = round_up(avail + avail_size, 16);
    let used_size = 4 + size as usize * 8 + 2;
    let total = round_up(used + used_size, 16);
    (desc, avail, used, total)
}

fn round_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

impl Fixture {
    /// Build a queue of `size` descriptors, programmed and marked ready.
    fn new(size: u16) -> Self {
        let (desc_base, avail_base, used_base, total) = layout(size);
        let mem = Arc::new(MockMem::new(total));
        let mut queue = VirtioQueue::new(0, size, mem.clone());
        queue
            .set_desc_table_addr(GuestPhysAddr::from(desc_base))
            .unwrap();
        queue
            .set_avail_ring_addr(GuestPhysAddr::from(avail_base))
            .unwrap();
        queue
            .set_used_ring_addr(GuestPhysAddr::from(used_base))
            .unwrap();
        queue.set_ready(true);
        Self {
            mem,
            queue,
            size,
            desc_base,
            avail_base,
            used_base,
        }
    }

    /// Write a descriptor at `index`.
    fn set_desc(&self, index: u16, addr: usize, len: u32, flags: u16, next: u16) {
        // VirtQueueDesc is repr(C): base_addr(usize), len(u32), flags(u16), next(u16).
        let off = self.desc_base + index as usize * 16;
        let mut b = [0u8; 16];
        b[0..8].copy_from_slice(&(addr as u64).to_le_bytes());
        b[8..12].copy_from_slice(&len.to_le_bytes());
        b[12..14].copy_from_slice(&flags.to_le_bytes());
        b[14..16].copy_from_slice(&next.to_le_bytes());
        self.mem.put(off, &b);
    }

    /// Set the avail-ring head index `avail.idx`.
    fn set_avail_idx(&self, idx: u16) {
        self.mem.put(self.avail_base + 2, &idx.to_le_bytes());
    }

    /// Set avail-ring entry `pos` to descriptor head `head`.
    fn set_avail_entry(&self, pos: u16, head: u16) {
        let off = self.avail_base + 4 + pos as usize * 2;
        self.mem.put(off, &head.to_le_bytes());
    }

    /// Set the avail-ring flags.
    fn set_avail_flags(&self, flags: u16) {
        self.mem.put(self.avail_base, &flags.to_le_bytes());
    }

    /// Read the used-ring head index `used.idx`.
    fn used_idx(&self) -> u16 {
        let off = self.used_base + 2;
        u16::from_le_bytes([self.mem.buf[off], self.mem.buf[off + 1]])
    }

    /// Read a used element's (id, len).
    fn used_elem(&self, pos: u16) -> (u32, u32) {
        let off = self.used_base + 4 + pos as usize * 8;
        let id = u32::from_le_bytes(self.mem.buf[off..off + 4].try_into().unwrap());
        let len = u32::from_le_bytes(self.mem.buf[off + 4..off + 8].try_into().unwrap());
        (id, len)
    }
}

// ---------------------------------------------------------------------------
// Queue configuration
// ---------------------------------------------------------------------------

#[test]
fn set_size_rejects_zero_non_pow2_and_too_large() {
    let mem = Arc::new(MockMem::new(4096));
    let mut q = VirtioQueue::new(0, 8, mem);
    assert_eq!(q.set_size(0).unwrap_err(), VirtioError::InvalidQueue);
    assert_eq!(q.set_size(3).unwrap_err(), VirtioError::InvalidQueue); // non power of two
    assert_eq!(q.set_size(16).unwrap_err(), VirtioError::InvalidQueue); // > max (8)
    q.set_size(8).unwrap(); // valid
}

#[test]
fn address_setters_overwrite_so_low_high_combine() {
    // Regression for plan 3.2: programming a 64-bit address via LOW then HIGH
    // must not be rejected on the second write.
    let mem = Arc::new(MockMem::new(4096));
    let mut q = VirtioQueue::new(0, 4, mem);
    // First (LOW) write sets a non-zero address.
    q.set_desc_table_addr(GuestPhysAddr::from(0x0000_00ff))
        .unwrap();
    // Second (HIGH) write replaces it with the combined value.
    q.set_desc_table_addr(GuestPhysAddr::from(0x1234_0000_0000_00ff))
        .unwrap();
    assert_eq!(
        q.desc_table_addr.as_usize(),
        0x1234_0000_0000_00ff,
        "second write must overwrite, not be rejected"
    );

    // HIGH-then-LOW order must also work.
    q.set_avail_ring_addr(GuestPhysAddr::from(0x1234_0000_0000_0000))
        .unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x0000_00ab))
        .unwrap();
    assert_eq!(q.avail_ring_addr.as_usize(), 0x0000_00ab);
}

#[test]
fn reset_clears_addresses_ready_and_indexes() {
    let mut f = Fixture::new(4);
    f.set_avail_idx(2);
    f.queue.pop_available_head().unwrap();
    f.queue.reset();
    assert!(!f.queue.ready);
    assert_eq!(f.queue.desc_table_addr.as_usize(), 0);
    assert_eq!(f.queue.avail_ring_addr.as_usize(), 0);
    assert_eq!(f.queue.used_ring_addr.as_usize(), 0);
    assert_eq!(f.queue.get_last_avail_idx(), 0);
}

// ---------------------------------------------------------------------------
// Available-ring consumption (plan 3.1)
// ---------------------------------------------------------------------------

#[test]
fn pop_available_head_empty_returns_none() {
    let mut f = Fixture::new(4);
    f.set_avail_idx(0); // empty
    assert!(f.queue.pop_available_head().unwrap().is_none());
}

#[test]
fn pop_available_head_consumes_in_order() {
    let mut f = Fixture::new(4);
    f.set_desc(0, 0x1000, 16, 0, 0);
    f.set_desc(1, 0x2000, 16, 0, 0);
    f.set_avail_entry(0, 0);
    f.set_avail_entry(1, 1);
    f.set_avail_idx(2);

    assert_eq!(f.queue.pop_available_head().unwrap(), Some(0));
    assert_eq!(f.queue.pop_available_head().unwrap(), Some(1));
    assert!(f.queue.pop_available_head().unwrap().is_none());
}

#[test]
fn pop_available_head_wraps_u16_index() {
    let mut f = Fixture::new(4);
    f.set_desc(0, 0x1000, 16, 0, 0);
    f.set_avail_entry(0, 0);
    // Simulate last_avail_idx already at u16::MAX and avail.idx wrapped to 0.
    f.queue.update_last_avail_idx(u16::MAX);
    f.set_avail_idx(0);
    assert_eq!(f.queue.pop_available_head().unwrap(), Some(0));
    // last_avail advanced to 0 (MAX + 1 wraps).
    assert_eq!(f.queue.get_last_avail_idx(), 0);
}

#[test]
fn pop_available_head_detects_ring_corruption() {
    let mut f = Fixture::new(4);
    // avail.idx more than `size` ahead of last_avail -> corruption.
    f.set_avail_idx(f.size + 5);
    assert_eq!(
        f.queue.pop_available_head().unwrap_err(),
        VirtioError::InvalidQueue
    );
}

// ---------------------------------------------------------------------------
// Descriptor-chain validation (plan 3.3, 4.3)
// ---------------------------------------------------------------------------

#[test]
fn descriptor_chain_single_and_multi() {
    let f = Fixture::new(4);
    f.set_desc(0, 0x1000, 8, VIRTQ_DESC_F_NEXT, 1);
    f.set_desc(1, 0x2000, 12, VIRTQ_DESC_F_WRITE, 0);
    let chain = f.queue.descriptor_chain(0).unwrap();
    assert_eq!(chain.head(), 0);
    assert_eq!(chain.len(), 2);
    assert_eq!(chain.readable_len().unwrap(), 8); // desc 0 readable
    assert_eq!(chain.writable_len().unwrap(), 12); // desc 1 writable
}

#[test]
fn descriptor_chain_rejects_indirect() {
    let f = Fixture::new(4);
    f.set_desc(
        0,
        0x1000,
        16,
        axvirtio_common::constants::VIRTQ_DESC_F_INDIRECT,
        0,
    );
    assert_eq!(
        f.queue.descriptor_chain(0).unwrap_err(),
        VirtioError::NotSupported
    );
}

#[test]
fn descriptor_chain_rejects_cycle() {
    let f = Fixture::new(4);
    // 0 -> 1 -> 0 (cycle)
    f.set_desc(0, 0x1000, 8, VIRTQ_DESC_F_NEXT, 1);
    f.set_desc(1, 0x2000, 8, VIRTQ_DESC_F_NEXT, 0);
    assert!(matches!(
        f.queue.descriptor_chain(0),
        Err(VirtioError::InvalidDescriptor)
    ));
}

#[test]
fn descriptor_chain_rejects_next_out_of_bounds() {
    let f = Fixture::new(4);
    f.set_desc(0, 0x1000, 8, VIRTQ_DESC_F_NEXT, 99);
    assert_eq!(
        f.queue.descriptor_chain(0).unwrap_err(),
        VirtioError::InvalidDescriptor
    );
}

// ---------------------------------------------------------------------------
// Used-ring completion and notification (plan 5.3)
// ---------------------------------------------------------------------------

#[test]
fn complete_writes_used_element_and_advances_idx() {
    let mut f = Fixture::new(4);
    let notify = f.queue.complete(7, 128).unwrap();
    assert!(notify, "without NO_INTERRUPT the driver must be notified");
    assert_eq!(f.used_idx(), 1);
    let (id, len) = f.used_elem(0);
    assert_eq!(id, 7);
    assert_eq!(len, 128);
}

#[test]
fn no_interrupt_flag_suppresses_notification() {
    let mut f = Fixture::new(4);
    f.set_avail_flags(VIRTQ_AVAIL_F_NO_INTERRUPT);
    let notify = f.queue.complete(3, 64).unwrap();
    // Completion still happens (used ring updated)...
    assert_eq!(f.used_idx(), 1);
    // ...but the driver must not be interrupted.
    assert!(!notify);
}

#[test]
fn complete_wraps_used_idx_at_queue_size() {
    let mut f = Fixture::new(2);
    f.queue.complete(1, 1).unwrap();
    f.queue.complete(2, 2).unwrap();
    assert_eq!(f.used_idx(), 2);
    // Third completion wraps the ring slot index (pos = 2 % 2 == 0).
    f.queue.complete(3, 3).unwrap();
    assert_eq!(f.used_idx(), 3);
    let (id, _) = f.used_elem(0);
    assert_eq!(id, 3, "slot 0 reused after wrap");
}

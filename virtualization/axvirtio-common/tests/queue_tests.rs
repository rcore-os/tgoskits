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

struct PublishAfterAvailEventWrite {
    mem: Arc<MockMem>,
    avail_event_addr: GuestPhysAddr,
    avail_idx_addr: GuestPhysAddr,
    published_idx: u16,
}

impl axvirtio_common::GuestMemory for PublishAfterAvailEventWrite {
    fn read(
        &mut self,
        guest_addr: GuestPhysAddr,
        data: &mut [u8],
    ) -> axvirtio_common::VirtioResult<()> {
        self.mem
            .read_buffer(guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    fn write(
        &mut self,
        guest_addr: GuestPhysAddr,
        data: &[u8],
    ) -> axvirtio_common::VirtioResult<()> {
        self.mem
            .write_buffer(guest_addr, data)
            .map_err(|_| VirtioError::InvalidAddress)?;
        if guest_addr == self.avail_event_addr {
            self.mem.put(
                self.avail_idx_addr.as_usize(),
                &self.published_idx.to_le_bytes(),
            );
        }
        Ok(())
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

    /// Set the driver-owned `used_event` footer in the available ring.
    fn set_used_event(&self, event: u16) {
        let off = self.avail_base + 4 + self.size as usize * 2;
        self.mem.put(off, &event.to_le_bytes());
    }

    /// Read the used-ring head index `used.idx`.
    fn used_idx(&self) -> u16 {
        let off = self.used_base + 2;
        u16::from_le_bytes([self.mem.buf[off], self.mem.buf[off + 1]])
    }

    /// Read the device-owned `avail_event` footer in the used ring.
    fn avail_event(&self) -> u16 {
        let off = self.used_base + 4 + self.size as usize * 8;
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
fn setter_combined_layout_fails_alignment_validation() {
    // The overwrite setter semantics intentionally accept non-aligned
    // addresses; the resulting layout must be rejected by `validate_layout`.
    // Constructed independently of `address_setters_overwrite_so_low_high_combine`
    // so this assertion does not depend on that test's intermediate state.
    let mem = Arc::new(MockMem::new(4096));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x0000_00ff))
        .unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    assert_eq!(
        q.validate_layout().unwrap_err(),
        VirtioError::RingMisaligned,
        "the desc table address 0x..ff is not 16-byte aligned"
    );
}

#[test]
fn set_size_after_ring_addresses_is_rejected() {
    // The ring objects snapshot the queue size when their address is
    // programmed, so changing `size` after that would validate the layout for
    // the new size while runtime ring accesses stay bounded by the old one: a
    // guest could serve requests outside the validated regions. The layout
    // must therefore stay consistent with the size the rings were built for.
    let mem = Arc::new(MockMem::new(4096));
    let mut q = VirtioQueue::new(0, 8, mem.clone());
    q.set_size(4).unwrap();
    q.set_desc_table_addr(GuestPhysAddr::from(0x1000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    assert_eq!(
        q.set_size(8).unwrap_err(),
        VirtioError::InvalidQueue,
        "resizing after ring addresses are programmed must be rejected"
    );
    assert_eq!(q.size, 4, "the size must not change on a rejected write");

    // The same guard applies while the queue is ready: the programmed rings
    // are in use.
    let mut f = Fixture::new(4);
    f.queue.set_ready(true);
    assert_eq!(f.queue.set_size(4).unwrap_err(), VirtioError::InvalidQueue);
    assert!(f.queue.is_valid(), "the ready queue must stay usable");
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
fn event_idx_rearm_publishes_the_next_expected_available_index() {
    let mut f = Fixture::new(4);
    f.queue.event_idx_enabled = true;
    f.queue.update_last_avail_idx(2);
    f.set_avail_idx(2);

    assert!(!f.queue.rearm_available_event().unwrap());
    assert_eq!(f.avail_event(), 2);
}

#[test]
fn event_idx_rearm_detects_buffers_published_before_the_recheck() {
    let mut f = Fixture::new(4);
    f.queue.event_idx_enabled = true;
    f.queue.update_last_avail_idx(2);
    f.set_avail_idx(2);
    let mut memory = PublishAfterAvailEventWrite {
        mem: Arc::clone(&f.mem),
        avail_event_addr: GuestPhysAddr::from(f.used_base + 4 + f.size as usize * 8),
        avail_idx_addr: GuestPhysAddr::from(f.avail_base + 2),
        published_idx: 3,
    };

    assert!(
        f.queue
            .rearm_available_event_with_memory(&mut memory)
            .unwrap()
    );
    assert_eq!(f.avail_event(), 2);
}

#[test]
fn event_idx_rearm_detects_available_index_wraparound() {
    let mut f = Fixture::new(4);
    f.queue.event_idx_enabled = true;
    f.queue.update_last_avail_idx(u16::MAX);
    f.set_avail_idx(0);

    assert!(f.queue.rearm_available_event().unwrap());
    assert_eq!(f.avail_event(), u16::MAX);
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

#[test]
fn descriptor_chain_accepts_full_size_chain() {
    let f = Fixture::new(4);
    for i in 0..4u16 {
        let next = if i == 3 { 0 } else { i + 1 };
        let flags = if i == 3 { 0 } else { VIRTQ_DESC_F_NEXT };
        f.set_desc(i, 0x1000 + i as usize * 16, 8, flags, next);
    }
    let chain = f.queue.descriptor_chain(0).unwrap();
    assert_eq!(
        chain.len(),
        4,
        "a full-size chain (len == size) must be accepted"
    );

    // A chain that walks one descriptor beyond `size` is a cycle and must be
    // rejected on both paths.
    f.set_desc(3, 0x1000 + 3 * 16, 8, VIRTQ_DESC_F_NEXT, 0); // 0->1->2->3->0 cycle
    assert!(matches!(
        f.queue.descriptor_chain(0),
        Err(VirtioError::InvalidDescriptor)
    ));
}

// ---------------------------------------------------------------------------
// Ring layout validation
// ---------------------------------------------------------------------------

#[test]
fn validate_layout_rejects_zero_addresses() {
    let mem = Arc::new(MockMem::new(4096));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    // desc address is still 0 -> layout invalid.
    assert_eq!(
        q.validate_layout().unwrap_err(),
        VirtioError::InvalidRingLayout
    );
    // A layout check must not touch `ready`.
    q.set_ready(true);
    assert!(!q.is_valid());
}

#[test]
fn validate_layout_rejects_misaligned_rings() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x1000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    // used ring at a 3-byte offset: not 4-byte aligned.
    q.set_used_ring_addr(GuestPhysAddr::from(0x3003)).unwrap();
    assert_eq!(
        q.validate_layout().unwrap_err(),
        VirtioError::RingMisaligned
    );
}

#[test]
fn validate_layout_rejects_overlapping_rings() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x1000)).unwrap();
    // avail ring starts inside the descriptor table region.
    q.set_avail_ring_addr(GuestPhysAddr::from(0x1008)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    assert_eq!(q.validate_layout().unwrap_err(), VirtioError::RingOverlap);
}

#[test]
fn validate_layout_rejects_used_ring_aliasing_avail_footer() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x1000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    // The used ring starts exactly at the available ring's `used_event`
    // footer address (avail + 4 + size*2). The 2-byte footer belongs to the
    // available ring region, so the two rings alias and must be rejected.
    q.set_used_ring_addr(GuestPhysAddr::from(0x200c)).unwrap();
    assert_eq!(q.validate_layout().unwrap_err(), VirtioError::RingOverlap);
}

#[test]
fn validate_layout_rejects_avail_ring_aliasing_used_footer() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x1000)).unwrap();
    // The available ring starts exactly at the used ring's `avail_event`
    // footer address (used + 4 + size*8), aliasing the footer bytes the
    // device writes after `add_used`.
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3024)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    assert_eq!(q.validate_layout().unwrap_err(), VirtioError::RingOverlap);
}

#[test]
fn validate_layout_rejects_overflowing_ring() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    // desc table base + size*16 overflows the address space.
    q.set_desc_table_addr(GuestPhysAddr::from(usize::MAX - 15))
        .unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    assert_eq!(
        q.validate_layout().unwrap_err(),
        VirtioError::InvalidRingLayout
    );
}

#[test]
fn validate_layout_accepts_proper_layout() {
    let f = Fixture::new(4);
    assert!(f.queue.validate_layout().is_ok());
}

#[test]
fn validate_layout_rejects_overflowing_region_adjacent_to_valid_ring() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    // desc table base + size*16 wraps around the address space.
    q.set_desc_table_addr(GuestPhysAddr::from(usize::MAX - 15))
        .unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    assert_eq!(
        q.validate_layout().unwrap_err(),
        VirtioError::InvalidRingLayout
    );
}

// ---------------------------------------------------------------------------
// Faulted state
// ---------------------------------------------------------------------------

#[test]
fn descriptor_chain_failure_faults_queue_and_blocks_pop_complete() {
    let mut f = Fixture::new(4);
    // A cyclic chain fails validation in `descriptor_chain`.
    f.set_desc(0, 0x1000, 8, VIRTQ_DESC_F_NEXT, 1);
    f.set_desc(1, 0x2000, 8, VIRTQ_DESC_F_NEXT, 0);
    assert!(matches!(
        f.queue.descriptor_chain(0),
        Err(VirtioError::InvalidDescriptor)
    ));
    assert!(
        f.queue.is_faulted(),
        "validation failure must fault the queue"
    );

    // pop/complete are rejected while faulted, with no partial completion.
    assert_eq!(
        f.queue.pop_available_head().unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.complete(0, 0).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.used_idx(),
        0,
        "no used element may be written while faulted"
    );

    // reset clears the faulted state (and the programmed layout, per the
    // existing reset semantics) and restores operation once re-programmed.
    f.queue.reset();
    assert!(!f.queue.is_faulted());
    f.queue
        .set_desc_table_addr(GuestPhysAddr::from(f.desc_base))
        .unwrap();
    f.queue
        .set_avail_ring_addr(GuestPhysAddr::from(f.avail_base))
        .unwrap();
    f.queue
        .set_used_ring_addr(GuestPhysAddr::from(f.used_base))
        .unwrap();
    f.queue.set_ready(true);
    // The ring memory is still valid for this fixture; a fresh chain works.
    f.set_desc(0, 0x1000, 8, 0, 0);
    f.set_avail_idx(1);
    f.set_avail_entry(0, 0);
    assert!(f.queue.pop_available().is_ok());
    assert_eq!(f.used_idx(), 0);

    assert!(f.queue.complete(3, 42).is_ok());
    assert_eq!(f.used_idx(), 1, "used ring must advance after reset");
    assert_eq!(f.used_elem(0), (3, 42));
}

#[test]
fn unconfigured_queue_failures_do_not_fault() {
    let mem = Arc::new(MockMem::new(4096));
    let mut q = VirtioQueue::new(0, 4, mem);
    assert_eq!(
        q.descriptor_chain(0).unwrap_err(),
        VirtioError::QueueNotReady
    );
    assert_eq!(
        q.get_status_addr(0).unwrap_err(),
        VirtioError::QueueNotReady
    );
    assert_eq!(q.should_notify().unwrap_err(), VirtioError::QueueNotReady);
    assert!(!q.is_faulted(), "unconfigured must not latch a fault");

    // A faulted queue reports QueueFaulted for these entry points before any
    // QueueNotReady check. Fault the queue via a real runtime failure (the
    // avail ring is unconfigured at address 0, so reading it fails).
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    assert_eq!(q.should_notify().unwrap_err(), VirtioError::InvalidAddress);
    assert!(q.is_faulted());
    assert_eq!(
        q.descriptor_chain(0).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(q.should_notify().unwrap_err(), VirtioError::QueueFaulted);
}

#[test]
fn descriptor_chain_memory_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x100));
    let mut q = VirtioQueue::new(0, 4, mem.clone());
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap(); // beyond the mock backing -> unmapped
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    let mut memory = axvirtio_common::AddressSpaceMemory::new(&*mem);
    assert_eq!(
        q.descriptor_chain_with_memory(0, &mut memory).unwrap_err(),
        VirtioError::InvalidAddress
    );
    assert!(q.is_faulted(), "memory read failure must fault the queue");
    assert_eq!(
        q.complete_with_memory(0, 0, &mut memory).unwrap_err(),
        VirtioError::QueueFaulted
    );
}

#[test]
fn pop_available_head_read_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x3005));
    let mut q = VirtioQueue::new(0, 4, mem.clone());
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap(); // beyond the mock backing -> unmapped
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    let mut memory = axvirtio_common::AddressSpaceMemory::new(&*mem);
    // avail.idx reads 0 -> empty, no memory touch beyond the header, no fault.
    assert!(
        q.pop_available_head_with_memory(&mut memory)
            .unwrap()
            .is_none()
    );
    assert!(!q.is_faulted());

    // A pending entry makes the head read at base + 4 cross the mapped
    // boundary, which must fault the queue.
    mem.put(0x3002, &1u16.to_le_bytes());
    assert_eq!(
        q.pop_available_head_with_memory(&mut memory).unwrap_err(),
        VirtioError::InvalidAddress
    );
    assert!(q.is_faulted(), "head read failure must fault the queue");
}

#[test]
fn read_avail_idx_pre_read_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x3002));
    let mut q = VirtioQueue::new(0, 4, mem.clone());
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap();
    // The avail ring header is configured but its `idx` field (base + 2)
    // crosses the mapped boundary: the pre-read must fail and latch.
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    let mut memory = axvirtio_common::AddressSpaceMemory::new(&*mem);
    assert_eq!(
        q.read_avail_idx_with_memory(&mut memory).unwrap_err(),
        VirtioError::InvalidAddress
    );
    assert!(
        q.is_faulted(),
        "avail-index pre-read failure on a configured queue must fault it"
    );
    // The queue stays faulted: the same read and the drain entry points reject
    // until reset, instead of repeating the failure every call.
    assert_eq!(
        q.read_avail_idx_with_memory(&mut memory).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        q.pop_available_head_with_memory(&mut memory).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        q.read_avail_entry_with_memory(0, &mut memory).unwrap_err(),
        VirtioError::QueueFaulted
    );
}

#[test]
fn read_avail_entry_pre_read_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x3004));
    let mut q = VirtioQueue::new(0, 4, mem.clone());
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    let mut memory = axvirtio_common::AddressSpaceMemory::new(&*mem);
    // entry 0 lives at avail + 4 == 0x3004, which is beyond the mapped
    // boundary, so the 2-byte entry read fails.
    assert_eq!(
        q.read_avail_entry_with_memory(0, &mut memory).unwrap_err(),
        VirtioError::InvalidAddress
    );
    assert!(
        q.is_faulted(),
        "avail-entry pre-read failure on a configured queue must fault it"
    );
    assert_eq!(
        q.read_avail_entry_with_memory(0, &mut memory).unwrap_err(),
        VirtioError::QueueFaulted
    );
}

#[test]
fn read_avail_idx_non_memory_read_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x3002));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap();
    // The avail ring header is configured but its `idx` field (base + 2)
    // crosses the mapped boundary; the non-`_with_memory` API reads through
    // the queue's own accessor and must latch the fault like its
    // `_with_memory` twin.
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    assert_eq!(q.read_avail_idx().unwrap_err(), VirtioError::InvalidAddress);
    assert!(
        q.is_faulted(),
        "avail-index read failure through the non-memory API must fault the queue"
    );
    // The queue stays faulted: repeating the read reports QueueFaulted
    // instead of retrying the failing access.
    assert_eq!(q.read_avail_idx().unwrap_err(), VirtioError::QueueFaulted);
}

#[test]
fn read_avail_entry_non_memory_read_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x3004));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    // Entry 0 lives at avail + 4 == 0x3004, beyond the mapped boundary; the
    // non-`_with_memory` API must latch the fault like its twin.
    assert_eq!(
        q.read_avail_entry(0).unwrap_err(),
        VirtioError::InvalidAddress
    );
    assert!(
        q.is_faulted(),
        "avail-entry read failure through the non-memory API must fault the queue"
    );
    assert_eq!(
        q.read_avail_entry(0).unwrap_err(),
        VirtioError::QueueFaulted
    );
}

#[test]
fn add_used_write_failure_faults_queue() {
    let mem = Arc::new(MockMem::new(0x100));
    let mut q = VirtioQueue::new(0, 4, mem.clone());
    q.set_desc_table_addr(GuestPhysAddr::from(0x2000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x3000)).unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(0x4000)).unwrap();
    q.set_ready(true);
    assert_eq!(q.add_used(0, 0).unwrap_err(), VirtioError::InvalidAddress);
    assert!(
        q.is_faulted(),
        "used-ring write failure must fault the queue"
    );
    assert_eq!(q.add_used(0, 0).unwrap_err(), VirtioError::QueueFaulted);
    assert_eq!(q.complete(0, 0).unwrap_err(), VirtioError::QueueFaulted);
}

#[test]
fn add_used_without_used_ring_reports_not_ready_without_faulting() {
    let mem = Arc::new(MockMem::new(0x1_0000));
    let mut q = VirtioQueue::new(0, 4, mem);
    q.set_desc_table_addr(GuestPhysAddr::from(0x1000)).unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(0x2000)).unwrap();
    // Program the used address through the public field so the layout
    // validates while the used-ring object stays unconfigured; this is the
    // state the historical fallback used to turn into a silent "success".
    q.used_ring_addr = GuestPhysAddr::from(0x3000);
    q.set_ready(true);
    assert!(q.is_valid());
    assert_eq!(q.add_used(0, 0).unwrap_err(), VirtioError::QueueNotReady);
    assert!(
        !q.is_faulted(),
        "an unconfigured used ring is not a runtime failure"
    );
    assert!(q.get_used_ring().is_none());
}

#[test]
fn faulted_queue_rejects_guest_data_paths() {
    let mut f = Fixture::new(4);
    f.set_desc(0, 0x1000, 8, VIRTQ_DESC_F_NEXT, 1);
    f.set_desc(1, 0x2000, 8, VIRTQ_DESC_F_NEXT, 0);
    assert!(f.queue.descriptor_chain(0).is_err());
    assert!(f.queue.is_faulted());

    assert_eq!(
        f.queue.get_status_addr(0).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.write_status_byte(0, 0).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue
            .get_data_buffers(0, axvirtio_common::VirtioDeviceID::Block)
            .unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.validate_virtio_block_chain(0, 1).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.should_notify().unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.pop_available_head().unwrap_err(),
        VirtioError::QueueFaulted
    );
    // The status byte of the chain is guest memory (desc 1 base_addr
    // 0x2000) and must not be written while faulted. The fixture backing is
    // small, so assert only through the queue API: the earlier
    // `write_status_byte` call already returned QueueFaulted, which is the
    // guarantee that no guest memory is written.
    assert_eq!(
        f.queue.write_status_byte(0, 0xab).unwrap_err(),
        VirtioError::QueueFaulted
    );
}

#[test]
fn write_status_byte_writes_guest_status_and_completes() {
    // Positive control for the faulted-path test: on a healthy queue,
    // `write_status_byte` really writes the chain's status byte (desc 1
    // base_addr 0x2000) and `get_status_addr` resolves it. The backing is
    // large enough to map the status address.
    let (desc_base, avail_base, used_base, _) = layout(4);
    let mem = Arc::new(MockMem::new(0x3000));
    let mut q = VirtioQueue::new(0, 4, mem.clone());
    q.set_desc_table_addr(GuestPhysAddr::from(desc_base))
        .unwrap();
    q.set_avail_ring_addr(GuestPhysAddr::from(avail_base))
        .unwrap();
    q.set_used_ring_addr(GuestPhysAddr::from(used_base))
        .unwrap();
    q.set_ready(true);
    let mut d0 = [0u8; 16];
    d0[0..8].copy_from_slice(&(0x1000u64).to_le_bytes());
    d0[8..12].copy_from_slice(&8u32.to_le_bytes());
    d0[12..14].copy_from_slice(&VIRTQ_DESC_F_NEXT.to_le_bytes());
    d0[14..16].copy_from_slice(&1u16.to_le_bytes());
    mem.put(desc_base, &d0);
    let mut d1 = [0u8; 16];
    d1[0..8].copy_from_slice(&(0x2000u64).to_le_bytes());
    d1[8..12].copy_from_slice(&8u32.to_le_bytes());
    d1[12..14].copy_from_slice(&VIRTQ_DESC_F_WRITE.to_le_bytes());
    mem.put(desc_base + 16, &d1);
    assert_eq!(q.get_status_addr(0).unwrap().as_usize(), 0x2000);
    q.write_status_byte(0, 0xab).unwrap();
    assert_eq!(
        mem.buf[0x2000], 0xab,
        "a healthy queue must write the status byte"
    );
}

#[test]
fn faulted_check_precedes_ready_check_on_complete() {
    let mut f = Fixture::new(4);
    f.set_desc(0, 0x1000, 8, VIRTQ_DESC_F_NEXT, 1);
    f.set_desc(1, 0x2000, 8, VIRTQ_DESC_F_NEXT, 0);
    assert!(f.queue.descriptor_chain(0).is_err());
    assert!(f.queue.is_faulted());
    f.queue.set_ready(false); // now neither ready nor faulted-clear
    assert_eq!(
        f.queue.complete(0, 0).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.add_used(0, 0).unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.pop_available_head().unwrap_err(),
        VirtioError::QueueFaulted
    );
}

#[test]
fn pop_available_faults_queue_on_ring_corruption() {
    let mut f = Fixture::new(4);
    // avail.idx more than `size` ahead of last_avail -> corruption error.
    f.set_avail_idx(f.size + 5);
    assert_eq!(
        f.queue.pop_available_head().unwrap_err(),
        VirtioError::InvalidQueue
    );
    assert!(f.queue.is_faulted());
    assert_eq!(
        f.queue.pop_available_head().unwrap_err(),
        VirtioError::QueueFaulted
    );
    assert_eq!(
        f.queue.complete(0, 0).unwrap_err(),
        VirtioError::QueueFaulted
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
fn event_idx_notifies_only_at_the_requested_used_index() {
    let mut f = Fixture::new(4);
    f.queue.event_idx_enabled = true;
    f.set_avail_flags(VIRTQ_AVAIL_F_NO_INTERRUPT);
    f.set_used_event(0);

    assert!(
        f.queue.complete(3, 64).unwrap(),
        "event_idx must ignore NO_INTERRUPT and notify for used_event 0"
    );
    assert!(
        !f.queue.complete(2, 32).unwrap(),
        "the next completion must stay suppressed until used_event advances"
    );
}

#[test]
fn event_idx_batch_notification_uses_the_previous_check_index() {
    let mut f = Fixture::new(4);
    f.queue.event_idx_enabled = true;
    f.set_used_event(0);

    f.queue.add_used(1, 8).unwrap();
    f.queue.add_used(2, 8).unwrap();

    assert!(f.queue.should_notify().unwrap());
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

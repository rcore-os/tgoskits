use alloc::{borrow::Cow, sync::Arc};
use core::{mem::size_of, task::Context};

use ax_alloc::{UsageKind, global_allocator};
use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange, VirtAddr, align_up_4k};
use ax_runtime::hal::mem::virt_to_phys;
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::io_uring::{
    IORING_FEAT_RW_CUR_POS, IORING_FEAT_SUBMIT_STABLE, IORING_OFF_CQ_RING, IORING_OFF_SQ_RING,
    IORING_OFF_SQES, io_uring_params,
};

use super::FileLike;
use crate::pseudofs::DeviceMmap;

const SQ_HEAD_OFFSET: usize = 0;
const SQ_TAIL_OFFSET: usize = 4;
const SQ_RING_MASK_OFFSET: usize = 8;
const SQ_RING_ENTRIES_OFFSET: usize = 12;
const SQ_FLAGS_OFFSET: usize = 16;
const SQ_DROPPED_OFFSET: usize = 20;
const SQ_ARRAY_OFFSET: usize = 64;

const CQ_HEAD_OFFSET: usize = 0;
const CQ_TAIL_OFFSET: usize = 4;
const CQ_RING_MASK_OFFSET: usize = 8;
const CQ_RING_ENTRIES_OFFSET: usize = 12;
const CQ_OVERFLOW_OFFSET: usize = 16;
const CQ_CQES_OFFSET: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IoUringSqe {
    pub opcode: u8,
    pub flags: u8,
    pub ioprio: u16,
    pub fd: i32,
    pub off: u64,
    pub addr: u64,
    pub len: u32,
    pub rw_flags: u32,
    pub user_data: u64,
    pub buf_index: u16,
    pub personality: u16,
    pub splice_fd_in: i32,
    pub addr3: u64,
    pub pad2: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IoUringCqe {
    pub user_data: u64,
    pub res: i32,
    pub flags: u32,
}

const _: () = assert!(size_of::<IoUringSqe>() == 64);
const _: () = assert!(size_of::<IoUringCqe>() == 16);

struct RingMemory {
    vaddr: VirtAddr,
    paddr: PhysAddr,
    size: usize,
    pages: usize,
}

impl RingMemory {
    fn new(size: usize) -> AxResult<Self> {
        let size = align_up_4k(size);
        let pages = size / PAGE_SIZE_4K;
        let vaddr = VirtAddr::from(
            global_allocator()
                .alloc_pages(pages, PAGE_SIZE_4K, UsageKind::VirtMem)
                .map_err(|_| AxError::NoMemory)?,
        );
        unsafe { core::ptr::write_bytes(vaddr.as_mut_ptr(), 0, size) };
        Ok(Self {
            vaddr,
            paddr: virt_to_phys(vaddr),
            size,
            pages,
        })
    }

    fn phys_range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(self.paddr, self.size)
    }

    fn ptr<T>(&self, offset: usize) -> *mut T {
        debug_assert!(offset + size_of::<T>() <= self.size);
        (self.vaddr.as_usize() + offset) as *mut T
    }

    fn read_u32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.ptr(offset)) }
    }

    fn write_u32(&self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile(self.ptr(offset), value) }
    }

    fn read_sqe(&self, index: u32) -> IoUringSqe {
        unsafe {
            core::ptr::read_volatile(
                self.ptr::<IoUringSqe>(index as usize * size_of::<IoUringSqe>()),
            )
        }
    }

    fn write_cqe(&self, index: u32, cqe: IoUringCqe) {
        unsafe {
            core::ptr::write_volatile(
                self.ptr::<IoUringCqe>(CQ_CQES_OFFSET + index as usize * size_of::<IoUringCqe>()),
                cqe,
            )
        }
    }
}

impl Drop for RingMemory {
    fn drop(&mut self) {
        global_allocator().dealloc_pages(self.vaddr.as_usize(), self.pages, UsageKind::VirtMem);
    }
}

pub struct IoUringRings {
    entries: u32,
    cq_entries: u32,
    sq_ring: RingMemory,
    cq_ring: RingMemory,
    sqes: RingMemory,
}

impl IoUringRings {
    fn new(entries: u32, cq_entries: u32) -> AxResult<Self> {
        let sq_ring_size = SQ_ARRAY_OFFSET + entries as usize * size_of::<u32>();
        let cq_ring_size = CQ_CQES_OFFSET + cq_entries as usize * size_of::<IoUringCqe>();
        let sqes_size = entries as usize * size_of::<IoUringSqe>();
        let rings = Self {
            entries,
            cq_entries,
            sq_ring: RingMemory::new(sq_ring_size)?,
            cq_ring: RingMemory::new(cq_ring_size)?,
            sqes: RingMemory::new(sqes_size)?,
        };
        rings.init();
        Ok(rings)
    }

    fn init(&self) {
        self.sq_ring
            .write_u32(SQ_RING_MASK_OFFSET, self.entries - 1);
        self.sq_ring.write_u32(SQ_RING_ENTRIES_OFFSET, self.entries);
        self.sq_ring.write_u32(SQ_FLAGS_OFFSET, 0);
        self.sq_ring.write_u32(SQ_DROPPED_OFFSET, 0);

        self.cq_ring
            .write_u32(CQ_RING_MASK_OFFSET, self.cq_entries - 1);
        self.cq_ring
            .write_u32(CQ_RING_ENTRIES_OFFSET, self.cq_entries);
        self.cq_ring.write_u32(CQ_OVERFLOW_OFFSET, 0);
    }

    fn mmap_region(self: &Arc<Self>, offset: u64) -> AxResult<DeviceMmap> {
        let range = if offset == IORING_OFF_SQ_RING as u64 {
            self.sq_ring.phys_range()
        } else if offset == IORING_OFF_CQ_RING as u64 {
            self.cq_ring.phys_range()
        } else if offset == IORING_OFF_SQES as u64 {
            self.sqes.phys_range()
        } else {
            return Ok(DeviceMmap::None);
        };
        Ok(DeviceMmap::PhysicalAnchored(range, self.clone()))
    }
}

pub struct IoUring {
    rings: Arc<IoUringRings>,
    submit_lock: Mutex<()>,
    poll_cq: PollSet,
}

impl IoUring {
    pub fn new(entries: u32, cq_entries: u32) -> AxResult<Self> {
        Ok(Self {
            rings: Arc::new(IoUringRings::new(entries, cq_entries)?),
            submit_lock: Mutex::new(()),
            poll_cq: PollSet::new(),
        })
    }

    pub fn fill_params(&self, params: &mut io_uring_params) {
        params.sq_entries = self.rings.entries;
        params.cq_entries = self.rings.cq_entries;
        params.features = IORING_FEAT_SUBMIT_STABLE | IORING_FEAT_RW_CUR_POS;
        params.wq_fd = 0;
        params.resv = [0; 3];

        params.sq_off.head = SQ_HEAD_OFFSET as u32;
        params.sq_off.tail = SQ_TAIL_OFFSET as u32;
        params.sq_off.ring_mask = SQ_RING_MASK_OFFSET as u32;
        params.sq_off.ring_entries = SQ_RING_ENTRIES_OFFSET as u32;
        params.sq_off.flags = SQ_FLAGS_OFFSET as u32;
        params.sq_off.dropped = SQ_DROPPED_OFFSET as u32;
        params.sq_off.array = SQ_ARRAY_OFFSET as u32;
        params.sq_off.resv1 = 0;
        params.sq_off.user_addr = 0;

        params.cq_off.head = CQ_HEAD_OFFSET as u32;
        params.cq_off.tail = CQ_TAIL_OFFSET as u32;
        params.cq_off.ring_mask = CQ_RING_MASK_OFFSET as u32;
        params.cq_off.ring_entries = CQ_RING_ENTRIES_OFFSET as u32;
        params.cq_off.overflow = CQ_OVERFLOW_OFFSET as u32;
        params.cq_off.cqes = CQ_CQES_OFFSET as u32;
        params.cq_off.flags = 0;
        params.cq_off.resv1 = 0;
        params.cq_off.user_addr = 0;
    }

    pub fn submit<F>(&self, to_submit: u32, mut execute: F) -> AxResult<u32>
    where
        F: FnMut(&IoUringSqe) -> i32,
    {
        let _guard = self.submit_lock.lock();
        let sq_head = self.rings.sq_ring.read_u32(SQ_HEAD_OFFSET);
        let sq_tail = self.rings.sq_ring.read_u32(SQ_TAIL_OFFSET);
        let pending = sq_tail.wrapping_sub(sq_head).min(self.rings.entries);
        let count = pending.min(to_submit);
        let mut submitted = 0;

        while submitted < count {
            let sq_pos = sq_head.wrapping_add(submitted) & (self.rings.entries - 1);
            let sqe_index = self
                .rings
                .sq_ring
                .read_u32(SQ_ARRAY_OFFSET + sq_pos as usize * size_of::<u32>());
            if sqe_index >= self.rings.entries {
                let dropped = self.rings.sq_ring.read_u32(SQ_DROPPED_OFFSET);
                self.rings
                    .sq_ring
                    .write_u32(SQ_DROPPED_OFFSET, dropped.wrapping_add(1));
                break;
            }

            let sqe = self.rings.sqes.read_sqe(sqe_index);
            let res = execute(&sqe);
            self.push_cqe(IoUringCqe {
                user_data: sqe.user_data,
                res,
                flags: 0,
            });
            submitted += 1;
        }

        if submitted > 0 {
            self.rings
                .sq_ring
                .write_u32(SQ_HEAD_OFFSET, sq_head.wrapping_add(submitted));
            Ok(submitted)
        } else if count == 0 {
            Ok(0)
        } else {
            Err(AxError::InvalidInput)
        }
    }

    pub fn completion_count(&self) -> u32 {
        let head = self.rings.cq_ring.read_u32(CQ_HEAD_OFFSET);
        let tail = self.rings.cq_ring.read_u32(CQ_TAIL_OFFSET);
        tail.wrapping_sub(head).min(self.rings.cq_entries)
    }

    fn push_cqe(&self, cqe: IoUringCqe) {
        let head = self.rings.cq_ring.read_u32(CQ_HEAD_OFFSET);
        let tail = self.rings.cq_ring.read_u32(CQ_TAIL_OFFSET);
        if tail.wrapping_sub(head) >= self.rings.cq_entries {
            let overflow = self.rings.cq_ring.read_u32(CQ_OVERFLOW_OFFSET);
            self.rings
                .cq_ring
                .write_u32(CQ_OVERFLOW_OFFSET, overflow.wrapping_add(1));
            return;
        }

        let cq_pos = tail & (self.rings.cq_entries - 1);
        self.rings.cq_ring.write_cqe(cq_pos, cqe);
        self.rings
            .cq_ring
            .write_u32(CQ_TAIL_OFFSET, tail.wrapping_add(1));
        self.poll_cq.wake();
    }
}

impl FileLike for IoUring {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[io_uring]".into()
    }

    fn device_mmap(&self, offset: u64) -> AxResult<DeviceMmap> {
        self.rings.mmap_region(offset)
    }
}

impl Pollable for IoUring {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::OUT;
        events.set(IoEvents::IN, self.completion_count() > 0);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_cq.register(context.waker());
        }
    }
}

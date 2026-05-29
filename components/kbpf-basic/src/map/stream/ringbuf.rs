//! BPF ring buffer map implementation.
//! See <https://elixir.bootlin.com/linux/v6.6/source/kernel/bpf/ringbuf.c>
//!
//! See <https://docs.ebpf.io/linux/map-type/BPF_MAP_TYPE_RINGBUF/>
use alloc::{sync::Arc, vec, vec::Vec};
use core::{
    fmt::Debug,
    mem::offset_of,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use crate::{
    BpfError, BpfResult as Result, KernelAuxiliaryOps, PollWaker,
    helper::ringbuf::BpfRingbufFlags,
    map::{BpfMapCommonOps, BpfMapMeta, flags::BpfMapCreateFlags, stream::InnerPage},
};

const RINGBUF_CREATE_FLAG_MASK: BpfMapCreateFlags = BpfMapCreateFlags::NUMA_NODE;
/// consumer page and producer page
const RINGBUF_PGOFF: usize = offset_of!(RingBuf<crate::DummyAuxImpl>, consumer_pos) >> 12;
const RINGBUF_POS_PAGES: usize = 2;
const RINGBUF_NR_META_PAGES: usize = RINGBUF_PGOFF + RINGBUF_POS_PAGES;
const RINGBUF_MAX_RECORD_SZ: u32 = u32::MAX / 4;
const PAGE_SHIFT: u32 = 12;

const PAGE_SIZE: usize = 1 << PAGE_SHIFT;

/// BPF ring buffer constants
const BPF_RINGBUF_BUSY_BIT: u32 = 1 << 31;
const BPF_RINGBUF_DISCARD_BIT: u32 = 1 << 30;
const BPF_RINGBUF_HDR_SZ: u32 = core::mem::size_of::<BpfRingBufHdr>() as u32;

#[repr(align(4096))]
struct AlignedPos(u64);

#[repr(C)]
pub struct RingBuf<F: KernelAuxiliaryOps> {
    nr_pages: u32,
    mask: u64,
    pages: &'static [InnerPage<F>],
    phys_addrs: &'static [usize],
    // we can't directly use Arc<dyn PollWaker> here because RingBuf is
    // created in a special way (with vmap), so we store a raw pointer.
    poll_waker: *const dyn PollWaker,
    // For user-space producer ring buffers, an atomic_t busy bit is used
    // to synchronize access to the ring buffers in the kernel, rather than
    // the spinlock that is used for kernel-producer ring buffers. This is
    // done because the ring buffer must hold a lock across a BPF program's
    // callback:
    //
    //    __bpf_user_ringbuf_peek() // lock acquired
    // -> program callback_fn()
    // -> __bpf_user_ringbuf_sample_release() // lock released
    //
    // It is unsafe and incorrect to hold an IRQ spinlock across what could
    // be a long execution window, so we instead simply disallow concurrent
    // access to the ring buffer by kernel consumers, and return -EBUSY from
    // __bpf_user_ringbuf_peek() if the busy bit is held by another task.
    busy: AtomicBool,
    // Consumer and producer counters are put into separate pages to
    // allow each position to be mapped with different permissions.
    // This prevents a user-space application from modifying the
    // position and ruining in-kernel tracking. The permissions of the
    // pages depend on who is producing samples: user-space or the
    // kernel.
    //
    // Kernel-producer
    // ---------------
    // The producer position and data pages are mapped as r/o in
    // userspace. For this approach, bits in the header of samples are
    // used to signal to user-space, and to other producers, whether a
    // sample is currently being written.
    //
    // User-space producer
    // -------------------
    // Only the page containing the consumer position is mapped r/o in
    // user-space. User-space producers also use bits of the header to
    // communicate to the kernel, but the kernel must carefully check and
    // validate each sample to ensure that they're correctly formatted, and
    // fully contained within the ring buffer.
    consumer_pos: AlignedPos,
    producer_pos: AlignedPos,
    data_pos: AlignedPos,
    _marker: core::marker::PhantomData<F>,
}

unsafe impl<F: KernelAuxiliaryOps> Send for RingBuf<F> {}
unsafe impl<F: KernelAuxiliaryOps> Sync for RingBuf<F> {}

impl<F: KernelAuxiliaryOps> Debug for RingBuf<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RingBuf")
            .field("nr_pages", &self.nr_pages)
            .field("mask", &self.mask)
            .field("busy", &self.busy)
            .finish()
    }
}

const fn is_page_aligned(size: u32) -> bool {
    size & (4096 - 1) == 0
}
/// 8-byte ring buffer record header structure
pub struct BpfRingBufHdr {
    len: u32,
    pg_off: u32,
}

impl<F: KernelAuxiliaryOps> RingBuf<F> {
    /// Create a new RingBuf.
    pub fn new(map_meta: &BpfMapMeta, poll_waker: Arc<dyn PollWaker>) -> Result<&'static mut Self> {
        if !(map_meta.map_flags & !RINGBUF_CREATE_FLAG_MASK).is_empty() {
            return Err(BpfError::EINVAL);
        }
        if map_meta.key_size != 0
            || map_meta.value_size != 0
            || !map_meta.max_entries.is_power_of_two()
            || !is_page_aligned(map_meta.max_entries)
        {
            return Err(BpfError::EINVAL);
        }

        let nr_meta_pages = RINGBUF_NR_META_PAGES;
        let nr_data_pages = (map_meta.max_entries >> PAGE_SHIFT) as usize;

        let nr_pages = nr_meta_pages + nr_data_pages;

        // Each data page is mapped twice to allow "virtual"
        // continuous read of samples wrapping around the end of ring
        // buffer area:
        // ------------------------------------------------------
        // | meta pages |  real data pages  |  same data pages  |
        // ------------------------------------------------------
        // |            | 1 2 3 4 5 6 7 8 9 | 1 2 3 4 5 6 7 8 9 |
        // ------------------------------------------------------
        // |            | TA             DA | TA             DA |
        // ------------------------------------------------------
        //                               ^^^^^^^
        //                                  |
        // Here, no need to worry about special handling of wrapped-around
        // data due to double-mapped data pages. This works both in kernel and
        // when mmap()'ed in user-space, simplifying both kernel and
        // user-space implementations significantly.

        let mut pages = Vec::with_capacity(nr_pages);
        let mut phys_addrs = vec![0usize; nr_meta_pages + 2 * nr_data_pages];

        log::trace!(
            "Creating ringbuf with {} pages ({} meta pages, {} data pages)",
            nr_pages,
            nr_meta_pages,
            nr_data_pages
        );
        // [meta1] [meta2] [data1 | data2 | ... ] [data1 | data2 | ... ]
        for i in 0..nr_pages {
            let page = InnerPage::<F>::new()?;
            phys_addrs[i] = page.phys_addr();
            if i >= nr_meta_pages {
                phys_addrs[nr_data_pages + i] = page.phys_addr();
            }
            pages.push(page);
        }

        let vaddr = F::vmap(&phys_addrs)?;

        let ringbuf = unsafe { &mut *(vaddr as *mut RingBuf<F>) };

        ringbuf.mask = (map_meta.max_entries - 1) as u64;
        ringbuf.nr_pages = nr_pages as u32;
        ringbuf.phys_addrs = phys_addrs.leak();

        let waker_ptr: *const dyn PollWaker = Arc::into_raw(poll_waker);
        ringbuf.poll_waker = waker_ptr;
        ringbuf.consumer_pos = AlignedPos(0);
        ringbuf.producer_pos = AlignedPos(0);
        ringbuf.busy = AtomicBool::new(false);

        ringbuf.pages = pages.leak();
        ringbuf._marker = core::marker::PhantomData;

        Ok(ringbuf)
    }

    fn waker(&self) -> &dyn PollWaker {
        unsafe { &*self.poll_waker }
    }

    fn map_mem_usage(&self) -> Result<usize> {
        let mut total = 0;
        total += self.nr_pages as usize * 4096;
        total += core::mem::size_of_val(self.pages);
        Ok(total)
    }

    pub(crate) fn total_data_size(&self) -> u64 {
        self.mask + 1
    }

    fn data_buf(data_buf_ptr: usize, size: usize) -> &'static mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(data_buf_ptr as *mut u8, size) }
    }

    pub(crate) fn consumer_pos(&self) -> u64 {
        unsafe { AtomicU64::from_ptr(&self.consumer_pos as *const AlignedPos as *mut u64) }
            .load(Ordering::Acquire)
    }

    pub(crate) fn producer_pos(&self) -> u64 {
        unsafe { AtomicU64::from_ptr(&self.producer_pos as *const AlignedPos as *mut u64) }
            .load(Ordering::Acquire)
    }

    fn set_producer_pos(&self, pos: u64) {
        unsafe {
            AtomicU64::from_ptr(&self.producer_pos as *const AlignedPos as *mut u64)
                .store(pos, Ordering::Release);
        }
    }

    fn set_consumer_pos(&self, pos: u64) {
        unsafe {
            AtomicU64::from_ptr(&self.consumer_pos as *const AlignedPos as *mut u64)
                .store(pos, Ordering::Release);
        }
    }

    // Given pointer to ring buffer record metadata and struct bpf_ringbuf itself,
    // calculate offset from record metadata to ring buffer in pages, rounded
    // down. This page offset is stored as part of record metadata and allows to
    // restore struct bpf_ringbuf * from record pointer. This page offset is
    // stored at offset 4 of record metadata header.
    fn bpf_ringbuf_rec_pg_off(&self, hdr: &BpfRingBufHdr) -> u32 {
        let hdr_ptr = hdr as *const BpfRingBufHdr as usize;
        let self_ptr = self as *const RingBuf<F> as usize;
        ((hdr_ptr - self_ptr) >> PAGE_SHIFT) as u32
    }

    fn bpf_ringbuf_restore_from_rec(hdr: &BpfRingBufHdr) -> &'static mut RingBuf<F> {
        const PAGE_MASK: usize = 4096 - 1;
        let hdr_ptr = hdr as *const BpfRingBufHdr as usize & !PAGE_MASK;
        let ringbuf_ptr = hdr_ptr - ((hdr.pg_off as usize) << PAGE_SHIFT);
        unsafe { &mut *(ringbuf_ptr as *mut RingBuf<F>) }
    }

    /// Reserve space in the ring buffer for a new record.
    pub(crate) fn reserve(&mut self, size: u64) -> Result<&mut [u8]> {
        if size > RINGBUF_MAX_RECORD_SZ as u64 {
            return Err(BpfError::EINVAL);
        }

        let total_size = size + BPF_RINGBUF_HDR_SZ as u64;
        let mut aligned_size = total_size;
        if (aligned_size & 7) != 0 {
            aligned_size = (aligned_size + 8) & !7;
        }

        if aligned_size > self.total_data_size() {
            return Err(BpfError::EINVAL);
        }

        let cons_pos = self.consumer_pos();
        let prod_pos = self.producer_pos();

        let new_prod_pos = prod_pos + aligned_size;

        // check for out of ringbuf space by ensuring producer position
        // doesn't advance more than (ringbuf_size - 1) ahead
        if new_prod_pos - cons_pos > self.mask {
            return Err(BpfError::ENOMEM);
        }

        // the prod_idx will automatically wrap around due to masking
        let prod_idx = prod_pos as usize & (self.mask as usize);

        let data_buf = Self::data_buf(
            &self.data_pos as *const AlignedPos as usize,
            self.total_data_size() as usize * 2,
        );

        let hdr_buf = &mut data_buf[prod_idx..prod_idx + BPF_RINGBUF_HDR_SZ as usize];

        let hdr = unsafe { &mut *(hdr_buf.as_ptr() as *mut BpfRingBufHdr) };

        hdr.len = size as u32 | BPF_RINGBUF_BUSY_BIT;
        hdr.pg_off = self.bpf_ringbuf_rec_pg_off(hdr);

        // update producer position
        self.set_producer_pos(new_prod_pos);

        let data_buf =
            &mut data_buf[prod_idx + BPF_RINGBUF_HDR_SZ as usize..prod_idx + aligned_size as usize];
        Ok(data_buf)
    }

    pub(crate) fn commit(sample: &[u8], flags: BpfRingbufFlags, discard: bool) -> Result<()> {
        let sample_ptr = sample.as_ptr() as usize;
        let hdr_ptr = sample_ptr - BPF_RINGBUF_HDR_SZ as usize;

        let hdr = unsafe { &mut *(hdr_ptr as *mut BpfRingBufHdr) };

        let ringbuf = Self::bpf_ringbuf_restore_from_rec(hdr);

        // remove busy bit
        let mut new_len = hdr.len & !BPF_RINGBUF_BUSY_BIT;

        if discard {
            new_len |= BPF_RINGBUF_DISCARD_BIT;
        }

        // update record header with correct final size prefix
        unsafe {
            AtomicU32::from_ptr(&mut hdr.len as *mut u32).store(new_len, Ordering::Release);
        }

        // if consumer caught up and is waiting for our record, notify about
        // new data availability
        let rec_pos = (hdr_ptr - (&ringbuf.data_pos as *const AlignedPos as usize)) as u64;
        let cons_pos = ringbuf.consumer_pos() & ringbuf.mask;

        if flags.contains(BpfRingbufFlags::FORCE_WAKEUP) {
            ringbuf.waker().wake_up();
            return Ok(());
        }

        if (cons_pos == rec_pos) && !flags.contains(BpfRingbufFlags::NO_WAKEUP) {
            ringbuf.waker().wake_up();
            return Ok(());
        }

        Ok(())
    }

    /// Return the available data size in the ring buffer.
    pub(crate) fn avail_data_size(&self) -> u64 {
        let prod_pos = self.producer_pos();
        let cons_pos = self.consumer_pos();
        prod_pos - cons_pos
    }
}

pub struct RingBufMap<F: KernelAuxiliaryOps> {
    ringbuf: &'static mut RingBuf<F>,
}

impl<F: KernelAuxiliaryOps> Debug for RingBufMap<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RingBufMap")
            .field("ringbuf", &self.ringbuf)
            .finish()
    }
}

impl<F: KernelAuxiliaryOps> Deref for RingBufMap<F> {
    type Target = RingBuf<F>;
    fn deref(&self) -> &Self::Target {
        self.ringbuf
    }
}

impl<F: KernelAuxiliaryOps> DerefMut for RingBufMap<F> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ringbuf
    }
}

impl<F: KernelAuxiliaryOps> BpfMapCommonOps for RingBufMap<F> {
    fn map_mem_usage(&self) -> Result<usize> {
        self.ringbuf.map_mem_usage()
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn map_mmap(
        &self,
        offset: usize,
        size: usize,
        _read: bool,
        _write: bool,
    ) -> Result<Vec<usize>> {
        let offset = offset + (RINGBUF_PGOFF << PAGE_SHIFT);
        let page_idx = offset >> PAGE_SHIFT;
        let range = size >> PAGE_SHIFT;
        let phys_addrs = self.ringbuf.phys_addrs[page_idx..page_idx + range].to_vec();
        Ok(phys_addrs)
    }

    fn readable(&self) -> bool {
        let prod_pos = self.producer_pos();
        let cons_pos = self.consumer_pos();
        prod_pos != cons_pos
    }
}

impl<F: KernelAuxiliaryOps> RingBufMap<F> {
    /// Create a new RingBufMap.
    pub fn new(map_meta: &BpfMapMeta, poll_waker: Arc<dyn PollWaker>) -> Result<Self> {
        let ringbuf = RingBuf::<F>::new(map_meta, poll_waker)?;
        Ok(RingBufMap { ringbuf })
    }
}

impl<F: KernelAuxiliaryOps> Drop for RingBufMap<F> {
    fn drop(&mut self) {
        let pages = unsafe {
            Vec::from_raw_parts(
                self.ringbuf.pages.as_ptr() as *mut InnerPage<F>,
                self.ringbuf.nr_pages as usize,
                self.ringbuf.nr_pages as usize,
            )
        };

        let waker = unsafe { Arc::from_raw(self.ringbuf.poll_waker) };

        let nr_meta_pages = RINGBUF_NR_META_PAGES;
        let nr_data_pages = self.ringbuf.total_data_size() as usize >> PAGE_SHIFT;

        let phys_pages = nr_meta_pages + 2 * nr_data_pages;

        let phys_addrs = unsafe {
            Vec::from_raw_parts(
                self.ringbuf.phys_addrs.as_ptr() as *mut usize,
                phys_pages,
                phys_pages,
            )
        };
        // Unmap the pages.
        F::unmap(self.ringbuf as *mut RingBuf<F> as usize);

        drop(phys_addrs);
        drop(pages);
        drop(waker);
    }
}

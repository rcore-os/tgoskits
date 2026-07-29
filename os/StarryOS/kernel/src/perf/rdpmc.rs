//! Dynamic `perf_event_mmap_page` ownership for direct PMU reads.
//!
//! A mapped page is owned by its VMA. The event retains only a weak reference,
//! so `munmap` permits a later mapping and fd close cannot free a page still
//! visible to userspace. Scheduler hooks publish active/inactive counter state
//! with Linux's sequence protocol; no callback or userspace pointer enters the
//! IRQ-off scheduling path.

use alloc::sync::{Arc, Weak};
use core::{
    any::Any,
    sync::atomic::{AtomicU32, Ordering, fence},
};

use ax_alloc::GlobalPage;
use ax_errno::{AxError, AxResult};
use ax_hal::mem::virt_to_phys;
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use super::hw_owner::Counter;

/// Values published atomically to one perf mmap page.
#[derive(Clone, Copy, Debug)]
pub(super) struct RdpmcSnapshot {
    pub(super) offset: u64,
    pub(super) time_enabled: u64,
    pub(super) time_running: u64,
}

/// One VMA-owned perf metadata page.
pub(super) struct PerfRdpmcPage {
    _pages: GlobalPage,
    kernel_address: usize,
    physical_address: PhysAddr,
    active_index: u32,
    /// Serializes the bounded ABI publication transaction across scheduler and
    /// sleepable control paths. The lock never covers PMU access, allocation,
    /// wakeup, or an owner-CPU rendezvous.
    publish_gate: SpinNoIrq<()>,
}

impl core::fmt::Debug for PerfRdpmcPage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerfRdpmcPage")
            .field("physical_address", &self.physical_address)
            .field("active_index", &self.active_index)
            .finish_non_exhaustive()
    }
}

impl PerfRdpmcPage {
    fn allocate(counter: Counter, initial: RdpmcSnapshot) -> AxResult<Arc<Self>> {
        let mut pages = GlobalPage::alloc_contiguous(1, PAGE_SIZE_4K)?;
        pages.zero();
        let kernel_address = pages.start_vaddr().as_usize();
        let physical_address = virt_to_phys(pages.start_vaddr());
        let (active_index, pmc_width) = counter.mmap_metadata();
        let page = Arc::new(Self {
            _pages: pages,
            kernel_address,
            physical_address,
            active_index,
            publish_gate: SpinNoIrq::new(()),
        });

        let header = page.header();
        // SAFETY: the page is freshly allocated, zeroed, and not yet published.
        // Every field is naturally aligned within `perf_event_mmap_page`.
        unsafe {
            core::ptr::addr_of_mut!((*header).version).write_volatile(1);
            core::ptr::addr_of_mut!((*header).compat_version).write_volatile(0);
            core::ptr::addr_of_mut!((*header).pmc_width).write_volatile(pmc_width);
            // `cap_user_rdpmc` is bit 2 after the two legacy bit-0 fields.
            core::ptr::addr_of_mut!((*header).__bindgen_anon_1.capabilities)
                .write_volatile(1u64 << 2);
        }
        page.publish(false, initial);
        Ok(page)
    }

    fn header(&self) -> *mut perf_event_mmap_page {
        self.kernel_address as *mut perf_event_mmap_page
    }

    fn sequence(&self) -> &AtomicU32 {
        // SAFETY: `lock` is a naturally aligned `u32` in the live, page-aligned
        // allocation. Kernel writers access it only through this atomic view.
        unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*self.header()).lock)) }
    }

    fn publish(&self, active: bool, snapshot: RdpmcSnapshot) {
        let _publish = self.publish_gate.lock();
        let sequence = self.sequence();
        let odd = sequence.load(Ordering::Relaxed).wrapping_add(1) | 1;

        // The bounded gate makes scheduler and control publications single
        // writer. SeqCst at the opening edge prevents metadata stores from
        // becoming visible before the odd marker.
        sequence.store(odd, Ordering::SeqCst);
        let header = self.header();
        // SAFETY: the VMA and this kernel object share the live page. Volatile
        // stores make every ABI field publication observable to userspace.
        unsafe {
            core::ptr::addr_of_mut!((*header).index).write_volatile(if active {
                self.active_index
            } else {
                0
            });
            core::ptr::addr_of_mut!((*header).offset).write_volatile(snapshot.offset as i64);
            core::ptr::addr_of_mut!((*header).time_enabled).write_volatile(snapshot.time_enabled);
            core::ptr::addr_of_mut!((*header).time_running).write_volatile(snapshot.time_running);
        }
        fence(Ordering::Release);
        sequence.store(odd.wrapping_add(1), Ordering::Release);
    }

    fn physical_address(&self) -> PhysAddr {
        self.physical_address
    }
}

/// Weak event-side publication for at most one live VMA.
#[derive(Debug)]
pub(super) struct RdpmcMapping {
    page: SpinNoIrq<Option<Weak<PerfRdpmcPage>>>,
}

impl RdpmcMapping {
    pub(super) const fn new() -> Self {
        Self {
            page: SpinNoIrq::new(None),
        }
    }

    /// Allocates and publishes a new inactive page.
    pub(super) fn install(
        &self,
        len: usize,
        counter: Counter,
        initial: RdpmcSnapshot,
    ) -> AxResult<Arc<PerfRdpmcPage>> {
        // Mapping more than the allocated metadata page would expose unrelated
        // adjacent physical memory through `DeviceMmap::Physical`.
        if len != PAGE_SIZE_4K {
            return Err(AxError::InvalidInput);
        }
        let page = PerfRdpmcPage::allocate(counter, initial)?;
        let mut published = self.page.lock();
        if published.as_ref().and_then(Weak::upgrade).is_some() {
            return Err(AxError::ResourceBusy);
        }
        *published = Some(Arc::downgrade(&page));
        Ok(page)
    }

    /// Withdraws a page whose mmap transaction failed before VMA publication.
    pub(super) fn withdraw(&self, page: &Arc<PerfRdpmcPage>) {
        let mut published = self.page.lock();
        if published
            .as_ref()
            .is_some_and(|weak| Weak::ptr_eq(weak, &Arc::downgrade(page)))
        {
            published.take();
        }
    }

    pub(super) fn publish_active(&self, snapshot: RdpmcSnapshot) {
        if let Some(page) = self.page.lock().as_ref().and_then(Weak::upgrade) {
            page.publish(true, snapshot);
        }
    }

    pub(super) fn publish_inactive(&self, snapshot: RdpmcSnapshot) {
        if let Some(page) = self.page.lock().as_ref().and_then(Weak::upgrade) {
            page.publish(false, snapshot);
        }
    }
}

pub(super) fn mapping_result(page: Arc<PerfRdpmcPage>) -> (PhysAddr, Arc<dyn Any + Send + Sync>) {
    let physical_address = page.physical_address();
    let anchor: Arc<dyn Any + Send + Sync> = page;
    (physical_address, anchor)
}

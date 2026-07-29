//! Ring, deferred notification, and output state for ARM PMUv3 sampling.

use alloc::sync::Arc;
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_alloc::GlobalPage;
use ax_errno::{AxError, AxResult};
use ax_hal::mem::virt_to_phys;
use ax_memory_addr::PhysAddr;
use axpoll::{IoEvents, PollSet};
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use super::{
    inheritance::PerfInheritanceFamily,
    output::{PerfOutputRoute, PerfRingOutput},
    sampling,
};
use crate::task::future::IrqNotify;

const RING_DATA_OFFSET: usize = ax_memory_addr::PAGE_SIZE_4K;

/// Sampling state attached to a system event.
pub(super) struct SamplingState {
    pub(super) period: u32,
    pub(super) freq: bool,
    pub(super) target_freq: u32,
    pub(super) sample_type: u64,
    pub(super) poll_ready: Arc<PollSet>,
    pub(super) notify: Arc<IrqNotify>,
    pub(super) poll_alive: Arc<AtomicBool>,
    pub(super) output: PerfOutputRoute,
}

impl core::fmt::Debug for SamplingState {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SamplingState")
            .field("period", &self.period)
            .field("sample_type", &self.sample_type)
            .field("has_own_ring", &self.output.owned().is_some())
            .field(
                "redirected",
                &self
                    .output
                    .effective()
                    .is_some_and(|(_, redirected)| redirected),
            )
            .finish()
    }
}

/// Starts the task-context bridge from PMU IRQ notification to poll readiness.
pub(super) fn start_sampling_notify_worker(
    poll_ready: Arc<PollSet>,
    notify: Arc<IrqNotify>,
    poll_alive: Arc<AtomicBool>,
) {
    crate::task::spawn_kernel_thread(
        move || loop {
            notify.wait();
            if !poll_alive.load(Ordering::Acquire) {
                break;
            }
            // The overflow handler publishes the ring record before notifying.
            unsafe { poll_ready.wake(IoEvents::IN) };
        },
        "hw-perf-sample-notify".into(),
    );
}

/// Allocates and initializes one Linux perf mmap ring.
pub(super) fn alloc_sampling_ring(len: usize) -> AxResult<(Arc<GlobalPage>, usize, PhysAddr)> {
    if len == 0 || !len.is_multiple_of(ax_memory_addr::PAGE_SIZE_4K) {
        return Err(AxError::InvalidInput);
    }
    let num_pages = len / ax_memory_addr::PAGE_SIZE_4K;
    if num_pages < 2 || !(num_pages - 1).is_power_of_two() {
        return Err(AxError::InvalidInput);
    }

    let mut pages = GlobalPage::alloc_contiguous(num_pages, ax_memory_addr::PAGE_SIZE_4K)?;
    pages.zero();
    let kernel_address = pages.start_vaddr();
    let physical_address = virt_to_phys(kernel_address);
    let header = kernel_address.as_usize() as *mut perf_event_mmap_page;
    let data_size = (len - RING_DATA_OFFSET) as u64;
    unsafe {
        // SAFETY: `header` points to the freshly allocated and zeroed first
        // page. The mapping is not published until these fields are complete.
        core::ptr::addr_of_mut!((*header).version).write(1);
        core::ptr::addr_of_mut!((*header).compat_version).write(0);
        core::ptr::addr_of_mut!((*header).data_offset).write(RING_DATA_OFFSET as u64);
        core::ptr::addr_of_mut!((*header).data_size).write(data_size);
        core::ptr::addr_of_mut!((*header).data_head).write(0);
        core::ptr::addr_of_mut!((*header).data_tail).write(0);
    }

    Ok((Arc::new(pages), kernel_address.as_usize(), physical_address))
}

pub(super) fn ring_has_data(ring: &PerfRingOutput) -> bool {
    let header = ring.ring_vaddr() as *const perf_event_mmap_page;
    let (head, tail) = unsafe {
        // SAFETY: the owned output snapshot pins the initialized ring pages.
        (
            core::ptr::addr_of!((*header).data_head).read_volatile(),
            core::ptr::addr_of!((*header).data_tail).read_volatile(),
        )
    };
    head != tail
}

/// Maps and publishes the shared output for a task-event family.
pub(super) fn device_mmap_per_task(
    family: &Arc<PerfInheritanceFamily>,
    len: usize,
) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
    let root = family.root();
    if !root.is_sampling() {
        return Err(AxError::Unsupported);
    }
    if root.ring_mapped() {
        return Err(AxError::ResourceBusy);
    }

    let (pages, ring_vaddr, physical_address) = alloc_sampling_ring(len)?;
    let poll_ready = Arc::new(PollSet::new());
    let notify = Arc::new(IrqNotify::new());
    let poll_alive = Arc::new(AtomicBool::new(true));
    start_sampling_notify_worker(
        Arc::clone(&poll_ready),
        Arc::clone(&notify),
        Arc::clone(&poll_alive),
    );

    let page_anchor: Arc<dyn Any + Send + Sync> = pages;
    let output = PerfRingOutput::new(ring_vaddr, len, page_anchor);
    family.publish_root_output(
        &output,
        super::task::SamplingAnchors::new(notify, poll_ready, poll_alive),
    )?;
    Ok((physical_address, output.mapping_anchor()))
}

/// Resolves a fixed sample period or frequency target.
pub(super) fn resolve_sampling(raw: u64, is_freq: bool) -> (u32, u32) {
    if is_freq {
        let frequency = raw.clamp(1, sampling::MAX_TARGET_FREQ as u64) as u32;
        (sampling::initial_period_for_freq(frequency), frequency)
    } else {
        (raw.min(u32::MAX as u64) as u32, 0)
    }
}

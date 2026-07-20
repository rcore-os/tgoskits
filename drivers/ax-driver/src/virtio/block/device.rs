//! Shared transport owner and task/IRQ exclusion boundary.

use alloc::boxed::Box;
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::PreemptIrqGuard;
use virtio_drivers::{queue::VirtQueue, transport::DeviceStatus};

use super::{
    initialization::{VIRTIO_BLK_F_RO, VirtioBlockInitPhase},
    lifecycle::VirtioLifecycleHardware,
    queue::{InflightRequest, InflightStorage, VIRTIO_BLK_QUEUE_SIZE, VirtioDmaQuarantine},
};
use crate::virtio::{VirtIoHalImpl, VirtIoTransport};

pub(super) struct VirtIoBlkDevice<T: VirtIoTransport> {
    inner: UnsafeCell<VirtIoBlkInner<T>>,
    access_active: AtomicBool,
    irq_enabled: AtomicBool,
}

pub(super) struct VirtIoBlkInner<T: VirtIoTransport> {
    pub(super) transport: T,
    pub(super) queue: Option<VirtQueue<VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>>,
    pub(super) init_phase: VirtioBlockInitPhase,
    pub(super) negotiated_features: u64,
    pub(super) config_generation: u32,
    pub(super) capacity_low: u32,
    pub(super) capacity: u64,
    pub(super) retained_capacity: Option<u64>,
    pub(super) retained_read_only: Option<bool>,
    pub(super) init_deadline_ns: u64,
    pub(super) init_error: Option<rdif_block::InitError>,
    // Allocated before the controller is published. A Box keeps the request
    // and response descriptor addresses stable while the inner owner moves;
    // Option permits bounded quarantine after an unacknowledged live DMA.
    pub(super) descriptor_storage: Option<Box<InflightStorage>>,
    pub(super) inflight: Option<InflightRequest>,
    // An unacknowledged reset must retain explicit ownership of every DMA-
    // visible allocation. The failed controller/queue handle keeps this
    // diagnostic owner alive and prevents reinitialization.
    pub(super) dma_quarantine: Option<VirtioDmaQuarantine>,
}

// SAFETY: `T: VirtIoTransport` proves the transport is movable. The transport
// and in-flight request are accessed only by the maintenance owner while
// holding `access_active`; moving the Arc does not bypass that exclusion.
unsafe impl<T: VirtIoTransport> Send for VirtIoBlkDevice<T> {}

// SAFETY: `T` needs only Send because no shared reference reaches it. Task
// access disables local IRQ/preemption before acquiring `access_active`.
// Hard IRQ owns a separate interrupt-status port and never reaches `inner`.
// Thus every dereference of `inner` is exclusive across task contexts.
unsafe impl<T: VirtIoTransport> Sync for VirtIoBlkDevice<T> {}

struct VirtioBlkAccessGuard<'state>(&'state AtomicBool);

impl<'state> VirtioBlkAccessGuard<'state> {
    fn enter(active: &'state AtomicBool) -> Self {
        while active
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Self(active)
    }
}

impl Drop for VirtioBlkAccessGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<T: VirtIoTransport> VirtIoBlkDevice<T> {
    pub(super) fn discovered(transport: T) -> Self {
        Self {
            inner: UnsafeCell::new(VirtIoBlkInner {
                transport,
                queue: None,
                init_phase: VirtioBlockInitPhase::Discovered,
                negotiated_features: 0,
                config_generation: 0,
                capacity_low: 0,
                capacity: 0,
                retained_capacity: None,
                retained_read_only: None,
                init_deadline_ns: 0,
                init_error: None,
                descriptor_storage: Some(Box::default()),
                inflight: None,
                dma_quarantine: None,
            }),
            access_active: AtomicBool::new(false),
            irq_enabled: AtomicBool::new(false),
        }
    }

    pub(super) fn with_task<R>(&self, f: impl FnOnce(&mut VirtIoBlkInner<T>) -> R) -> R {
        let _irq_guard = PreemptIrqGuard::new();
        let _active = VirtioBlkAccessGuard::enter(&self.access_active);
        // SAFETY: `access_active` serializes all mutable access to the raw
        // transport, and task-side callers keep local IRQ/preemption disabled.
        let inner = unsafe { &mut *self.inner.get() };
        f(inner)
    }

    pub(super) fn enable_irq(&self) {
        self.irq_enabled.store(true, Ordering::Release);
        self.with_task(|inner| inner.set_interrupts(true));
    }

    pub(super) fn disable_irq(&self) {
        mask_and_publish_irq_disabled(&self.irq_enabled, || {
            self.with_task(|inner| inner.set_interrupts(false));
        });
    }

    pub(super) fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    pub(super) fn poll_init(&self, input: rdif_block::InitInput) -> rdif_block::InitPoll<()> {
        let irq_enabled = self.is_irq_enabled();
        self.with_task(|inner| inner.poll_init(input, irq_enabled))
    }

    pub(super) fn is_ready(&self) -> bool {
        self.with_task(|inner| inner.init_phase == VirtioBlockInitPhase::Ready)
    }

    pub(super) fn capacity_if_ready(&self) -> Option<u64> {
        self.with_task(|inner| {
            (inner.init_phase == VirtioBlockInitPhase::Ready).then_some(inner.capacity)
        })
    }

    pub(super) fn read_only_if_ready(&self) -> Option<bool> {
        self.with_task(|inner| {
            (inner.init_phase == VirtioBlockInitPhase::Ready)
                .then_some(inner.negotiated_features & VIRTIO_BLK_F_RO != 0)
        })
    }
}

pub(super) fn mask_and_publish_irq_disabled(
    irq_enabled: &AtomicBool,
    mask_device_source: impl FnOnce(),
) {
    // Keep the acknowledgement endpoint live until future device-generated
    // notifications are suppressed. Otherwise an IRQ in this interval can be
    // consumed by the interrupt controller but left unacknowledged in the
    // VirtIO transport, with no later edge to reactivate service.
    mask_device_source();
    irq_enabled.store(false, Ordering::Release);
}

impl<T: VirtIoTransport> VirtioLifecycleHardware for VirtIoBlkDevice<T> {
    fn controller_cookie(&self) -> usize {
        core::ptr::from_ref(self).expose_provenance()
    }

    fn begin_device_reset(&self) {
        self.irq_enabled.store(false, Ordering::Release);
        self.with_task(|inner| {
            inner.set_interrupts(false);
            inner.transport.set_status(DeviceStatus::empty());
        });
    }

    fn finish_reset_after_acknowledgement(&self) -> bool {
        self.with_task(VirtIoBlkInner::finish_reset_after_acknowledgement)
    }

    fn prepare_reinitialize(&self) -> Result<(), rdif_block::InitError> {
        self.with_task(VirtIoBlkInner::prepare_reinitialize)
    }

    fn poll_reinitialize(&self, input: rdif_block::InitInput) -> rdif_block::InitPoll<()> {
        self.poll_init(input)
    }
}

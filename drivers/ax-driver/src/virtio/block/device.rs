//! Shared transport owner and task/IRQ exclusion boundary.

use alloc::boxed::Box;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::PreemptIrqGuard;
use virtio_drivers::{queue::VirtQueue, transport::DeviceStatus};

use super::{
    initialization::{VIRTIO_BLK_F_RO, VirtioBlockInitPhase},
    irq::{
        VirtioBlkAccessGuard, continue_deferred_virtio_queue_irq, initialization_irq_outcome,
        service_deferred_initialization_irq, virtio_blk_irq_outcome,
    },
    lifecycle::VirtioLifecycleHardware,
    queue::{InflightRequest, InflightStorage, VIRTIO_BLK_QUEUE_SIZE},
};
use crate::virtio::{VirtIoHalImpl, VirtIoTransport};

pub(super) struct VirtIoBlkDevice<T: VirtIoTransport> {
    inner: UnsafeCell<VirtIoBlkInner<T>>,
    access_active: AtomicBool,
    irq_ack_pending: AtomicBool,
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
}

// SAFETY: `T: VirtIoTransport` proves the transport is movable. The transport
// and in-flight request are accessed only while holding `access_active`, and
// moving the Arc between CPUs does not bypass that exclusion.
unsafe impl<T: VirtIoTransport> Send for VirtIoBlkDevice<T> {}

// SAFETY: `T` needs only Send because no shared reference reaches it. Task
// access disables local IRQ/preemption before acquiring `access_active`; hard
// IRQ access is try-only and never waits on task state. Thus every dereference
// of `inner` is exclusive across CPUs and IRQ context.
unsafe impl<T: VirtIoTransport> Sync for VirtIoBlkDevice<T> {}

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
            }),
            access_active: AtomicBool::new(false),
            irq_ack_pending: AtomicBool::new(false),
            irq_enabled: AtomicBool::new(false),
        }
    }

    pub(super) fn with_task<R>(&self, f: impl FnOnce(&mut VirtIoBlkInner<T>) -> R) -> R {
        let _irq_guard = PreemptIrqGuard::new();
        let _active = VirtioBlkAccessGuard::enter_task(&self.access_active);
        // SAFETY: `access_active` serializes all mutable access to the raw
        // transport, and task-side callers keep local IRQ/preemption disabled.
        let inner = unsafe { &mut *self.inner.get() };
        f(inner)
    }

    pub(super) fn try_with_task<R>(
        &self,
        f: impl FnOnce(&mut VirtIoBlkInner<T>) -> R,
    ) -> Option<R> {
        let _irq_guard = PreemptIrqGuard::new();
        let _active = VirtioBlkAccessGuard::try_enter_task(&self.access_active)?;
        // SAFETY: the successful try-only guard gives this callback exclusive
        // transport ownership without waiting in a shared worker callback.
        let inner = unsafe { &mut *self.inner.get() };
        Some(f(inner))
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

    pub(super) fn handle_irq(&self) -> rdif_block::IrqOutcome {
        virtio_blk_irq_outcome(
            &self.access_active,
            &self.irq_ack_pending,
            self.is_irq_enabled(),
            || {
                // SAFETY: the IRQ helper calls this closure only while holding
                // the IRQ-side access guard.
                let inner = unsafe { &mut *self.inner.get() };
                inner.transport.ack_interrupt()
            },
        )
    }

    pub(super) fn handle_initialization_irq(&self) -> rdif_block::IrqOutcome {
        initialization_irq_outcome(
            &self.access_active,
            &self.irq_ack_pending,
            self.is_irq_enabled(),
            || {
                // SAFETY: the init IRQ helper invokes this callback only after
                // its try-only guard acquired exclusive transport access.
                let inner = unsafe { &mut *self.inner.get() };
                inner.transport.ack_interrupt()
            },
        )
    }

    pub(super) fn continue_deferred_irq(&self) -> rdif_block::DeferredIrqProgress {
        continue_deferred_virtio_queue_irq(
            &self.access_active,
            &self.irq_ack_pending,
            self.is_irq_enabled(),
            || {
                // SAFETY: the continuation helper invokes this callback only
                // while its try-only guard owns the transport.
                let inner = unsafe { &mut *self.inner.get() };
                inner.transport.ack_interrupt()
            },
        )
    }

    pub(super) fn service_deferred_initialization_irq(&self) -> rdif_block::InitIrqProgress {
        service_deferred_initialization_irq(
            &self.access_active,
            &self.irq_ack_pending,
            self.is_irq_enabled(),
            || {
                // SAFETY: the task-side helper invokes this callback only
                // while its try-only guard owns the transport.
                let inner = unsafe { &mut *self.inner.get() };
                inner.transport.ack_interrupt()
            },
        )
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
        self.irq_ack_pending.store(false, Ordering::Release);
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

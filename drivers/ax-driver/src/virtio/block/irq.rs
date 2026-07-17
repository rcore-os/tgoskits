//! Hard-IRQ acknowledgement and deferred transport ownership handoff.

use alloc::sync::Arc;
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::PreemptIrqGuard;
use virtio_drivers::transport::InterruptStatus;

use super::{VIRTIO_BLK_QUEUE_ID, device::VirtIoBlkDevice};
use crate::virtio::VirtIoTransport;

pub(super) struct VirtioBlkIrqHandler<T: VirtIoTransport> {
    pub(super) inner: Arc<VirtIoBlkDevice<T>>,
}

pub(super) struct VirtioBlkInitIrqHandler<T: VirtIoTransport> {
    pub(super) inner: Arc<VirtIoBlkDevice<T>>,
}

impl<T: VirtIoTransport> rdif_block::IrqHandler for VirtioBlkInitIrqHandler<T> {
    fn handle_irq(&mut self) -> rdif_block::IrqOutcome {
        self.inner.handle_initialization_irq()
    }
}

impl<T: VirtIoTransport> rdif_block::IrqHandler for VirtioBlkIrqHandler<T> {
    fn handle_irq(&mut self) -> rdif_block::IrqOutcome {
        self.inner.handle_irq()
    }

    fn continue_deferred_irq(&mut self) -> rdif_block::DeferredIrqProgress {
        self.inner.continue_deferred_irq()
    }
}

pub(super) struct VirtioBlkAccessGuard<'a>(&'a AtomicBool);

impl<'a> VirtioBlkAccessGuard<'a> {
    pub(super) fn enter_task(active: &'a AtomicBool) -> Self {
        Self::enter(active)
    }

    fn try_enter_irq(active: &'a AtomicBool) -> Option<Self> {
        Self::try_enter(active)
    }

    pub(super) fn try_enter_task(active: &'a AtomicBool) -> Option<Self> {
        Self::try_enter(active)
    }

    fn try_enter(active: &'a AtomicBool) -> Option<Self> {
        if active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        Some(Self(active))
    }

    fn enter(active: &'a AtomicBool) -> Self {
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

pub(super) fn virtio_blk_irq_outcome(
    access_active: &AtomicBool,
    irq_ack_pending: &AtomicBool,
    irq_enabled: bool,
    ack_status: impl FnOnce() -> InterruptStatus,
) -> rdif_block::IrqOutcome {
    if !irq_enabled {
        return rdif_block::IrqOutcome::unhandled();
    }
    let Some(_active) = VirtioBlkAccessGuard::try_enter_irq(access_active) else {
        let first = !irq_ack_pending.swap(true, Ordering::AcqRel);
        let queues = if first {
            rdif_block::IdList::from_bits(1 << VIRTIO_BLK_QUEUE_ID)
        } else {
            rdif_block::IdList::none()
        };
        return rdif_block::IrqOutcome::deferred(queues);
    };
    irq_ack_pending.store(false, Ordering::Release);
    let status = ack_status();
    let event = virtio_blk_event_from_irq_status(true, status);
    if !event.is_empty() {
        rdif_block::IrqOutcome::handled(event)
    } else if status.is_empty() {
        rdif_block::IrqOutcome::unhandled()
    } else {
        rdif_block::IrqOutcome::handled_control()
    }
}

pub(super) fn initialization_irq_outcome(
    access_active: &AtomicBool,
    irq_ack_pending: &AtomicBool,
    irq_enabled: bool,
    ack_status: impl FnOnce() -> InterruptStatus,
) -> rdif_block::IrqOutcome {
    if !irq_enabled {
        return rdif_block::IrqOutcome::unhandled();
    }
    let Some(_active) = VirtioBlkAccessGuard::try_enter_irq(access_active) else {
        irq_ack_pending.store(true, Ordering::Release);
        return rdif_block::IrqOutcome::deferred(rdif_block::IdList::from_bits(
            1 << VIRTIO_BLK_QUEUE_ID,
        ));
    };
    irq_ack_pending.store(false, Ordering::Release);
    let status = ack_status();
    if status.is_empty() {
        rdif_block::IrqOutcome::unhandled()
    } else {
        rdif_block::IrqOutcome::handled_control()
    }
}

pub(super) fn service_deferred_initialization_irq(
    access_active: &AtomicBool,
    irq_ack_pending: &AtomicBool,
    irq_enabled: bool,
    ack_status: impl FnOnce() -> InterruptStatus,
) -> rdif_block::InitIrqProgress {
    if !irq_enabled {
        return rdif_block::InitIrqProgress::Unhandled;
    }
    let _context = PreemptIrqGuard::new();
    let Some(_active) = VirtioBlkAccessGuard::try_enter_task(access_active) else {
        return rdif_block::InitIrqProgress::Deferred;
    };
    if !irq_ack_pending.swap(false, Ordering::AcqRel) {
        return rdif_block::InitIrqProgress::Unhandled;
    }
    if ack_status().is_empty() {
        rdif_block::InitIrqProgress::Unhandled
    } else {
        rdif_block::InitIrqProgress::Acknowledged
    }
}

pub(super) fn virtio_blk_event_from_irq_status(
    irq_enabled: bool,
    status: InterruptStatus,
) -> rdif_block::Event {
    if !irq_enabled || !status.contains(InterruptStatus::QUEUE_INTERRUPT) {
        return rdif_block::Event::none();
    }
    rdif_block::Event::from_queue_bits(1 << VIRTIO_BLK_QUEUE_ID)
}

pub(super) fn continue_deferred_virtio_queue_irq(
    access_active: &AtomicBool,
    irq_ack_pending: &AtomicBool,
    irq_enabled: bool,
    ack_status: impl FnOnce() -> InterruptStatus,
) -> rdif_block::DeferredIrqProgress {
    if !irq_enabled {
        return rdif_block::DeferredIrqProgress::Unhandled;
    }
    let _context = PreemptIrqGuard::new();
    let Some(_active) = VirtioBlkAccessGuard::try_enter_task(access_active) else {
        return rdif_block::DeferredIrqProgress::Deferred;
    };
    if !irq_ack_pending.swap(false, Ordering::AcqRel) {
        return rdif_block::DeferredIrqProgress::Unhandled;
    }
    let status = ack_status();
    let facts = virtio_blk_event_from_irq_status(true, status);
    rdif_block::DeferredIrqProgress::Acknowledged(facts)
}

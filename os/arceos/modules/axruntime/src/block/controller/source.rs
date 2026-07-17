//! Per-IRQ-source acknowledgement ownership and threaded continuation.

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use ax_hal::irq::{IrqContinuationSlot, IrqContinuationToken, IrqContinuationWake, IrqReturn};
use rdif_block::{
    AcknowledgedEvent, BIrqHandler, DeferredIrqProgress, IdList, IrqEventEpoch, IrqOutcome,
};

use super::ControllerOwnerLink;
use crate::{
    block::HardwareQueue,
    workqueue::{WorkItem, WorkOutcome, WorkPriority, WorkQueue},
};

const MAX_DEFERRED_ACK_ATTEMPTS: u8 = 64;

/// Pinned owner of one controller IRQ source and its unique handler endpoint.
pub(super) struct RuntimeIrqSource {
    source_id: usize,
    routes: Box<[Pin<&'static HardwareQueue>]>,
    handler: UnsafeCell<BIrqHandler>,
    owner_link: &'static ControllerOwnerLink,
    domain: Pin<&'static WorkQueue>,
    work: WorkItem,
    wake: IrqContinuationWake,
    token: IrqContinuationSlot,
    next_epoch: AtomicU64,
    pending_epoch: AtomicU64,
    pending_queues: AtomicU64,
    pending_controller_epoch: AtomicU64,
    deferred_attempts: AtomicU8,
    failed: AtomicBool,
}

// The registered action is non-reentrant. Its hard handler is the only source
// endpoint user until it returns Defer; the framework then masks the line
// before publishing a token to the affinity worker. That worker finishes the
// token only after it stops using the same handler, so UnsafeCell is never
// mutably aliased.
unsafe impl Sync for RuntimeIrqSource {}

impl RuntimeIrqSource {
    pub(super) fn allocate(
        source_id: usize,
        routes: Vec<Pin<&'static HardwareQueue>>,
        handler: BIrqHandler,
        owner_link: &'static ControllerOwnerLink,
    ) -> &'static Self {
        let cpu = routes.first().map_or(0, |queue| queue.affinity_cpu());
        let domain = Box::leak(Box::new(WorkQueue::new(cpu, WorkPriority::High)));
        let domain = unsafe {
            // SAFETY: source domains have shutdown lifetime and never move
            // after their intrusive work item is published.
            Pin::new_unchecked(&*domain)
        };
        let mut source = Box::new(Self {
            source_id,
            routes: routes.into_boxed_slice(),
            handler: UnsafeCell::new(handler),
            owner_link,
            domain,
            work: WorkItem::new(source_work_entry, 0),
            wake: unsafe {
                // SAFETY: replaced with the final shutdown-lifetime address
                // before the IRQ action becomes dispatchable.
                IrqContinuationWake::new(0, source_continuation_wake)
            },
            token: IrqContinuationSlot::new(),
            next_epoch: AtomicU64::new(0),
            pending_epoch: AtomicU64::new(0),
            pending_queues: AtomicU64::new(0),
            pending_controller_epoch: AtomicU64::new(0),
            deferred_attempts: AtomicU8::new(0),
            failed: AtomicBool::new(false),
        });
        let address = ptr::from_ref(source.as_ref()).expose_provenance();
        source.work = WorkItem::new(source_work_entry, address);
        source.wake = unsafe {
            // SAFETY: the Box address is stable and the object is leaked below
            // before either callback can run.
            IrqContinuationWake::new(address, source_continuation_wake)
        };
        Box::leak(source)
    }

    pub(super) fn handle_irq(&'static self) -> IrqReturn {
        if self.failed.load(Ordering::Acquire) {
            return IrqReturn::QuenchAndWake;
        }
        let outcome = unsafe {
            // SAFETY: the non-reentrant registered action is the sole endpoint
            // user until a deferred token is delivered after IRQ return.
            (&mut *self.handler.get()).handle_irq()
        };
        if !outcome.is_handled() {
            return IrqReturn::Unhandled;
        }
        if outcome.is_deferred() {
            return self.publish_deferred(outcome);
        }
        let Some(facts) = outcome.acknowledged_event() else {
            return IrqReturn::Handled;
        };
        let event = AcknowledgedEvent::new(self.source_id, self.next_source_epoch(), facts);
        match self.route_acknowledged(event, None) {
            Ok(true) => IrqReturn::Wake,
            Ok(false) => IrqReturn::Handled,
            Err(()) => IrqReturn::QuenchAndWake,
        }
    }

    fn publish_deferred(&'static self, outcome: IrqOutcome) -> IrqReturn {
        let Some(queues) = outcome.deferred_queues() else {
            return IrqReturn::QuenchAndWake;
        };
        let epoch = self.next_source_epoch();
        let controller_epoch = self.capture_controller_epoch().unwrap_or(0);
        self.pending_queues.store(queues.bits(), Ordering::Relaxed);
        self.pending_controller_epoch
            .store(controller_epoch, Ordering::Relaxed);
        if self
            .pending_epoch
            .compare_exchange(0, epoch.get(), Ordering::Release, Ordering::Acquire)
            .is_err()
        {
            self.failed.store(true, Ordering::Release);
            return IrqReturn::QuenchAndWake;
        }
        IrqReturn::Defer(&self.wake)
    }

    fn next_source_epoch(&self) -> IrqEventEpoch {
        let epoch = self
            .next_epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .expect("block IRQ source epoch exhausted")
            + 1;
        IrqEventEpoch::new(epoch).expect("incremented IRQ source epoch is non-zero")
    }

    fn capture_controller_epoch(&self) -> Option<u64> {
        self.routes
            .iter()
            .find_map(|queue| queue.irq_publication_epoch())
    }

    fn route_acknowledged(
        &'static self,
        event: AcknowledgedEvent,
        expected_controller_epoch: Option<u64>,
    ) -> Result<bool, ()> {
        let expected_controller_epoch =
            expected_controller_epoch.or_else(|| self.capture_controller_epoch());
        let mut queued = false;
        if let Some(controller_epoch) = expected_controller_epoch {
            for queue in &self.routes {
                match queue.record_irq_event(controller_epoch, event) {
                    Ok(routed) => queued |= routed,
                    Err(_) => {
                        let _ = self.owner_link.request_irq_recovery(queue.info().id);
                        return Err(());
                    }
                }
            }
        } else if !event.facts().is_empty() {
            // Facts from a closed controller epoch must never be relabelled as
            // belonging to the next activation.
            self.failed.store(true, Ordering::Release);
            return Err(());
        }
        Ok(queued || self.owner_link.record_lifecycle_irq(self.source_id))
    }

    fn work(&'static self) -> Pin<&'static WorkItem> {
        unsafe {
            // SAFETY: RuntimeIrqSource is leaked before publication and its
            // embedded intrusive work item never moves.
            Pin::new_unchecked(&self.work)
        }
    }

    fn queue_work(&'static self) -> bool {
        self.domain.queue_work_on(self.work()).is_ok()
    }

    fn service_continuation(&'static self) -> WorkOutcome {
        let Some(token) = self.token.take() else {
            return WorkOutcome::Complete;
        };
        let progress = unsafe {
            // SAFETY: the framework-minted token proves the action has returned
            // and its backing line is masked for this sole source worker.
            (&mut *self.handler.get()).continue_deferred_irq()
        };
        match progress {
            DeferredIrqProgress::Deferred => {
                let attempts = self.deferred_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                if self.token.restore(token).is_err() || attempts >= MAX_DEFERRED_ACK_ATTEMPTS {
                    self.fail_without_reopening();
                    WorkOutcome::Complete
                } else {
                    WorkOutcome::Requeue
                }
            }
            DeferredIrqProgress::Acknowledged(facts) => {
                let Some(epoch) = IrqEventEpoch::new(self.pending_epoch.load(Ordering::Acquire))
                else {
                    self.fail_without_reopening();
                    return WorkOutcome::Complete;
                };
                let allowed = IdList::from_bits(self.pending_queues.load(Ordering::Acquire));
                if facts.queues.bits() & !allowed.bits() != 0
                    || facts
                        .completions
                        .iter()
                        .any(|hint| !allowed.contains(hint.queue_id()))
                {
                    let _ = self.token.restore(token);
                    self.fail_without_reopening();
                    return WorkOutcome::Complete;
                }
                let controller_epoch = self.pending_controller_epoch.load(Ordering::Acquire);
                let event = AcknowledgedEvent::new(self.source_id, epoch, facts);
                if controller_epoch == 0
                    || self
                        .route_acknowledged(event, Some(controller_epoch))
                        .is_err()
                {
                    let _ = self.token.restore(token);
                    self.fail_without_reopening();
                    return WorkOutcome::Complete;
                }
                self.finish(token)
            }
            DeferredIrqProgress::Unhandled => self.finish(token),
            DeferredIrqProgress::Failed(_) => {
                let _ = self.token.restore(token);
                self.fail_without_reopening();
                WorkOutcome::Complete
            }
        }
    }

    fn finish(&'static self, token: IrqContinuationToken) -> WorkOutcome {
        self.clear_pending();
        if ax_hal::irq::finish_irq_continuation(token).is_err() {
            self.fail_without_reopening();
        }
        self.owner_link.wake_recovery();
        WorkOutcome::Complete
    }

    fn clear_pending(&self) {
        self.pending_queues.store(0, Ordering::Relaxed);
        self.pending_controller_epoch.store(0, Ordering::Relaxed);
        self.deferred_attempts.store(0, Ordering::Relaxed);
        self.pending_epoch.store(0, Ordering::Release);
    }

    fn fail_without_reopening(&'static self) {
        self.failed.store(true, Ordering::Release);
        let queue_id = self.routes.first().map_or(0, |queue| queue.info().id);
        let _ = self.owner_link.request_irq_recovery(queue_id);
    }

    /// Releases a retained token only after controller code masked the device.
    pub(super) fn finish_after_device_masked(&'static self) -> bool {
        if self.pending_epoch.load(Ordering::Acquire) == 0 {
            return true;
        }
        let Some(token) = self.token.take() else {
            // The affinity worker still owns the token. It always wakes
            // recovery after finishing or restoring it.
            return false;
        };
        self.clear_pending();
        ax_hal::irq::finish_irq_continuation(token).is_ok()
    }
}

unsafe fn source_continuation_wake(data: usize, token: IrqContinuationToken) {
    let source = unsafe {
        // SAFETY: callback data points to a leaked RuntimeIrqSource and the IRQ
        // framework holds the registered action until this callback returns.
        &*ptr::with_exposed_provenance::<RuntimeIrqSource>(data)
    };
    if source.token.publish(token).is_err() || !source.queue_work() {
        // The framework still records the active generation and keeps the line
        // masked even if the sole token handoff cannot be serviced.
        source.fail_without_reopening();
    }
}

fn source_work_entry(data: usize) -> WorkOutcome {
    let source = unsafe {
        // SAFETY: work callback data follows the same leaked source contract as
        // the continuation wake target.
        &*ptr::with_exposed_provenance::<RuntimeIrqSource>(data)
    };
    source.service_continuation()
}

//! Hard-IRQ completion capture and logical-vector topology.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};

use rdif_block::{Event, IdList, IrqHandler, IrqSourceInfo, IrqSourceList};

use super::{CompletionDrain, NvmeBlockOwner};

const IRQ_COMPLETION_BUDGET: usize = 64;

struct IrqCompletionBudget {
    remaining: usize,
}

pub(super) fn vector_for_queue(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_id: usize,
) -> Option<u16> {
    if msix_interrupts {
        vectors.get(queue_id).copied()
    } else {
        Some(0)
    }
}

pub(super) fn queue_interrupt_sources(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_id: usize,
) -> IdList {
    let mut sources = IdList::none();
    if let Some(source_id) = vector_for_queue(msix_interrupts, vectors, queue_id) {
        sources.insert(usize::from(source_id));
    }
    sources
}

pub(super) fn source_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    source_id: usize,
    queue_bits: u64,
) -> u64 {
    if !msix_interrupts {
        return if source_id == 0 { queue_bits } else { 0 };
    }

    let mut bits = 0;
    for queue_id in 0..u64::BITS as usize {
        if queue_bits & (1 << queue_id) == 0 {
            continue;
        }
        if vector_for_queue(msix_interrupts, vectors, queue_id) == Some(source_id as u16) {
            bits |= 1 << queue_id;
        }
    }
    bits
}

pub(super) fn irq_sources_from_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_bits: u64,
) -> IrqSourceList {
    if !msix_interrupts {
        return vec![IrqSourceInfo::legacy(IdList::from_bits(queue_bits))];
    }

    let mut sources = Vec::new();
    for vector in unique_interrupt_vectors(vectors) {
        let queues = source_queue_bits(msix_interrupts, vectors, usize::from(vector), queue_bits);
        if queues != 0 {
            sources.push(IrqSourceInfo::new(
                usize::from(vector),
                IdList::from_bits(queues),
            ));
        }
    }
    sources
}

pub(super) fn unique_interrupt_vectors(vectors: &[u16]) -> Vec<u16> {
    let mut unique = Vec::new();
    for vector in vectors {
        if !unique.contains(vector) {
            unique.push(*vector);
        }
    }
    unique
}

pub(super) fn new_initial_irq_handler(owner: Arc<NvmeBlockOwner>) -> Box<dyn IrqHandler> {
    Box::new(NvmeInitialIrqHandler { owner })
}

pub(super) fn new_queue_irq_handler(
    owner: Arc<NvmeBlockOwner>,
    source_id: usize,
) -> Box<dyn IrqHandler> {
    Box::new(NvmeBlockIrqHandler { owner, source_id })
}

struct NvmeInitialIrqHandler {
    owner: Arc<NvmeBlockOwner>,
}

impl IrqHandler for NvmeInitialIrqHandler {
    fn handle_irq(&mut self) -> rdif_block::IrqOutcome {
        if !self.owner.irq_delivery_enabled() {
            return rdif_block::IrqOutcome::unhandled();
        }
        captured_irq_outcome(Event::none(), self.owner.drain_admin_irq_completion())
    }
}

struct NvmeBlockIrqHandler {
    owner: Arc<NvmeBlockOwner>,
    source_id: usize,
}

impl IrqHandler for NvmeBlockIrqHandler {
    fn handle_irq(&mut self) -> rdif_block::IrqOutcome {
        if !self.owner.irq_delivery_enabled() {
            return rdif_block::IrqOutcome::unhandled();
        }
        let admin_completed = self.owner.admin_irq_source_id() == Some(self.source_id)
            && self.owner.drain_admin_irq_completion();
        let mut completion_budget = IrqCompletionBudget::after_admin(admin_completed);
        let mut event = Event::none();
        let source_queue_bits = self
            .owner
            .source_queue_bits(self.source_id, self.owner.created_queue_bits());
        for queue in self.owner.queues() {
            if source_queue_bits & (1 << queue.id()) == 0 {
                continue;
            }
            capture_queue_irq(
                queue.id(),
                &mut completion_budget,
                &mut event,
                |budget| queue.drain_irq_completions(budget),
                || queue.request_irq_completion_continuation(),
            );
        }
        captured_irq_outcome(event, admin_completed)
    }
}

fn capture_queue_irq(
    queue_id: usize,
    completion_budget: &mut IrqCompletionBudget,
    event: &mut Event,
    drain: impl FnOnce(usize) -> CompletionDrain,
    request_continuation: impl FnOnce(),
) {
    if completion_budget.is_exhausted() {
        // This queue was named by the acknowledged source but was not
        // inspected. Publish both the worker event and its one-shot CQ credit;
        // an event alone is intentionally forbidden from reading a CQ.
        request_continuation();
        event.push_queue(queue_id);
        return;
    }
    let drain = drain(completion_budget.remaining());
    completion_budget.consume(drain.completed);
    if drain.needs_service() {
        event.push_queue(queue_id);
    }
}

fn captured_irq_outcome(event: Event, control_completed: bool) -> rdif_block::IrqOutcome {
    if event.is_empty() && control_completed {
        rdif_block::IrqOutcome::handled_control()
    } else {
        rdif_block::IrqOutcome::from_event(event)
    }
}

impl IrqCompletionBudget {
    const fn after_admin(admin_completed: bool) -> Self {
        Self {
            remaining: IRQ_COMPLETION_BUDGET - admin_completed as usize,
        }
    }

    const fn remaining(&self) -> usize {
        self.remaining
    }

    const fn is_exhausted(&self) -> bool {
        self.remaining == 0
    }

    fn consume(&mut self, completed: usize) {
        debug_assert!(
            completed <= self.remaining,
            "NVMe IRQ completion drain exceeded its hard-IRQ budget"
        );
        self.remaining = self.remaining.saturating_sub(completed);
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use rdif_block::Event;

    use super::{
        IRQ_COMPLETION_BUDGET, IrqCompletionBudget, capture_queue_irq, captured_irq_outcome,
    };

    #[test]
    fn admin_and_io_queues_share_one_sixty_four_completion_irq_budget() {
        let mut budget = IrqCompletionBudget::after_admin(true);

        assert_eq!(budget.remaining(), IRQ_COMPLETION_BUDGET - 1);
        budget.consume(31);
        budget.consume(32);
        assert!(budget.is_exhausted());
    }

    #[test]
    fn captured_nvme_irq_never_requests_task_side_destructive_acknowledgement() {
        let control = captured_irq_outcome(Event::none(), true);
        assert!(control.is_handled());
        assert!(!control.is_deferred());

        let queues = captured_irq_outcome(Event::from_queue_bits(1), false);
        assert!(queues.is_handled());
        assert!(!queues.is_deferred());

        let empty = captured_irq_outcome(Event::none(), false);
        assert!(!empty.is_handled());
        assert!(!empty.is_deferred());
    }

    #[test]
    fn queue_skipped_by_shared_irq_budget_receives_continuation_credit() {
        let mut budget = IrqCompletionBudget::after_admin(false);
        budget.consume(IRQ_COMPLETION_BUDGET);
        let mut event = Event::none();
        let continuation_requested = Cell::new(false);

        capture_queue_irq(
            3,
            &mut budget,
            &mut event,
            |_| panic!("an exhausted hard-IRQ budget must not inspect another CQ"),
            || continuation_requested.set(true),
        );

        assert!(!event.is_empty());
        assert!(
            continuation_requested.get(),
            "the queue worker needs an IRQ continuation credit before it may consume the CQ"
        );
    }
}

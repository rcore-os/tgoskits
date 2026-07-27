//! Generation-graced intrusive MPSC head publication.
//!
//! A Treiber producer retains the observed head pointer until its CAS finishes.
//! Detaching that head concurrently would let the consumer release the pointed
//! allocation and turn a later same-address CAS into a provenance-losing ABA.
//!
//! This primitive maps monotonic publication generations onto two heads. A
//! producer counts itself in the generation's slot and rechecks the complete
//! generation before it may read that head. The single consumer can then detach
//! a retired generation after only that slot reaches a grace period; a storm of
//! producers in the new generation cannot delay old-head reclamation.

use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering, fence},
};

use crate::runtime::task_runtime;

const SLOT_COUNT: usize = 2;
const NO_RETIRING_GENERATION: usize = usize::MAX;
const PUBLISHER_OVERFLOW_INVARIANT: u32 = 0x494e_0002;
const PUBLISHER_UNDERFLOW_INVARIANT: u32 = 0x494e_0003;
const GENERATION_EXHAUSTED_INVARIANT: u32 = 0x494e_0004;

/// Two-slot intrusive publication state shared by one MPSC inbox.
#[derive(Debug)]
pub(crate) struct EpochMpscQueue<Node> {
    heads: [AtomicPtr<Node>; SLOT_COUNT],
    active_generation: AtomicUsize,
    slot_publishers: [AtomicUsize; SLOT_COUNT],
    retiring_generation: AtomicUsize,
    #[cfg(test)]
    head_publish_test_stage: AtomicUsize,
    #[cfg(test)]
    generation_publish_test_stage: AtomicUsize,
}

impl<Node> EpochMpscQueue<Node> {
    pub(crate) const fn new() -> Self {
        Self {
            heads: [
                AtomicPtr::new(ptr::null_mut()),
                AtomicPtr::new(ptr::null_mut()),
            ],
            active_generation: AtomicUsize::new(0),
            slot_publishers: [AtomicUsize::new(0), AtomicUsize::new(0)],
            retiring_generation: AtomicUsize::new(NO_RETIRING_GENERATION),
            #[cfg(test)]
            head_publish_test_stage: AtomicUsize::new(0),
            #[cfg(test)]
            generation_publish_test_stage: AtomicUsize::new(0),
        }
    }

    /// Reports whether a published or grace-period-protected head remains.
    pub(crate) fn is_empty(&self) -> bool {
        self.retiring_generation.load(Ordering::Acquire) == NO_RETIRING_GENERATION
            && self.heads[0].load(Ordering::Acquire).is_null()
            && self.heads[1].load(Ordering::Acquire).is_null()
    }

    /// Publishes `node` and reports an empty-to-nonempty transition in its slot.
    ///
    /// # Safety
    ///
    /// `node` must remain pinned and live through the queue membership created by
    /// a successful return. `next` must be the intrusive link belonging to
    /// `node`, be exclusively producer-owned until publication completes, and
    /// `node` must be absent from both heads before this call.
    pub(crate) unsafe fn publish(&self, node: *mut Node, next: &AtomicPtr<Node>) -> bool {
        let publisher = GenerationPublisher::enter_stable(self);
        let head = &self.heads[publisher.slot];
        let mut observed = head.load(Ordering::Acquire);
        #[cfg(test)]
        self.pause_test_publisher_after_head_load();

        loop {
            next.store(observed, Ordering::Relaxed);
            match head.compare_exchange_weak(observed, node, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return observed.is_null(),
                Err(updated) => observed = updated,
            }
        }
    }

    /// Detaches one fully graced producer stack, or returns null without waiting.
    ///
    /// The first call that observes work advances the active generation. If a
    /// producer can still retain an address from the old head, this and
    /// subsequent calls return null until its fixed publication critical section
    /// finishes. Producers that begin against the new generation use the other
    /// slot and therefore cannot prolong that grace period.
    ///
    /// # Safety
    ///
    /// Only the queue's single consumer may call this function. The returned
    /// stack is exclusively consumer-owned, and the consumer must preserve every
    /// node's queue lifetime until it removes that node from the detached stack.
    pub(crate) unsafe fn take_graced_stack(&self) -> *mut Node {
        let mut retiring = self.retiring_generation.load(Ordering::SeqCst);
        if retiring == NO_RETIRING_GENERATION {
            let active = self.active_generation.load(Ordering::SeqCst);
            let active_slot = generation_slot(active);
            if self.heads[active_slot].load(Ordering::Acquire).is_null() {
                return ptr::null_mut();
            }

            let Some(next) = active.checked_add(1) else {
                task_runtime::fatal_invariant(GENERATION_EXHAUSTED_INVARIANT, active);
            };
            if next == NO_RETIRING_GENERATION {
                task_runtime::fatal_invariant(GENERATION_EXHAUSTED_INVARIANT, active);
            }
            self.active_generation.store(next, Ordering::SeqCst);
            self.retiring_generation.store(active, Ordering::SeqCst);
            // Pair with publisher registration. The consumer must not both miss
            // a slot increment and let that publisher miss this generation
            // advance before the publisher reads the retired head.
            fence(Ordering::SeqCst);
            retiring = active;
        }

        let retiring_slot = generation_slot(retiring);
        if self.slot_publishers[retiring_slot].load(Ordering::SeqCst) != 0 {
            return ptr::null_mut();
        }

        let stack = self.heads[retiring_slot].swap(ptr::null_mut(), Ordering::AcqRel);
        self.retiring_generation
            .store(NO_RETIRING_GENERATION, Ordering::SeqCst);
        stack
    }

    #[cfg(test)]
    pub(crate) fn arm_test_publisher_pause(&self) {
        arm_test_pause(&self.head_publish_test_stage, "publication");
    }

    #[cfg(test)]
    pub(crate) fn wait_for_test_publisher_pause(&self) {
        wait_for_test_pause(&self.head_publish_test_stage);
    }

    #[cfg(test)]
    pub(crate) fn resume_test_publisher(&self) {
        resume_test_pause(&self.head_publish_test_stage, "publisher");
    }

    #[cfg(test)]
    fn pause_test_publisher_after_head_load(&self) {
        pause_test_point(&self.head_publish_test_stage);
    }

    #[cfg(test)]
    pub(crate) fn arm_test_generation_pause(&self) {
        arm_test_pause(&self.generation_publish_test_stage, "generation");
    }

    #[cfg(test)]
    pub(crate) fn wait_for_test_generation_pause(&self) {
        wait_for_test_pause(&self.generation_publish_test_stage);
    }

    #[cfg(test)]
    pub(crate) fn resume_test_generation_publisher(&self) {
        resume_test_pause(&self.generation_publish_test_stage, "generation publisher");
    }

    #[cfg(test)]
    fn pause_test_publisher_after_generation_load(&self) {
        pause_test_point(&self.generation_publish_test_stage);
    }
}

/// Counts one producer against the generation slot whose head it may retain.
struct GenerationPublisher<'queue, Node> {
    queue: &'queue EpochMpscQueue<Node>,
    generation: usize,
    slot: usize,
}

impl<'queue, Node> GenerationPublisher<'queue, Node> {
    fn enter_stable(queue: &'queue EpochMpscQueue<Node>) -> Self {
        loop {
            let generation = queue.active_generation.load(Ordering::SeqCst);
            #[cfg(test)]
            queue.pause_test_publisher_after_generation_load();
            let slot = generation_slot(generation);
            increment_counter(
                &queue.slot_publishers[slot],
                PUBLISHER_OVERFLOW_INVARIANT,
                generation,
            );
            // Pair with generation retirement as a Dekker-style registration
            // handshake across two independent atomics.
            fence(Ordering::SeqCst);
            let publisher = Self {
                queue,
                generation,
                slot,
            };
            if queue.active_generation.load(Ordering::SeqCst) == generation {
                return publisher;
            }
            drop(publisher);
        }
    }
}

impl<Node> Drop for GenerationPublisher<'_, Node> {
    fn drop(&mut self) {
        let publishers = self.queue.slot_publishers[self.slot].fetch_sub(1, Ordering::SeqCst);
        if publishers == 0 {
            task_runtime::fatal_invariant(PUBLISHER_UNDERFLOW_INVARIANT, self.generation);
        }
    }
}

const fn generation_slot(generation: usize) -> usize {
    generation & (SLOT_COUNT - 1)
}

fn increment_counter(counter: &AtomicUsize, invariant: u32, argument: usize) {
    if counter.fetch_add(1, Ordering::SeqCst) == usize::MAX {
        task_runtime::fatal_invariant(invariant, argument);
    }
}

#[cfg(test)]
fn arm_test_pause(stage: &AtomicUsize, name: &str) {
    assert_eq!(
        stage.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst),
        Ok(0),
        "only one {name} pause may be armed per inbox"
    );
}

#[cfg(test)]
fn wait_for_test_pause(stage: &AtomicUsize) {
    while stage.load(Ordering::SeqCst) != 2 {
        std::thread::yield_now();
    }
}

#[cfg(test)]
fn resume_test_pause(stage: &AtomicUsize, name: &str) {
    assert_eq!(
        stage.compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst),
        Ok(2),
        "{name} must be paused before it is resumed"
    );
}

#[cfg(test)]
fn pause_test_point(stage: &AtomicUsize) {
    if stage
        .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    while stage.load(Ordering::SeqCst) == 2 {
        std::thread::yield_now();
    }
    stage.store(0, Ordering::SeqCst);
}

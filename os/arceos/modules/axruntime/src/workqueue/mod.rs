//! Allocation-free workqueue state and bounded shared-worker execution.
//!
//! The generic [`WorkQueueSystem`] and intrusive [`WorkItem`] state machine are
//! independent of scheduler threads. With the `workqueue` feature, ax-runtime
//! installs one normal and one high-priority shared worker per online CPU;
//! logical [`WorkQueue`] domains share those workers and own only affinity,
//! admission, flush, and drain policy.
//!
//! [`DelayedWork`] embeds both its user work and a target-CPU control item. IRQ
//! and remote producers publish only an atomic deadline command. The control
//! item is the sole timer-heap mutator, while timer IRQ writes fixed ax-task
//! expiration storage and the scheduler safe point queues the user work.

include!("types.rs");
#[cfg(feature = "workqueue")]
include!("runtime.rs");
#[cfg(feature = "workqueue")]
include!("delayed.rs");
include!("model.rs");

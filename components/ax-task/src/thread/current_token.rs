//! Typed identity capability for the executing scheduler thread.

use core::marker::PhantomData;

use super::ThreadId;

/// Move-only proof of the scheduler thread executing this task context.
///
/// Scheduler-adjacent primitives may retain this token on the current stack
/// across preemption and park/resume, then reuse it for bounded metadata
/// transitions. It owns no scheduler resource and cannot cross threads.
#[derive(Debug)]
pub struct CurrentThreadToken {
    thread: ThreadId,
    _not_send: PhantomData<*mut ()>,
}

impl CurrentThreadToken {
    pub(crate) const fn new(thread: ThreadId) -> Self {
        Self {
            thread,
            _not_send: PhantomData,
        }
    }

    /// Returns the generation-bearing identity captured for this execution.
    pub const fn id(&self) -> ThreadId {
        self.thread
    }
}
